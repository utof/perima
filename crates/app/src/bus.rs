//! Application-wide event bus backed by `async_broadcast`.
//!
//! Replaces the synchronous `CompositeEventBus` (deleted in Task 6).
//! Single construction site: [`crate::container::AppContainer::new`].

use std::sync::Arc;

use async_broadcast::{InactiveReceiver, Receiver, Sender, TrySendError, broadcast};
use perima_core::{AppEvent, CoreError, EventBus};

/// Bus's bounded shared-buffer capacity. Backpressure mode (default —
/// `set_overflow(false)`): when the buffer is saturated, `try_broadcast`
/// returns `Err(Full)` (mapped to `Ok(())` with a warn log in `Bus::emit`).
/// Receivers track their own cursor into the shared ring buffer; the
/// next `recv()` simply yields the next un-dropped event. Bus's
/// backpressure mode never surfaces `RecvError::Overflowed(n)` — that
/// path only fires under `set_overflow(true)` (overflow mode), which
/// the raw-channel test `receiver_overflowed_keeps_going` exercises
/// to document the contract async-broadcast provides for late-joiner
/// replay (umbrella spec §A7 future work).
///
/// Capacity 256 is the umbrella spec §A7 number; a fast subscriber
/// drains 256 in milliseconds.
const CAPACITY: usize = 256;

/// Application-wide event bus.
///
/// WHY no ring buffer: deferred per spec §2.2 OUT (umbrella §A7
/// requirement, but no v1 late-joiner consumer exists). Add when first
/// late-joiner ships (likely Phase 6 mobile / HTTP shell). See GH issue
/// `architecture-spec-followup` filed in Batch E Task 13.
pub struct Bus {
    sender: Sender<AppEvent>,
    /// WHY `InactiveReceiver`: async-broadcast closes the channel when
    /// the last active receiver drops. We hold an inactive receiver
    /// alive so `Bus::emit` doesn't get `TrySendError::Closed` between
    /// the moment a handler task drops and the next is spawned.
    _inactive: InactiveReceiver<AppEvent>,
}

// WHY allow: `InactiveReceiver<T>` does not implement `Debug`, so `_inactive`
// cannot be included. The field intentionally omitted — it is a channel anchor
// with no user-visible state beyond "present/absent".
#[allow(clippy::missing_fields_in_debug)]
impl std::fmt::Debug for Bus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bus")
            .field("capacity", &CAPACITY)
            .field("receivers", &self.sender.receiver_count())
            .finish()
    }
}

impl Bus {
    /// Construct a fresh bus with a `CAPACITY`-sized shared ring buffer.
    /// Holds an inactive receiver alive so the channel doesn't close
    /// when subscribers drop.
    #[must_use]
    pub fn new() -> Arc<Self> {
        let (sender, receiver) = broadcast(CAPACITY);
        let inactive = receiver.deactivate();
        Arc::new(Self {
            sender,
            _inactive: inactive,
        })
    }

    /// Subscribe to the bus. Returns a fresh `Receiver` whose cursor
    /// tracks the shared ring buffer. Drop the receiver to unsubscribe.
    #[must_use]
    pub fn subscribe(&self) -> Receiver<AppEvent> {
        self.sender.new_receiver()
    }

    /// Number of active receivers — useful for tests + introspection.
    #[must_use]
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl EventBus for Bus {
    fn emit(&self, event: &AppEvent) -> Result<(), CoreError> {
        // WHY try_broadcast (sync) not broadcast (async): writer thread
        // (Batch C) is on std::thread::spawn, NOT on the tokio runtime.
        // Async emit would force block_on with a runtime handle, which
        // risks deadlock if any subscriber panics. Trade-off documented
        // in spec §3 row G + §2.2 OUT.
        match self.sender.try_broadcast(event.clone()) {
            Ok(_) => Ok(()),
            Err(TrySendError::Full(_)) => {
                tracing::warn!(
                    event_kind = event_kind(event),
                    receivers = self.sender.receiver_count(),
                    "broadcast inbox full; subscriber too slow"
                );
                Ok(())
            }
            Err(TrySendError::Closed(_)) => {
                tracing::info!("bus closed (no active subscribers)");
                Ok(())
            }
            Err(TrySendError::Inactive(_)) => {
                // WHY: we hold an InactiveReceiver alive in self._inactive,
                // so this arm should be unreachable in practice. Log loudly
                // if it ever fires — it indicates the inactive-receiver
                // invariant was broken.
                tracing::error!("bus has zero receivers (impossible state)");
                Ok(())
            }
        }
    }
}

const fn event_kind(e: &AppEvent) -> &'static str {
    match e {
        AppEvent::File(_) => "File",
        AppEvent::ScanCompleted { .. } => "ScanCompleted",
        AppEvent::IndexInvalidated { .. } => "IndexInvalidated",
    }
}
