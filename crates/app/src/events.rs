//! `EventHandler` trait + shared `recv_loop` helper.
//!
//! `AppContainer::new` (in `crates/app/src/container.rs`) spawns one
//! tokio task per registered handler, each running `recv_loop` against
//! its own `Receiver<AppEvent>` from the bus.

use async_broadcast::{Receiver, RecvError};
use futures::future::FutureExt;
use perima_core::AppEvent;

/// Long-lived consumer of [`AppEvent`]s broadcast by the bus.
///
/// Implementors are spawned as tokio tasks by `AppContainer::new`.
/// Each task owns its own `Receiver` cursor into the shared ring buffer
/// (currently 256 capacity — see [`crate::bus`]).
///
/// **Performance contract.** `handle` should be fast (typical:
/// microseconds for log, milliseconds for DB write or Tauri emit).
/// A handler that blocks for >100ms per event risks filling its inbox
/// under burst load — the next `recv()` returns `Overflowed(n)` and
/// the loop logs a warning. Capacity 256 means a sustained 100ms
/// handler can absorb a 25-second burst.
#[async_trait::async_trait]
pub trait EventHandler: Send + 'static {
    /// Stable identifier for telemetry / logging. Convention: `snake_case`
    /// matching the impl struct (e.g. `"log_event_handler"`).
    fn name(&self) -> &'static str;

    /// Process one event. Panics inside this function are caught by
    /// `recv_loop` and logged; the loop continues.
    async fn handle(&mut self, event: AppEvent);
}

/// Run the recv loop for a single handler. Spawned by
/// `AppContainer::new`.
///
/// Exits when the bus closes (all senders dropped — typically
/// container shutdown). On `Overflowed(n)` from the receiver, logs +
/// continues. On panic inside `handle`, logs + continues to next event.
///
/// WHY `dead_code` allow: `recv_loop` is `pub(crate)` and its only
/// caller will be `AppContainer::new` in Task 6 (not yet landed).
/// Remove this allow once Task 6 wires it up.
#[allow(dead_code)]
pub(crate) async fn recv_loop(
    name: &'static str,
    mut handler: Box<dyn EventHandler>,
    mut recv: Receiver<AppEvent>,
) {
    loop {
        match recv.recv().await {
            Ok(event) => {
                // WHY catch_unwind via FutureExt: a panic inside `handle`
                // should not kill the recv loop. AssertUnwindSafe is
                // required because `&mut handler` is not UnwindSafe by
                // default; we manually assert that handler state recovery
                // is the impl's responsibility.
                let result = std::panic::AssertUnwindSafe(handler.handle(event))
                    .catch_unwind()
                    .await;
                if let Err(panic) = result {
                    tracing::error!(
                        handler = name,
                        panic = ?panic,
                        "event handler panicked; loop continues"
                    );
                }
            }
            Err(RecvError::Overflowed(n)) => {
                tracing::warn!(handler = name, missed = n, "broadcast lag; events dropped");
            }
            Err(RecvError::Closed) => {
                tracing::info!(handler = name, "bus closed; recv loop exiting");
                return;
            }
        }
    }
}
