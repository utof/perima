//! Tauri IPC commands exposed to the frontend.
//!
//! WHY per-command DB connections: Tauri's command system runs handlers in a
//! thread-pool. Holding a single shared `rusqlite::Connection` across commands
//! would require `Arc<Mutex<Connection>>`, adding contention and lifetime
//! complexity. Opening per-command is cheap under `SQLite` WAL mode — the second
//! `open_and_migrate` call is a no-op migration that returns instantly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use perima_core::{
    CoreError, DeviceId, EventBus, FileEvent, LocationStatus, MetadataRepository, VolumeId,
};
use perima_db::{SqliteFileRepository, SqliteVolumeRepository, open_and_migrate};
use perima_fs::{DebouncedWatcher, WalkdirScanner};
use perima_hash::Blake3Service;
use rayon::prelude::*;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::events::TauriEventEmitter;
use crate::payloads::FileWithMetadataPayload;
use crate::state::{AppState, WatcherState};

// ---------------------------------------------------------------------------
// Event handlers (duplicated from crates/cli/src/cmd/watch.rs)
//
// WHY duplicated: `crates/cli` is a binary crate shell; `crates/desktop`
// cannot depend on it (that would create an unwanted coupling between two
// app shells and pull in CLI-only dependencies). Consolidating these into a
// shared crate (`crates/watch-handlers` or similar) is deferred to post-v1
// once the API has stabilised. The duplication is intentional and small.
// ---------------------------------------------------------------------------

/// Fans out events to multiple [`EventBus`] implementations.
///
/// Individual handler errors are logged but do not abort the fan-out —
/// all registered handlers always fire regardless of prior failures.
///
/// WHY lives in commands.rs (not core): `CompositeEventBus` uses
/// `tracing::warn!` which requires the `tracing` crate. `crates/core`
/// deliberately has zero framework dependencies.
struct CompositeEventBus {
    handlers: Vec<Arc<dyn EventBus>>,
}

impl CompositeEventBus {
    /// Construct from a list of handlers.
    fn new(handlers: Vec<Arc<dyn EventBus>>) -> Self {
        Self { handlers }
    }
}

impl EventBus for CompositeEventBus {
    fn emit(&self, event: &FileEvent) -> Result<(), CoreError> {
        for h in &self.handlers {
            if let Err(e) = h.emit(event) {
                tracing::warn!(error = %e, "event handler failed");
            }
        }
        Ok(())
    }
}

