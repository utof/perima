//! `perima watch` implementation.
//!
//! Starts a [`perima_fs::DebouncedWatcher`] on the given directory and
//! updates the database as filesystem events arrive:
//! - [`FileEvent::Created`] → log advice (no hash in watch mode; use scan).
//! - [`FileEvent::Modified`] → `status = stale`.
//! - [`FileEvent::Deleted`] → `status = missing`.
//! - [`FileEvent::Renamed`] → rename the location row, reset to active.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use perima_core::{CoreError, DeviceId, EventBus, FileEvent, LocationStatus, VolumeRepository};
use perima_db::{SqliteFileRepository, SqliteVolumeRepository, open_and_migrate};
use perima_fs::DebouncedWatcher;

use crate::signals::Cancellation;

// ---------------------------------------------------------------------------
// CompositeEventBus
// ---------------------------------------------------------------------------

/// Fans out events to multiple [`EventBus`] implementations.
///
/// Individual handler errors are logged but do not abort the fan-out —
/// all registered handlers always fire regardless of prior failures.
///
/// WHY lives in watch.rs (not core): `CompositeEventBus` uses `tracing::warn!`
/// which requires the `tracing` crate. `crates/core` deliberately has zero
/// framework dependencies, so the composite lives in the CLI shell where
/// `tracing` is already a direct dependency.
pub(crate) struct CompositeEventBus {
    handlers: Vec<Arc<dyn EventBus>>,
}

impl CompositeEventBus {
    /// Construct from a list of handlers.
    #[must_use]
    pub(crate) fn new(handlers: Vec<Arc<dyn EventBus>>) -> Self {
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

// ---------------------------------------------------------------------------
// DbEventHandler
// ---------------------------------------------------------------------------

/// Updates the database in response to filesystem events.
///
/// WHY `Arc<SqliteFileRepository>`: `EventBus` requires `Send + Sync`.
/// `SqliteFileRepository` uses `Mutex<Connection>` internally, satisfying both.
/// `Arc` lets `DbEventHandler` be cheaply cloneable and placed in a composite.
///
/// WHY no `volume` field: every [`FileEvent`] variant carries its own
/// `volume: VolumeId` derived from the watcher's configured volume. We use the
/// event's volume directly rather than shadowing it with a stored copy.
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
                    "new file detected; run `perima scan` to index it"
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
                tracing::debug!(
                    path = path.as_str(),
                    rows_affected = n,
                    "marked location stale"
                );
                Ok(())
            }
            FileEvent::Deleted { path, volume } => {
                let n = self.repo.update_location_status(
                    *volume,
                    path,
                    LocationStatus::Missing,
                    self.device,
                )?;
                tracing::debug!(
                    path = path.as_str(),
                    rows_affected = n,
                    "marked location missing"
                );
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
// LogEventHandler
// ---------------------------------------------------------------------------

/// Logs every filesystem event at INFO level.
struct LogEventHandler;

impl EventBus for LogEventHandler {
    fn emit(&self, event: &FileEvent) -> Result<(), CoreError> {
        tracing::info!(?event, "file event");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Validate helpers (mirrors scan.rs)
// ---------------------------------------------------------------------------

fn validate_root(root: &Path) -> Result<(), CoreError> {
    if !root.exists() {
        return Err(CoreError::InvalidPath(format!(
            "does not exist: {}",
            root.display()
        )));
    }
    if !root.is_dir() {
        return Err(CoreError::InvalidPath(format!(
            "not a directory: {}",
            root.display()
        )));
    }
    Ok(())
}

fn canonicalize(root: &Path) -> Result<PathBuf, CoreError> {
    // WHY: routes through perima_fs::platform_path::canonicalize — the single
    // source of truth for the #[cfg(windows)] dunce / std fallback.
    perima_fs::platform_path::canonicalize(root).map_err(CoreError::Io)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the `watch` subcommand.
///
/// Opens the database, detects the volume for `root`, then starts a
/// [`DebouncedWatcher`] that emits status updates for every filesystem event.
/// Blocks until the cancellation token fires (Ctrl-C).
///
/// # Errors
/// Returns [`CoreError::InvalidPath`] if `root` is not an existing directory;
/// propagates [`CoreError`] from volume detection, DB access, or watcher init.
pub(crate) async fn run(
    data_dir: &Path,
    device_id: DeviceId,
    root: &Path,
    cancel: &Cancellation,
) -> Result<(), CoreError> {
    validate_root(root)?;
    let canonical_root = canonicalize(root)?;

    let detected = perima_fs::detect_volume(&canonical_root)?;
    let db_path = data_dir.join("perima.db");

    // WHY two connections: SqliteVolumeRepository and SqliteFileRepository each
    // take an owned Connection. Under WAL mode a second open is instant and
    // allows both repos to operate without a shared connection mutex.
    let vol_conn = open_and_migrate(&db_path)?;
    let file_conn = open_and_migrate(&db_path)?;

    let vol_repo = SqliteVolumeRepository::new(vol_conn);
    let volume_id = vol_repo.find_or_create(&detected.identifiers, device_id)?;
    vol_repo.record_mount(volume_id, device_id, &detected.mount_point)?;
    drop(vol_repo);

    let file_repo = Arc::new(SqliteFileRepository::new(file_conn));

    let db_handler = Arc::new(DbEventHandler {
        repo: Arc::clone(&file_repo),
        device: device_id,
    });
    let log_handler = Arc::new(LogEventHandler);

    let composite = CompositeEventBus::new(vec![
        db_handler as Arc<dyn EventBus>,
        log_handler as Arc<dyn EventBus>,
    ]);

    // WHY 1 s production debounce: short enough for responsive feedback, long
    // enough to coalesce rapid saves (e.g. editors that write-then-chmod).
    // WHY `watcher` (not `_watcher`): the binding must stay live until after
    // `cancelled().await` so the underlying OS watch registration persists for
    // the full watch session. Using a plain name avoids the
    // `clippy::used_underscore_binding` warning when we explicitly drop it.
    let watcher = DebouncedWatcher::start(
        std::slice::from_ref(&canonical_root),
        &canonical_root,
        volume_id,
        Arc::new(composite),
        cancel.token(),
        Duration::from_secs(1),
    )?;

    eprintln!("watching {}... (Ctrl-C to stop)", canonical_root.display());

    // Block until Ctrl-C (or test teardown) cancels the token.
    cancel.token().cancelled().await;

    drop(watcher);
    eprintln!("watch stopped");

    Ok(())
}
