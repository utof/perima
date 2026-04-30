//! File + location repository port (implementations land in phase 1b).

use std::path::PathBuf;

use crate::{
    BlakeHash, CollisionGroup, CoreError, DeviceId, FileLocationRecord, FileUuid, HashedFile,
    MediaPath, UpsertOutcome, VolumeId,
};

/// One row returned by [`FileRepository::list_files_needing_backfill`].
///
/// Contains enough information for the backfill worker to compute and
/// store `quick_hash` without any additional DB reads per row.
#[derive(Debug, Clone)]
pub struct BackfillFileRow {
    /// BLAKE3 full-content hash — used as the `files` PK for the write path.
    pub hash: BlakeHash,
    /// File size in bytes — passed to `quick_hash_prefix_suffix` to select
    /// the prefix-‖-suffix vs whole-file hashing strategy (spec §4.4).
    pub size_bytes: u64,
    /// Absolute path of an active `file_locations` entry on the current device.
    ///
    /// `None` when no active location exists (file on an unmounted volume or
    /// all locations soft-deleted). The backfill worker skips these rows.
    pub active_path: Option<PathBuf>,
}

/// Persistence boundary for `files` + `file_locations`.
pub trait FileRepository: Send + Sync {
    /// Upsert the content-addressed `files` row.
    ///
    /// # Errors
    /// Adapter-level errors are surfaced as `CoreError::Internal`
    /// unless they map to a typed variant.
    fn upsert_file(&self, file: &HashedFile, device: DeviceId) -> Result<UpsertOutcome, CoreError>;

    /// Upsert the content-addressed `files` row, optionally populating
    /// `files.quick_hash` on INSERT.
    ///
    /// `quick_hash` is the cheap BLAKE3 prefix+suffix fingerprint computed
    /// during scan (spec §4.1.1). Adapters that persist this field (the
    /// `SQLite` adapter in `perima_db`) override this method; all others fall
    /// back to the base [`Self::upsert_file`] call via the default
    /// implementation.
    ///
    /// # Errors
    /// Same as [`Self::upsert_file`].
    fn upsert_file_with_quick_hash(
        &self,
        file: &HashedFile,
        device: DeviceId,
        quick_hash: Option<BlakeHash>,
    ) -> Result<UpsertOutcome, CoreError> {
        // WHY default ignores quick_hash: trait impls that don't persist
        // the fingerprint (mocks, test stubs, future in-memory adapters)
        // get correct behaviour without boilerplate. The SQLite adapter
        // overrides this to populate files.quick_hash per spec §4.1.1.
        let _ = quick_hash;
        self.upsert_file(file, device)
    }

