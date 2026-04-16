//! Shared application state injected into every Tauri command.

use std::path::PathBuf;

use perima_core::DeviceId;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// State shared across all Tauri commands via `tauri::State<AppState>`.
///
/// WHY: only `data_dir` and `device_id` are held here — no DB connection.
/// Each command opens its own connection to avoid lifetime / `Send` issues
/// with Tauri's async command system. Under `SQLite` WAL mode the second
/// open is instant because migrations already ran.
pub struct AppState {
    /// Resolved data directory (where `perima.db` lives).
    pub data_dir: PathBuf,
    /// Stable device identifier.
    pub device_id: DeviceId,
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
