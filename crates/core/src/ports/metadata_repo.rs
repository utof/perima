//! Media metadata repository port (implementation lands in `perima-db`).

use crate::{
    BlakeHash, CoreError, DeviceId, FileLocationRecord, MediaMetadata, UpsertOutcome, VolumeId,
};

/// Persistence boundary for `file_metadata`.
///
/// WHY `&self` everywhere (not `&mut self` like `FileRepository`):
/// the existing `FileRepository` trait's `&mut self` signature
/// collides with the `Arc<SqliteFileRepository>` sharing pattern used
/// in desktop state. Newer traits align with the actual usage —
/// interior mutability via `Mutex<Connection>` inside the adapter.
/// `FileRepository` will migrate to `&self` in v0.5.x as a
/// fast-follow (tracked in GH issue).
pub trait MetadataRepository: Send + Sync {
    /// Insert or update the metadata row keyed by `meta.hash`.
    ///
    /// # Errors
    /// Adapter-level failures surface as `CoreError::Internal`.
    fn upsert_metadata(
        &self,
        meta: &MediaMetadata,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError>;

    /// Fetch the metadata row for `hash`, if one exists.
    ///
    /// # Errors
    /// Adapter-level failures surface as `CoreError::Internal`.
    fn find_by_hash(&self, hash: &BlakeHash) -> Result<Option<MediaMetadata>, CoreError>;

    /// List `(file_location, metadata)` pairs up to `limit`, optionally
    /// filtered by `volume`.
    ///
    /// `None` metadata means the extractor has not yet run for that
    /// file (the scanner enqueued it but the worker is behind) or
    /// extraction failed — callers should treat it as "pending", not
    /// "absent".
    ///
    /// # Errors
    /// Adapter-level failures surface as `CoreError::Internal`.
    fn list_with_metadata(
        &self,
        limit: usize,
        volume: Option<VolumeId>,
    ) -> Result<Vec<(FileLocationRecord, Option<MediaMetadata>)>, CoreError>;

    /// Update the thumbnail columns on the `file_metadata` row for
    /// `hash`. Returns the number of rows updated (0 if no metadata row
    /// exists yet, 1 otherwise).
    ///
    /// WHY decoupled from `upsert_metadata`: the queue worker writes
    /// metadata first, then attempts thumbnail generation; the two
    /// writes occur at different times and should not share transaction
    /// semantics. `upsert_metadata`'s Unchanged/Updated equivalence
    /// proxy compares `device_id` + `mime_type` only — a thumbnail
    /// status flip (pending → ready) would otherwise be classified
    /// Unchanged and lost.
    ///
    /// # Errors
    /// Adapter-level failures surface as `CoreError::Internal`.
    fn update_thumbnail(
        &self,
        hash: &BlakeHash,
        path: Option<&str>,
        status: &str,
        device: DeviceId,
    ) -> Result<u64, CoreError>;
}
