//! Shared application state injected into every Tauri command.

use std::path::PathBuf;
use std::sync::Arc;

use perima_app::AppContainer;
use perima_core::DeviceId;
use perima_db::{SqliteMetadataRepository, SqliteSearchRepository, SqliteTagRepository};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// State shared across all Tauri commands via `tauri::State<AppState>`.
///
/// WHY `data_dir` + `device_id` without a general DB handle: commands
/// that read through `FileRepository` still open their own
/// `rusqlite::Connection` per-call (Tasks 3-6 migrate them to the
/// writer actor). `VolumeRepository` as of Batch C Task 2 goes through
/// `container.volumes` — no per-call open. WAL mode makes the remaining
/// second open cheap for tests and the not-yet-migrated repos.
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
    ///
    /// WHY kept post-Batch-B: `start_watch` + the `_inner` test helpers
    /// (`run_scan_inner`, `list_files_inner`, `list_volumes_inner`) still
    /// open short-lived per-call connections rooted at this directory.
    /// Remove once those sites are fully ported (tracked as post-Batch-B
    /// cleanup; see spec §5 risk mitigation "additive `AppState`").
    pub data_dir: PathBuf,
    /// Stable device identifier.
    pub device_id: DeviceId,
    /// Shared metadata repository handle.
    ///
    /// WHY retained during Batch B: test helpers in
    /// `crates/desktop/tests/commands_test.rs` still construct this repo
    /// directly via `_inner` entry points. The migrated `#[tauri::command]`
    /// handlers go through `container.metadata.execute` instead; this
    /// field's only remaining consumer is the preserved test-helper seam.
    pub metadata_repo: Arc<SqliteMetadataRepository>,
    /// Shared tag repository handle.
    ///
    /// WHY retained: same rationale as `metadata_repo` — preserved during
    /// Batch B for the `_inner` test-helper contract; migrated commands
    /// call `container.tag.execute` instead.
    pub tag_repo: Arc<SqliteTagRepository>,
    /// Shared search repository handle.
    ///
    /// WHY retained: same rationale as `metadata_repo` — preserved during
    /// Batch B for the `_inner` test-helper contract; migrated commands
    /// call `container.search.execute` instead.
    pub search_repo: Arc<SqliteSearchRepository>,
    /// Single orchestration hub — every migrated handler calls through
    /// one of the `UseCase` fields on this container.
    ///
    /// WHY `Arc<AppContainer>`: `AppContainer` is already internally
    /// `Arc`-shared across its `UseCase` fields (see `perima_app::container`).
    /// Wrapping the container itself in an `Arc` lets Tauri's
    /// `manage(state)` move the value without forcing a clone per command
    /// dispatch; every command then accesses `state.container.*` through
    /// a single dereference.
    pub container: Arc<AppContainer>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

impl AppState {
    /// Construct a new `AppState` from a resolved config + repo handles +
    /// an assembled [`AppContainer`].
    ///
    /// WHY a constructor (rather than public struct literal): keeps the
    /// Arc-sharing contract for every field explicit at the single
    /// construction site in `run()`. Also preserves the additive-migration
    /// invariant — callers that forget to pass `container` get a compile
    /// error rather than a silently missing dependency.
    #[must_use]
    pub const fn new(
        data_dir: PathBuf,
        device_id: DeviceId,
        metadata_repo: Arc<SqliteMetadataRepository>,
        tag_repo: Arc<SqliteTagRepository>,
        search_repo: Arc<SqliteSearchRepository>,
        container: Arc<AppContainer>,
    ) -> Self {
        Self {
            data_dir,
            device_id,
            metadata_repo,
            tag_repo,
            search_repo,
            container,
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

impl std::fmt::Debug for WatcherState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatcherState").finish_non_exhaustive()
    }
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
