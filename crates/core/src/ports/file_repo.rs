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
    fn upsert_file(
        &mut self,
        file: &HashedFile,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError>;

    /// Upsert a `file_locations` row for `(volume, relative_path)`.
    ///
    /// # Errors
    /// Returns `CoreError::Duplicate` if the app-level uniqueness
    /// check rejects the row.
    fn upsert_location(
        &mut self,
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
