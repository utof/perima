//! `FileRepository` adapter — writer-actor + read-pool backed.
//!
//! Post-Batch-C Task 5. The struct holds two cheap-to-clone handles:
//! a [`flume::Sender<WriteCmd>`] connected to the single writer actor
//! (spec §3.1) and a [`ReadPool`] of read-only `r2d2_sqlite`
//! connections (spec §3.4). Writes build a [`FileWriteCmd`] variant with
//! a `flume::bounded(1)` reply channel and block on the reply. Reads
//! run SQL directly against a pooled connection.
//!
//! No `Mutex<Connection>`. The legacy `::new(conn)` constructor is
//! deprecated and will be removed in Batch C Task 7 once all callers
//! are updated to supply `(writer_sender, read_pool)`.

use std::sync::Mutex;

use flume::Sender;
use perima_core::{
    BlakeHash, CoreError, DeviceId, FileLocationRecord, FileRepository, FileSize, HashedFile,
    LocationStatus, MediaPath, UpsertOutcome, VolumeId,
};
use rusqlite::Connection;
// WHY: OptionalExtension adds `.optional()` to query_row results, converting
// QueryReturnedNoRows into Ok(None) for our two-statement SELECT-then-upsert
// pattern (read path).
use rusqlite::OptionalExtension;

use crate::cmd::{FileWriteCmd, WriteCmd};
use crate::errors::Error;
use crate::pool::ReadPool;

/// Writer-actor + read-pool backed file + location repository.
///
/// Cheap to [`Clone`]: both fields (`flume::Sender`, `ReadPool`) are
/// internally refcounted.
///
/// The deprecated `Mutex<Connection>`-based shape still compiles for
/// Task-7 callsites; it will be removed once those are updated.
#[derive(Clone)]
pub struct SqliteFileRepository {
    inner: Inner,
}

/// Internal state: either the new writer+pool shape (post-Task-5) or
/// the legacy `Mutex<Connection>` shape (pre-Task-7 callsites).
///
/// WHY enum: tasks 5 and 7 are separate commits; this bridge keeps
/// existing callers compiling while the migration lands incrementally.
/// Task 7 deletes the `Legacy` arm and the enum itself, leaving only
/// a plain `writer + reads` pair on the struct.
#[derive(Clone)]
enum Inner {
    /// Post-Batch-C Task 5 shape.
    WriterPool {
        writer: Sender<WriteCmd>,
        reads: ReadPool,
    },
    /// Pre-Batch-C Task 5 legacy shape; deprecated — Task 7 removes.
    #[deprecated(note = "Use SqliteFileRepository::new(writer, reads) (Task 7 cleanup)")]
    Legacy(std::sync::Arc<Mutex<Connection>>),
}

impl std::fmt::Debug for SqliteFileRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            Inner::WriterPool { .. } => f
                .debug_struct("SqliteFileRepository")
                .field("shape", &"writer+pool")
                .finish_non_exhaustive(),
            #[allow(deprecated)]
            Inner::Legacy(_) => f
                .debug_struct("SqliteFileRepository")
                .field("shape", &"legacy(Mutex<Connection>)")
                .finish_non_exhaustive(),
        }
    }
}

impl SqliteFileRepository {
    /// Construct an adapter from a writer-command sender + a read pool.
    ///
    /// WHY no migration run here: migrations happen exactly once inside
    /// [`crate::SqliteWriter::start`] BEFORE the writer thread spawns
    /// (spec §3.6). The read pool is opened after migrations complete.
    #[must_use]
    pub const fn new(writer: Sender<WriteCmd>, reads: ReadPool) -> Self {
        Self {
            inner: Inner::WriterPool { writer, reads },
        }
    }

