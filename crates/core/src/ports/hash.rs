//! Hash service port.

use std::path::Path;

use crate::{BlakeHash, CoreError};

/// BLAKE3-based content hashing.
///
/// WHY `Send + Sync`: although phase 1a is entirely synchronous, the
/// scan loop uses `rayon` to parallelize `full_hash` calls across
/// files. Keeping the trait `Send + Sync` also leaves room for a
/// future async adapter without breaking the trait.
pub trait HashService: Send + Sync {
    /// Hash only the first 64 KiB. Cheap change-detection used by
    /// the phase 3 watcher. Phase 1a callers always use `full_hash`.
    ///
    /// # Errors
    /// Returns `CoreError::Io` on read failures.
    fn quick_hash(&self, path: &Path) -> Result<BlakeHash, CoreError>;

    /// Hash the entire file.
    ///
    /// # Errors
    /// Returns `CoreError::Io` on read failures.
    fn full_hash(&self, path: &Path) -> Result<BlakeHash, CoreError>;
}