/// Updates the database in response to filesystem events.
///
/// WHY `Arc<SqliteFileRepository>`: `EventBus` requires `Send + Sync`.
/// `SqliteFileRepository` uses `Mutex<Connection>` internally, satisfying both.
struct DbEventHandler {
    repo: Arc<SqliteFileRepository>,
    device: DeviceId,
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

/// Logs every filesystem event at INFO level.
struct LogEventHandler;

impl EventBus for LogEventHandler {
    fn emit(&self, event: &FileEvent) -> Result<(), CoreError> {
        tracing::info!(?event, "file event");
        Ok(())
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
pub type OnPersistFn<'a> = Option<&'a dyn Fn(&perima_core::MediaPath, VolumeId, DeviceId)>;

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
pub fn scan(
    path: String,
    dry_run: bool,
    state: tauri::State<'_, AppState>,
) -> Result<ScanResult, String> {
    let root = PathBuf::from(&path);
    run_scan_inner(&root, dry_run, &state.data_dir, state.device_id).map_err(|e| e.to_string())
}

/// Inner scan logic extracted for testability without a live Tauri state.
///
/// WHY: `tauri::State` cannot be constructed outside a running Tauri app, so
/// integration tests call this function directly with plain `Path`/`DeviceId`
/// arguments.
///
/// # Errors
/// Returns [`perima_core::CoreError`] on filesystem, volume detection, hash,
/// or database failures.
pub fn run_scan_inner(
    root: &Path,
    dry_run: bool,
    data_dir: &Path,
    device_id: DeviceId,
) -> Result<ScanResult, perima_core::CoreError> {
    validate_root(root)?;
    let canonical_root = dunce::canonicalize(root).map_err(perima_core::CoreError::Io)?;
    let scanner = WalkdirScanner::new();
    let hasher = Blake3Service::new();

    // WHY AtomicBool(false): no signal handler in the desktop backend.
    // The desktop user closes the window rather than issuing Ctrl-C.
    // A proper cancellation channel will be introduced in phase 3 when the
    // file-watcher IPC arrives and we need a cancel RPC.
    let never_cancel = Arc::new(AtomicBool::new(false));

    if dry_run {
        return run_scan_dry(&scanner, &hasher, &canonical_root, &never_cancel);
    }

    let db_path = data_dir.join("perima.db");
    // WHY three opens: SqliteFileRepository, SqliteVolumeRepository, and the
    // sentinel repo each take an owned Connection. WAL mode makes the extra
    // opens instant once migrations have run on the first connection.
    let file_conn = open_and_migrate(&db_path)?;
    let vol_conn = open_and_migrate(&db_path)?;
    let sentinel_conn = open_and_migrate(&db_path)?;

    let mut file_repo = SqliteFileRepository::new(file_conn);
    let mut vol_repo = SqliteVolumeRepository::new(vol_conn);
    let sentinel_repo = SqliteFileRepository::new(sentinel_conn);

    let on_persist = |path: &perima_core::MediaPath, volume: VolumeId, dev: DeviceId| {
        if let Err(e) = sentinel_repo.migrate_sentinel_row(path, volume, dev) {
            tracing::warn!(error = %e, "sentinel migration failed (non-fatal)");
        }
    };

    run_scan_live(
        &scanner,
        &hasher,
        &mut file_repo,
        &mut vol_repo,
        Some(&on_persist),
        device_id,
        &canonical_root,
        &never_cancel,
    )
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
pub fn list_files(
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
    list_files_inner(&state.data_dir, limit, volume_id).map_err(|e| e.to_string())
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
    let conn = open_and_migrate(&db_path)?;
    let repo = SqliteFileRepository::new(conn);
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
    list_files_with_metadata_inner(state.metadata_repo.as_ref(), limit, volume_id)
        .map_err(|e| e.to_string())
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
pub fn list_volumes(state: tauri::State<'_, AppState>) -> Result<Vec<VolumeEntry>, String> {
    list_volumes_inner(&state.data_dir, state.device_id).map_err(|e| e.to_string())
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
    let db_path = data_dir.join("perima.db");
    let conn = open_and_migrate(&db_path)?;
    let repo = SqliteVolumeRepository::new(conn);
    let records = perima_core::VolumeRepository::list(&repo, device_id)?;
    Ok(records.into_iter().map(VolumeEntry::from).collect())
}

// ---------------------------------------------------------------------------
// Watcher commands
// ---------------------------------------------------------------------------

/// Start watching `path` for filesystem changes.
///
/// Validates the path, detects the volume, opens the database, cancels any
/// prior watcher, then starts a new [`DebouncedWatcher`] that emits
/// `"file-event"` Tauri events and DB updates on every filesystem change.
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
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    watcher_state: tauri::State<'_, WatcherState>,
) -> Result<(), String> {
    let root = PathBuf::from(&path);
    validate_root(&root).map_err(|e| e.to_string())?;
    let canonical_root = dunce::canonicalize(&root).map_err(|e| format!("canonicalize: {e}"))?;

    let detected = perima_fs::detect_volume(&canonical_root).map_err(|e| e.to_string())?;
    let db_path = state.data_dir.join("perima.db");
    let device_id = state.device_id;

    // WHY two connections: SqliteVolumeRepository and SqliteFileRepository
    // each take an owned Connection. Under WAL mode a second open is instant.
    let vol_conn = open_and_migrate(&db_path).map_err(|e| e.to_string())?;
    let file_conn = open_and_migrate(&db_path).map_err(|e| e.to_string())?;

    let mut vol_repo = SqliteVolumeRepository::new(vol_conn);
    let volume_id = perima_core::VolumeRepository::find_or_create(
        &mut vol_repo,
        &detected.identifiers,
        device_id,
    )
    .map_err(|e| e.to_string())?;
    perima_core::VolumeRepository::record_mount(
        &mut vol_repo,
        volume_id,
        device_id,
        &detected.mount_point,
    )
    .map_err(|e| e.to_string())?;
    drop(vol_repo);

    let file_repo = Arc::new(SqliteFileRepository::new(file_conn));

    let db_handler: Arc<dyn EventBus> = Arc::new(DbEventHandler {
        repo: Arc::clone(&file_repo),
        device: device_id,
    });
    let tauri_emitter: Arc<dyn EventBus> = Arc::new(TauriEventEmitter {
        app_handle: app_handle.clone(),
    });
    let log_handler: Arc<dyn EventBus> = Arc::new(LogEventHandler);

    let composite = Arc::new(CompositeEventBus::new(vec![
        db_handler,
        tauri_emitter,
        log_handler,
    ]));

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

    // WHY 1 s production debounce: short enough for responsive feedback,
    // long enough to coalesce rapid saves (e.g. editors that write-then-chmod).
    let watcher = DebouncedWatcher::start(
        std::slice::from_ref(&canonical_root),
        &canonical_root,
        volume_id,
        composite,
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
    never_cancel: &Arc<AtomicBool>,
) -> Result<ScanResult, perima_core::CoreError>
where
    S: perima_core::Scanner + ?Sized,
    H: perima_core::HashService + ?Sized,
{
    let discovered: Vec<perima_core::DiscoveredFile> = scanner
        .walk(canonical_root, canonical_root)?
        .take_while(|_| !never_cancel.load(Ordering::SeqCst))
        .collect();

    let cancel_flag = Arc::clone(never_cancel);
    let results: Vec<Result<_, perima_core::CoreError>> = discovered
        .into_par_iter()
        .map(|d| {
            if cancel_flag.load(Ordering::SeqCst) {
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

#[allow(clippy::too_many_arguments)]
fn run_scan_live<S, H, FR, VR>(
    scanner: &S,
    hasher: &H,
    file_repo: &mut FR,
    volume_repo: &mut VR,
    on_persist: OnPersistFn<'_>,
    device_id: DeviceId,
    canonical_root: &Path,
    never_cancel: &Arc<AtomicBool>,
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

    let discovered: Vec<perima_core::DiscoveredFile> = scanner
        .walk(canonical_root, canonical_root)?
        .take_while(|_| !never_cancel.load(Ordering::SeqCst))
        .collect();

    let cancel_flag = Arc::clone(never_cancel);
    let results: Vec<Result<_, perima_core::CoreError>> = discovered
        .into_par_iter()
        .map(|d| {
            if cancel_flag.load(Ordering::SeqCst) {
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

    Ok(ScanResult {
        total: new_count + existing + errors,
        new: new_count,
        existing,
        errors,
    })
}

fn persist_file<R: perima_core::FileRepository + ?Sized>(
    repo: &mut R,
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
