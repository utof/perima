//! Volume + volume-mount repository port (implementations in 1b/1c).

use std::path::Path;

use crate::{CoreError, DeviceId, VolumeId, VolumeIdentifiers, VolumeRecord};

/// Persistence boundary for `volumes` + `volume_mounts`.
pub trait VolumeRepository: Send + Sync {
    /// Find a known volume matching the observed identifiers, or
    /// create a new one.
    ///
    /// # Errors
    /// `CoreError::Internal` on adapter failure.
    fn find_or_create(
        &mut self,
        ident: &VolumeIdentifiers,
        device: DeviceId,
    ) -> Result<VolumeId, CoreError>;

    /// Record the current mount path for `volume` on `machine`.
    ///
    /// # Errors
    /// `CoreError::Internal` on adapter failure.
    fn record_mount(
        &mut self,
        volume: VolumeId,
        machine: DeviceId,
        mount: &Path,
    ) -> Result<(), CoreError>;

    /// Enumerate all known volumes with their current mounts for
    /// the given `machine`.
    ///
    /// # Errors
    /// `CoreError::Internal` on adapter failure.
    fn list(&self, machine: DeviceId) -> Result<Vec<VolumeRecord>, CoreError>;
}
