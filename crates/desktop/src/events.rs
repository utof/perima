//! Tauri-specific event payload types and the `TauriEventEmitter`.
//!
//! WHY separate module: `perima_core::FileEvent` uses `MediaPath` and
//! `VolumeId` which carry no framework dependencies. Adding `specta::Type`
//! or `tauri` imports to core would violate that constraint. This module
//! defines thin wrapper types that implement IPC-boundary traits without
//! touching core domain types.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use perima_core::{CoreError, EventBus, FileEvent};

// ---------------------------------------------------------------------------
// Wire type
// ---------------------------------------------------------------------------

/// Payload emitted on the `"file-event"` Tauri channel.
///
/// WHY `#[serde(tag = "type")]`: produces a discriminated union
/// `{"type":"Created","path":"...","volume":"..."}` which matches the
/// TypeScript `FileEvent` union defined in `apps/desktop/src/types.ts`.
#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(tag = "type")]
pub enum FileEventPayload {
    /// A new file appeared at `path`.
    Created {
        /// Relative path within the volume.
        path: String,
        /// Volume UUID string.
        volume: String,
    },
    /// An existing file's content was modified.
    Modified {
        /// Relative path within the volume.
        path: String,
        /// Volume UUID string.
        volume: String,
    },
    /// A file was deleted from `path`.
    Deleted {
        /// Relative path within the volume.
        path: String,
        /// Volume UUID string.
        volume: String,
    },
    /// A file was renamed/moved within the same volume.
    Renamed {
        /// Previous relative path.
        from: String,
        /// New relative path.
        to: String,
        /// Volume UUID string.
        volume: String,
    },
}

impl From<&FileEvent> for FileEventPayload {
    fn from(event: &FileEvent) -> Self {
        match event {
            FileEvent::Created { path, volume } => Self::Created {
                path: path.as_str().to_owned(),
                volume: volume.0.to_string(),
            },
            FileEvent::Modified { path, volume } => Self::Modified {
                path: path.as_str().to_owned(),
                volume: volume.0.to_string(),
            },
            FileEvent::Deleted { path, volume } => Self::Deleted {
                path: path.as_str().to_owned(),
                volume: volume.0.to_string(),
            },
            FileEvent::Renamed { from, to, volume } => Self::Renamed {
                from: from.as_str().to_owned(),
                to: to.as_str().to_owned(),
                volume: volume.0.to_string(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// TauriEventEmitter
// ---------------------------------------------------------------------------

/// Emits [`FileEventPayload`] on the `"file-event"` Tauri channel.
///
/// WHY `AppHandle`: `tauri::AppHandle::emit` broadcasts to all frontend
/// windows without requiring a specific window reference, which is correct
/// for a single-window desktop app and is forward-compatible with multi-window
/// if that ever lands.
pub struct TauriEventEmitter {
    /// The Tauri application handle used to emit events to the frontend.
    pub app_handle: AppHandle,
}

impl EventBus for TauriEventEmitter {
    fn emit(&self, event: &FileEvent) -> Result<(), CoreError> {
        let payload: FileEventPayload = event.into();
        self.app_handle
            .emit("file-event", payload)
            .map_err(|e| CoreError::Internal(format!("tauri emit: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use perima_core::{MediaPath, VolumeId};
    use uuid::Uuid;

    #[test]
    fn file_event_created_serializes_correctly() {
        let volume = VolumeId(Uuid::nil());
        let path = MediaPath::new("photos/img.jpg");
        let event = FileEvent::Created {
            path: path.clone(),
            volume,
        };

        let payload: FileEventPayload = (&event).into();
        let json = serde_json::to_string(&payload).expect("serialize");

        // Assert discriminated-union shape: {"type":"Created","path":"...","volume":"..."}
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["type"], "Created");
        assert_eq!(v["path"], path.as_str());
        assert_eq!(v["volume"], Uuid::nil().to_string());
    }
}
