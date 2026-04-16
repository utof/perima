//! Domain types and trait ports for perima.
//!
//! Zero framework dependencies.

pub mod errors;
pub mod events;
pub mod ids;
pub mod types;

pub use errors::CoreError;
pub use events::{EventBus, FileEvent};
pub use types::{
    BlakeHash, DeviceId, DiscoveredFile, FileLocationRecord, FileSize, HashedFile, LocationStatus,
    MediaPath, UpsertOutcome, VolumeId, VolumeIdentifiers, VolumeRecord,
};

pub mod ports;
pub use ports::{FileRepository, HashService, Scanner, VolumeRepository};

/// Marker placeholder. Retained as a public symbol for phase-0
/// compatibility tests; will be removed in phase 1b when the real
/// public surface covers it.
pub const CRATE_NAME: &str = "perima-core";
