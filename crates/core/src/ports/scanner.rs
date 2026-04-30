//! Filesystem scanner port.

use std::path::Path;

use crate::{CoreError, DiscoveredFile};

/// Per-file filesystem stat used as the Tier-0 identity-cache lookup tuple.
///
/// Components match `CacheKey`'s mutable triple (`size_bytes`, `mtime_ns`,
/// `fs_file_id`) so the use case can pass values straight through without
/// a second syscall. Spec §4.3 (Tier-0 cache logic).
///
/// WHY `i64` for `mtime_ns` and `fs_file_id`: `SQLite` stores integers as
/// signed 64-bit. The Linux/macOS inode (`u64`) is cast bit-faithfully to
/// `i64` (overflow only for inodes ≥ 2^63 which are not produced by any
/// shipping filesystem). Equality semantics survive the cast as long as
/// every reader uses the same conversion — `CacheKey::fs_file_id: i64`
/// (in `crates/core::ports::identity_cache`) pairs with this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStat {
    /// File size in bytes at observation time.
    pub size_bytes: u64,
    /// Last-modified timestamp in nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    /// Filesystem inode (Unix) or file-id (Windows), bit-faithful `as i64`.
    pub fs_file_id: i64,
}

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

    /// Stat a single file and return the `(size, mtime_ns, fs_file_id)`
    /// triple used to look up the Tier-0 identity cache.
    ///
    /// WHY a separate trait method (instead of widening `walk`'s
    /// `DiscoveredFile`): the walk path is shared with the dry-run + the
    /// no-cache fallback paths; only the cache lookup path needs the
    /// inode + mtime. Keeping `walk` cheap and adding a per-file stat
    /// call where the cache is consulted preserves the dry-run perf
    /// profile.
    ///
    /// # Errors
    /// Returns `CoreError::Io` if the file cannot be opened or
    /// `fstat`-equivalent fails.
    fn stat_with_id(&self, path: &Path) -> Result<FileStat, CoreError>;
}
