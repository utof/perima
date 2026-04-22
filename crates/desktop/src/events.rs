//! Tauri-specific event emitter for `perima_core::FileEvent`.
//!
//! WHY separate module: `perima_core::FileEvent` uses `MediaPath` and
//! `VolumeId` which carry no framework dependencies. Adding `tauri` imports
//! to core would violate that constraint. This module hosts only the
//! `TauriEventEmitter` adapter; the `FileEvent` type itself already
//! derives `Serialize + specta::Type + #[serde(tag = "type")]` (landed
//! in Batch D Task 4), so no wire-mirror enum is needed here.
//!
//! WHY `FileEventPayload` was deleted (Batch D Task 8): it was a 1:1
//! mirror of `FileEvent` with manual field-string conversions. Now that
//! `FileEvent` derives `Serialize` with `#[serde(tag = "type")]`,
//! `TauriEventEmitter::emit` passes `&FileEvent` directly to `AppHandle::emit`.
//! The JSON wire shape is byte-compatible with the pre-Task-8 channel contract:
//! `{"type":"Created","path":"...","volume":"..."}`.

use tauri::{AppHandle, Emitter};

use perima_core::{CoreError, EventBus, FileEvent};

// ---------------------------------------------------------------------------
// TauriEventEmitter
// ---------------------------------------------------------------------------

/// Emits [`FileEvent`] on the `"file-event"` Tauri channel.
///
/// WHY `AppHandle`: `tauri::AppHandle::emit` broadcasts to all frontend
/// windows without requiring a specific window reference, which is correct
/// for a single-window desktop app and is forward-compatible with multi-window
/// if that ever lands.
pub struct TauriEventEmitter {
    /// The Tauri application handle used to emit events to the frontend.
    pub app_handle: AppHandle,
}

impl std::fmt::Debug for TauriEventEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TauriEventEmitter").finish_non_exhaustive()
    }
}

impl EventBus for TauriEventEmitter {
    fn emit(&self, event: &FileEvent) -> Result<(), CoreError> {
        // WHY direct emit of &FileEvent: post-Batch-D, FileEvent
        // derives Serialize + specta::Type + #[serde(tag = "type")]
        // matching the pre-Batch-D FileEventPayload mirror exactly.
        // The frontend "file-event" channel listener consumes the
        // same JSON shape with no rename.
        self.app_handle
            .emit("file-event", event)
            .map_err(|e| CoreError::Internal(format!("tauri emit: {e}")))
    }
}
