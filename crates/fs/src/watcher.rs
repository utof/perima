//! Filesystem watcher that maps debounced `notify` events to [`FileEvent`]s.
//!
//! [`DebouncedWatcher`] wraps `notify-debouncer-full` and emits typed
//! [`FileEvent`]s via an [`EventBus`]. Dropping the watcher stops the
//! background task and the underlying debouncer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify::event::{EventKind, ModifyKind, RenameMode};
use notify_debouncer_full::new_debouncer;
use perima_core::{CoreError, EventBus, FileEvent, VolumeId};
use tokio_util::sync::CancellationToken;

use crate::paths::relativize;

/// A running filesystem watcher. Drop to stop watching.
///
/// WHY struct-holds-debouncer: the debouncer's `Drop` impl unregisters the
/// OS watcher. We rely on that to clean up without an explicit `stop()` API —
/// RAII keeps the lifetime predictable.
pub struct DebouncedWatcher {
    /// The debouncer handle — dropping it stops the underlying notify watcher
    /// and the OS-level watch registrations.
    _debouncer: notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
}

impl std::fmt::Debug for DebouncedWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DebouncedWatcher").finish_non_exhaustive()
    }
}

impl DebouncedWatcher {
    /// Start watching `paths` and emit [`FileEvent`]s on `bus`.
    ///
    /// Events are debounced with a `debounce` timeout (1 s in production, 100 ms
    /// in tests). A background tokio task bridges the std `mpsc` channel from
    /// the debouncer to the async world and calls `bus.emit` for each event.
    ///
    /// Dropping the returned `DebouncedWatcher` stops the debouncer, which
    /// closes the sender side of the channel and causes the background task to
    /// exit naturally.
    ///
    /// # Errors
    /// Returns [`CoreError::Internal`] if the debouncer or any initial watch
    /// registration fails.
    pub fn start(
        paths: &[PathBuf],
        volume_root: &Path,
        volume_id: VolumeId,
        bus: Arc<dyn EventBus>,
        cancel: CancellationToken,
        debounce: Duration,
    ) -> Result<Self, CoreError> {
        // WHY canonicalize: on macOS, tempdir() (and other paths) may return
        // /var/folders/... which is a symlink to /private/var/folders/....
        // FSEvents reports the canonical /private/var/... form, so
        // strip_prefix in relativize() fails for every event and nothing
        // reaches the bus.  Canonicalizing both the registered paths and
        // volume_root ensures they match whatever the OS reports in events.
        // crate::platform_path::canonicalize wraps dunce on Windows (avoids
        // UNC prefix pollution) and std::fs::canonicalize elsewhere.
        let canonical_root =
            crate::platform_path::canonicalize(volume_root).map_err(CoreError::Io)?;
        let canonical_paths: Vec<PathBuf> = paths
            .iter()
            .map(|p| crate::platform_path::canonicalize(p).map_err(CoreError::Io))
            .collect::<Result<_, _>>()?;

        // WHY std mpsc: notify-debouncer-full uses std::sync::mpsc for its
        // callback channel. We bridge to async by spawning a
        // tokio::task::spawn_blocking receiver loop.
        let (tx, rx) = std::sync::mpsc::channel();

        let mut debouncer = new_debouncer(debounce, None, tx)
            .map_err(|e| CoreError::Internal(format!("debouncer init: {e}")))?;

        for path in &canonical_paths {
            debouncer
                .watch(path, RecursiveMode::Recursive)
                .map_err(|e| CoreError::Internal(format!("watch {}: {e}", path.display())))?;
        }

        // Use the canonicalized root in the event loop.
        let volume_root = canonical_root;

        // WHY spawn_blocking: `rx.recv()` is a blocking call; running it on the
        // async executor would stall the runtime thread. `spawn_blocking` offloads
        // to the dedicated blocking thread pool.
        tokio::task::spawn_blocking(move || {
            run_event_loop(&rx, &volume_root, volume_id, &bus, &cancel);
        });

        Ok(Self {
            _debouncer: debouncer,
        })
    }
}

