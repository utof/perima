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
//!
//! WHY wire-mirror types were deleted (Batch D Task 8): `ScanResult`,
//! `FileEntry`, `VolumeEntry`, `TagPayload`, and `SearchHitPayload` were
//! 1:1 mirrors of `perima-core` types. Now that the core types derive
//! `specta::Type`, the handlers return them directly and the mirrors are
//! obsolete. `FileWithMetadataPayload` + `FileWithTagsPayload` are retained
//! (see `crates/desktop/src/payloads.rs`) because they flatten composite
//! `(record, Option<record>)` pairs with no clean core analogue.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use perima_app::{
    EventHandler, FullScan, KEYRING_SERVICE, MetadataCommand, MetadataOutput, ScanCommand,
    ScanReport, SearchCommand, SearchOutput, TagCommand, TagFilter, TagOutput, TranscribeCommand,
    TranscribeOutput, VolumeCommand, VolumeOutput, config::transcription::TranscriptionConfig,
};
use perima_core::{
    AppEvent, BatchHandle, BatchId, BlakeHash, CollisionGroup, CoreError, DeviceId, EventBus,
    FileEvent, FileLocationRecord, FileUuid, LocationStatus, MetadataExtractor, MetadataRepository,
    SearchRepository, Tag, TagRepository, VolumeId, VolumeRecord,
};
use perima_db::{ReadPool, SqliteFileRepository, SqliteVolumeRepository, SqliteWriter};
use perima_fs::{DebouncedWatcher, WalkdirScanner};
use perima_hash::Blake3Service;
use perima_media::{
    CompositeExtractor, ImageExtractor, MetadataQueue, ThumbnailGenerator, VideoExtractor,
};
use rayon::prelude::*;
use tokio_util::sync::CancellationToken;

use crate::payloads::{
    FileWithMetadataPayload, FileWithTagsPayload, ListProvidersPayload, ProviderListEntry,
    TranscribeStartedPayload,
};
use crate::state::{AppState, WatcherState};

/// Parses a string into a `VolumeId`, wrapping a `Uuid` parse failure
/// as `CoreError::Internal`.
///
/// WHY a typed `CoreError::InvalidId` would be cleaner — that's the
/// post-Batch-D follow-up tracked in the spec §10 #1 list. `Internal`
/// is the pragmatic v1 shape.
fn parse_volume_id(s: &str) -> Result<VolumeId, CoreError> {
    uuid::Uuid::parse_str(s)
        .map(VolumeId)
        .map_err(|e| CoreError::Internal(format!("bad volume UUID: {e}")))
}

/// Parses a string into a tag `Uuid`, wrapping a parse failure as
/// `CoreError::Internal`. See [`parse_volume_id`] for the typing
/// rationale.
fn parse_tag_id(s: &str) -> Result<uuid::Uuid, CoreError> {
    uuid::Uuid::parse_str(s).map_err(|e| CoreError::Internal(format!("bad tag UUID: {e}")))
}

/// Parses a string into a `FileUuid`, wrapping a parse failure as
/// `CoreError::Internal`.
fn parse_file_uuid_str(s: &str) -> Result<FileUuid, CoreError> {
    uuid::Uuid::parse_str(s)
        .map(FileUuid)
        .map_err(|e| CoreError::Internal(format!("bad file UUID: {e}")))
}

