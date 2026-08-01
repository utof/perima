//! Domain types and trait ports for perima.
//!
//! Zero framework dependencies.

#![forbid(unsafe_code)]

pub mod dedup;
pub mod errors;
pub mod events;
pub mod hlc;
pub mod ids;
pub mod metadata;
pub mod search;
pub mod tag;
pub mod transcription;
pub mod types;

pub use dedup::{BatchHandle, BatchId, CollisionGroup, DeviceKind, FullHashOutcome, VerifiedState};
pub use errors::{CoreError, FullHashUnavailableReason};
pub use events::{AppEvent, EventBus, FileEvent, InvalidationReason};
pub use hlc::{HLC_MAX_COUNTER, HLC_MAX_MS, Hlc};
pub use metadata::{MediaMetadata, MetadataExtractor};
pub use search::SearchHit;
pub use tag::{MAX_TAG_LEN, Tag, normalize as normalize_tag};
pub use types::{
    BlakeHash, DeviceId, DiscoveredFile, FileLocationRecord, FileSize, FileUuid, HashedFile,
    LocationStatus, MediaPath, UpsertOutcome, VolumeId, VolumeIdentifiers, VolumeRecord,
};

pub mod ports;
pub use ports::{
    BackfillFileRow, CacheEntry, CacheKey, FileRepository, FileStat, FileWithMetadataRow,
    HashService, IdentityCacheRepository, LocationStatusUpdate, LocationToVerify,
    MetadataRepository, Scanner, SearchRepository, TagRepository, VerifyCandidates,
    VolumeRepository,
};

/// Marker placeholder. Retained as a public symbol for phase-0
/// compatibility tests; will be removed in phase 1b when the real
/// public surface covers it.
pub const CRATE_NAME: &str = "perima-core";
