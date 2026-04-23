//! Tauri-specific event handler for [`perima_core::AppEvent`].
//!
//! WHY separate module: `perima_core::AppEvent` uses `MediaPath` and
//! `VolumeId` which carry no framework dependencies. Adding `tauri` imports
//! to core would violate that constraint. This module hosts only the
//! `TauriEventHandler` adapter; the `AppEvent` type itself already
//! derives `Serialize + specta::Type` (Batch E spec §4.1), so no
//! wire-mirror enum is needed here.
//!
//! WHY `FileEventPayload` was deleted (Batch D Task 8): it was a 1:1
//! mirror of `FileEvent` with manual field-string conversions. Now that
//! `FileEvent` derives `Serialize` with `#[serde(tag = "type")]`,
//! the Tauri handler passes the full `AppEvent` envelope directly.
//!
//! WHY channel renamed from `"file-event"` to `"app-event"` (Batch E Task 11):
//! the frontend now receives the entire `AppEvent` envelope — including
//! `ScanCompleted` and `IndexInvalidated` — via a single `"app-event"` channel.
//! The previous `"file-event"` channel only delivered `FileEvent` variants.
//! `apps/desktop/src/api.ts::subscribeToAppEvents` (Task 12) is the single
//! subscriber.

use tauri::{AppHandle, Emitter};

use perima_app::EventHandler;
use perima_core::AppEvent;

// ---------------------------------------------------------------------------
// TauriEventHandler
// ---------------------------------------------------------------------------

/// Emits [`AppEvent`] on the `"app-event"` Tauri channel.
///
/// WHY `AppHandle`: `tauri::AppHandle::emit` broadcasts to all frontend
/// windows without requiring a specific window reference, which is correct
/// for a single-window desktop app and is forward-compatible with multi-window
/// if that ever lands.
///
/// WHY `"app-event"` channel (not `"file-event"`): the full `AppEvent`
/// envelope carries `File`, `ScanCompleted`, and `IndexInvalidated` variants.
/// The frontend's `subscribeToAppEvents` in `api.ts` (Task 12) is the
/// single subscriber. The previous `"file-event"` channel is gone.
pub struct TauriEventHandler {
    /// The Tauri application handle used to emit events to the frontend.
    pub app_handle: AppHandle,
}

impl std::fmt::Debug for TauriEventHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TauriEventHandler").finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl EventHandler for TauriEventHandler {
    fn name(&self) -> &'static str {
        "tauri_event_handler"
    }

    async fn handle(&mut self, event: AppEvent) {
        // WHY direct emit of AppEvent: AppEvent derives Serialize +
        // cfg_attr specta::Type (Batch E spec §4.1). Channel name
        // "app-event" replaces the pre-Batch-E "file-event" channel —
        // frontend's subscribeToAppEvents (api.ts in Task 12) is the
        // single subscriber. Emitting the full envelope (not just the
        // FileEvent inner) lets the frontend receive ScanCompleted +
        // IndexInvalidated uniformly.
        if let Err(e) = self.app_handle.emit("app-event", &event) {
            tracing::warn!(error = %e, "failed to emit AppEvent to Tauri channel");
        }
    }
}