    /// Wrap an existing connection. **Deprecated** — use
    /// `SqliteFileRepository::new(writer, reads)` instead.
    ///
    /// WHY kept: Batch C Task 7 migrates all callsites to the
    /// `new(writer, reads)` constructor. Until that commit lands the
    /// legacy callers (CLI, desktop, test helpers outside this module)
    /// still compile via this constructor. Task 7 deletes it.
    ///
    /// The caller must have run migrations before constructing this.
    #[must_use]
    #[deprecated(note = "Use SqliteFileRepository::new(writer, reads) (Task 7 cleanup)")]
    pub fn new_legacy(conn: Connection) -> Self {
        #[allow(deprecated)]
        Self {
            inner: Inner::Legacy(std::sync::Arc::new(Mutex::new(conn))),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers (read path)
// ---------------------------------------------------------------------------

/// Convert the `i64` stored in `SQLite` back to `FileSize`.
///
/// WHY: values we wrote were originally `u64` that fit in `i64`, so
/// `as u64` here is always exact. A negative value in the DB indicates
/// data corruption; we propagate as `Internal`.
fn i64_to_size(v: i64) -> Result<FileSize, CoreError> {
    u64::try_from(v)
        .map(FileSize)
        .map_err(|_| CoreError::Internal(format!("stored file_size {v} is negative")))
}

/// Convert a `usize` limit to `i64` for `LIMIT ?`.
///
/// WHY: `LIMIT` in `SQLite` accepts a signed 64-bit integer. A `usize` larger
/// than `i64::MAX` is capped to `i64::MAX` (effectively unlimited), which is
/// the safest behaviour for a limit argument.
fn limit_to_i64(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

/// Convert a `FileSize` (`u64`) to the `i64` that `SQLite` stores.
///
/// WHY: `SQLite` integers are signed 64-bit. A file larger than `i64::MAX`
/// (~8 EiB) cannot exist on current hardware; we propagate as `Internal`
/// rather than silently wrapping.
fn size_to_i64(size: FileSize) -> Result<i64, CoreError> {
    i64::try_from(size.0)
        .map_err(|_| CoreError::Internal(format!("file size {} overflows i64", size.0)))
}

/// Convert a `LocationStatus` to its DB string representation.
///
/// WHY: status values are stored as lowercase strings so they are human-readable
/// in `SQLite` tooling and stable across future Rust refactors.
const fn status_to_str(status: LocationStatus) -> &'static str {
    match status {
        LocationStatus::Active => "active",
        LocationStatus::Missing => "missing",
        LocationStatus::Moved => "moved",
        LocationStatus::Stale => "stale",
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// Inherent methods (writer-actor shim variants)
// ---------------------------------------------------------------------------

impl SqliteFileRepository {
    /// Migrate a sentinel row from phase 1b to the real `volume`.
    ///
    /// WHY: scan in phase 1b wrote every `file_locations` row with
    /// `volume_id = '00000000-0000-0000-0000-000000000000'` (the nil UUID).
    /// Phase 1c resolves the real volume for each scan root. Rather than
    /// a bulk UPDATE (which could race across concurrent scans), we update
    /// one row at a time — scoped by `(relative_path, sentinel volume_id,
    /// deleted_at IS NULL)` — immediately after the live upsert confirms
    /// the path still exists on disk.
    ///
    /// Returns the number of rows updated (0 if no sentinel row existed).
    ///
    /// # Errors
    /// `CoreError::Internal` on DB failure.
    pub fn migrate_sentinel_row(
        &self,
        path: &MediaPath,
        real_volume: VolumeId,
        device: DeviceId,
    ) -> Result<u64, CoreError> {
        match &self.inner {
            Inner::WriterPool { writer, .. } => {
                let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
                writer
                    .send(WriteCmd::File(FileWriteCmd::MigrateSentinelRow {
                        path: path.clone(),
                        real_volume,
                        device,
                        reply: reply_tx,
                    }))
                    .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
                reply_rx
                    .recv()
                    .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
            }
            #[allow(deprecated)]
            Inner::Legacy(conn) => {
                let conn = conn
                    .lock()
                    .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;
                let now = now_iso();
                let vol_str = real_volume.0.to_string();
                let dev_str = device.0.to_string();
                let path_str = path.as_str();
                let n = conn
                    .execute(
                        "UPDATE file_locations
                         SET volume_id = ?1, updated_at = ?2, device_id = ?3
                         WHERE volume_id = '00000000-0000-0000-0000-000000000000'
                           AND relative_path = ?4 AND deleted_at IS NULL",
                        rusqlite::params![vol_str, now, dev_str, path_str],
                    )
                    .map_err(Error::from)?;
                drop(conn);
                #[allow(clippy::cast_possible_truncation)]
                Ok(n as u64)
            }
        }
    }

    /// Update the status of a non-deleted file location identified by
    /// `(volume, path)`.
    ///
    /// Returns the number of rows updated (0 if no matching row exists,
    /// 1 on success).
    ///
    /// # Errors
    /// `CoreError::Internal` on DB failure.
    pub fn update_location_status(
        &self,
        volume: VolumeId,
        path: &MediaPath,
        status: LocationStatus,
        device: DeviceId,
    ) -> Result<u64, CoreError> {
        match &self.inner {
            Inner::WriterPool { writer, .. } => {
                let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
                writer
                    .send(WriteCmd::File(FileWriteCmd::UpdateLocationStatus {
                        volume,
                        path: path.clone(),
                        status,
                        device,
                        reply: reply_tx,
                    }))
                    .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
                reply_rx
                    .recv()
                    .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
            }
            #[allow(deprecated)]
            Inner::Legacy(conn) => {
                let conn = conn
                    .lock()
                    .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;
                let vol_str = volume.0.to_string();
                let path_str = path.as_str();
                let status_str = status_to_str(status);
                let dev_str = device.0.to_string();
                let now = now_iso();
                let n = conn
                    .execute(
                        "UPDATE file_locations
                         SET status = ?1, updated_at = ?2, device_id = ?3
                         WHERE volume_id = ?4 AND relative_path = ?5 AND deleted_at IS NULL",
                        rusqlite::params![status_str, now, dev_str, vol_str, path_str],
                    )
                    .map_err(Error::from)?;
                drop(conn);
                #[allow(clippy::cast_possible_truncation)]
                Ok(n as u64)
            }
        }
    }

    /// Update the relative path of a non-deleted file location and reset its
    /// status to `active`.
    ///
    /// Used when the watcher detects a rename/move within the same volume.
    /// If an active row already exists at `new_path`, the source row is
    /// soft-deleted and the destination row is left untouched — the
    /// filesystem reports a file at `new_path`, the DB agrees, and the
    /// source identity is retired (LWW semantics; formal CRDT resolution
    /// lands in phase 8+).
    ///
    /// Returns the number of rows written (0 if no source row exists, or
    /// 1 if either the source was updated OR soft-deleted on collision).
    ///
    /// # Errors
    /// `CoreError::Internal` on DB failure.
    pub fn update_location_path(
        &self,
        volume: VolumeId,
        old_path: &MediaPath,
        new_path: &MediaPath,
        device: DeviceId,
    ) -> Result<u64, CoreError> {
        match &self.inner {
            Inner::WriterPool { writer, .. } => {
                let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
                writer
                    .send(WriteCmd::File(FileWriteCmd::UpdateLocationPath {
                        volume,
                        old_path: old_path.clone(),
                        new_path: new_path.clone(),
                        device,
                        reply: reply_tx,
                    }))
                    .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
                reply_rx
                    .recv()
                    .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
            }
            #[allow(deprecated)]
            Inner::Legacy(conn) => {
                let mut conn = conn
                    .lock()
                    .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;
                let vol_str = volume.0.to_string();
                let old_str = old_path.as_str();
                let new_str = new_path.as_str();
                let dev_str = device.0.to_string();
                let now = now_iso();
                let tx = conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(Error::from)?;
                let collision: Option<String> = tx
                    .query_row(
                        "SELECT id FROM file_locations
                         WHERE volume_id = ?1 AND relative_path = ?2 AND deleted_at IS NULL",
                        rusqlite::params![vol_str, new_str],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(Error::from)?;
                let n = if collision.is_some() {
                    tx.execute(
                        "UPDATE file_locations
                             SET deleted_at = ?1, updated_at = ?1, device_id = ?2
                             WHERE volume_id = ?3 AND relative_path = ?4 AND deleted_at IS NULL",
                        rusqlite::params![now, dev_str, vol_str, old_str],
                    )
                    .map_err(Error::from)?
                } else {
                    tx.execute(
                        "UPDATE file_locations
                         SET relative_path = ?1, status = 'active', updated_at = ?2, device_id = ?3
                         WHERE volume_id = ?4 AND relative_path = ?5 AND deleted_at IS NULL",
                        rusqlite::params![new_str, now, dev_str, vol_str, old_str],
                    )
                    .map_err(Error::from)?
                };
                tx.commit().map_err(Error::from)?;
                drop(conn);
                #[allow(clippy::cast_possible_truncation)]
                Ok(n as u64)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// FileRepository trait impl
// ---------------------------------------------------------------------------

impl FileRepository for SqliteFileRepository {
    fn upsert_file(&self, file: &HashedFile, device: DeviceId) -> Result<UpsertOutcome, CoreError> {
        match &self.inner {
            Inner::WriterPool { writer, .. } => {
                let (reply_tx, reply_rx) = flume::bounded::<Result<UpsertOutcome, CoreError>>(1);
                // WHY clone `file`: the command crosses a thread boundary via
                // `flume::Sender::send`, which requires `'static`. `HashedFile`
                // is `Clone` (shallow: hash + path + size).
                writer
                    .send(WriteCmd::File(FileWriteCmd::UpsertFile {
                        file: file.clone(),
                        device,
                        reply: reply_tx,
                    }))
                    .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
                reply_rx
                    .recv()
                    .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
            }
            #[allow(deprecated)]
            Inner::Legacy(conn) => {
                // WHY: PoisonError can only occur if a thread panicked while holding
                // the lock. In that case the DB state is unknown; propagate as Internal.
                let conn = conn
                    .lock()
                    .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;
                let hash_hex = file.hash.to_hex();
                let now = now_iso();
                let dev_str = device.0.to_string();
                let size_i64 = size_to_i64(file.discovered.size)?;
                let existing: Option<(i64, String)> = conn
                    .query_row(
                        "SELECT file_size, device_id FROM files WHERE blake3_hash = ?1",
                        [&hash_hex],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(Error::from)?;
                let outcome = match existing {
                    None => {
                        conn.execute(
                            "INSERT INTO files
                             (blake3_hash, file_size, first_seen, updated_at, device_id)
                             VALUES (?1, ?2, ?3, ?3, ?4)",
                            rusqlite::params![hash_hex, size_i64, now, dev_str],
                        )
                        .map_err(Error::from)?;
                        UpsertOutcome::Inserted
                    }
                    Some((existing_size, ref existing_dev))
                        if existing_size == size_i64 && *existing_dev == dev_str =>
                    {
                        UpsertOutcome::Unchanged
                    }
                    Some(_) => {
                        conn.execute(
                            "UPDATE files SET file_size = ?1, updated_at = ?2, device_id = ?3
                             WHERE blake3_hash = ?4",
                            rusqlite::params![size_i64, now, dev_str, hash_hex],
                        )
                        .map_err(Error::from)?;
                        UpsertOutcome::Updated
                    }
                };
                drop(conn);
                Ok(outcome)
            }
        }
    }

    fn upsert_location(
        &self,
        hash: &BlakeHash,
        volume: VolumeId,
        path: &MediaPath,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError> {
        match &self.inner {
            Inner::WriterPool { writer, .. } => {
                let (reply_tx, reply_rx) = flume::bounded::<Result<UpsertOutcome, CoreError>>(1);
                writer
                    .send(WriteCmd::File(FileWriteCmd::UpsertLocation {
                        hash: *hash,
                        volume,
                        path: path.clone(),
                        device,
                        reply: reply_tx,
                    }))
                    .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
                reply_rx
                    .recv()
                    .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
            }
            #[allow(deprecated)]
            Inner::Legacy(conn) => {
                let mut conn = conn
                    .lock()
                    .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;
                let hash_hex = hash.to_hex();
                let vol_str = volume.0.to_string();
                let path_str = path.as_str();
                let dev_str = device.0.to_string();
                let now = now_iso();
                let tx = conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(Error::from)?;
                let existing: Option<(String, String, String)> = tx
                    .query_row(
                        "SELECT id, blake3_hash, device_id FROM file_locations
                         WHERE volume_id = ?1 AND relative_path = ?2 AND deleted_at IS NULL",
                        rusqlite::params![vol_str, path_str],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(Error::from)?;
                let outcome = match existing {
                    None => {
                        let id = perima_core::ids::new_id().to_string();
                        tx.execute(
                            "INSERT INTO file_locations
                             (id, blake3_hash, volume_id, relative_path, status,
                              first_seen, updated_at, device_id)
                             VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, ?6)",
                            rusqlite::params![id, hash_hex, vol_str, path_str, now, dev_str],
                        )
                        .map_err(Error::from)?;
                        UpsertOutcome::Inserted
                    }
                    Some((_, ref existing_hash, ref existing_dev))
                        if *existing_hash == hash_hex && *existing_dev == dev_str =>
                    {
                        UpsertOutcome::Unchanged
                    }
                    Some((ref row_id, _, _)) => {
                        tx.execute(
                            "UPDATE file_locations
                             SET blake3_hash = ?1, updated_at = ?2, device_id = ?3
                             WHERE id = ?4",
                            rusqlite::params![hash_hex, now, dev_str, row_id],
                        )
                        .map_err(Error::from)?;
                        UpsertOutcome::Updated
                    }
                };
                tx.commit().map_err(Error::from)?;
                drop(conn);
                Ok(outcome)
            }
        }
    }

    fn list_file_locations(
        &self,
        limit: usize,
        volume: Option<VolumeId>,
    ) -> Result<Vec<FileLocationRecord>, CoreError> {
        // WHY pool-only (no writer hop): `list_file_locations` is a
        // pure SELECT. Reads go directly through the `r2d2_sqlite` pool
        // (spec §3.5). `PooledConnection` derefs to
        // `rusqlite::Connection`, so the SQL body is lifted verbatim
        // from the pre-Batch-C impl.
        match &self.inner {
            Inner::WriterPool { reads, .. } => {
                let conn = reads.get()?;
                list_file_locations_sql(&conn, limit, volume)
            }
            #[allow(deprecated)]
            Inner::Legacy(conn) => {
                // WHY allow(significant_drop_tightening): the Mutex guard
                // must outlive `stmt` and `rows` because they borrow
                // through the guard.
                #[allow(clippy::significant_drop_tightening)]
                let conn = conn
                    .lock()
                    .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;
                list_file_locations_sql(&conn, limit, volume)
            }
        }
    }
}

/// Shared SELECT body for `list_file_locations`.
///
/// WHY separate function: both the writer+pool path and the legacy
/// `Mutex<Connection>` path execute identical SQL; factoring it out
/// avoids duplication while keeping the `Inner` dispatch clean.
fn list_file_locations_sql(
    conn: &Connection,
    limit: usize,
    volume: Option<VolumeId>,
) -> Result<Vec<FileLocationRecord>, CoreError> {
    // WHY separate SQL strings per branch instead of `(?1 IS NULL OR fl.volume_id = ?1)`:
    // the OR-with-NULL predicate defeats index use on `idx_file_locations_volume_path`;
    // EXPLAIN QUERY PLAN reports SCAN + TEMP B-TREE sort even when a concrete
    // volume_id is supplied. Branching here keeps both shapes index-eligible.
    let vol_filter = volume.map(|v| v.0.to_string());
    let sql: &str = if vol_filter.is_some() {
        "SELECT f.blake3_hash, f.file_size, fl.volume_id, fl.relative_path,
                fl.status, fl.first_seen
         FROM file_locations fl
         JOIN files f ON f.blake3_hash = fl.blake3_hash
         WHERE fl.deleted_at IS NULL AND fl.volume_id = ?1
         ORDER BY fl.relative_path
         LIMIT ?2"
    } else {
        "SELECT f.blake3_hash, f.file_size, fl.volume_id, fl.relative_path,
                fl.status, fl.first_seen
         FROM file_locations fl
         JOIN files f ON f.blake3_hash = fl.blake3_hash
         WHERE fl.deleted_at IS NULL
         ORDER BY fl.relative_path
         LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql).map_err(Error::from)?;

    let limit_i64 = limit_to_i64(limit);
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(v) = vol_filter.as_deref() {
        params.push(Box::new(v.to_owned()));
    }
    params.push(Box::new(limit_i64));

    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            let hash_hex: String = row.get(0)?;
            let size: i64 = row.get(1)?;
            let vol_str: String = row.get(2)?;
            let rel_path: String = row.get(3)?;
            let status_str: String = row.get(4)?;
            let first_seen: String = row.get(5)?;
            Ok((hash_hex, size, vol_str, rel_path, status_str, first_seen))
        })
        .map_err(Error::from)?;

    let mut out = Vec::new();
    for row in rows {
        let (hash_hex, size, vol_str, rel_path, status_str, first_seen) =
            row.map_err(Error::from)?;
        let hash = BlakeHash::parse_hex(&hash_hex)?;
        let volume_id = VolumeId(
            uuid::Uuid::parse_str(&vol_str)
                .map_err(|e| CoreError::Internal(format!("bad volume uuid: {e}")))?,
        );
        let status = match status_str.as_str() {
            "active" => LocationStatus::Active,
            "missing" => LocationStatus::Missing,
            "moved" => LocationStatus::Moved,
            "stale" => LocationStatus::Stale,
            other => {
                return Err(CoreError::Internal(format!(
                    "unknown location status: {other}"
                )));
            }
        };
        out.push(FileLocationRecord {
            hash,
            size: i64_to_size(size)?,
            volume_id,
            relative_path: MediaPath::new(&rel_path),
            status,
            first_seen,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[allow(
    clippy::unwrap_used,
    reason = "tests: unwrap is the assertion — a panic is a failing test by design"
)]
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use perima_core::{EventBus, FileEvent};
    use tempfile::TempDir;

    use super::*;
    use crate::pool::ReadPool;
    use crate::writer::{SqliteWriter, SqliteWriterHandle};

    /// No-op event bus used by writer-backed test fixtures.
    struct NoopBus;
    impl EventBus for NoopBus {
        fn emit(&self, _: &FileEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }

    /// Test harness: tempdir-backed DB, writer actor, read pool, repo.
    ///
    /// WHY tempfile-on-disk (not in-memory): writer + pool must share
    /// the same DB file; `:memory:` is per-connection private.
    fn test_db() -> (TempDir, SqliteFileRepository, SqliteWriterHandle) {
        let td = tempfile::tempdir().expect("tempdir");
        let db_path = td.path().join("test.db");
        let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
        let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
        let reads = ReadPool::open(&db_path).expect("pool open");
        let repo = SqliteFileRepository::new(writer.sender(), reads);
        (td, repo, writer)
    }

    fn sample_hashed_file(content: &[u8], rel_path: &str) -> HashedFile {
        let hash = BlakeHash::from_bytes(*blake3::hash(content).as_bytes());
        HashedFile {
            discovered: perima_core::DiscoveredFile {
                absolute_path: PathBuf::from("/tmp/fake"),
                relative_path: MediaPath::new(rel_path),
                size: FileSize(content.len() as u64),
            },
            hash,
        }
    }

    fn device() -> DeviceId {
        DeviceId::new()
    }

    fn sentinel_volume() -> VolumeId {
        VolumeId(uuid::Uuid::nil())
    }

    #[test]
    fn upsert_file_inserts_new() {
        let (_td, repo, _writer) = test_db();
        let f = sample_hashed_file(b"hello", "a.txt");
        let out = repo.upsert_file(&f, device()).expect("upsert");
        assert_eq!(out, UpsertOutcome::Inserted);
    }

    #[test]
    fn upsert_file_unchanged_on_repeat() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let f = sample_hashed_file(b"hello", "a.txt");
        repo.upsert_file(&f, dev).expect("first");
        let out = repo.upsert_file(&f, dev).expect("second");
        assert_eq!(out, UpsertOutcome::Unchanged);
    }

    #[test]
    fn upsert_file_updated_on_size_change() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let f1 = sample_hashed_file(b"hello", "a.txt");
        repo.upsert_file(&f1, dev).expect("first");
        // Same hash, different size (contrived but tests the branch).
        let mut f2 = f1;
        f2.discovered.size = FileSize(999);
        let out = repo.upsert_file(&f2, dev).expect("second");
        assert_eq!(out, UpsertOutcome::Updated);
    }

    #[test]
    fn upsert_location_inserts_new() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let f = sample_hashed_file(b"hello", "a.txt");
        repo.upsert_file(&f, dev).expect("file");
        let out = repo
            .upsert_location(&f.hash, sentinel_volume(), &f.discovered.relative_path, dev)
            .expect("loc");
        assert_eq!(out, UpsertOutcome::Inserted);
    }

    #[test]
    fn upsert_location_unchanged_on_repeat() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let f = sample_hashed_file(b"hello", "a.txt");
        repo.upsert_file(&f, dev).expect("file");
        let vol = sentinel_volume();
        let path = &f.discovered.relative_path;
        repo.upsert_location(&f.hash, vol, path, dev)
            .expect("first");
        let out = repo
            .upsert_location(&f.hash, vol, path, dev)
            .expect("second");
        assert_eq!(out, UpsertOutcome::Unchanged);
    }

    #[test]
    fn upsert_location_updated_on_hash_change() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let f1 = sample_hashed_file(b"hello", "a.txt");
        let f2 = sample_hashed_file(b"world", "a.txt");
        repo.upsert_file(&f1, dev).expect("file1");
        repo.upsert_file(&f2, dev).expect("file2");
        let vol = sentinel_volume();
        let path = MediaPath::new("a.txt");
        repo.upsert_location(&f1.hash, vol, &path, dev)
            .expect("first");
        let out = repo
            .upsert_location(&f2.hash, vol, &path, dev)
            .expect("second");
        assert_eq!(out, UpsertOutcome::Updated);
    }

    #[test]
    fn list_file_locations_returns_all() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let vol = sentinel_volume();
        for (i, name) in ["a.txt", "b.txt", "c.txt"].iter().enumerate() {
            let f = sample_hashed_file(format!("content{i}").as_bytes(), name);
            repo.upsert_file(&f, dev).expect("file");
            repo.upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
                .expect("loc");
        }
        let results = repo.list_file_locations(100, None).expect("list");
        assert_eq!(results.len(), 3);
        // Ordered by relative_path.
        assert_eq!(results[0].relative_path.as_str(), "a.txt");
        assert_eq!(results[2].relative_path.as_str(), "c.txt");
    }

    #[test]
    fn list_file_locations_respects_limit() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let vol = sentinel_volume();
        for i in 0..5 {
            let f = sample_hashed_file(format!("c{i}").as_bytes(), &format!("f{i}.txt"));
            repo.upsert_file(&f, dev).expect("file");
            repo.upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
                .expect("loc");
        }
        let results = repo.list_file_locations(2, None).expect("list");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn list_file_locations_filters_by_volume() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let vol_a = VolumeId::new();
        let vol_b = VolumeId::new();
        let f1 = sample_hashed_file(b"alpha", "a.txt");
        let f2 = sample_hashed_file(b"beta", "b.txt");
        repo.upsert_file(&f1, dev).expect("f1");
        repo.upsert_file(&f2, dev).expect("f2");
        repo.upsert_location(&f1.hash, vol_a, &f1.discovered.relative_path, dev)
            .expect("loc1");
        repo.upsert_location(&f2.hash, vol_b, &f2.discovered.relative_path, dev)
            .expect("loc2");
        let a_only = repo.list_file_locations(100, Some(vol_a)).expect("list");
        assert_eq!(a_only.len(), 1);
        assert_eq!(a_only[0].relative_path.as_str(), "a.txt");
    }

    #[test]
    fn migrate_sentinel_row_updates_volume_id() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let sentinel = sentinel_volume();
        let real_vol = VolumeId::new();

        // Insert a file with the sentinel volume_id.
        let f = sample_hashed_file(b"sentinel_test", "photo.jpg");
        repo.upsert_file(&f, dev).expect("file");
        repo.upsert_location(&f.hash, sentinel, &f.discovered.relative_path, dev)
            .expect("location with sentinel");

        // Migrate the sentinel row to the real volume.
        let updated = repo
            .migrate_sentinel_row(&f.discovered.relative_path, real_vol, dev)
            .expect("migrate");
        assert_eq!(updated, 1, "exactly 1 sentinel row must be migrated");

        // Confirm the row now has the real volume_id.
        let rows = repo
            .list_file_locations(10, Some(real_vol))
            .expect("list by real vol");
        assert_eq!(rows.len(), 1, "row must be found under real volume");
        assert_eq!(rows[0].relative_path.as_str(), "photo.jpg");

        // Confirm it no longer appears under sentinel.
        let sentinel_rows = repo
            .list_file_locations(10, Some(sentinel))
            .expect("list by sentinel");
        assert_eq!(
            sentinel_rows.len(),
            0,
            "no rows under sentinel after migration"
        );
    }

    #[test]
    fn migrate_sentinel_row_skips_non_sentinel() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let real_vol = VolumeId::new();
        let other_vol = VolumeId::new();

        // Insert a file with a real (non-sentinel) volume_id.
        let f = sample_hashed_file(b"real_vol_test", "video.mp4");
        repo.upsert_file(&f, dev).expect("file");
        repo.upsert_location(&f.hash, real_vol, &f.discovered.relative_path, dev)
            .expect("location");

        // migrate_sentinel_row must not touch rows with a real volume_id.
        let updated = repo
            .migrate_sentinel_row(&f.discovered.relative_path, other_vol, dev)
            .expect("migrate");
        assert_eq!(updated, 0, "non-sentinel row must not be touched");
    }

    // --- update_location_status tests ---

    #[test]
    fn update_status_to_missing() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let vol = VolumeId::new();
        let f = sample_hashed_file(b"missing_test", "img.jpg");
        repo.upsert_file(&f, dev).expect("file");
        repo.upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
            .expect("location");

        let updated = repo
            .update_location_status(
                vol,
                &f.discovered.relative_path,
                LocationStatus::Missing,
                dev,
            )
            .expect("update status");
        assert_eq!(updated, 1, "exactly 1 row must be updated");

        // Confirm the status is now Missing.
        let rows = repo.list_file_locations(10, Some(vol)).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, LocationStatus::Missing);
    }

    #[test]
    fn update_status_to_stale() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let vol = VolumeId::new();
        let f = sample_hashed_file(b"stale_test", "doc.txt");
        repo.upsert_file(&f, dev).expect("file");
        repo.upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
            .expect("location");

        let updated = repo
            .update_location_status(vol, &f.discovered.relative_path, LocationStatus::Stale, dev)
            .expect("update status");
        assert_eq!(updated, 1, "exactly 1 row must be updated");

        // Confirm the status is now Stale.
        let rows = repo.list_file_locations(10, Some(vol)).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, LocationStatus::Stale);
    }

    // --- update_location_path tests ---

    #[test]
    fn update_location_path_renames() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let vol = VolumeId::new();
        let f = sample_hashed_file(b"rename_test", "old_name.jpg");
        repo.upsert_file(&f, dev).expect("file");
        repo.upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
            .expect("location");

        // First set status to Stale to verify rename resets it to Active.
        repo.update_location_status(vol, &f.discovered.relative_path, LocationStatus::Stale, dev)
            .expect("set stale");

        let old_path = MediaPath::new("old_name.jpg");
        let new_path = MediaPath::new("new_name.jpg");
        let updated = repo
            .update_location_path(vol, &old_path, &new_path, dev)
            .expect("rename");
        assert_eq!(updated, 1, "exactly 1 row must be renamed");

        // Confirm new path exists with Active status; old path is gone.
        let rows = repo.list_file_locations(10, Some(vol)).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].relative_path.as_str(), "new_name.jpg");
        assert_eq!(rows[0].status, LocationStatus::Active);
    }

    #[test]
    fn update_location_path_collision_softdeletes_source() {
        // WHY: if an active row already exists at `new_path`, renaming
        // `old_path` → `new_path` cannot just UPDATE without introducing
        // two active rows for the same (volume, path). The fix soft-deletes
        // the source row; the destination wins (defensible LWW — the
        // filesystem already has a file at new_path). Observable:
        // list_file_locations shows exactly the destination row, with the
        // destination's original hash untouched.
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let vol = VolumeId::new();

        // Seed destination row with a distinct hash.
        let f_dest = sample_hashed_file(b"destination_content", "dest.jpg");
        repo.upsert_file(&f_dest, dev).expect("dest file");
        repo.upsert_location(&f_dest.hash, vol, &f_dest.discovered.relative_path, dev)
            .expect("dest location");

        // Seed source row at a different path with its own hash.
        let f_src = sample_hashed_file(b"source_content", "src.jpg");
        repo.upsert_file(&f_src, dev).expect("src file");
        repo.upsert_location(&f_src.hash, vol, &f_src.discovered.relative_path, dev)
            .expect("src location");

        // Attempt the colliding rename: src.jpg → dest.jpg.
        let old_path = MediaPath::new("src.jpg");
        let new_path = MediaPath::new("dest.jpg");
        let touched = repo
            .update_location_path(vol, &old_path, &new_path, dev)
            .expect("rename with collision");
        assert_eq!(
            touched, 1,
            "source row must be soft-deleted (counts as 1 update)"
        );

        // Only the destination survives as an active row, and it still
        // points at its original hash (destination is authoritative).
        let rows = repo.list_file_locations(10, Some(vol)).expect("list");
        assert_eq!(rows.len(), 1, "exactly one active row after collision");
        assert_eq!(rows[0].relative_path.as_str(), "dest.jpg");
        assert_eq!(
            rows[0].hash.to_hex(),
            f_dest.hash.to_hex(),
            "destination hash must be preserved",
        );
    }

    #[test]
    fn update_location_path_normal_case() {
        // WHY: regression pin for the non-colliding rename path after the
        // 1b edit. A plain rename (no active row at new_path) must update
        // the row in place and keep exactly one active row with the new
        // path and active status.
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let vol = VolumeId::new();
        let f = sample_hashed_file(b"normal_rename", "a.jpg");
        repo.upsert_file(&f, dev).expect("file");
        repo.upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
            .expect("location");

        let old_path = MediaPath::new("a.jpg");
        let new_path = MediaPath::new("b.jpg");
        let touched = repo
            .update_location_path(vol, &old_path, &new_path, dev)
            .expect("rename");
        assert_eq!(touched, 1);

        let rows = repo.list_file_locations(10, Some(vol)).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].relative_path.as_str(), "b.jpg");
        assert_eq!(rows[0].status, LocationStatus::Active);
    }

    #[test]
    fn upsert_location_concurrent_unique() {
        // WHY: two concurrent repo handles (cloned) upserting the same
        // (hash, volume, path) tuple must produce exactly ONE active row.
        // Under the writer actor this is guaranteed by single-threaded
        // serialization — the test still covers the observable behaviour
        // contract (both return Ok; exactly one row in DB).
        use std::sync::{Arc as ArcStd, Barrier};
        use std::thread;

        let (_td, repo, _writer) = test_db();
        let repo = ArcStd::new(repo);
        let dev = device();
        let vol = VolumeId::new();

        // Seed the files row so both threads can link a location to it.
        let f = sample_hashed_file(b"shared", "race.jpg");
        repo.upsert_file(&f, dev).expect("seed file");

        let barrier = ArcStd::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let repo = ArcStd::clone(&repo);
            let barrier = ArcStd::clone(&barrier);
            let hash = f.hash;
            let path = f.discovered.relative_path.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                repo.upsert_location(&hash, vol, &path, dev)
                    .expect("upsert_location")
            }));
        }
        let results: Vec<UpsertOutcome> = handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect();

        // Writer serializes: first caller Inserted, second caller sees
        // the same (hash, device) row and returns Unchanged. If the
        // second ever returned Updated that'd mean the app-level
        // uniqueness guard skipped a check — regression we want to catch.
        assert!(
            results.contains(&UpsertOutcome::Inserted),
            "at least one Inserted"
        );
        assert!(
            results.contains(&UpsertOutcome::Unchanged),
            "at least one Unchanged (second caller must dedup)"
        );
        // Cross-check via list: exactly one active row.
        let rows = repo.list_file_locations(10, Some(vol)).expect("list");
        assert_eq!(rows.len(), 1, "exactly one active file_locations row");
        assert_eq!(rows[0].relative_path.as_str(), "race.jpg");
    }

    #[test]
    fn update_location_path_nonexistent() {
        let (_td, repo, _writer) = test_db();
        let dev = device();
        let vol = VolumeId::new();

        // No rows in DB — update must return 0 rows affected.
        let old_path = MediaPath::new("ghost.jpg");
        let new_path = MediaPath::new("phantom.jpg");
        let updated = repo
            .update_location_path(vol, &old_path, &new_path, dev)
            .expect("rename on empty DB");
        assert_eq!(
            updated, 0,
            "no rows must be affected for a nonexistent path"
        );
    }
}