/// Blocking event loop — runs on a `spawn_blocking` thread.
///
/// Reads from the debouncer's `std::sync::mpsc::Receiver`, maps each
/// [`notify_debouncer_full::DebouncedEvent`] to a [`FileEvent`], and calls
/// `bus.emit`. Exits when the channel is closed (debouncer dropped) or the
/// cancellation token is cancelled.
fn run_event_loop(
    rx: &std::sync::mpsc::Receiver<notify_debouncer_full::DebounceEventResult>,
    volume_root: &Path,
    volume_id: VolumeId,
    bus: &Arc<dyn EventBus>,
    cancel: &CancellationToken,
) {
    // WHY loop with recv_timeout: recv() blocks indefinitely; using
    // recv_timeout lets us poll cancel between batches so the task
    // exits promptly on Ctrl-C without waiting for the next event.
    let poll = Duration::from_millis(100);

    loop {
        if cancel.is_cancelled() {
            break;
        }

        let batch = match rx.recv_timeout(poll) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            // Channel closed (debouncer dropped) — exit cleanly.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        match batch {
            Ok(debounced_events) => {
                for de in debounced_events {
                    if cancel.is_cancelled() {
                        return;
                    }
                    if let Some(event) = map_event(&de.event, volume_root, volume_id)
                        && let Err(e) = bus.emit(&event)
                    {
                        tracing::warn!(error = %e, "EventBus::emit failed");
                    }
                }
            }
            Err(errors) => {
                for e in errors {
                    tracing::warn!(error = %e, "debouncer error");
                }
            }
        }
    }
}

