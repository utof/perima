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
//! wraps all handlers in the single [`perima_app::Bus`].
//! `run` then receives `container.events` — the already-composed bus —
//! and forwards it directly to `DebouncedWatcher`. No second bus
//! construction happens in the shell layer (resolves spec §4 acceptance).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use perima_app::{AppContainer, EventHandler};
use perima_core::{AppEvent, CoreError, DeviceId, FileEvent, LocationStatus};
use perima_db::SqliteFileRepository;
use perima_fs::DebouncedWatcher;

use crate::signals::Cancellation;

// ---------------------------------------------------------------------------
// DbEventHandler
// ---------------------------------------------------------------------------

/// Updates the database in response to filesystem events.
///
/// WHY `Arc<SqliteFileRepository>`: `EventHandler` requires `Send + 'static`.
/// `SqliteFileRepository` uses interior mutability (flume sender + r2d2 pool),
/// satisfying both. `Arc` gives shared ownership without cloning the heavy repo.
struct DbEventHandler {
    repo: Arc<SqliteFileRepository>,
    device: DeviceId,
}

#[async_trait::async_trait]
impl EventHandler for DbEventHandler {
    fn name(&self) -> &'static str {
        "db_event_handler"
    }

    async fn handle(&mut self, event: AppEvent) {
        // CLI watch handler only acts on FileEvents — domain events
        // (ScanCompleted, IndexInvalidated) are no-ops since the CLI
        // has no frontend cache to invalidate.
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

/// Construct a [`DbEventHandler`] boxed as `Box<dyn EventHandler>`.
///
/// WHY `pub(crate)`: `main.rs::dispatch_watch` builds this handler
/// before calling `build_container`, so it can pass it as an extra
/// handler and `AppContainer::new` absorbs it into the single [`perima_app::Bus`].
/// Only the watch dispatcher needs this — keeping it `pub(crate)` limits
/// the API surface.
pub(crate) fn make_db_event_handler(
    repo: Arc<SqliteFileRepository>,
    device: DeviceId,
) -> Box<dyn perima_app::EventHandler> {
    Box::new(DbEventHandler { repo, device })
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
    perima_fs::platform_path::canonicalize(root).map_err(CoreError::from)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the `watch` subcommand.
///
/// Detects the volume for `root`, then starts a [`DebouncedWatcher`] that
/// forwards every filesystem event to `container.events` — the shared bus
/// already wired with the `DbEventHandler` and `LogEventHandler` by
/// `main.rs::dispatch_watch` before this call.
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
    let _ = data_dir; // WHY: kept for signature stability; no raw DB open remains post-Batch-C Task 2.

    // WHY delegate to `container.volumes` post-Batch-C Task 2: the
    // writer-actor + read-pool adapter is already built inside the
    // container. `find_or_create` has no UseCase surface (scan/watch
    // startup concern); the `Arc<dyn VolumeRepository>` field on
    // `AppContainer` is the supported shell entry point.
    let volume_id = container
        .volumes
        .find_or_create(&detected.identifiers, device_id)?;
    container
        .volumes
        .record_mount(volume_id, device_id, &detected.mount_point)?;

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
