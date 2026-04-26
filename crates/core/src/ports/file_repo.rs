//! File + location repository port (implementations land in phase 1b).

use crate::{
    BlakeHash, CoreError, DeviceId, FileLocationRecord, HashedFile, MediaPath, UpsertOutcome,
    VolumeId,
};

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
}
