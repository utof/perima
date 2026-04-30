//! `IdentityCacheRepository` adapter — writer-actor + read-pool backed.
//!
//! `file_identity_cache` is device-local (never synced). It caches per-device
//! filesystem metadata (inode, mtime, size) alongside `quick_hash` so the
//! scan loop can skip rehashing unchanged files.
//!
//! Post-Batch-C pattern (spec §3.1 + §3.4): the struct holds two cheap-to-clone
//! handles — a [`flume::Sender<WriteCmd>`] for the single writer actor and a
//! [`ReadPool`] of read-only connections.
//!
//! Write paths send a [`crate::cmd::CacheWriteCmd`] variant with a
//! `flume::bounded(1)` reply channel and block on the reply (sync `&self`,
//! runtime-agnostic). Read path runs SQL directly against the [`ReadPool`].
//!
//! WHY sync `&self` + `flume::Receiver::recv` (not `blocking_recv`): `flume`
//! `recv` is runtime-agnostic. `tokio::sync::oneshot::Receiver::blocking_recv`
//! panics inside a tokio runtime context. Single `flume` dep covers both the
//! command channel and reply channels (Batch C rationale).

use flume::Sender;
use perima_core::{BlakeHash, CacheEntry, CacheKey, CoreError, IdentityCacheRepository};
use rusqlite::OptionalExtension;

use crate::cmd::{CacheWriteCmd, WriteCmd};
use crate::errors::Error;
use crate::pool::ReadPool;

/// Writer-actor + read-pool backed identity-cache repository.
///
/// Cheap to [`Clone`]: both fields (`flume::Sender`, `ReadPool`) are
/// internally refcounted.
#[derive(Clone)]
pub struct SqliteIdentityCacheRepository {
    writer: Sender<WriteCmd>,
    reads: ReadPool,
}

impl std::fmt::Debug for SqliteIdentityCacheRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteIdentityCacheRepository")
            .finish_non_exhaustive()
    }
}

impl SqliteIdentityCacheRepository {
    /// Construct an adapter from a writer-command sender + a read pool.
    ///
    /// WHY no migration run here: migrations run once inside
    /// [`crate::SqliteWriter::start`] BEFORE the writer thread spawns
    /// (spec §3.6). The read pool opens after migrations complete.
    #[must_use]
    pub const fn new(writer: Sender<WriteCmd>, reads: ReadPool) -> Self {
        Self { writer, reads }
    }
}

// ---------------------------------------------------------------------------
// IdentityCacheRepository impl
// ---------------------------------------------------------------------------

impl IdentityCacheRepository for SqliteIdentityCacheRepository {
    fn lookup(&self, key: &CacheKey) -> Result<Option<CacheEntry>, CoreError> {
        let conn = self.reads.get()?;
        let dev_str = key.device_id.0.to_string();
        let vol_str = key.volume_id.0.to_string();
        let size_bytes = i64::try_from(key.size_bytes).map_err(|_| {
            CoreError::Internal(format!("size_bytes {} overflows i64", key.size_bytes))
        })?;

        let row: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT quick_hash, full_hash
                 FROM file_identity_cache
                 WHERE device_id = ?1
                   AND volume_id = ?2
                   AND fs_file_id = ?3
                   AND size_bytes = ?4
                   AND mtime_ns = ?5
                   AND deleted_at IS NULL
                 LIMIT 1",
                rusqlite::params![dev_str, vol_str, key.fs_file_id, size_bytes, key.mtime_ns],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Error::from)?;

        match row {
            None => Ok(None),
            Some((qh_hex, fh_hex)) => {
                let quick_hash = BlakeHash::parse_hex(&qh_hex)?;
                let full_hash = fh_hex.as_deref().map(BlakeHash::parse_hex).transpose()?;
                Ok(Some(CacheEntry {
                    quick_hash,
                    full_hash,
                }))
            }
        }
    }

    fn upsert(&self, key: &CacheKey, entry: &CacheEntry) -> Result<(), CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<(), CoreError>>(1);
        self.writer
            .send(WriteCmd::Cache(CacheWriteCmd::UpsertCacheRow {
                key: key.clone(),
                entry: entry.clone(),
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer channel send: {e}")))?;
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer reply recv: {e}")))?
    }

    fn soft_delete(&self, key: &CacheKey) -> Result<(), CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<(), CoreError>>(1);
        self.writer
            .send(WriteCmd::Cache(CacheWriteCmd::SoftDeleteCacheRow {
                key: key.clone(),
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer channel send: {e}")))?;
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer reply recv: {e}")))?
    }
}
