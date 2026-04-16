//! Filesystem event types and the `EventBus` trait.

use serde::Serialize;

use crate::{CoreError, MediaPath, VolumeId};

/// A filesystem event detected by the watcher.
#[derive(Clone, Debug, Serialize)]
pub enum FileEvent {
    /// A new file appeared at this path.
    Created {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
    /// An existing file's content was modified.
    Modified {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
    /// A file was deleted from this path.
    Deleted {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
    /// A file was renamed/moved within the same volume.
    Renamed {
        /// Previous relative path.
        from: MediaPath,
        /// New relative path.
        to: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
}

/// Consumer of filesystem events.
///
/// Multiple implementations can be composed via a fan-out adapter
/// (e.g., `CompositeEventBus`). The composite logs errors from
/// individual handlers but does not abort — remaining handlers
/// still fire.
pub trait EventBus: Send + Sync {
    /// Process an event.
    ///
    /// # Errors
    /// Returns `CoreError` if the handler fails (e.g., DB write).
    fn emit(&self, event: &FileEvent) -> Result<(), CoreError>;
}