    /// Upsert a `file_locations` row for `(volume, relative_path)`.
    ///
    /// # Errors
    /// Returns `CoreError::Duplicate` if the app-level uniqueness
    /// check rejects the row.
    fn upsert_location(
        &self,
        hash: &BlakeHash,
        volume: VolumeId,
        path: &MediaPath,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError>;

    /// List `(file, location)` joins. Used by `perima ls` in phase 1b.
    ///
    /// # Errors
    /// Adapter-level errors become `CoreError::Internal`.
    fn list_file_locations(
        &self,
        limit: usize,
        volume: Option<VolumeId>,
    ) -> Result<Vec<FileLocationRecord>, CoreError>;

    /// Return up to `limit` file rows whose `quick_hash` column is `NULL`,
    /// joined with the most-recent active `file_locations` entry to provide
    /// an on-disk path.
    ///
    /// Used by the quick-hash backfill worker (`perima_app::QuickHashBackfillWorker`)
    /// at startup to seed its work iterator (spec §4.1.5).
    ///
    /// # Default implementation
    ///
    /// Returns an empty `Vec` — adapters that do not persist `quick_hash`
    /// (test stubs, future in-memory adapters) need no backfill. The
    /// `perima_db::SqliteFileRepository` overrides this with the real
    /// `SELECT … WHERE quick_hash IS NULL` query.
    ///
    /// # Errors
    /// Adapter-level errors become `CoreError::Internal`.
    fn list_files_needing_backfill(&self, limit: u32) -> Result<Vec<BackfillFileRow>, CoreError> {
        let _ = limit;
        // WHY: default no-op; adapters without a `quick_hash` column never
        // have NULL rows to backfill. The SQLite adapter overrides.
        Ok(Vec::new())
    }

    /// Look up a `(blake3_hash, absolute_path, size_bytes)` tuple by `file_uuid`.
    ///
    /// Used by `ComputeFullHashUseCase::execute_single` (spec §4.7.2) to fetch
    /// the on-disk path + size for a `full_hash` compute. Returns `None` if no
    /// row exists for `file_uuid` or if no active mounted location is available
    /// (which the caller treats as `CoreError::FullHashUnavailable`).
    ///
    /// The `blake3_hash` in the tuple is `None` for rows whose `full_hash`
    /// has not yet been computed (V011 nullable `files.blake3_hash`). The
    /// compute path only needs the on-disk path + size; callers that need a
    /// real hash must check for `Some` (e.g., tag-attach by uuid).
    ///
    /// # Default implementation
    ///
    /// Returns `Ok(None)` so non-SQLite adapters compile without surface change.
    /// `perima_db::SqliteFileRepository` overrides with a real query.
    ///
    /// # Errors
    /// Adapter-level errors become `CoreError::Internal`.
    fn lookup_by_file_uuid(
        &self,
        file_uuid: FileUuid,
    ) -> Result<Option<(Option<BlakeHash>, PathBuf, u64)>, CoreError> {
        let _ = file_uuid;
        Ok(None)
    }

    /// Promote a freshly computed `full_hash` onto the `files` row keyed
    /// by `file_uuid`. Updates `files.blake3_hash` AND a placeholder
    /// `full_hash` column (today the schema has only `blake3_hash` — the
    /// V0NN column split is tracked as #161).
    ///
    /// # Default implementation
    ///
    /// Returns `Ok(())` (no-op) so non-SQLite adapters compile.
    ///
    /// # Errors
    /// Adapter-level errors become `CoreError::Internal`.
    fn update_full_hash(&self, file_uuid: FileUuid, hash: BlakeHash) -> Result<(), CoreError> {
        let _ = (file_uuid, hash);
        Ok(())
    }

    /// Return all groups of files whose `quick_hash` matches one or more
    /// other rows AND that have not been marked `verified_distinct`.
    ///
    /// Used by `DedupUseCase::list_collisions` (spec §4.6) — surfaces
    /// candidate duplicates for the frontend dedup UX.
    ///
    /// # Default implementation
    ///
    /// Returns an empty `Vec` so non-SQLite adapters compile.
    ///
    /// # Errors
    /// Adapter-level errors become `CoreError::Internal`.
    fn list_quick_hash_collisions(&self) -> Result<Vec<CollisionGroup>, CoreError> {
        Ok(Vec::new())
    }

    /// Mark every `file_uuid` in `file_uuids` as `verified_distinct = 1`.
    ///
    /// Used by `DedupUseCase::mark_verified_distinct` after a manual or
    /// automatic verification proves a quick-hash collision is a false
    /// positive (full hashes differ).
    ///
    /// # Default implementation
    ///
    /// Returns `Ok(())` (no-op) so non-SQLite adapters compile.
    ///
    /// # Errors
    /// Adapter-level errors become `CoreError::Internal`.
    fn mark_verified_distinct(
        &self,
        file_uuids: Vec<FileUuid>,
        device: DeviceId,
    ) -> Result<(), CoreError> {
        let _ = (file_uuids, device);
        Ok(())
    }

    /// Return file UUIDs for every row whose `full_hash` (`blake3_hash`) is
    /// `NULL` AND that has an active mounted location on this device.
    ///
    /// Used by `perima hash --pending` to identify files that have been scanned
    /// (and have a `quick_hash`) but whose canonical full hash has not yet been
    /// computed.
    ///
    /// # Default implementation
    ///
    /// Returns an empty `Vec` so non-SQLite adapters compile.
    ///
    /// # Errors
    /// Adapter-level errors become `CoreError::Internal`.
    fn list_files_pending_full_hash(&self, limit: usize) -> Result<Vec<FileUuid>, CoreError> {
        let _ = limit;
        Ok(Vec::new())
    }
}
