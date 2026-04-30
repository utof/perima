//! Media metadata repository port (implementation lands in `perima-db`).

use crate::{
    BlakeHash, CoreError, DeviceId, FileLocationRecord, MediaMetadata, UpsertOutcome, VolumeId,
};

/// A `(location, metadata, quick_hash)` row returned by
/// [`MetadataRepository::list_with_metadata`].
///
/// WHY type alias: `clippy::type_complexity` fires on the 3-tuple when it
/// appears inline in the trait method signature; a named alias both satisfies
/// the lint and documents the shape in one place.
///
/// `quick_hash` is `files.quick_hash` as a lowercase hex string, or `None`
/// if the backfill worker has not yet run for this row. The frontend uses
/// equality with `hash` to detect placeholder rows
/// (`hash == quick_hash` → full hash not yet computed).
pub type FileWithMetadataRow = (FileLocationRecord, Option<MediaMetadata>, Option<String>);

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

    /// List `(file_location, metadata, quick_hash)` triples up to `limit`,
    /// optionally filtered by `volume`.
    ///
    /// `None` metadata means the extractor has not yet run for that
    /// file (the scanner enqueued it but the worker is behind) or
    /// extraction failed — callers should treat it as "pending", not
    /// "absent".
    ///
    /// The third element is `files.quick_hash` as a lowercase hex string,
    /// or `None` if the backfill worker has not yet run for this row.
    /// The frontend uses it to detect placeholder rows: when
    /// `location.hash == quick_hash` the file has a quick-hash-only
    /// identity and the full canonical hash has not been computed yet.
    ///
    /// # Errors
    /// Adapter-level failures surface as `CoreError::Internal`.
    fn list_with_metadata(
        &self,
        limit: usize,
        volume: Option<VolumeId>,
    ) -> Result<Vec<FileWithMetadataRow>, CoreError>;

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
