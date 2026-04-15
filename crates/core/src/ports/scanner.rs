//! Filesystem scanner port.

use std::path::Path;

use crate::{CoreError, DiscoveredFile};

/// Walks a directory tree and produces `DiscoveredFile`s.
pub trait Scanner: Send + Sync {
    /// Walk `root` recursively. Per-entry errors are logged via
    /// `tracing` and skipped (the iterator continues). Only
    /// terminal failures (e.g. permission denied on the root)
    /// return `Err`.
    ///
    /// `volume_root` is used to compute each file's relative path.
    ///
    /// # Errors
    /// Returns `CoreError::Io` if `root` cannot be opened, or
    /// `CoreError::InvalidPath` if `volume_root` is not a prefix
    /// of `root`.
    fn walk<'a>(
        &'a self,
        root: &Path,
        volume_root: &Path,
    ) -> Result<Box<dyn Iterator<Item = DiscoveredFile> + Send + 'a>, CoreError>;
}