/// Maximum time `scan` waits for the metadata worker to drain after
/// the walk loop completes.
///
/// WHY 30 s: mirrors the CLI's `METADATA_DRAIN_TIMEOUT`. Long enough
/// for a typical small-tree scan to complete comfortably, short enough
/// that a stuck extractor cannot hang the Tauri command indefinitely.
const METADATA_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Production stub `EventBus` for the `_inner` test-helper writers.
///
/// WHY a single module-level definition: `clippy::items_after_statements`
/// fires on inline `struct NoopBus` declarations inside the `_inner`
/// helpers. Hoisting once here removes the lint and consolidates the
/// shell-local stub. The shared `perima_db::test_utils::NoopBus` is
/// gated behind the `test-utils` feature and not available to production
/// builds — see #119/#125 for the long-term consolidation issue.
struct LocalNoopBus;
impl EventBus for LocalNoopBus {
    fn emit(&self, _: &AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

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
// coupling. The shell-local `TauriEventHandler` (Batch E Task 11)
// replaces the old `TauriEventEmitter` + `EventBus` wiring with the
// `EventHandler` trait pattern — exactly one `Bus::new` call in the
// codebase, inside `crates/app/src/container.rs::AppContainer::new`.
// ---------------------------------------------------------------------------

/// Updates the database in response to filesystem events.
///
/// WHY `Arc<SqliteFileRepository>`: `EventHandler` requires `Send + 'static`.
/// `SqliteFileRepository` uses interior mutability (flume sender + r2d2 pool),
/// satisfying both. `Arc` gives shared ownership without cloning the heavy repo.
pub struct DbEventHandler {
    repo: Arc<SqliteFileRepository>,
    device: DeviceId,
}

impl std::fmt::Debug for DbEventHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // WHY manual: `Arc<SqliteFileRepository>` lacks `Debug`.
        // Print the type name + device for tracing-instrument span context.
        f.debug_struct("DbEventHandler")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

impl DbEventHandler {
    /// Construct a [`DbEventHandler`] bound to the given file repository
    /// and device.
    ///
    /// WHY a `new` constructor: `lib.rs::setup` builds the handler before
    /// `AppContainer::new` wraps it into the single `Bus`.
    /// Keeping the struct fields private + exposing `new` preserves
    /// encapsulation across the crate boundary.
    #[must_use]
    pub const fn new(repo: Arc<SqliteFileRepository>, device: DeviceId) -> Self {
        Self { repo, device }
    }
}

#[async_trait::async_trait]
impl EventHandler for DbEventHandler {
    fn name(&self) -> &'static str {
        "db_event_handler"
    }

    async fn handle(&mut self, event: AppEvent) {
        // Desktop DB handler only acts on FileEvents — ScanCompleted and
        // IndexInvalidated are handled by TauriEventHandler for the frontend.
        if let AppEvent::File(file_event) = event
            && let Err(e) = self.record_file_event(&file_event)
        {
            tracing::warn!(error = %e, "failed to record file event");
        }
    }
}

impl DbEventHandler {
    fn record_file_event(&self, event: &FileEvent) -> Result<(), CoreError> {
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
            FileEvent::Modified { path, volume, .. } => {
                let n = self.repo.update_location_status(
                    *volume,
                    path,
                    LocationStatus::Stale,
                    self.device,
                )?;
                tracing::debug!(path = path.as_str(), rows_affected = n, "marked stale");
                Ok(())
            }
            FileEvent::Deleted { path, volume, .. } => {
                let n = self.repo.update_location_status(
                    *volume,
                    path,
                    LocationStatus::Missing,
                    self.device,
                )?;
                tracing::debug!(path = path.as_str(), rows_affected = n, "marked missing");
                Ok(())
            }
            FileEvent::Renamed {
                from, to, volume, ..
            } => {
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
// Composite payload types for commands that join multiple records
// ---------------------------------------------------------------------------
//
// WHY here instead of payloads.rs: `FileWithMetadataPayload` and
// `FileWithTagsPayload` are retained composites (spec §8 #6). They live in
// `payloads.rs`; this module imports them for use in command bodies.
// The 1:1 wire-mirror types (`ScanResult`, `FileEntry`, `VolumeEntry`,
// `TagPayload`, `SearchHitPayload`) were deleted in Batch D Task 8.

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
/// Returns a [`ScanReport`] with per-category file counts and timing.
/// When `dry_run` is true, hashing still occurs but nothing is written
/// to the DB.
///
/// # Errors
/// Returns a [`CoreError`] if volume detection, walking, hashing, or
/// persistence fails.
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
) -> Result<ScanReport, CoreError> {
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
    let report = state.container.scan.execute(cmd).await?;

    // WHY write_manifest stays in the shell: per spec §2 IN, `crates/app`
    // deliberately does not depend on `perima-db`. The `ScanReport`
    // surfaces `volume_mount` + `manifest_files` for the shell to wire
    // manifest persistence; the CLI does the same in `crates/cli/src/
    // cmd/scan.rs::run`.
    if let Some((vol_id, mount)) = report.volume_mount.as_ref() {
        perima_db::manifest::write_manifest(mount, *vol_id, &report.manifest_files)?;
    }

    Ok(report)
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
) -> Result<ScanReport, perima_core::CoreError> {
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
) -> Result<ScanReport, perima_core::CoreError> {
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
    let writer = SqliteWriter::start(&db_path, Arc::new(LocalNoopBus))?;
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

    // WHY plain `writer.join()` (no explicit repo drops): the handle's
    // Drop / join sends `WriteCmd::Shutdown` directly, so the writer
    // thread exits regardless of how many `Sender<WriteCmd>` clones
    // (held by `file_repo` / `vol_repo` / `sentinel_repo`) are still in
    // scope. Pre-Shutdown, this site needed an N-deep `drop(repo)`
    // ladder matching how many senders had been cloned — the magic-drop
    // antipattern that produced GH #131.
    writer.join();

    result
}

/// List indexed file locations, optionally filtered by volume.
///
/// Returns up to `limit` [`FileLocationRecord`] records ordered by
/// relative path.
///
/// # Errors
/// Returns a [`CoreError`] on volume UUID parse failure or database errors.
// WHY allow: same reason as `scan` — Tauri owns `State` and `Option<String>` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn list_files(
    limit: u32,
    volume: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<FileLocationRecord>, CoreError> {
    let volume_id = volume.as_deref().map(parse_volume_id).transpose()?;

    let out = state
        .container
        .metadata
        .execute(MetadataCommand::ListFiles {
            limit: Some(limit),
            offset: None,
            device: state.device_id,
        })
        .await?;
    let MetadataOutput::Files(records) = out else {
        return Err(CoreError::Internal(
            "ListFiles returned non-Files output".into(),
        ));
    };

    // WHY post-filter by volume in the shell: `MetadataCommand::ListFiles`
    // does not yet accept a volume filter (Batch B kept its surface
    // narrow); filtering in memory after the UseCase returns matches the
    // CLI `ls.rs` pattern for the same constraint.
    let filtered: Vec<FileLocationRecord> = records
        .into_iter()
        .filter(|r| volume_id.is_none_or(|v| r.volume_id == v))
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
) -> Result<Vec<FileLocationRecord>, perima_core::CoreError> {
    let db_path = data_dir.join("perima.db");
    // WHY local writer+pool: this helper is a test-seam; it does not have
    // access to the AppContainer / Tauri state. The writer is dropped at
    // end of scope; the read pool keeps its connection alive until after
    // the query completes.
    let writer = SqliteWriter::start(&db_path, Arc::new(LocalNoopBus))?;
    let reads = ReadPool::open(&db_path)?;
    let repo = SqliteFileRepository::new(writer.sender(), reads);
    // WHY explicit drop: close the writer sender before returning so
    // the writer thread can exit cleanly if no other senders exist.
    drop(writer);
    perima_core::FileRepository::list_file_locations(&repo, limit as usize, volume_id)
}

/// List indexed file locations joined with any extracted media metadata.
///
/// Returns up to `limit` [`FileWithMetadataPayload`] rows ordered by
/// relative path. Locations without a corresponding `file_metadata`
/// row surface with all metadata fields as `None` — callers should
/// treat that as "pending extraction", not "no metadata exists".
///
/// # Errors
/// Returns a [`CoreError`] on volume UUID parse failure or database errors.
// WHY allow: same reason as `scan` — Tauri owns `State` and `Option<String>` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn list_files_with_metadata(
    limit: u32,
    volume: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<FileWithMetadataPayload>, CoreError> {
    let volume_id = volume.as_deref().map(parse_volume_id).transpose()?;

    let out = state
        .container
        .metadata
        .execute(MetadataCommand::ListFilesWithMetadata {
            limit: Some(limit),
            offset: None,
            device: state.device_id,
        })
        .await?;
    let MetadataOutput::FilesWithMetadata(rows) = out else {
        return Err(CoreError::Internal(
            "ListFilesWithMetadata returned non-FilesWithMetadata output".into(),
        ));
    };

    // WHY post-filter by volume: see `list_files` — the `MetadataCommand`
    // variants don't expose a volume filter yet. Kept symmetric with
    // `list_files` for maintainability.
    let filtered: Vec<FileWithMetadataPayload> = rows
        .into_iter()
        .filter(|(loc, _, _)| volume_id.is_none_or(|v| loc.volume_id == v))
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
/// Returns a [`CoreError`] on database failures.
// WHY allow: Tauri requires `State<'_, T>` to be owned. See `scan` for rationale.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn list_volumes(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<VolumeRecord>, CoreError> {
    let out = state
        .container
        .volume
        .execute(VolumeCommand::List {
            device: state.device_id,
        })
        .await?;
    let VolumeOutput::Volumes(records) = out else {
        return Err(CoreError::Internal(
            "VolumeCommand::List returned non-Volumes output".into(),
        ));
    };
    Ok(records)
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
) -> Result<Vec<VolumeRecord>, perima_core::CoreError> {
    // WHY a self-contained writer+pool here (test-only seam): same
    // rationale as `run_scan_inner_with_metadata`. Production
    // `#[tauri::command] list_volumes` delegates to
    // `state.container.volume.execute(List)`.
    struct NoopBus;
    impl EventBus for NoopBus {
        fn emit(&self, _: &AppEvent) -> Result<(), CoreError> {
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
    Ok(records)
}

// ---------------------------------------------------------------------------
// Watcher commands
// ---------------------------------------------------------------------------

/// Start watching `path` for filesystem changes.
///
/// Validates the path, detects the volume, then starts a new
/// [`DebouncedWatcher`] that forwards every filesystem event to the
/// shared `container.events` bus. The bus was assembled once at
/// `lib.rs::setup` with the `LogEventHandler`, `DbEventHandler`, and
/// `TauriEventHandler` already wired — no second `Bus` is
/// constructed here (spec §4 acceptance).
///
/// # Errors
/// Returns a [`CoreError`] if the path is invalid, volume detection fails,
/// or the database cannot be opened or migrated.
// WHY allow needless_pass_by_value: Tauri's command dispatcher owns `State`
// and `String` params; the signature cannot be changed.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn start_watch(
    path: String,
    state: tauri::State<'_, AppState>,
    watcher_state: tauri::State<'_, WatcherState>,
) -> Result<(), CoreError> {
    let root = PathBuf::from(&path);
    validate_root(&root)?;
    // WHY direct ?: From<io::Error> for CoreError lowers to the typed
    // Io { kind, message } variant, preserving io::ErrorKind for the
    // frontend pattern-match path (E11).
    let canonical_root = perima_fs::platform_path::canonicalize(&root)?;

    // Resolve or create the volume record for this mount.
    //
    // WHY delegate to `state.container.volumes` for both `find_or_create`
    // and `record_mount` post-Batch-C Task 2: the writer actor owns the
    // sole writable connection. `find_or_create` still has no UseCase
    // surface (scan/watch startup concern); the container exposes the
    // raw `Arc<dyn VolumeRepository>` field for this purpose.
    let detected = perima_fs::detect_volume(&canonical_root)?;
    let device_id = state.device_id;

    let volume_id = state
        .container
        .volumes
        .find_or_create(&detected.identifiers, device_id)?;

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
        .await?;

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
    // `events` field is the already-composed `Bus` built at setup time
    // with every shell handler (Log, Db, Tauri). DebouncedWatcher takes
    // an `Arc<dyn EventBus>`; cloning the Arc avoids a second bus
    // construction in this shell.
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
    )?;

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
pub async fn stop_watch(watcher_state: tauri::State<'_, WatcherState>) -> Result<(), CoreError> {
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
pub async fn is_watching(watcher_state: tauri::State<'_, WatcherState>) -> Result<bool, CoreError> {
    let inner_guard = watcher_state.inner.lock().await;
    Ok(inner_guard.is_some())
}

// ---------------------------------------------------------------------------
// Tag commands
// ---------------------------------------------------------------------------

/// List all active (non-deleted) tags, sorted by name.
///
/// # Errors
/// Returns a [`CoreError`] on database failures.
// WHY allow: Tauri requires `State<'_, T>` to be owned. See `scan` for rationale.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn list_tags(state: tauri::State<'_, AppState>) -> Result<Vec<Tag>, CoreError> {
    let out = state.container.tag.execute(TagCommand::List).await?;
    let TagOutput::Tags(tags) = out else {
        return Err(CoreError::Internal(
            "TagCommand::List returned non-Tags output".into(),
        ));
    };
    Ok(tags)
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
) -> Result<Vec<Tag>, perima_core::CoreError> {
    tag_repo.list_tags()
}

/// Attach a tag to a file by content hash (upsert the tag first, then attach).
///
/// Returns the [`Tag`] so the frontend can immediately display it
/// without a round-trip `list_tags` call.
///
/// # Errors
/// Returns a [`CoreError`] if the hash is malformed, the tag name is
/// invalid, or the repository fails.
// WHY allow: Tauri owns the `State`, `String` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn attach_tag(
    hash: String,
    tag_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<Tag, CoreError> {
    // WHY parse the hash + resolve tag through the shell-side
    // `state.tag_repo` instead of adding a "return the tag" variant to
    // `TagCommand::Attach`: the frontend currently expects the full
    // [`Tag`] (id + name + first_seen) back from `attach_tag`.
    // The `TagUseCase::Attach` response is `TagOutput::Attached(u64)`
    // — just a rows-changed count. Rather than widen the UseCase
    // output mid-batch, we do the attach via the container and then
    // read the freshly-upserted tag via the legacy `state.tag_repo`
    // handle. A future follow-up ("Attached { tag: Tag }") removes
    // this second lookup.
    let parsed_hash = perima_core::BlakeHash::parse_hex(&hash)?;
    state
        .container
        .tag
        .execute(TagCommand::Attach {
            hash: parsed_hash,
            name: tag_name.clone(),
            device: state.device_id,
        })
        .await?;

    // Look up the freshly-upserted tag so the frontend gets the full
    // payload. `upsert_tag` is idempotent — calling it here returns the
    // same row the UseCase just wrote.
    let tag = state.tag_repo.upsert_tag(&tag_name, state.device_id)?;
    Ok(tag)
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
) -> Result<Tag, perima_core::CoreError> {
    // WHY direct `?` propagation: `parse_hex` already returns the typed
    // `CoreError::InvalidHash` variant. Wrapping it in `Internal` would
    // discard that signal for future HTTP/FFI adapters that match on
    // variants.
    let hash = perima_core::BlakeHash::parse_hex(hash_hex)?;
    let tag = tag_repo.upsert_tag(tag_name, device)?;
    tag_repo.attach(&hash, tag.id, device)?;
    Ok(tag)
}

/// Remove a tag from a file by content hash + tag UUID.
///
/// # Errors
/// Returns a [`CoreError`] if either the hash or tag UUID is malformed, or
/// if the repository fails.
// WHY allow: Tauri owns the `State`, `String` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn detach_tag(
    hash: String,
    tag_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), CoreError> {
    // WHY resolve tag_id -> tag name in the shell: the
    // `TagCommand::Detach` variant takes `{ hash, name, device }` — it
    // looks up the tag by name via `upsert_tag`, the same idempotent
    // path used pre-Batch-B. The frontend still passes the tag's UUID
    // (string) because the `Tag` it already has in hand surfaces
    // `id`, not `name`. We resolve id → name via the legacy
    // `state.tag_repo` handle. A future "Detach by id" variant on the
    // UseCase obsoletes this lookup.
    let parsed_hash = perima_core::BlakeHash::parse_hex(&hash)?;
    let parsed_id = parse_tag_id(&tag_id)?;

    let tags = state.tag_repo.list_tags()?;
    let tag_name = tags
        .into_iter()
        .find(|t| t.id == parsed_id)
        .map(|t| t.name)
        .ok_or_else(|| CoreError::Internal(format!("tag not found: {parsed_id}")))?;

    state
        .container
        .tag
        .execute(TagCommand::Detach {
            hash: parsed_hash,
            name: tag_name,
            device: state.device_id,
        })
        .await?;
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
    let tag_id = parse_tag_id(tag_id_str)?;
    tag_repo.detach(&hash, tag_id, device)?;
    Ok(())
}

/// Attach a tag to a file by `file_uuid` instead of `blake3_hash`.
///
/// WHY (Task 11, spec §4.8): `file_uuid` is the stable surrogate present
/// on every `files` row from V011 on. This is the uuid-keyed analogue of
/// [`attach_tag`] — the frontend can drive tagging by the stable surrogate
/// instead of the (eventually nullable) content hash. Internally resolves
/// `file_uuid` → `blake3_hash` via [`perima_core::FileRepository::lookup_by_file_uuid`]
/// and reuses the existing tag-attach SQL path.
///
/// **Pending-file caveat:** in v0.6.x every files row carries a
/// `blake3_hash` value (post-Task-7 the placeholder is the row's
/// `quick_hash` until the on-demand `compute_full_hash` promotes it).
/// Tag attach therefore succeeds for both real-hash and placeholder-hash
/// rows. Once a future migration relaxes `files.blake3_hash` to nullable
/// (tracked at #161), this command will need to surface
/// [`CoreError::FullHashUnavailable`] for the genuinely-NULL case; today
/// that path is unreachable.
///
/// # Errors
/// - [`CoreError::Internal`] if `file_uuid` cannot be parsed, or any
///   adapter-level failure inside [`perima_core::FileRepository::lookup_by_file_uuid`].
/// - [`CoreError::NotFound`] if no `files` row exists for `file_uuid`.
/// - Other variants from the underlying [`TagCommand::Attach`] path.
// WHY allow: Tauri owns the `State`, `String` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn attach_tag_by_uuid(
    file_uuid: String,
    tag_name: String,
    state: tauri::State<'_, AppState>,
) -> Result<Tag, CoreError> {
    let parsed_uuid = parse_file_uuid_str(&file_uuid)?;
    let (hash_opt, _path, _size) = state
        .container
        .files_repo
        .lookup_by_file_uuid(parsed_uuid)?
        .ok_or_else(|| {
            CoreError::NotFound(format!("no files row for file_uuid={}", parsed_uuid.0))
        })?;
    // WHY require Some(hash): tag attach keys on blake3_hash — pending
    // files (blake3_hash IS NULL in V011) cannot yet be tagged via this
    // path. Surface a typed error pointing the user at `perima hash` first.
    let hash = hash_opt.ok_or_else(|| {
        CoreError::Unsupported(format!(
            "file has no full_hash yet (file_uuid={}): run `perima hash` first",
            parsed_uuid.0
        ))
    })?;
    state
        .container
        .tag
        .execute(TagCommand::Attach {
            hash,
            name: tag_name.clone(),
            device: state.device_id,
        })
        .await?;
    let tag = state.tag_repo.upsert_tag(&tag_name, state.device_id)?;
    Ok(tag)
}

/// Detach a tag from a file by `file_uuid` instead of `blake3_hash`.
///
/// Symmetric to [`attach_tag_by_uuid`] (Task 11, spec §4.8).
///
/// # Errors
/// Same shape as [`attach_tag_by_uuid`].
// WHY allow: Tauri owns the `State`, `String` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn detach_tag_by_uuid(
    file_uuid: String,
    tag_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), CoreError> {
    let parsed_uuid = parse_file_uuid_str(&file_uuid)?;
    let parsed_id = parse_tag_id(&tag_id)?;
    let (hash_opt, _path, _size) = state
        .container
        .files_repo
        .lookup_by_file_uuid(parsed_uuid)?
        .ok_or_else(|| {
            CoreError::NotFound(format!("no files row for file_uuid={}", parsed_uuid.0))
        })?;
    // WHY require Some(hash): tag detach keys on blake3_hash — pending
    // files (blake3_hash IS NULL in V011) cannot yet have tags detached via
    // this path (they have no tags since attach also requires a hash).
    let hash = hash_opt.ok_or_else(|| {
        CoreError::Unsupported(format!(
            "file has no full_hash yet (file_uuid={}): run `perima hash` first",
            parsed_uuid.0
        ))
    })?;

    let tags = state.tag_repo.list_tags()?;
    let tag_name = tags
        .into_iter()
        .find(|t| t.id == parsed_id)
        .map(|t| t.name)
        .ok_or_else(|| CoreError::Internal(format!("tag not found: {parsed_id}")))?;

    state
        .container
        .tag
        .execute(TagCommand::Detach {
            hash,
            name: tag_name,
            device: state.device_id,
        })
        .await?;
    Ok(())
}

/// List files with their metadata and any attached tags.
///
/// Returns up to `limit` [`FileWithTagsPayload`] rows. Tags are fetched
/// in a second query and merged in Rust.
///
/// # Errors
/// Returns a [`CoreError`] on database failures or UUID parse failure.
// WHY allow: Tauri owns `State` + `Option<String>` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn list_files_with_tags(
    limit: u32,
    volume: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<FileWithTagsPayload>, CoreError> {
    let volume_id = volume.as_deref().map(parse_volume_id).transpose()?;

    let out = state
        .container
        .tag
        .execute(TagCommand::ListFilesWithTags {
            filter: Some(TagFilter {
                limit,
                volume: volume_id,
            }),
        })
        .await?;
    let TagOutput::FilesWithTags(files) = out else {
        return Err(CoreError::Internal(
            "TagCommand::ListFilesWithTags returned non-FilesWithTags output".into(),
        ));
    };

    Ok(files
        .into_iter()
        .map(|fwt| FileWithTagsPayload {
            file: FileWithMetadataPayload::from((fwt.location, fwt.metadata, fwt.quick_hash)),
            tags: fwt.tags,
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
    // WHY filter_map: post-Task-11 `loc.hash` is `Option<BlakeHash>`. Pending
    // files (no full_hash yet) cannot match the tag-by-hash lookup; their tags
    // surface as the empty Vec below. To attach tags to pending files use the
    // `attach_tag_by_uuid` IPC command.
    let hashes: Vec<perima_core::BlakeHash> =
        rows.iter().filter_map(|(loc, _, _)| loc.hash).collect();
    let tag_map = tag_repo.tags_for_hashes(&hashes)?;
    Ok(rows
        .into_iter()
        .map(|(loc, meta, quick_hash)| {
            let tags = loc
                .hash
                .map_or_else(Vec::new, |h| tag_map.get(&h).cloned().unwrap_or_default());
            let file = FileWithMetadataPayload::from((loc, meta, quick_hash));
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
) -> Result<ScanReport, perima_core::CoreError>
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

    let mut files_new: u64 = 0;
    let mut files_errored: u64 = 0;
    for r in results {
        match r {
            Ok(_) => files_new += 1,
            Err(_) => files_errored += 1,
        }
    }
    Ok(ScanReport {
        files_seen: files_new + files_errored,
        files_new,
        files_updated: 0,
        files_errored,
        ..Default::default()
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
) -> Result<ScanReport, perima_core::CoreError>
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

    let mut files_new: u64 = 0;
    let mut files_updated: u64 = 0;
    let mut files_errored: u64 = 0;
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
                        perima_core::UpsertOutcome::Inserted => files_new += 1,
                        perima_core::UpsertOutcome::Updated
                        | perima_core::UpsertOutcome::Unchanged => files_updated += 1,
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "persist failed");
                    files_errored += 1;
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "hash failed, skipping");
                files_errored += 1;
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

    Ok(ScanReport {
        files_seen: files_new + files_updated + files_errored,
        files_new,
        files_updated,
        files_errored,
        volume_label: Some(label),
        volume_mount: Some((vol_id, mount_point)),
        manifest_files,
        ..Default::default()
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
/// Returns a [`CoreError`] on `SQLite`/`FTS5` errors.
// WHY allow: Tauri owns `State` + primitive params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn search(
    query: String,
    limit: u32,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<perima_core::SearchHit>, CoreError> {
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
        .await?;
    Ok(out.hits)
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
) -> Result<Vec<perima_core::SearchHit>, CoreError> {
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
    repo.search(query, clamped)
}

/// Wipe and rebuild the `FTS5` search index from the current DB state.
///
/// # Errors
/// Returns a [`CoreError`] on `SQLite` errors.
// WHY allow: Tauri owns `State`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn search_rebuild(state: tauri::State<'_, AppState>) -> Result<(), CoreError> {
    // WHY `SearchCommand::Rebuild` through the container: matches the
    // CLI `--rebuild` pattern (`perima_app::SearchCommand::Rebuild`);
    // the shell discards the empty `hits` payload.
    let _: SearchOutput = state
        .container
        .search
        .execute(SearchCommand::Rebuild)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Dedup commands (Task 9)
// ---------------------------------------------------------------------------

/// Compute and persist the full BLAKE3 hash for a single file (by `file_uuid`).
///
/// Reads the active mounted location for the file, computes the full hash via
/// the `HashService` dispatch matrix, and promotes it onto the `files` row.
///
/// # Errors
/// - [`CoreError::FullHashUnavailable`] when no mounted location exists for
///   the given `file_uuid` or when the hash compute fails with an I/O error.
/// - Other [`CoreError`] variants from the underlying repository.
// WHY allow: Tauri owns `State` and value params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn compute_full_hash(
    file_uuid: FileUuid,
    state: tauri::State<'_, AppState>,
) -> Result<BlakeHash, CoreError> {
    state
        .container
        .compute_full_hash
        .execute_single(file_uuid)
        .await
}

/// Spawn a background batch that computes `full_hash` for every uuid in
/// `file_uuids`, returning a [`BatchHandle`] immediately.
///
/// Per-file progress events are emitted on the `app-event` channel as
/// [`AppEvent::VerifyProgress`]; the batch emits [`AppEvent::VerifyComplete`]
/// when the last file finishes (or cancellation fires).
///
/// # Errors
/// Currently infallible — per-file failures are routed through events
/// rather than aborting the batch.
// WHY allow: Tauri owns `State` and value params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn compute_full_hash_batch(
    file_uuids: Vec<FileUuid>,
    state: tauri::State<'_, AppState>,
) -> Result<BatchHandle, CoreError> {
    state
        .container
        .compute_full_hash
        .execute_batch(file_uuids)
        .await
}

/// Cancel an in-flight `compute_full_hash_batch` by `batch_id`.
///
/// # Errors
/// Returns [`CoreError::NotFound`] when no batch is currently running with
/// that id (already finished, never started, or already cancelled).
// WHY allow: Tauri owns `State` and value params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn cancel_verify_batch(
    batch_id: BatchId,
    state: tauri::State<'_, AppState>,
) -> Result<(), CoreError> {
    state
        .container
        .compute_full_hash
        .cancel_batch(batch_id)
        .await
}

/// List every group of files whose `quick_hash` matches one or more other
/// rows AND that have not been marked `verified_distinct`.
///
/// Cheap query — groups by `quick_hash` and joins to active `file_locations`
/// rows. Empty result means there are no candidate duplicates today.
///
/// # Errors
/// Returns a [`CoreError`] on database failures.
// WHY allow: Tauri owns `State`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn list_quick_hash_collisions(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<CollisionGroup>, CoreError> {
    // WHY blocking call wrapped in async fn: `DedupUseCase::list_collisions`
    // is sync (the underlying SELECT is fast). The Tauri command surface is
    // async-by-convention; we keep the signature uniform with the rest of
    // the dedup commands.
    state.container.dedup.list_collisions()
}

/// Mark the given `file_uuids` as `verified_distinct = 1`.
///
/// Memorises that these files share a `quick_hash` but were verified to
/// have distinct `full_hash` values. They will be excluded from subsequent
/// [`list_quick_hash_collisions`] results. Single transaction —
/// all-or-nothing.
///
/// # Errors
/// Returns a [`CoreError`] on database failures.
// WHY allow: Tauri owns `State` and value params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn mark_verified_distinct(
    file_uuids: Vec<FileUuid>,
    state: tauri::State<'_, AppState>,
) -> Result<(), CoreError> {
    state
        .container
        .dedup
        .mark_verified_distinct(file_uuids, state.device_id)
}

// ---------------------------------------------------------------------------
// Backup commands (slice 1, GH #168)
// ---------------------------------------------------------------------------

/// Produce a single-file consistent `SQLite` snapshot of the database.
///
/// Delegates to [`perima_app::BackupDatabaseUseCase`], which resolves the
/// target path (`<data_dir>/backups/perima-<ISO 8601>.sqlite` when `target`
/// is `None`), enforces single-flight via an `AtomicBool` guard,
/// pre-removes when `force` is `true`, and dispatches the actual copy
/// through [`perima_core::ports::DatabaseAdmin`] (writer-actor `VACUUM INTO`).
///
/// Returns a [`perima_app::BackupOutput`] carrying the absolute path
/// written to plus the size in bytes of the freshly written file.
///
/// # Errors
/// Returns [`CoreError::BackupFailed`] with a typed
/// [`perima_core::errors::BackupFailureReason`]:
/// - `TargetExists` — `target` already exists and `force` was not passed.
/// - `TargetUnwritable` — parent directory could not be created or
///   pre-existing file could not be removed.
/// - `AlreadyInProgress` — another backup is currently running on this
///   process.
/// - `DiskFull` / `Internal(...)` — propagated from the writer-actor
///   adapter on lower-level `SQLite` failures.
// WHY allow: Tauri owns `State` and value params (Option<PathBuf> + bool).
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn backup_database(
    state: tauri::State<'_, AppState>,
    target: Option<PathBuf>,
    force: bool,
) -> Result<perima_app::BackupOutput, CoreError> {
    state
        .container
        .backup
        .execute(perima_app::BackupCommand { target, force })
        .await
}

// ---------------------------------------------------------------------------
// Transcription commands (T7)
//
// All eight handlers return `Result<T, CoreError>` per the Batch D IPC contract
// (CLAUDE.md). Wire-types crossing IPC derive `specta::Type`; see
// `crates/desktop/src/payloads.rs` for the wire-type WHY-block.
//
// Keyring posture mirrors the CLI `auth` subcommand
// (`crates/cli/src/cmd/auth.rs`): same `KEYRING_SERVICE` constant, same
// idempotent-delete semantics, same `NoEntry`-as-success policy on lookups.
// ---------------------------------------------------------------------------

/// Construct a [`keyring::Entry`] for the given provider, mapping construction
/// failures to [`CoreError::Internal`]. Mirrors `cli/src/cmd/auth.rs`.
fn provider_keyring_entry(provider: &str) -> Result<keyring::Entry, CoreError> {
    keyring::Entry::new(KEYRING_SERVICE, provider)
        .map_err(|e| CoreError::Internal(format!("keyring entry: {e}")))
}

/// Start a new transcription job.
///
/// Returns immediately with the freshly-minted `request_uuid` and 1-based
/// queue position; the worker picks up the job asynchronously and publishes
/// [`perima_core::AppEvent::TranscriptionStarted`] on the shared bus when
/// it begins (the Tauri channel `"app-event"` carries every `AppEvent`).
///
/// `source` is a [`String`] (not `PathBuf`) because Tauri serializes
/// paths as strings; the handler converts to [`PathBuf`] before invoking
/// the use-case.
///
/// # Errors
/// Returns [`CoreError::Transcription`] wrapping
/// [`perima_core::transcription::TranscriptionError::QueueFull`] when the
/// bounded queue is at capacity; or other [`CoreError`] variants from the
/// transcription use-case.
// WHY allow: Tauri owns `State` + `String`/`Option<String>` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn transcribe(
    file_uuid: String,
    file_name: String,
    source: String,
    language_hint: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<TranscribeStartedPayload, CoreError> {
    let out = state
        .container
        .transcription
        .execute(TranscribeCommand::Start {
            file_uuid,
            file_name,
            source: PathBuf::from(source),
            language_hint,
        })
        .await?;
    // WHY explicit match (not `let TranscribeOutput::Started { .. } = out else
    // ...`): the `Cancelled` variant must never appear in response to a
    // `Start` command. Surfacing it as `CoreError::Internal` keeps the IPC
    // contract typed without widening `TranscribeStartedPayload` to also
    // model the cancelled case.
    match out {
        TranscribeOutput::Started {
            request_uuid,
            queue_position,
        } => Ok(TranscribeStartedPayload {
            request_uuid,
            queue_position,
        }),
        TranscribeOutput::Cancelled { .. } => Err(CoreError::Internal(
            "TranscribeCommand::Start returned Cancelled output".into(),
        )),
    }
}

/// Cancel an in-flight or queued transcription job by `request_uuid`.
///
/// Idempotent — cancelling an unknown id is a no-op (the use-case removes
/// the entry from its cancel map and returns Cancelled either way).
///
/// # Errors
/// Returns a [`CoreError`] only if the use-case itself fails; the typical
/// path returns `Ok(())`.
// WHY allow: Tauri owns `State` and `String` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
pub async fn cancel_transcription(
    request_uuid: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), CoreError> {
    let _ = state
        .container
        .transcription
        .execute(TranscribeCommand::Cancel { request_uuid })
        .await?;
    Ok(())
}

/// Store an API key under the given provider name in the OS keyring under
/// service [`perima_app::KEYRING_SERVICE`] (`"perima.transcription"`).
///
/// Overwrites any existing key for the same provider (matches the CLI
/// `auth set` semantics).
///
/// # Errors
/// Returns [`CoreError::Internal`] on keyring failures.
// WHY allow: Tauri owns `State` and `String` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
#[allow(clippy::unused_async)]
// WHY allow unused_async: keyring operations are sync, but the Tauri command
// surface is async-by-convention to match every other handler in this module.
pub async fn set_provider_key(
    provider: String,
    api_key: String,
    _state: tauri::State<'_, AppState>,
) -> Result<(), CoreError> {
    let entry = provider_keyring_entry(&provider)?;
    entry
        .set_password(&api_key)
        .map_err(|e| CoreError::Internal(format!("keyring set: {e}")))
}

/// Remove the keyring entry for `provider`. Idempotent — returns `Ok(())`
/// when no entry exists (matches CLI `auth delete`).
///
/// # Errors
/// Returns [`CoreError::Internal`] on keyring failures other than
/// [`keyring::Error::NoEntry`].
// WHY allow: Tauri owns `State` and `String` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
#[allow(clippy::unused_async)]
pub async fn delete_provider_key(
    provider: String,
    _state: tauri::State<'_, AppState>,
) -> Result<(), CoreError> {
    let entry = provider_keyring_entry(&provider)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CoreError::Internal(format!("keyring delete: {e}"))),
    }
}

/// Return `true` if a keyring entry exists for `provider`, `false` otherwise.
///
/// # Errors
/// Returns [`CoreError::Internal`] on keyring failures other than
/// [`keyring::Error::NoEntry`] (which maps to `Ok(false)`).
// WHY allow: Tauri owns `State` and `String` params.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
#[allow(clippy::unused_async)]
pub async fn has_provider_key(
    provider: String,
    _state: tauri::State<'_, AppState>,
) -> Result<bool, CoreError> {
    let entry = provider_keyring_entry(&provider)?;
    match entry.get_password() {
        Ok(_) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(CoreError::Internal(format!("keyring get: {e}"))),
    }
}

/// List every configured transcription provider with its preset, optional
/// model override, and a `has_key` flag computed via a per-provider keyring
/// lookup.
///
/// `active` echoes [`TranscriptionConfig::active_provider`].
///
/// # Errors
/// Returns [`CoreError::Internal`] when [`TranscriptionConfig::load`] fails
/// (malformed `config.toml`).
///
/// # Panics
/// Panics only on logic-bug paths inside the `HashMap::get(name)` lookup
/// — `name` was just enumerated from `cfg.providers.keys()`, so the entry
/// must exist. Treated as an unrecoverable bug if it ever fires.
// WHY allow: Tauri owns `State`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
#[allow(clippy::unused_async)]
pub async fn list_providers(
    state: tauri::State<'_, AppState>,
) -> Result<ListProvidersPayload, CoreError> {
    // WHY load via state.data_dir.parent(): config_dir is not on AppState; the
    // Tauri shell stores it implicitly via `resolve_with_app_data_dir` —
    // `data_dir` lives at `<config_dir>/perima/`, so config_dir is its
    // parent. This avoids widening AppState mid-task.
    let config_dir = state
        .data_dir
        .parent()
        .ok_or_else(|| CoreError::Internal("data_dir has no parent (config_dir)".into()))?;
    let cfg = TranscriptionConfig::load(config_dir)?;

    // Sort provider names for deterministic output (matches CLI `auth list`).
    let mut names: Vec<&String> = cfg.providers.keys().collect();
    names.sort();

    let providers = names
        .into_iter()
        .map(|name| {
            let entry = cfg.providers.get(name).expect("name was just enumerated");
            // WHY treat construction failure as "no key": surfacing the
            // raw keyring error here would block the whole settings
            // panel; the user's recovery path is to set the key
            // (which goes through `set_provider_key` and surfaces its
            // own error if construction fails again). Log the
            // construction failure so a system-keyring outage is
            // diagnosable without piecing together other commands' errors.
            let has_key = match provider_keyring_entry(name) {
                Ok(entry) => entry.get_password().is_ok(),
                Err(e) => {
                    tracing::warn!(provider = %name, error = %e, "keyring entry construction failed in list_providers; reporting has_key=false");
                    false
                }
            };
            ProviderListEntry {
                name: name.clone(),
                preset: entry.preset.clone(),
                model: entry.model.clone(),
                has_key,
            }
        })
        .collect();

    Ok(ListProvidersPayload {
        active: cfg.active_provider,
        providers,
    })
}

/// Persist `config` to `<config_dir>/config.toml` via [`TranscriptionConfig::save`].
///
/// **Hot reload not implemented in T7.** Rebuilding the active
/// [`perima_app::TranscriptionUseCase`] in-place would require swapping the
/// `Arc<TranscriptionUseCase>` field on `AppContainer` (which is currently
/// immutable after construction) and gracefully draining the existing worker
/// task. That refactor is deferred — for now we save the file and emit a
/// `tracing::warn!` instructing the user to relaunch the app for the new
/// provider config to take effect. The frontend should surface a banner with
/// the same message.
///
/// # Errors
/// Returns [`CoreError::Internal`] on filesystem or TOML serialization
/// failures.
// WHY allow: Tauri owns `State` and the value-param `config`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
#[allow(clippy::unused_async)]
pub async fn update_transcription_config(
    config: TranscriptionConfig,
    state: tauri::State<'_, AppState>,
) -> Result<(), CoreError> {
    let config_dir = state
        .data_dir
        .parent()
        .ok_or_else(|| CoreError::Internal("data_dir has no parent (config_dir)".into()))?;
    // WHY sync I/O on a Tauri worker thread (no spawn_blocking): TOML
    // serialize + write of a tiny config file (<2 KiB) completes in
    // sub-ms on every realistic disk; the runtime cost of spawn_blocking
    // (thread handoff + cache miss) would dwarf the I/O it dispatches.
    config.save(config_dir)?;
    tracing::warn!(
        "transcription config updated; restart perima for the new provider \
         configuration to take effect"
    );
    Ok(())
}

/// Read the current [`TranscriptionConfig`] from `<config_dir>/config.toml`.
/// Returns the default (no providers, no active) when the file is missing.
///
/// # Errors
/// Returns [`CoreError::Internal`] when the config file exists but is
/// malformed TOML.
// WHY allow: Tauri owns `State`.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
#[specta::specta]
#[allow(clippy::unused_async)]
pub async fn get_transcription_config(
    state: tauri::State<'_, AppState>,
) -> Result<TranscriptionConfig, CoreError> {
    let config_dir = state
        .data_dir
        .parent()
        .ok_or_else(|| CoreError::Internal("data_dir has no parent (config_dir)".into()))?;
    TranscriptionConfig::load(config_dir)
}
