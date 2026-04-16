//! Shared application state injected into every Tauri command.

use std::path::PathBuf;
use std::sync::Arc;

use perima_core::DeviceId;
use perima_db::{SqliteMetadataRepository, SqliteSearchRepository, SqliteTagRepository};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// State shared across all Tauri commands via `tauri::State<AppState>`.
///
/// WHY `data_dir` + `device_id` without a general DB handle: commands
/// that read through `FileRepository` / `VolumeRepository` open their
/// own `rusqlite::Connection` per-call. That adapter predates `&self`
/// traits and uses `&mut self`, which collides with `Arc`-sharing. WAL
/// mode makes the second open a no-op so per-call-open is cheap.
///
/// WHY `metadata_repo: Arc<SqliteMetadataRepository>` is a deliberate
/// deviation from the per-call-open pattern: the `MetadataRepository`
/// trait was authored with `&self` (`Mutex<Connection>` inside) expressly
/// so a single handle can be cloned into the background
/// `MetadataQueue` worker (future v0.4.1 work) AND into Tauri
/// commands. Opening per-call would create a new `Mutex` each time,
/// defeating the shared-worker pattern the queue relies on. Task 5 of
/// the v0.4.0 plan calls this out explicitly.
pub struct AppState {
    /// Resolved data directory (where `perima.db` lives).
    pub data_dir: PathBuf,
    /// Stable device identifier.
    pub device_id: DeviceId,
    /// Shared metadata repository handle.
    ///
    /// See struct-level WHY for the rationale behind holding this
    /// directly (rather than re-opening a connection per command).
    pub metadata_repo: Arc<SqliteMetadataRepository>,
    /// Shared tag repository handle.
    ///
    /// WHY `Arc<SqliteTagRepository>` (same pattern as `metadata_repo`):
    /// `TagRepository` trait uses `&self` with interior `Mutex<Connection>`,
    /// so a single handle can be shared across commands without per-call-open.
    pub tag_repo: Arc<SqliteTagRepository>,
    /// Shared search repository handle.
    ///
    /// WHY `Arc<SqliteSearchRepository>`: `SearchRepository` uses `&self`
    /// with interior `Mutex<Connection>`, enabling Arc-sharing across commands
    /// without per-call connection opens.
    pub search_repo: Arc<SqliteSearchRepository>,
}

impl AppState {
    /// Construct a new `AppState` from a resolved config, metadata repo, and
    /// tag repo.
    ///
    /// WHY a constructor (rather than public struct literal): keeps the
    /// Arc-sharing contract for both repos explicit at the single
    /// construction site in `run()`.
    #[must_use]
    pub const fn new(
        data_dir: PathBuf,
        device_id: DeviceId,
        metadata_repo: Arc<SqliteMetadataRepository>,
        tag_repo: Arc<SqliteTagRepository>,
        search_repo: Arc<SqliteSearchRepository>,
    ) -> Self {
        Self {
            data_dir,
            device_id,
            metadata_repo,
            tag_repo,
            search_repo,
        }
    }
}

/// Holds an active filesystem watcher so commands can start and stop it.
///
/// WHY `tokio::sync::Mutex`: Tauri v2 async commands run on the tokio
/// executor. Holding a `std::sync::Mutex` across an `.await` point triggers
/// `clippy::await_holding_lock` and can deadlock the async runtime thread.
/// `tokio::sync::Mutex` is await-safe and is the correct choice here.
///
/// WHY two fields: the `DebouncedWatcher` drives the OS-level registration;
/// the `CancellationToken` signals the background event-loop thread to exit
/// before the watcher is dropped. Separating them lets `stop_watch` cancel
/// first and then drop, maintaining the intended shutdown order.
pub struct WatcherState {
    /// The running watcher, or `None` when idle.
    pub(crate) inner: Mutex<Option<perima_fs::DebouncedWatcher>>,
    /// Cancellation token for the background event loop, or `None` when idle.
    pub(crate) cancel: Mutex<Option<CancellationToken>>,
}

impl WatcherState {
    /// Construct an idle (no active watcher) `WatcherState`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: Mutex::const_new(None),
            cancel: Mutex::const_new(None),
        }
    }
}

impl Default for WatcherState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn watcher_state_cancel_lifecycle() {
        let state = WatcherState::new();

        // Starts empty.
        assert!(state.cancel.lock().await.is_none());

        // Inject a token (simulating start_watch's effect on the cancel field).
        let token = CancellationToken::new();
        {
            let mut guard = state.cancel.lock().await;
            *guard = Some(token.clone());
        }
        assert!(state.cancel.lock().await.is_some());
        assert!(!token.is_cancelled());

        // Simulate stop_watch: take + cancel.
        let extracted = {
            let mut guard = state.cancel.lock().await;
            guard.take()
        };
        if let Some(t) = extracted {
            t.cancel();
        }
        assert!(state.cancel.lock().await.is_none());
        assert!(
            token.is_cancelled(),
            "the token we handed to state.cancel must propagate cancellation"
        );
    }
}