/// Map a single `notify` event to a [`FileEvent`], or `None` if unrecognised.
///
/// WHY returns Option: events like `Access` or unknown `Modify` subtypes are
/// noise for our use-case. Callers silently skip `None`.
fn map_event(event: &notify::Event, volume_root: &Path, volume_id: VolumeId) -> Option<FileEvent> {
    match &event.kind {
        EventKind::Create(_) => {
            let path = relativize(event.paths.first()?, volume_root).ok()?;
            Some(FileEvent::Created {
                path,
                volume: volume_id,
            })
        }
        EventKind::Modify(ModifyKind::Data(_)) => {
            let path = relativize(event.paths.first()?, volume_root).ok()?;
            Some(FileEvent::Modified {
                path,
                volume: volume_id,
            })
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            // WHY index 0 / 1: notify-debouncer-full merges rename From+To
            // events into a single Both event with paths[0]=old, paths[1]=new.
            let from = relativize(event.paths.first()?, volume_root).ok()?;
            let to = relativize(event.paths.get(1)?, volume_root).ok()?;
            Some(FileEvent::Renamed {
                from,
                to,
                volume: volume_id,
            })
        }
        EventKind::Remove(_) => {
            let path = relativize(event.paths.first()?, volume_root).ok()?;
            Some(FileEvent::Deleted {
                path,
                volume: volume_id,
            })
        }
        // All other event kinds (Access, Modify::Metadata, etc.) are ignored.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use perima_core::{CoreError, EventBus, FileEvent, VolumeId};
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::DebouncedWatcher;

    /// A test-only `EventBus` that collects every emitted event.
    struct MockEventBus {
        events: std::sync::Arc<Mutex<Vec<FileEvent>>>,
    }

    impl EventBus for MockEventBus {
        fn emit(&self, event: &FileEvent) -> Result<(), CoreError> {
            self.events
                .lock()
                .expect("MockEventBus mutex poisoned")
                .push(event.clone());
            Ok(())
        }
    }

    /// Shared handle used by tests to inspect collected events.
    type EventStore = std::sync::Arc<Mutex<Vec<FileEvent>>>;

    fn make_bus() -> (std::sync::Arc<MockEventBus>, EventStore) {
        let store: EventStore = std::sync::Arc::new(Mutex::new(Vec::new()));
        let bus = std::sync::Arc::new(MockEventBus {
            events: std::sync::Arc::clone(&store),
        });
        (bus, store)
    }

    /// Wait up to `timeout` for `predicate` to return `true`, polling every
    /// 50 ms. Returns whether the predicate became true within the timeout.
    async fn wait_for(
        store: &EventStore,
        timeout: Duration,
        mut predicate: impl FnMut(&[FileEvent]) -> bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            {
                let events = store.lock().expect("MockEventBus mutex poisoned");
                if predicate(&events) {
                    return true;
                }
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // WHY 100ms debounce in tests: the production default is 1 s to avoid
    // spurious events; tests need faster feedback so 100 ms is the sweet spot —
    // fast enough not to time out, slow enough for the OS to flush the event.
    const TEST_DEBOUNCE: Duration = Duration::from_millis(100);
    const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

    // Stable test volume id — arbitrary but fixed.
    fn test_volume() -> VolumeId {
        VolumeId(uuid::Uuid::nil())
    }

    #[tokio::test]
    async fn watcher_detects_create() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let (bus, store) = make_bus();
        let cancel = CancellationToken::new();

        let _watcher = DebouncedWatcher::start(
            std::slice::from_ref(&root),
            &root,
            test_volume(),
            bus,
            cancel.clone(),
            TEST_DEBOUNCE,
        )
        .expect("start watcher");

        // Give the watcher a moment to initialise before writing.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let new_file = root.join("hello.txt");
        std::fs::write(&new_file, b"hi").expect("write file");

        let found = wait_for(&store, WAIT_TIMEOUT, |evs| {
            evs.iter().any(|e| matches!(e, FileEvent::Created { .. }))
        })
        .await;

        cancel.cancel();
        assert!(found, "expected FileEvent::Created; got: {store:?}");
    }

    // WHY cfg(target_os = "linux"): notify's macOS backend (FSEvents)
    // coalesces delete into a `Modify` event instead of `Remove`, and
    // coalesces rename into separate Create events with empty paths
    // for the watcher root itself. This is a documented notify quirk
    // on macOS. The watcher works correctly on macOS but the event
    // *kind* we observe is OS-dependent. Testing the Linux semantics
    // is what guarantees our event-mapping logic is correct; macOS
    // behavior is covered by integration-level observation.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn watcher_detects_delete() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().to_path_buf();

        // Pre-create file before starting watcher so the delete is the event.
        let target = root.join("bye.txt");
        std::fs::write(&target, b"bye").expect("write file");

        let (bus, store) = make_bus();
        let cancel = CancellationToken::new();

        let _watcher = DebouncedWatcher::start(
            std::slice::from_ref(&root),
            &root,
            test_volume(),
            bus,
            cancel.clone(),
            TEST_DEBOUNCE,
        )
        .expect("start watcher");

        tokio::time::sleep(Duration::from_millis(50)).await;

        std::fs::remove_file(&target).expect("remove file");

        let found = wait_for(&store, WAIT_TIMEOUT, |evs| {
            evs.iter().any(|e| matches!(e, FileEvent::Deleted { .. }))
        })
        .await;

        cancel.cancel();
        assert!(found, "expected FileEvent::Deleted; got: {store:?}");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn watcher_detects_rename() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().to_path_buf();

        let src = root.join("old_name.txt");
        std::fs::write(&src, b"rename me").expect("write file");

        let (bus, store) = make_bus();
        let cancel = CancellationToken::new();

        let _watcher = DebouncedWatcher::start(
            std::slice::from_ref(&root),
            &root,
            test_volume(),
            bus,
            cancel.clone(),
            TEST_DEBOUNCE,
        )
        .expect("start watcher");

        tokio::time::sleep(Duration::from_millis(50)).await;

        let dst = root.join("new_name.txt");
        std::fs::rename(&src, &dst).expect("rename file");

        let found = wait_for(&store, WAIT_TIMEOUT, |evs| {
            evs.iter().any(|e| {
                if let FileEvent::Renamed { from, to, .. } = e {
                    from.as_str() == "old_name.txt" && to.as_str() == "new_name.txt"
                } else {
                    false
                }
            })
        })
        .await;

        cancel.cancel();
        assert!(found, "expected FileEvent::Renamed; got: {store:?}");
    }
}
