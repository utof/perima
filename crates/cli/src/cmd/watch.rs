//! `perima watch` implementation.
//!
//! Starts a [`perima_fs::DebouncedWatcher`] on the given directory and
//! updates the database as filesystem events arrive:
//! - [`FileEvent::Created`] → log advice (no hash in watch mode; use scan).
//! - [`FileEvent::Modified`] → `status = stale`.
//! - [`FileEvent::Deleted`] → `status = missing`.
//! - [`FileEvent::Renamed`] → rename the location row, reset to active.
//!
//! WHY `run` consumes `container.events` directly: `main.rs::dispatch_watch`
//! constructs a [`DbEventHandler`] via [`make_db_event_handler`] and passes
//! it as an `extra_handler` to `build_container` before `AppContainer::new`
//! wraps all handlers in the single [`perima_app::CompositeEventBus`].
//! `run` then receives `container.events` — the already-composed bus —
//! and forwards it directly to `DebouncedWatcher`. No second bus
//! construction happens in the shell layer (resolves spec §4 acceptance).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use perima_app::AppContainer;
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

/// Construct a [`DbEventHandler`] wrapped as `Arc<dyn EventBus>`.
///
/// WHY `pub(crate)`: `main.rs::dispatch_watch` builds this handler
/// before calling `build_container`, so it can pass it as an extra
/// handler and `AppContainer`'s single [`perima_app::CompositeEventBus`]
/// absorbs it. Only the watch dispatcher needs this — keeping it
/// `pub(crate)` limits the API surface.
pub(crate) fn make_db_event_handler(
    repo: Arc<SqliteFileRepository>,
    device: DeviceId,
) -> Arc<dyn perima_core::EventBus> {
    Arc::new(DbEventHandler { repo, device })
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
/// Detects the volume for `root`, then starts a [`DebouncedWatcher`] that
/// forwards every filesystem event to `container.events` — the shared
/// [`perima_app::CompositeEventBus`] already wired with the `DbEventHandler`
/// and `LogEventHandler` by `main.rs::dispatch_watch` before this call.
/// Blocks until the cancellation token fires (Ctrl-C).
///
/// # Errors
/// Returns [`CoreError::InvalidPath`] if `root` is not an existing directory;
/// propagates [`CoreError`] from volume detection, DB access, or watcher init.
pub(crate) async fn run(
    container: &AppContainer,
    data_dir: &Path,
    device_id: DeviceId,
    root: &Path,
    cancel: &Cancellation,
) -> Result<(), CoreError> {
    validate_root(root)?;
    let canonical_root = canonicalize(root)?;

    let detected = perima_fs::detect_volume(&canonical_root)?;
    let db_path = data_dir.join("perima.db");

    // WHY open only a volume connection here: the file repo connection for
    // event handling was already opened by main.rs::build_watch_db_handler
    // and injected into container.events via extra_handlers. We still need
    // a volume repo to resolve or create the volume record at startup.
    let vol_conn = open_and_migrate(&db_path)?;

    let vol_repo = SqliteVolumeRepository::new(vol_conn);
    let volume_id = vol_repo.find_or_create(&detected.identifiers, device_id)?;
    vol_repo.record_mount(volume_id, device_id, &detected.mount_point)?;
    drop(vol_repo);

    // WHY Arc::clone: DebouncedWatcher requires an owned Arc<dyn EventBus>.
    // container.events is already the composed fan-out bus (DbEventHandler +
    // LogEventHandler), so we clone the Arc without constructing a new bus.
    let bus = Arc::clone(&container.events);

    // WHY 1 s production debounce: short enough for responsive feedback, long
    // enough to coalesce rapid saves (e.g. editors that write-then-chmod).
    let watcher = DebouncedWatcher::start(
        std::slice::from_ref(&canonical_root),
        &canonical_root,
        volume_id,
        bus,
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
