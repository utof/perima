//! Tauri IPC commands exposed to the frontend.
//!
//! After the Batch B Task 9 migration (#69 consolidation), every migrated
//! handler delegates to one of the `UseCase` fields on `AppState.container`
//! via a short `container.xx.execute(cmd).await` call. Pre-existing
//! `_inner` helpers are retained unchanged so `crates/desktop/tests/
//! commands_test.rs` keeps exercising the underlying logic without
//! constructing `tauri::State` (those helpers re-open per-call connections
//! exactly like the pre-migration code path did).
//!
//! WHY two styles coexist: the `#[tauri::command]` production path is
//! thin-delegation to `container.*.execute`; the `_inner` helpers remain
//! as a test seam until a future batch replaces them with a
//! container-based test harness.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use perima_app::{
    FullScan, MetadataCommand, MetadataOutput, ScanCommand, SearchCommand, SearchOutput,
    TagCommand, TagFilter, TagOutput, VolumeCommand, VolumeOutput,
};
use perima_core::{
    CoreError, DeviceId, EventBus, FileEvent, LocationStatus, MetadataExtractor,
    MetadataRepository, SearchRepository, TagRepository, VolumeId,
};
use perima_db::{ReadPool, SqliteFileRepository, SqliteVolumeRepository, SqliteWriter};
use perima_fs::{DebouncedWatcher, WalkdirScanner};
use perima_hash::Blake3Service;
use perima_media::{
    CompositeExtractor, ImageExtractor, MetadataQueue, ThumbnailGenerator, VideoExtractor,
};
use rayon::prelude::*;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::payloads::{FileWithMetadataPayload, FileWithTagsPayload, SearchHitPayload, TagPayload};
use crate::state::{AppState, WatcherState};

/// Maximum time `scan` waits for the metadata worker to drain after
/// the walk loop completes.
///
/// WHY 30 s: mirrors the CLI's `METADATA_DRAIN_TIMEOUT`. Long enough
/// for a typical small-tree scan to complete comfortably, short enough
/// that a stuck extractor cannot hang the Tauri command indefinitely.
const METADATA_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Event handlers
//
// WHY `DbEventHandler` stays shell-local: it touches
// `SqliteFileRepository` (a `crates/db` concrete adapter) via
// `Arc<SqliteFileRepository>`. Hoisting it into `perima-app` would
// force the app crate to depend on a concrete adapter, violating the
// ports-and-adapters boundary. `LogEventHandler` — previously the
// duplicate sibling in this section — was hoisted to
// `perima_app::telemetry` in Task 10 because it has zero adapter
// coupling. The shell-local `CompositeEventBus` struct that lived
// here pre-Task-9 is also deleted — spec §4 acceptance demands
// exactly one `CompositeEventBus::new` call in the codebase and that
// site is now `crates/app/src/container.rs::AppContainer::new`.
// ---------------------------------------------------------------------------

/// Updates the database in response to filesystem events.
///
/// WHY `Arc<SqliteFileRepository>`: `EventBus` requires `Send + Sync`.
/// `SqliteFileRepository` uses `Mutex<Connection>` internally, satisfying both.
pub struct DbEventHandler {
    repo: Arc<SqliteFileRepository>,
    device: DeviceId,
}

impl DbEventHandler {
    /// Construct a [`DbEventHandler`] bound to the given file repository
    /// and device.
    ///
    /// WHY a `new` constructor: `lib.rs::setup` builds the handler before
    /// `AppContainer::new` wraps it into the single `CompositeEventBus`.
    /// Keeping the struct fields private + exposing `new` preserves
    /// encapsulation across the crate boundary.
    #[must_use]
    pub const fn new(repo: Arc<SqliteFileRepository>, device: DeviceId) -> Self {
        Self { repo, device }
    }
}

