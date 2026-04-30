//! Handler spawn behavior — task lifetime, panic recovery.
//!
//! These tests exercise `recv_loop` indirectly via the public
//! `AppContainer::new` API once Task 6 lands. For Task 4 standalone,
//! we test the trait + a manual spawn pattern that mirrors what
//! `AppContainer::new` will do.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_broadcast::Receiver;
use perima_app::{Bus, EventHandler};
use perima_core::{AppEvent, EventBus, FileEvent, MediaPath, VolumeId};

const fn nil_volume() -> VolumeId {
    VolumeId(uuid::Uuid::nil())
}

fn file_event(name: &str) -> AppEvent {
    AppEvent::File(FileEvent::Created {
        path: MediaPath::new(name),
        volume: nil_volume(),
        file_uuid: None,
    })
}

/// Counting handler — increments a shared counter on every event.
struct CountingHandler {
    counter: Arc<Mutex<usize>>,
}

#[async_trait::async_trait]
impl EventHandler for CountingHandler {
    fn name(&self) -> &'static str {
        "counting_handler"
    }
    async fn handle(&mut self, _event: AppEvent) {
        *self.counter.lock().expect("counting_handler counter lock") += 1;
    }
}

/// Panicking-then-recovering handler — panics on first call, succeeds after.
struct PanicOnceHandler {
    counter: Arc<Mutex<usize>>,
    has_panicked: Arc<Mutex<bool>>,
}

#[async_trait::async_trait]
impl EventHandler for PanicOnceHandler {
    fn name(&self) -> &'static str {
        "panic_once_handler"
    }
    async fn handle(&mut self, _event: AppEvent) {
        // WHY: read + set the flag in a scoped block so the `MutexGuard`
        // is dropped before we panic. Panicking while holding a `MutexGuard`
        // poisons the mutex, which would cause the second call's
        // `.lock().expect(...)` to panic with `PoisonError` rather than
        // incrementing the counter.
        let should_panic = {
            let mut p = self
                .has_panicked
                .lock()
                .expect("panic_once_handler has_panicked lock");
            if *p {
                false
            } else {
                *p = true;
                true
            }
        };
        assert!(!should_panic, "first call panic");
        *self
            .counter
            .lock()
            .expect("panic_once_handler counter lock") += 1;
    }
}

/// Mirror what `AppContainer::new` will do — spawn the handler task.
/// Calls `perima_app::events::recv_loop` indirectly by inlining its
/// shape (`recv_loop` is `pub(crate)`; for the test we re-implement the
/// loop inline). After Task 6, this test re-points at `AppContainer::new`.
fn spawn_handler_task(
    bus: &Arc<Bus>,
    handler: Box<dyn EventHandler>,
) -> tokio::task::JoinHandle<()> {
    let recv = bus.subscribe();
    tokio::spawn(run_loop(handler, recv))
}

async fn run_loop(mut handler: Box<dyn EventHandler>, mut recv: Receiver<AppEvent>) {
    use futures::FutureExt;
    let name = handler.name();
    loop {
        match recv.recv().await {
            Ok(event) => {
                let result = std::panic::AssertUnwindSafe(handler.handle(event))
                    .catch_unwind()
                    .await;
                if let Err(panic) = result {
                    tracing::error!(handler = name, panic = ?panic, "panic; continuing");
                }
            }
            Err(async_broadcast::RecvError::Overflowed(n)) => {
                tracing::warn!(handler = name, missed = n, "lag");
            }
            Err(async_broadcast::RecvError::Closed) => return,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handler_runs_until_bus_dropped() {
    let bus = Bus::new();
    let counter = Arc::new(Mutex::new(0_usize));
    let handler = Box::new(CountingHandler {
        counter: Arc::clone(&counter),
    });
    let task = spawn_handler_task(&bus, handler);

    bus.emit(&file_event("a.jpg")).expect("emit");
    bus.emit(&file_event("b.jpg")).expect("emit");

    // Give the task time to drain.
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(*counter.lock().expect("counter lock"), 2);

    // Drop the bus → channel sender drops → recv returns Closed → task exits.
    // WHY this works: `Bus._inactive` is an `InactiveReceiver` (does NOT prevent
    // sender-close from propagating `Closed` to active receivers). Dropping `Bus`
    // drops the only `Sender`, so all active `recv()` calls immediately return
    // `RecvError::Closed`.
    drop(bus);
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("task timeout")
        .expect("task panic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handler_panic_recovers_once() {
    let bus = Bus::new();
    let counter = Arc::new(Mutex::new(0_usize));
    let has_panicked = Arc::new(Mutex::new(false));
    let handler = Box::new(PanicOnceHandler {
        counter: Arc::clone(&counter),
        has_panicked: Arc::clone(&has_panicked),
    });
    let _task = spawn_handler_task(&bus, handler);

    // First emit triggers the panic; recv_loop logs + continues.
    bus.emit(&file_event("a.jpg")).expect("emit");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        *has_panicked.lock().expect("has_panicked lock"),
        "expected first call to panic"
    );
    assert_eq!(*counter.lock().expect("counter lock"), 0);

    // Second emit: panic flag set, handler increments counter.
    bus.emit(&file_event("b.jpg")).expect("emit");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(*counter.lock().expect("counter lock"), 1);
}
