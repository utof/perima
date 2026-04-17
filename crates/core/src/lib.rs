//! Domain types and trait ports for perima.
//!
//! Zero framework dependencies.

#![forbid(unsafe_code)]

pub mod errors;
pub mod events;
pub mod ids;
pub mod metadata;
pub mod search;
pub mod tag;
pub mod types;

pub use errors::CoreError;
pub use events::{EventBus, FileEvent};
pub use metadata::{MediaMetadata, MetadataExtractor};
pub use search::SearchHit;
pub use tag::{MAX_TAG_LEN, Tag, normalize as normalize_tag};
pub use types::{
    BlakeHash, DeviceId, DiscoveredFile, FileLocationRecord, FileSize, HashedFile, LocationStatus,
    MediaPath, UpsertOutcome, VolumeId, VolumeIdentifiers, VolumeRecord,
};

pub mod ports;
pub use ports::{
    FileRepository, HashService, MetadataRepository, Scanner, SearchRepository, TagRepository,
    VolumeRepository,
};

/// Marker placeholder. Retained as a public symbol for phase-0
/// compatibility tests; will be removed in phase 1b when the real
/// public surface covers it.
pub const CRATE_NAME: &str = "perima-core";