impl EventBus for DbEventHandler {
    fn emit(&self, event: &FileEvent) -> Result<(), CoreError> {
        match event {
            FileEvent::Created { path, .. } => {
                // WHY: we do not hash new files in watch mode. Hashing requires
                // reading the entire file which could be slow or incomplete for
                // large files mid-write. A subsequent `perima scan` will index it.
                tracing::info!(
                    path = path.as_str(),
                    "new file detected; run scan to index it"
                );
                Ok(())
            }
            FileEvent::Modified { path, volume } => {
                let n = self.repo.update_location_status(
                    *volume,
                    path,
                    LocationStatus::Stale,
                    self.device,
                )?;
                tracing::debug!(path = path.as_str(), rows_affected = n, "marked stale");
                Ok(())
            }
            FileEvent::Deleted { path, volume } => {
                let n = self.repo.update_location_status(
                    *volume,
                    path,
                    LocationStatus::Missing,
                    self.device,
                )?;
                tracing::debug!(path = path.as_str(), rows_affected = n, "marked missing");
                Ok(())
            }
            FileEvent::Renamed { from, to, volume } => {
                let n = self
                    .repo
                    .update_location_path(*volume, from, to, self.device)?;
                tracing::debug!(
                    from = from.as_str(),
                    to = to.as_str(),
                    rows_affected = n,
                    "renamed location"
                );
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Wire-types
//
// WHY wrapper structs: `FileLocationRecord` and `VolumeRecord` live in
// `perima-core` which has zero framework dependencies. Adding `specta` to
// core would violate that constraint. Thin wrappers here carry `specta::Type`
// without touching the core domain types.
// ---------------------------------------------------------------------------

/// Scan statistics returned to the frontend.
#[derive(Debug, Clone, Copy, Serialize, specta::Type)]
pub struct ScanResult {
    /// Total files encountered (new + existing + errors).
    pub total: u64,
    /// Files newly inserted into the index.
    pub new: u64,
    /// Files already indexed (unchanged or updated).
    pub existing: u64,
    /// Files skipped due to hash or persist errors.
    pub errors: u64,
}

/// Wire-type for a file location record, safe to cross the IPC boundary.
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct FileEntry {
    /// BLAKE3-256 content hash as hex string.
    pub hash: String,
    /// File size in bytes.
    pub size: u64,
    /// Volume UUID.
    pub volume_id: String,
    /// Relative path within the volume.
    pub relative_path: String,
    /// Location status string.
    pub status: String,
    /// ISO 8601 UTC timestamp of first sighting.
    pub first_seen: String,
}

impl From<perima_core::FileLocationRecord> for FileEntry {
    fn from(r: perima_core::FileLocationRecord) -> Self {
        Self {
            hash: r.hash.to_hex(),
            size: r.size.0,
            volume_id: r.volume_id.0.to_string(),
            relative_path: r.relative_path.as_str().to_owned(),
            status: format!("{:?}", r.status),
            first_seen: r.first_seen,
        }
    }
}

/// Wire-type for a volume record, safe to cross the IPC boundary.
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct VolumeEntry {
    /// Volume UUID.
    pub id: String,
    /// Volume label if any.
    pub label: Option<String>,
    /// Total capacity in bytes.
    pub capacity_bytes: u64,
    /// Whether the OS reports this as removable.
    pub is_removable: bool,
    /// Mount paths on this machine as strings.
    pub mounts_on_this_machine: Vec<String>,
    /// ISO 8601 UTC timestamp of last sighting.
    pub last_seen: String,
}

impl From<perima_core::VolumeRecord> for VolumeEntry {
    fn from(r: perima_core::VolumeRecord) -> Self {
        Self {
            id: r.id.0.to_string(),
            label: r.label,
            capacity_bytes: r.capacity_bytes,
            is_removable: r.is_removable,
            mounts_on_this_machine: r
                .mounts_on_this_machine
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            last_seen: r.last_seen,
        }
    }
}

/// Callback type for the sentinel migration; factored out to avoid
/// `clippy::type_complexity` on the `run_scan_live` signature.
///
/// WHY type alias: the full `Option<&dyn Fn(...)>` signature trips
/// `clippy::type_complexity`. This alias keeps call sites readable.
///
/// WHY `Send + Sync` bounds: the async `scan` command's future must be
/// `Send` (Tauri's command dispatcher requires it). Holding a plain
/// `&dyn Fn(...)` across the queue-drain `.await` pins the future to a
/// non-Send type. The sentinel closure wraps a `SqliteFileRepository`
/// whose `Mutex<Connection>` is already `Send + Sync`, so adding the
/// bound here is a documentation change, not a behavioural one.
pub type OnPersistFn<'a> =
    Option<&'a (dyn Fn(&perima_core::MediaPath, VolumeId, DeviceId) + Send + Sync)>;

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Walk `path`, hash every file, and persist results to the database.
///
/// Returns a [`ScanResult`] with per-category file counts. When `dry_run` is
/// true, hashing still occurs but nothing is written to the DB.
///
/// # Errors
/// Returns a `String` description of any [`perima_core::CoreError`] that
/// surfaces during volume detection, walking, hashing, or persistence.
// WHY allow: Tauri requires `State<'_, T>` and `String` params to be owned.
// The lint fires because we immediately borrow through Deref, but we cannot
// change the signature — Tauri's command dispatcher owns these values.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn scan(
    path: String,
    dry_run: bool,
    state: tauri::State<'_, AppState>,
) -> Result<ScanResult, String> {
    let cmd = ScanCommand::Full(FullScan {
        path: PathBuf::from(&path),
        device_id: state.device_id,
        // WHY `with_metadata = !dry_run`: preserves the pre-migration
        // desktop default — every non-dry scan spawns the metadata queue.
        with_metadata: !dry_run,
        dry_run,
        // WHY no_wait_metadata = false: the Tauri command blocks until
        // the bounded drain completes so the frontend sees a fully
        // populated metadata + thumbnails set by the time `scan` returns.
        no_wait_metadata: false,
        // WHY no_thumbnails = false: the desktop UI grid depends on
        // WebP thumbnails in `<data_dir>/thumbnails/**`; disabling them
        // would yield an empty grid.
        no_thumbnails: false,
        // WHY fresh `CancellationToken::new()`: the desktop has no
        // Ctrl-C handler today (users close the window). A cancel RPC
        // is future work (Batch E); for now we hand the UseCase a
        // never-cancelled token.
        cancel: CancellationToken::new(),
        on_persist: None,
    });
    let report = state
        .container
        .scan
        .execute(cmd)
        .await
        .map_err(|e| e.to_string())?;

    // WHY write_manifest stays in the shell: per spec §2 IN, `crates/app`
    // deliberately does not depend on `perima-db`. The `ScanReport`
    // surfaces `volume_mount` + `manifest_files` for the shell to wire
    // manifest persistence; the CLI does the same in `crates/cli/src/
    // cmd/scan.rs::run`.
    if let Some((vol_id, mount)) = report.volume_mount.as_ref() {
        perima_db::manifest::write_manifest(mount, *vol_id, &report.manifest_files)
            .map_err(|e| e.to_string())?;
    }

    Ok(ScanResult {
        total: report.files_new + report.files_updated + report.files_errored,
        new: report.files_new,
        existing: report.files_updated,
        errors: report.files_errored,
    })
}

/// Inner scan logic extracted for testability without a live Tauri state.
///
/// WHY: `tauri::State` cannot be constructed outside a running Tauri app, so
/// integration tests call this function directly with plain `Path`/`DeviceId`
/// arguments. This overload skips the metadata queue; use
/// [`run_scan_inner_with_metadata`] to exercise the full extract + thumbnail
/// pipeline.
///
/// WHY retained post-Batch-B: `crates/desktop/tests/commands_test.rs`
/// references this function directly (search for `run_scan_inner`). The
/// production `scan` command delegates through `AppContainer` instead;
/// this helper keeps the test seam alive until a container-based test
/// harness lands in a follow-up batch.
///
/// # Errors
/// Returns [`perima_core::CoreError`] on filesystem, volume detection, hash,
/// or database failures.
pub async fn run_scan_inner(
    root: &Path,
    dry_run: bool,
    data_dir: &Path,
    device_id: DeviceId,
) -> Result<ScanResult, perima_core::CoreError> {
    run_scan_inner_with_metadata(root, dry_run, data_dir, device_id, None).await
}

/// Inner scan logic with optional metadata-queue wiring.
///
/// When `metadata_repo` is `Some`, a [`MetadataQueue`] is spawned
/// before the walk; every successful `Inserted`/`Updated` upsert
/// enqueues `(hash, absolute_path)`. The queue is drained with a
/// bounded 30 s timeout at exit.
///
/// The thumbnailer is rooted at `data_dir` (mirrors the CLI pattern)
/// so generated WebP files live under `<data_dir>/thumbnails/...` —
/// the same directory the Tauri asset protocol scope exposes.
///
/// WHY retained post-Batch-B: see `run_scan_inner`. This helper is
/// referenced directly by the `desktop_scan_populates_metadata_and_thumbnails`
/// regression test.
///
/// # Errors
/// Returns [`perima_core::CoreError`] on filesystem, volume detection, hash,
/// or database failures.
pub async fn run_scan_inner_with_metadata(
    root: &Path,
    dry_run: bool,
    data_dir: &Path,
    device_id: DeviceId,
    metadata_repo: Option<Arc<dyn MetadataRepository>>,
) -> Result<ScanResult, perima_core::CoreError> {
    validate_root(root)?;
    // WHY: routes through perima_fs::platform_path::canonicalize — the single
    // source of truth for the #[cfg(windows)] dunce / std fallback.
    let canonical_root =
        perima_fs::platform_path::canonicalize(root).map_err(perima_core::CoreError::from)?;
    let scanner = WalkdirScanner::new();
    let hasher = Blake3Service::new();

    // WHY fresh `CancellationToken`: no signal handler in the desktop
    // backend (users close the window rather than issuing Ctrl-C).
    // A proper cancellation channel will be introduced when the
    // file-watcher IPC arrives and we need a cancel RPC.
    let never_cancel = CancellationToken::new();

    if dry_run {
        return run_scan_dry(&scanner, &hasher, &canonical_root, &never_cancel);
    }

    let db_path = data_dir.join("perima.db");
    // WHY a self-contained writer+pool here (test-only seam): this
    // helper exists purely for `crates/desktop/tests/commands_test.rs`
    // to exercise scan logic without constructing `tauri::State`. Its
    // production peer (the `#[tauri::command] scan` handler) delegates
    // to `AppContainer.volume` via `state.container`. The writer
    // handle is dropped at end of scope — its `Sender` is held via
    // `vol_repo` + `file_repo` + `sentinel_repo` for the duration of
    // this function call.
    struct NoopBus;
    impl EventBus for NoopBus {
        fn emit(&self, _: &FileEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }
    let writer = SqliteWriter::start(&db_path, Arc::new(NoopBus))?;
    let reads = ReadPool::open(&db_path)?;

    // WHY clone `reads` for each adapter: `ReadPool` is cheap to
    // [`Clone`] (inner `r2d2::Pool` is `Arc`-backed).
    let file_repo = SqliteFileRepository::new(writer.sender(), reads.clone());
    let vol_repo = SqliteVolumeRepository::new(writer.sender(), reads.clone());
    let sentinel_repo = SqliteFileRepository::new(writer.sender(), reads);

    let on_persist = |path: &perima_core::MediaPath, volume: VolumeId, dev: DeviceId| {
        if let Err(e) = sentinel_repo.migrate_sentinel_row(path, volume, dev) {
            tracing::warn!(error = %e, "sentinel migration failed (non-fatal)");
        }
    };

    // WHY thumbnailer rooted at `data_dir`: the Tauri asset-protocol
    // scope (`tauri.conf.json`) exposes
    // `$APPDATA/perima/thumbnails/**`; resolving the generator to the
    // same directory tree keeps `convertFileSrc` calls from the
    // frontend working end-to-end.
    let thumbnailer: Arc<ThumbnailGenerator> =
        Arc::new(ThumbnailGenerator::new(data_dir.to_path_buf()));

    let result = run_scan_live(
        &scanner,
        &hasher,
        &file_repo,
        &vol_repo,
        Some(&on_persist),
        device_id,
        &canonical_root,
        &never_cancel,
        metadata_repo,
        thumbnailer,
    )
    .await;

    // WHY explicit join: flush any pending writer commands AND reap the
    // writer thread before this function returns. Without the drop
    // ordering here, `writer` and `vol_repo` drop in definition order —
    // `writer` first (reverse of declaration) — leaving the vol_repo
    // sender orphaned momentarily. Explicit drop → join pattern makes
    // the teardown deterministic for the test seam.
    drop(vol_repo);
    writer.join();

    result
}

/// List indexed file locations, optionally filtered by volume.
///
/// Returns up to `limit` [`FileEntry`] records ordered by relative path.
///
/// # Errors
/// Returns a `String` description of any [`perima_core::CoreError`].
// WHY allow: same reason as `scan` — Tauri owns `State` and `Option<String>` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn list_files(
    limit: u32,
    volume: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<FileEntry>, String> {
    let volume_id = volume
        .map(|v| {
            uuid::Uuid::parse_str(&v)
                .map(VolumeId)
                .map_err(|e| format!("bad volume UUID: {e}"))
        })
        .transpose()?;

    let out = state
        .container
        .metadata
        .execute(MetadataCommand::ListFiles {
            limit: Some(limit),
            offset: None,
            device: state.device_id,
        })
        .await
        .map_err(|e| e.to_string())?;
    let MetadataOutput::Files(records) = out else {
        return Err("ListFiles returned non-Files output".to_owned());
    };

    // WHY post-filter by volume in the shell: `MetadataCommand::ListFiles`
    // does not yet accept a volume filter (Batch B kept its surface
    // narrow); filtering in memory after the UseCase returns matches the
    // CLI `ls.rs` pattern for the same constraint.
    let filtered: Vec<FileEntry> = records
        .into_iter()
        .filter(|r| volume_id.is_none_or(|v| r.volume_id == v))
        .map(FileEntry::from)
        .collect();
    Ok(filtered)
}

/// Inner list-files logic extracted for testability without a live Tauri state.
///
/// WHY: mirrors `run_scan_inner` — allows integration tests to exercise the
/// list path without constructing `tauri::State`.
///
/// # Errors
/// Returns [`perima_core::CoreError`] on database open or query failure.
pub fn list_files_inner(
    data_dir: &Path,
    limit: u32,
    volume_id: Option<VolumeId>,
) -> Result<Vec<FileEntry>, perima_core::CoreError> {
    let db_path = data_dir.join("perima.db");
    // WHY local writer+pool: this helper is a test-seam; it does not have
    // access to the AppContainer / Tauri state. The writer is dropped at
    // end of scope; the read pool keeps its connection alive until after
    // the query completes.
    struct NoopBus;
    impl EventBus for NoopBus {
        fn emit(&self, _: &FileEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }
    let writer = SqliteWriter::start(&db_path, Arc::new(NoopBus))?;
    let reads = ReadPool::open(&db_path)?;
    let repo = SqliteFileRepository::new(writer.sender(), reads);
    // WHY explicit drop: close the writer sender before returning so
    // the writer thread can exit cleanly if no other senders exist.
    drop(writer);
    let records =
        perima_core::FileRepository::list_file_locations(&repo, limit as usize, volume_id)?;
    Ok(records.into_iter().map(FileEntry::from).collect())
}

/// List indexed file locations joined with any extracted media metadata.
///
/// Returns up to `limit` [`FileWithMetadataPayload`] rows ordered by
/// relative path. Locations without a corresponding `file_metadata`
/// row surface with all metadata fields as `None` — callers should
/// treat that as "pending extraction", not "no metadata exists".
///
/// # Errors
/// Returns a `String` description of any [`perima_core::CoreError`].
// WHY allow: same reason as `scan` — Tauri owns `State` and `Option<String>` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn list_files_with_metadata(
    limit: u32,
    volume: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<FileWithMetadataPayload>, String> {
    let volume_id = volume
        .map(|v| {
            uuid::Uuid::parse_str(&v)
                .map(VolumeId)
                .map_err(|e| format!("bad volume UUID: {e}"))
        })
        .transpose()?;

    let out = state
        .container
        .metadata
        .execute(MetadataCommand::ListFilesWithMetadata {
            limit: Some(limit),
            offset: None,
            device: state.device_id,
        })
        .await
        .map_err(|e| e.to_string())?;
    let MetadataOutput::FilesWithMetadata(rows) = out else {
        return Err("ListFilesWithMetadata returned non-FilesWithMetadata output".to_owned());
    };

    // WHY post-filter by volume: see `list_files` — the `MetadataCommand`
    // variants don't expose a volume filter yet. Kept symmetric with
    // `list_files` for maintainability.
    let filtered: Vec<FileWithMetadataPayload> = rows
        .into_iter()
        .filter(|(loc, _)| volume_id.is_none_or(|v| loc.volume_id == v))
        .map(FileWithMetadataPayload::from)
        .collect();
    Ok(filtered)
}

/// Inner list-files-with-metadata logic extracted for testability without
/// a live Tauri state.
///
/// WHY: mirrors `run_scan_inner` — allows integration tests to exercise
/// the command's logic through the [`MetadataRepository`] trait without
/// constructing a `tauri::State`.
///
/// # Errors
/// Returns [`perima_core::CoreError`] on any repository failure.
pub fn list_files_with_metadata_inner<R>(
    repo: &R,
    limit: u32,
    volume: Option<VolumeId>,
) -> Result<Vec<FileWithMetadataPayload>, perima_core::CoreError>
where
    R: MetadataRepository + ?Sized,
{
    let rows = repo.list_with_metadata(limit as usize, volume)?;
    Ok(rows
        .into_iter()
        .map(FileWithMetadataPayload::from)
        .collect())
}

/// List all known volumes with their current mount paths on this machine.
///
/// # Errors
/// Returns a `String` description of any [`perima_core::CoreError`].
// WHY allow: Tauri requires `State<'_, T>` to be owned. See `scan` for rationale.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn list_volumes(state: tauri::State<'_, AppState>) -> Result<Vec<VolumeEntry>, String> {
    let out = state
        .container
        .volume
        .execute(VolumeCommand::List {
            device: state.device_id,
        })
        .await
        .map_err(|e| e.to_string())?;
    let VolumeOutput::Volumes(records) = out else {
        return Err("VolumeCommand::List returned non-Volumes output".to_owned());
    };
    Ok(records.into_iter().map(VolumeEntry::from).collect())
}

/// Inner list-volumes logic extracted for testability without a live Tauri state.
///
/// WHY: mirrors `run_scan_inner` — allows integration tests to exercise the
/// volumes list path without constructing `tauri::State`.
///
/// # Errors
/// Returns [`perima_core::CoreError`] on database open or query failure.
pub fn list_volumes_inner(
    data_dir: &Path,
    device_id: DeviceId,
) -> Result<Vec<VolumeEntry>, perima_core::CoreError> {
    // WHY a self-contained writer+pool here (test-only seam): same
    // rationale as `run_scan_inner_with_metadata`. Production
    // `#[tauri::command] list_volumes` delegates to
    // `state.container.volume.execute(List)`.
    struct NoopBus;
    impl EventBus for NoopBus {
        fn emit(&self, _: &FileEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }
    let db_path = data_dir.join("perima.db");
    let writer = SqliteWriter::start(&db_path, Arc::new(NoopBus))?;
    let reads = ReadPool::open(&db_path)?;
    let repo = SqliteVolumeRepository::new(writer.sender(), reads);
    let records = perima_core::VolumeRepository::list(&repo, device_id)?;
    drop(repo);
    writer.join();
    Ok(records.into_iter().map(VolumeEntry::from).collect())
}

// ---------------------------------------------------------------------------
// Watcher commands
// ---------------------------------------------------------------------------

/// Start watching `path` for filesystem changes.
///
/// Validates the path, detects the volume, then starts a new
/// [`DebouncedWatcher`] that forwards every filesystem event to the
/// shared `container.events` bus. The bus was assembled once at
/// `lib.rs::setup` with the `DbEventHandler`, `TauriEventEmitter`, and
/// `LogEventHandler` already wired — no second `CompositeEventBus` is
/// constructed here (spec §4 acceptance).
///
/// # Errors
/// Returns a `String` if the path is invalid, volume detection fails, or the
/// database cannot be opened or migrated.
// WHY allow needless_pass_by_value: Tauri's command dispatcher owns `State`
// and `String` params; the signature cannot be changed.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn start_watch(
    path: String,
    state: tauri::State<'_, AppState>,
    watcher_state: tauri::State<'_, WatcherState>,
) -> Result<(), String> {
    let root = PathBuf::from(&path);
    validate_root(&root).map_err(|e| e.to_string())?;
    let canonical_root =
        perima_fs::platform_path::canonicalize(&root).map_err(|e| format!("canonicalize: {e}"))?;

    // Resolve or create the volume record for this mount.
    //
    // WHY delegate to `state.container.volumes` for both `find_or_create`
    // and `record_mount` post-Batch-C Task 2: the writer actor owns the
    // sole writable connection. `find_or_create` still has no UseCase
    // surface (scan/watch startup concern); the container exposes the
    // raw `Arc<dyn VolumeRepository>` field for this purpose.
    let detected = perima_fs::detect_volume(&canonical_root).map_err(|e| e.to_string())?;
    let device_id = state.device_id;

    let volume_id = state
        .container
        .volumes
        .find_or_create(&detected.identifiers, device_id)
        .map_err(|e| e.to_string())?;

    // WHY delegate mount-recording to the VolumeUseCase: this is the
    // single call the UseCase's `RecordMount` variant was built for;
    // routing it through the container keeps the event-bus emission
    // contract consistent once Batch E wires volume events.
    state
        .container
        .volume
        .execute(VolumeCommand::RecordMount {
            volume_id,
            path: detected.mount_point.clone(),
            device: device_id,
        })
        .await
        .map_err(|e| e.to_string())?;

    // Cancel any existing watcher before starting a new one.
    {
        let mut cancel_guard = watcher_state.cancel.lock().await;
        if let Some(token) = cancel_guard.take() {
            token.cancel();
        }
    }
    // Drop any existing watcher so its OS registration is released.
    {
        let mut inner_guard = watcher_state.inner.lock().await;
        *inner_guard = None;
    }

    let cancel = CancellationToken::new();

    // WHY `Arc::clone(&state.container.events)`: the container's
    // `events` field is the already-composed `CompositeEventBus` built
    // at setup time with every shell handler (log, DB, Tauri-emit).
    // DebouncedWatcher takes an `Arc<dyn EventBus>`; cloning the Arc
    // avoids a second bus construction in this shell.
    let bus: Arc<dyn EventBus> = Arc::clone(&state.container.events);

    // WHY 1 s production debounce: short enough for responsive feedback,
    // long enough to coalesce rapid saves (e.g. editors that write-then-chmod).
    let watcher = DebouncedWatcher::start(
        std::slice::from_ref(&canonical_root),
        &canonical_root,
        volume_id,
        bus,
        cancel.clone(),
        Duration::from_secs(1),
    )
    .map_err(|e| e.to_string())?;

    {
        let mut cancel_guard = watcher_state.cancel.lock().await;
        *cancel_guard = Some(cancel);
    }
    {
        let mut inner_guard = watcher_state.inner.lock().await;
        *inner_guard = Some(watcher);
    }

    Ok(())
}

/// Stop an active filesystem watcher, if any.
///
/// Cancels the background event loop and drops the watcher, releasing the
/// OS-level watch registration.
///
/// # Errors
/// This command is currently infallible; the `Result` signature is kept for
/// consistency with `start_watch` and for forward-compatibility.
// WHY allow needless_pass_by_value: Tauri's command dispatcher owns `State`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn stop_watch(watcher_state: tauri::State<'_, WatcherState>) -> Result<(), String> {
    {
        let mut cancel_guard = watcher_state.cancel.lock().await;
        if let Some(token) = cancel_guard.take() {
            token.cancel();
        }
    }
    {
        let mut inner_guard = watcher_state.inner.lock().await;
        *inner_guard = None;
    }
    Ok(())
}

/// Returns `true` if a filesystem watcher is currently active.
///
/// # Errors
/// This command is currently infallible; the `Result` signature is kept for
/// consistency with the other watcher commands.
// WHY allow needless_pass_by_value: Tauri's command dispatcher owns `State`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn is_watching(watcher_state: tauri::State<'_, WatcherState>) -> Result<bool, String> {
    let inner_guard = watcher_state.inner.lock().await;
    Ok(inner_guard.is_some())
}

// ---------------------------------------------------------------------------
// Tag commands
// ---------------------------------------------------------------------------

/// List all active (non-deleted) tags, sorted by name.
///
/// # Errors
/// Returns a `String` description of any [`perima_core::CoreError`].
// WHY allow: Tauri requires `State<'_, T>` to be owned. See `scan` for rationale.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn list_tags(state: tauri::State<'_, AppState>) -> Result<Vec<TagPayload>, String> {
    let out = state
        .container
        .tag
        .execute(TagCommand::List)
        .await
        .map_err(|e| e.to_string())?;
    let TagOutput::Tags(tags) = out else {
        return Err("TagCommand::List returned non-Tags output".to_owned());
    };
    Ok(tags.into_iter().map(TagPayload::from).collect())
}

/// Inner list-tags logic extracted for testability without a live Tauri state.
///
/// WHY: mirrors `run_scan_inner` — allows integration tests to call without
/// constructing `tauri::State`.
///
/// # Errors
/// Returns [`perima_core::CoreError`] on any repository failure.
pub fn list_tags_inner<T: TagRepository + ?Sized>(
    tag_repo: &T,
) -> Result<Vec<TagPayload>, perima_core::CoreError> {
    let tags = tag_repo.list_tags()?;
    Ok(tags.into_iter().map(TagPayload::from).collect())
}

/// Attach a tag to a file by content hash (upsert the tag first, then attach).
///
/// Returns the [`TagPayload`] so the frontend can immediately display it
/// without a round-trip `list_tags` call.
///
/// # Errors
/// Returns a `String` if the hash is malformed, the tag name is invalid, or
/// the repository fails.
// WHY allow: Tauri owns the `State`, `String` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn attach_tag(
    hash: String,
    tag_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<TagPayload, String> {
    // WHY parse the hash + resolve tag through the shell-side
    // `state.tag_repo` instead of adding a "return the tag" variant to
    // `TagCommand::Attach`: the frontend currently expects the full
    // [`TagPayload`] (id + name + first_seen) back from `attach_tag`.
    // The `TagUseCase::Attach` response is `TagOutput::Attached(u64)`
    // — just a rows-changed count. Rather than widen the UseCase
    // output mid-batch, we do the attach via the container and then
    // read the freshly-upserted tag via the legacy `state.tag_repo`
    // handle. A future follow-up ("Attached { tag: Tag }") removes
    // this second lookup.
    let parsed_hash = perima_core::BlakeHash::parse_hex(&hash).map_err(|e| e.to_string())?;
    state
        .container
        .tag
        .execute(TagCommand::Attach {
            hash: parsed_hash,
            name: tag_name.clone(),
            device: state.device_id,
        })
        .await
        .map_err(|e| e.to_string())?;

    // Look up the freshly-upserted tag so the frontend gets the full
    // payload. `upsert_tag` is idempotent — calling it here returns the
    // same row the UseCase just wrote.
    let tag = state
        .tag_repo
        .upsert_tag(&tag_name, state.device_id)
        .map_err(|e| e.to_string())?;
    Ok(TagPayload::from(tag))
}

/// Inner attach-tag logic extracted for testability without a live Tauri state.
///
/// WHY: upsert-then-attach in a single helper keeps both operations testable
/// as a unit without the Tauri dispatcher.
///
/// # Errors
/// Returns [`perima_core::CoreError`] on hash parse failure, invalid tag name,
/// or repository failure.
pub fn attach_tag_inner<T: TagRepository + ?Sized>(
    tag_repo: &T,
    hash_hex: &str,
    tag_name: &str,
    device: DeviceId,
) -> Result<TagPayload, perima_core::CoreError> {
    // WHY direct `?` propagation: `parse_hex` already returns the typed
    // `CoreError::InvalidHash` variant. Wrapping it in `Internal` would
    // discard that signal for future HTTP/FFI adapters that match on
    // variants.
    let hash = perima_core::BlakeHash::parse_hex(hash_hex)?;
    let tag = tag_repo.upsert_tag(tag_name, device)?;
    tag_repo.attach(&hash, tag.id, device)?;
    Ok(TagPayload::from(tag))
}

/// Remove a tag from a file by content hash + tag UUID.
///
/// # Errors
/// Returns a `String` if either the hash or tag UUID is malformed, or if the
/// repository fails.
// WHY allow: Tauri owns the `State`, `String` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn detach_tag(
    hash: String,
    tag_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // WHY resolve tag_id -> tag name in the shell: the
    // `TagCommand::Detach` variant takes `{ hash, name, device }` — it
    // looks up the tag by name via `upsert_tag`, the same idempotent
    // path used pre-Batch-B. The frontend still passes the tag's UUID
    // (string) because the `TagPayload` it already has in hand surfaces
    // `id`, not `name`. We resolve id → name via the legacy
    // `state.tag_repo` handle. A future "Detach by id" variant on the
    // UseCase obsoletes this lookup.
    let parsed_hash = perima_core::BlakeHash::parse_hex(&hash).map_err(|e| e.to_string())?;
    let parsed_id = uuid::Uuid::parse_str(&tag_id).map_err(|e| format!("bad tag UUID: {e}"))?;

