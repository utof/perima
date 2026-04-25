//! Identity-cache repository port.
//!
//! `file_identity_cache` is a **device-local** table (never synced) that
//! stores per-file filesystem metadata alongside a `quick_hash` so the scan
//! loop can skip rehashing unchanged files. Because the table is device-local
//! it carries NO `hlc` column (CLAUDE.md "Schema rules expansion").
//!
//! WHY sync `&self` methods: the writer actor (Batch C) is an OS thread,
//! not a tokio task; port trait methods are sync (no `.await`). Interior
//! mutability lives in the adapter, not the port.

use crate::{BlakeHash, CoreError, DeviceId, VolumeId};

/// Lookup key for a `file_identity_cache` row.
///
/// Identifies a specific on-disk file at a specific point in time via the
/// device + volume coordinates, the filesystem inode / file-id, the exact
/// byte size, and the last-modified nanosecond timestamp.
///
/// WHY `mtime_ns` is in the key: when `mtime_ns` changes, the cached
/// `quick_hash` is stale. Including `mtime_ns` in the lookup key means a
/// changed file simply misses the cache; the stale row is separately
/// soft-deleted by the scan loop before inserting the fresh entry.
///
/// WHY the lookup index is non-unique: `mtime_ns` is mutable — a UNIQUE
/// constraint on a mutable column violates CLAUDE.md "Schema rules". The
/// application layer enforces the "one live entry per lookup tuple"
/// invariant by soft-deleting the old row before inserting the new one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheKey {
    /// Device that observed this file.
    pub device_id: DeviceId,
    /// Volume the file lives on.
    pub volume_id: VolumeId,
    /// Filesystem inode / file-id, cast to `i64` for the `SQLite` `INTEGER`
    /// column. Callers cast `u64` → `i64` directly (Unix `Metadata::ino()`
    /// → `as i64` is bit-faithful; equality semantics preserved as long as
    /// every read site uses the same cast).
    pub fs_file_id: i64,
    /// Exact byte size at observation time.
    pub size_bytes: u64,
    /// Last-modified timestamp in nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
}

/// Cached hashing result for a specific file snapshot identified by
/// [`CacheKey`].
#[derive(Clone, Debug)]
pub struct CacheEntry {
    /// Cheap 64 KiB BLAKE3 fingerprint. Always present — rows are only
    /// inserted after a successful quick-hash.
    pub quick_hash: BlakeHash,
    /// Full BLAKE3-256 hash, populated asynchronously by the backfill
    /// worker (Task 8). `None` until the worker processes the file.
    pub full_hash: Option<BlakeHash>,
}

/// Persistence boundary for `file_identity_cache`.
///
/// All methods are sync `&self` — adapters use interior mutability
/// (writer-actor channel + read pool) so the trait is `Send + Sync`.
pub trait IdentityCacheRepository: Send + Sync {
    /// Retrieve the cached entry for `key`, filtering `deleted_at IS NULL`.
    ///
    /// Returns `Ok(None)` when no live cache row matches the lookup tuple
    /// (cache miss — caller should hash the file and call `upsert`).
    ///
    /// # Errors
    /// Returns `CoreError::Internal` on adapter-level errors.
    fn lookup(&self, key: &CacheKey) -> Result<Option<CacheEntry>, CoreError>;

    /// Insert or update the cache entry for `key`.
    ///
    /// WHY writer-side select-then-insert: the lookup index
    /// `idx_fic_lookup` is **non-unique** (CLAUDE.md "Schema rules" — no
    /// UNIQUE on mutable columns) so `INSERT … ON CONFLICT DO UPDATE`
    /// cannot target it. The writer handler performs a single-transaction
    /// SELECT-then-INSERT-or-UPDATE, giving one writer round-trip without
    /// exposing a second writable connection.
    ///
    /// # Errors
    /// Returns `CoreError::Internal` on adapter-level errors.
    fn upsert(&self, key: &CacheKey, entry: &CacheEntry) -> Result<(), CoreError>;

    /// Soft-delete the live cache row matching `key`.
    ///
    /// Sets `deleted_at = NOW` on the row where the full lookup tuple
    /// matches AND `deleted_at IS NULL`. If no live row exists the
    /// operation is a no-op (idempotent — already deleted or never existed).
    ///
    /// # Errors
    /// Returns `CoreError::Internal` on adapter-level errors.
    fn soft_delete(&self, key: &CacheKey) -> Result<(), CoreError>;
}
