//! `perima watch` implementation.
//!
//! Starts a [`perima_fs::DebouncedWatcher`] on the given directory and
//! updates the database as filesystem events arrive:
//! - [`FileEvent::Created`] → log advice (no hash in watch mode; use scan).
//! - [`FileEvent::Modified`] → `status = stale`.
//! - [`FileEvent::Deleted`] → `status = missing`.
//! - [`FileEvent::Renamed`] → rename the location row, reset to active.
//!
//! WHY still opens its own DB connections (vs. reading them from
//! `AppContainer`): the watch command needs a `DbEventHandler` with a
//! live `FileRepository` handle so inbound filesystem events can mutate
//! location rows. `AppContainer` does not (yet) expose the repo ports
//! directly — only the `UseCases`. A future batch will either hoist
//! `DbEventHandler` into `perima_app` or surface the deps on the
//! container; Task 8 keeps the shell minimal without revising Task 7.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use perima_app::{AppContainer, CompositeEventBus};
use perima_core::{CoreError, DeviceId, EventBus, FileEvent, LocationStatus, VolumeRepository};
use perima_db::{SqliteFileRepository, SqliteVolumeRepository, open_and_migrate};
use perima_fs::DebouncedWatcher;

use crate::signals::Cancellation;

// ---------------------------------------------------------------------------
// DbEventHandler
// ---------------------------------------------------------------------------

/// Updates the database in response to filesystem events.
///
/// WHY `Arc<SqliteFileRepository>`: `EventBus` requires `Send + Sync`.
/// `SqliteFileRepository` uses `Mutex<Connection>` internally, satisfying both.
/// `Arc` lets `DbEventHandler` be cheaply cloneable and placed in a composite.
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
///
/// WHY `pub(crate)`: Task 10 will hoist this into `perima_app::telemetry`
/// alongside the `tracing-subscriber` bootstrap. Until then, `main.rs`
/// references it during `AppContainer` construction — hence crate-visible.
pub(crate) struct LogEventHandler;

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
/// WHY builds its own `CompositeEventBus` (rather than reading
/// `container.events`): watch needs to fan out to a `DbEventHandler` bound
/// to a `FileRepository` that can mutate location rows in response to
/// `Modified` / `Deleted` / `Renamed` events. `container.events` carries
/// the shell's chosen listener set (log-only today; Task 10 hoists the
/// log handler up) — the watcher needs the DB handler too, which
/// `AppContainer` does not currently construct. Re-using
/// [`perima_app::CompositeEventBus`] (#69 hoist) here is still a net
/// reduction because the type lives in the app layer, not in this file.
///
/// # Errors
/// Returns [`CoreError::InvalidPath`] if `root` is not an existing directory;
/// propagates [`CoreError`] from volume detection, DB access, or watcher init.
pub(crate) async fn run(
    _container: &AppContainer,
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

    let db_handler: Arc<dyn EventBus> = Arc::new(DbEventHandler {
        repo: Arc::clone(&file_repo),
        device: device_id,
    });
    let log_handler: Arc<dyn EventBus> = Arc::new(LogEventHandler);

    let composite = CompositeEventBus::new(vec![db_handler, log_handler]);

    // WHY 1 s production debounce: short enough for responsive feedback, long
    // enough to coalesce rapid saves (e.g. editors that write-then-chmod).
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