    let tags = state.tag_repo.list_tags().map_err(|e| e.to_string())?;
    let tag_name = tags
        .into_iter()
        .find(|t| t.id == parsed_id)
        .map(|t| t.name)
        .ok_or_else(|| format!("tag not found: {parsed_id}"))?;

    state
        .container
        .tag
        .execute(TagCommand::Detach {
            hash: parsed_hash,
            name: tag_name,
            device: state.device_id,
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Inner detach-tag logic extracted for testability without a live Tauri state.
///
/// # Errors
/// Returns [`perima_core::CoreError`] on hash/UUID parse failure or
/// repository failure.
pub fn detach_tag_inner<T: TagRepository + ?Sized>(
    tag_repo: &T,
    hash_hex: &str,
    tag_id_str: &str,
    device: DeviceId,
) -> Result<(), perima_core::CoreError> {
    // WHY direct `?` on `parse_hex`: preserves the typed
    // `CoreError::InvalidHash` variant for downstream consumers.
    let hash = perima_core::BlakeHash::parse_hex(hash_hex)?;
    // WHY `Internal` wrap on `Uuid::parse_str`: `CoreError` has no
    // dedicated UUID variant; `Internal` is the pragmatic fallback
    // until a validation-error variant is introduced.
    let tag_id = uuid::Uuid::parse_str(tag_id_str)
        .map_err(|e| perima_core::CoreError::Internal(format!("bad tag UUID: {e}")))?;
    tag_repo.detach(&hash, tag_id, device)?;
    Ok(())
}

/// List files with their metadata and any attached tags.
///
/// Returns up to `limit` [`FileWithTagsPayload`] rows. Tags are fetched
/// in a second query and merged in Rust.
///
/// # Errors
/// Returns a `String` description of any [`perima_core::CoreError`] or
/// UUID parse failure.
// WHY allow: Tauri owns `State` + `Option<String>` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn list_files_with_tags(
    limit: u32,
    volume: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<FileWithTagsPayload>, String> {
    let volume_id = volume
        .map(|v| {
            uuid::Uuid::parse_str(&v)
                .map(VolumeId)
                .map_err(|e| format!("bad volume UUID: {e}"))
        })
        .transpose()?;

    let out = state
        .container
        .tag
        .execute(TagCommand::ListFilesWithTags {
            filter: Some(TagFilter {
                limit,
                volume: volume_id,
            }),
        })
        .await
        .map_err(|e| e.to_string())?;
    let TagOutput::FilesWithTags(files) = out else {
        return Err("TagCommand::ListFilesWithTags returned non-FilesWithTags output".to_owned());
    };

    Ok(files
        .into_iter()
        .map(|fwt| FileWithTagsPayload {
            file: FileWithMetadataPayload::from((fwt.location, fwt.metadata)),
            tags: fwt.tags.into_iter().map(TagPayload::from).collect(),
        })
        .collect())
}

/// Inner list-files-with-tags: two queries + merge in Rust.
///
/// WHY two queries + merge (not a shared tx): the two-SELECT sequence
/// has a benign WAL race — a `file_tags` insert between calls could
/// produce tags for a hash not in the metadata set (harmless — we
/// iterate the metadata list and look up by hash, so extra tags are
/// ignored), and a metadata delete between calls leaves a stale tag
/// entry in the map (also harmless for the same reason). Transient
/// inconsistency is acceptable for UI list refresh.
///
/// # Errors
/// Returns [`perima_core::CoreError`] on any repository failure.
pub fn list_files_with_tags_inner<M, T>(
    metadata_repo: &M,
    tag_repo: &T,
    limit: u32,
    volume: Option<VolumeId>,
) -> Result<Vec<FileWithTagsPayload>, perima_core::CoreError>
where
    M: MetadataRepository + ?Sized,
    T: TagRepository + ?Sized,
{
    let rows = metadata_repo.list_with_metadata(limit as usize, volume)?;
    let hashes: Vec<perima_core::BlakeHash> = rows.iter().map(|(loc, _)| loc.hash).collect();
    let tag_map = tag_repo.tags_for_hashes(&hashes)?;
    Ok(rows
        .into_iter()
        .map(|(loc, meta)| {
            let hash = loc.hash;
            let file = FileWithMetadataPayload::from((loc, meta));
            let tags = tag_map
                .get(&hash)
                .map(|ts| ts.iter().cloned().map(TagPayload::from).collect())
                .unwrap_or_default();
            FileWithTagsPayload { file, tags }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Private scan helpers
// ---------------------------------------------------------------------------

fn validate_root(root: &Path) -> Result<(), perima_core::CoreError> {
    if !root.exists() {
        return Err(perima_core::CoreError::InvalidPath(format!(
            "does not exist: {}",
            root.display()
        )));
    }
    if !root.is_dir() {
        return Err(perima_core::CoreError::InvalidPath(format!(
            "not a directory: {}",
            root.display()
        )));
    }
    Ok(())
}

fn run_scan_dry<S, H>(
    scanner: &S,
    hasher: &H,
    canonical_root: &Path,
    never_cancel: &CancellationToken,
) -> Result<ScanResult, perima_core::CoreError>
where
    S: perima_core::Scanner + ?Sized,
    H: perima_core::HashService + ?Sized,
{
    let discovered: Vec<perima_core::DiscoveredFile> = scanner
        .walk(canonical_root, canonical_root)?
        .take_while(|_| !never_cancel.is_cancelled())
        .collect();

    let cancel_flag = never_cancel.clone();
    let results: Vec<Result<_, perima_core::CoreError>> = discovered
        .into_par_iter()
        .map(|d| {
            if cancel_flag.is_cancelled() {
                return Err(perima_core::CoreError::Internal("cancelled".into()));
            }
            let h = hasher.full_hash(&d.absolute_path)?;
            Ok((d, h))
        })
        .collect();

    let mut new_count: u64 = 0;
    let mut errors: u64 = 0;
    for r in results {
        match r {
            Ok(_) => new_count += 1,
            Err(_) => errors += 1,
        }
    }
    Ok(ScanResult {
        total: new_count + errors,
        new: new_count,
        existing: 0,
        errors,
    })
}

// WHY allow(clippy::cognitive_complexity): the loop body grew a single
// additional branch for metadata enqueue per task 4 of the v0.4.2
// hotfix plan. Splitting it would require threading half a dozen
// borrowed locals through a helper signature — same trade-off the CLI
// scan made for the same reason.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
#[allow(clippy::cognitive_complexity)]
async fn run_scan_live<S, H, FR, VR>(
    scanner: &S,
    hasher: &H,
    file_repo: &FR,
    volume_repo: &VR,
    on_persist: OnPersistFn<'_>,
    device_id: DeviceId,
    canonical_root: &Path,
    never_cancel: &CancellationToken,
    metadata_repo: Option<Arc<dyn MetadataRepository>>,
    thumbnailer: Arc<ThumbnailGenerator>,
) -> Result<ScanResult, perima_core::CoreError>
where
    S: perima_core::Scanner + ?Sized,
    H: perima_core::HashService + ?Sized,
    FR: perima_core::FileRepository + ?Sized,
    VR: perima_core::VolumeRepository + ?Sized,
{
    let detected = perima_fs::detect_volume(canonical_root)?;
    let label = detected
        .identifiers
        .label
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let vol_id = volume_repo.find_or_create(&detected.identifiers, device_id)?;
    volume_repo.record_mount(vol_id, device_id, &detected.mount_point)?;
    let mount_point = detected.mount_point.clone();

    tracing::debug!(volume_label = %label, "volume resolved");

    // WHY cancel + queue spawned BEFORE the walk: the queue worker
    // needs to be alive before the first enqueue, and `queue` must
    // outlive the persist loop so all enqueues have somewhere to land.
    // `CancellationToken` is cheap to clone and never actually tripped
    // by this command (desktop has no Ctrl-C handler); its presence
    // keeps the MetadataQueue API uniform across callers.
    let queue_cancel = CancellationToken::new();
    let mut queue: Option<MetadataQueue> = metadata_repo.as_ref().map(|repo| {
        let extractor: Arc<dyn MetadataExtractor> = Arc::new(CompositeExtractor::new(vec![
            Arc::new(ImageExtractor::new()) as Arc<dyn MetadataExtractor>,
            Arc::new(VideoExtractor::new()) as Arc<dyn MetadataExtractor>,
        ]));
        MetadataQueue::spawn(
            extractor,
            Arc::clone(repo),
            Arc::clone(&thumbnailer),
            device_id,
            queue_cancel.clone(),
        )
    });

    let discovered: Vec<perima_core::DiscoveredFile> = scanner
        .walk(canonical_root, canonical_root)?
        .take_while(|_| !never_cancel.is_cancelled())
        .collect();

    let cancel_flag = never_cancel.clone();
    let results: Vec<Result<_, perima_core::CoreError>> = discovered
        .into_par_iter()
        .map(|d| {
            if cancel_flag.is_cancelled() {
                return Err(perima_core::CoreError::Internal("cancelled".into()));
            }
            let h = hasher.full_hash(&d.absolute_path)?;
            Ok((d, h))
        })
        .collect();

    let mut new_count: u64 = 0;
    let mut existing: u64 = 0;
    let mut errors: u64 = 0;
    let mut manifest_files: Vec<perima_core::HashedFile> = Vec::new();

    for res in results {
        match res {
            Ok((d, h)) => match persist_file(file_repo, &d, &h, device_id, vol_id) {
                Ok(outcome) => {
                    if let Some(cb) = on_persist {
                        cb(&d.relative_path, vol_id, device_id);
                    }
                    // WHY enqueue only on Inserted|Updated (matches the
                    // CLI scan). `Unchanged` means the hash already has
                    // a metadata row from a prior run — re-extracting
                    // would do identical work.
                    if matches!(
                        outcome,
                        perima_core::UpsertOutcome::Inserted | perima_core::UpsertOutcome::Updated
                    ) && let Some(q) = queue.as_ref()
                        && let Err(e) = q.enqueue(h, d.absolute_path.clone(), &queue_cancel)
                    {
                        tracing::warn!(
                            error = %e,
                            path = %d.absolute_path.display(),
                            "metadata enqueue failed; continuing scan",
                        );
                    }
                    manifest_files.push(perima_core::HashedFile {
                        discovered: d,
                        hash: h,
                    });
                    match outcome {
                        perima_core::UpsertOutcome::Inserted => new_count += 1,
                        perima_core::UpsertOutcome::Updated
                        | perima_core::UpsertOutcome::Unchanged => existing += 1,
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "persist failed");
                    errors += 1;
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "hash failed, skipping");
                errors += 1;
            }
        }
    }

    perima_db::manifest::write_manifest(&mount_point, vol_id, &manifest_files)?;

    // Bounded drain of the metadata queue (mirrors CLI scan). Drop the
    // queue handle to close the channel; the worker sees `recv() ==
    // None` and exits once it has drained the buffer. Await its
    // `JoinHandle` with a timeout so a stuck extractor cannot hang the
    // Tauri command.
    if let Some(mut q) = queue.take() {
        let worker = q.take_worker();
        drop(q);
        if let Some(handle) = worker {
            match tokio::time::timeout(METADATA_DRAIN_TIMEOUT, handle).await {
                Ok(Ok(())) => tracing::debug!("desktop scan: metadata queue drained cleanly"),
                Ok(Err(e)) => tracing::warn!(error = %e, "metadata worker join failed"),
                Err(_) => tracing::warn!(
                    "metadata queue did not drain within {METADATA_DRAIN_TIMEOUT:?}; \
                     stragglers will be reprocessed on next scan",
                ),
            }
        }
    }

    Ok(ScanResult {
        total: new_count + existing + errors,
        new: new_count,
        existing,
        errors,
    })
}

fn persist_file<R: perima_core::FileRepository + ?Sized>(
    repo: &R,
    d: &perima_core::DiscoveredFile,
    h: &perima_core::BlakeHash,
    device: DeviceId,
    volume: VolumeId,
) -> Result<perima_core::UpsertOutcome, perima_core::CoreError> {
    let hf = perima_core::HashedFile {
        discovered: d.clone(),
        hash: *h,
    };
    repo.upsert_file(&hf, device)?;
    repo.upsert_location(h, volume, &d.relative_path, device)
}

// ---------------------------------------------------------------------------
// Full-text search commands
// ---------------------------------------------------------------------------

/// Upper bound on `limit` for the `search` command.
///
/// WHY 500: FTS5 ranking cost scales with result set size; 500 is enough
/// for paginated UI while preventing a runaway `u32::MAX` from pinning
/// the mutex for seconds. Matches the plan's Task 5 Step 3 guard.
const SEARCH_LIMIT_MAX: u32 = 500;

/// Default `limit` when the frontend omits one.
const SEARCH_LIMIT_DEFAULT: u32 = 100;

/// Run a `FTS5` full-text search and return ranked results.
///
/// Empty/whitespace-only queries short-circuit to `[]` without hitting
/// `FTS5` (empty-match queries are an error at the FTS5 parser, not a
/// valid zero-result query).
///
/// `limit` is clamped to `[1, 500]`; callers passing `0` get the
/// default; callers passing anything larger than `500` get `500`.
///
/// # Errors
/// Returns a string error on `SQLite`/`FTS5` errors.
// WHY allow: Tauri owns `State` + primitive params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn search(
    query: String,
    limit: u32,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<SearchHitPayload>, String> {
    // WHY keep the empty / whitespace short-circuit in the shell: the
    // `SearchUseCase` returns `CoreError::Unsupported` for an empty
    // query, but the frontend's contract with pre-Batch-B `search` was
    // "empty input -> []". Preserving that contract here until the
    // frontend migrates to typed errors (Batch D) keeps the UI stable.
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    // Clamp limit: 0 -> default; anything > MAX -> MAX.
    let clamped = if limit == 0 {
        SEARCH_LIMIT_DEFAULT
    } else {
        limit.min(SEARCH_LIMIT_MAX)
    };

    let out = state
        .container
        .search
        .execute(SearchCommand::Query {
            q: query,
            limit: Some(clamped),
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(out.hits.into_iter().map(SearchHitPayload::from).collect())
}

/// Inner search logic extracted for testability without a live Tauri
/// state. Follows the `run_scan_inner` / `list_files_with_metadata_inner`
/// convention of all other commands in this module.
///
/// # Errors
/// Returns [`CoreError`] on repository failure.
pub fn search_inner(
    repo: &dyn SearchRepository,
    query: &str,
    limit: u32,
) -> Result<Vec<SearchHitPayload>, CoreError> {
    // Guard: empty / whitespace-only queries return `[]` without touching
    // FTS5. The FTS5 MATCH parser rejects empty strings.
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    // Clamp limit: 0 → default; anything > MAX → MAX.
    let clamped = if limit == 0 {
        SEARCH_LIMIT_DEFAULT
    } else {
        limit.min(SEARCH_LIMIT_MAX)
    };
    let hits = repo.search(query, clamped)?;
    Ok(hits.into_iter().map(SearchHitPayload::from).collect())
}

/// Wipe and rebuild the `FTS5` search index from the current DB state.
///
/// # Errors
/// Returns a string error on `SQLite` errors.
// WHY allow: Tauri owns `State`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn search_rebuild(state: tauri::State<'_, AppState>) -> Result<(), String> {
    // WHY `SearchCommand::Rebuild` through the container: matches the
    // CLI `--rebuild` pattern (`perima_app::SearchCommand::Rebuild`);
    // the shell discards the empty `hits` payload.
    let _: SearchOutput = state
        .container
        .search
        .execute(SearchCommand::Rebuild)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
