//! `FileRepository` adapter — writer-actor + read-pool backed.
//!
//! Post-Batch-C Task 7. The struct holds two cheap-to-clone handles:
//! a [`flume::Sender<WriteCmd>`] connected to the single writer actor
//! (spec §3.1) and a [`ReadPool`] of read-only `r2d2_sqlite`
//! connections (spec §3.4). Writes build a [`FileWriteCmd`] variant with
//! a `flume::bounded(1)` reply channel and block on the reply. Reads
//! run SQL directly against a pooled connection.
//!
//! No `Mutex<Connection>`. Every caller now supplies
//! `(writer_sender, read_pool)` via `SqliteFileRepository::new`.

use flume::Sender;
use perima_core::{
    BackfillFileRow, BlakeHash, CollisionGroup, CoreError, DeviceId, FileLocationRecord,
    FileRepository, FileSize, FileUuid, HashedFile, LocationStatus, LocationStatusUpdate,
    LocationToVerify, MediaPath, UpsertOutcome, VerifiedState, VerifyCandidates, VolumeId,
};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

use crate::cmd::{FileWriteCmd, WriteCmd};
use crate::errors::Error;
use crate::pool::ReadPool;

/// Writer-actor + read-pool backed file + location repository.
///
/// Cheap to [`Clone`]: both fields (`flume::Sender`, `ReadPool`) are
/// internally refcounted.
#[derive(Clone)]
pub struct SqliteFileRepository {
    writer: Sender<WriteCmd>,
    reads: ReadPool,
}

impl std::fmt::Debug for SqliteFileRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteFileRepository")
            .finish_non_exhaustive()
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
        Self { writer, reads }
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

/// Parse the `files.file_uuid` text column into a [`FileUuid`].
pub(crate) fn parse_file_uuid(s: &str) -> Result<FileUuid, CoreError> {
    Uuid::parse_str(s)
        .map(FileUuid)
        .map_err(|e| CoreError::Internal(format!("bad file_uuid: {e}")))
}

/// Parse the optional `files.blake3_hash` text column.
///
/// WHY optional: post-Task-11 the schema permits a `NULL` `blake3_hash` for
/// rows whose `full_hash` has not yet been computed (pending dedup).
pub(crate) fn parse_optional_hash(s: Option<&str>) -> Result<Option<BlakeHash>, CoreError> {
    match s {
        Some(hex) => Ok(Some(BlakeHash::parse_hex(hex)?)),
        None => Ok(None),
    }
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
        let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
        self.writer
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
        let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
        self.writer
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
        let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
        self.writer
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
}

// ---------------------------------------------------------------------------
// FileRepository trait impl
// ---------------------------------------------------------------------------

impl FileRepository for SqliteFileRepository {
    fn upsert_file(&self, file: &HashedFile, device: DeviceId) -> Result<UpsertOutcome, CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<UpsertOutcome, CoreError>>(1);
        // WHY clone `file`: the command crosses a thread boundary via
        // `flume::Sender::send`, which requires `'static`. `HashedFile`
        // is `Clone` (shallow: hash + path + size).
        self.writer
            .send(WriteCmd::File(FileWriteCmd::UpsertFile {
                file: file.clone(),
                device,
                quick_hash: None,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
    }

    fn upsert_file_with_quick_hash(
        &self,
        file: &HashedFile,
        device: DeviceId,
        quick_hash: Option<BlakeHash>,
    ) -> Result<UpsertOutcome, CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<UpsertOutcome, CoreError>>(1);
        // WHY pass quick_hash through: scan-path callers supply the
        // cheap prefix+suffix fingerprint computed during resolve_with_cache
        // so the writer can populate files.quick_hash on INSERT per spec §4.1.1.
        // Non-scan callers (watcher, tag attach) use the base upsert_file
        // which sends None — the COALESCE in the UPDATE arm preserves any
        // previously-stored value.
        self.writer
            .send(WriteCmd::File(FileWriteCmd::UpsertFile {
                file: file.clone(),
                device,
                quick_hash,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
    }

    fn upsert_location(
        &self,
        hash: &BlakeHash,
        volume: VolumeId,
        path: &MediaPath,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<UpsertOutcome, CoreError>>(1);
        self.writer
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
        let conn = self.reads.get()?;
        list_file_locations_sql(&conn, limit, volume)
    }

    fn list_files_needing_backfill(&self, limit: u32) -> Result<Vec<BackfillFileRow>, CoreError> {
        // WHY pool-only: this is a pure SELECT — no write needed.
        // WHY LEFT JOIN volume_mounts: we want to return the absolute
        // path for the backfill worker so it can read the bytes without
        // opening a second DB connection. `volume_mounts` stores the
        // mount_path on the local machine. Rows without an active mount
        // will have NULL mount_path → active_path = None, which the
        // worker treats as "skip (no active location)". Spec §4.1.5.
        let conn = self.reads.get()?;
        list_files_needing_backfill_sql(&conn, limit)
    }

    fn lookup_by_file_uuid(
        &self,
        file_uuid: FileUuid,
    ) -> Result<Option<(Option<BlakeHash>, std::path::PathBuf, u64)>, CoreError> {
        // WHY pool-only: pure SELECT joining `files` → `file_locations` →
        // `volume_mounts` to assemble an absolute path the caller can read.
        // Same shape as `list_files_needing_backfill_sql` but keyed on
        // `file_uuid` instead of NULL-ness of `quick_hash`.
        let conn = self.reads.get()?;
        lookup_by_file_uuid_sql(&conn, file_uuid)
    }

    fn update_full_hash(&self, file_uuid: FileUuid, hash: BlakeHash) -> Result<(), CoreError> {
        // WHY device sourced from a sentinel here: the writer needs SOME
        // device for CRDT bookkeeping. This adapter does not carry a
        // device handle (it's per-process, not per-machine in the API);
        // the use case supplies the right device through `mark_verified_distinct`,
        // but `update_full_hash` is a single-row promotion that fires from
        // the backfill / on-demand path which already binds the device at
        // hash-compute time. We thread a fresh `DeviceId::new()` here for
        // now — Task 11 (file_uuid migration sweep) will widen the trait
        // signature to take `device: DeviceId`. The placeholder is safe
        // because `device_id` on `files` is overwritten on every UPDATE.
        let device = DeviceId::new();
        let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
        self.writer
            .send(WriteCmd::File(FileWriteCmd::PromoteFullHash {
                file_uuid,
                full_hash: hash,
                device,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        let rows = reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))??;
        if rows == 0 {
            // No row matched — caller asked us to promote a file_uuid that
            // does not exist (or was soft-deleted). Surface this as NotFound
            // so the use case can map to `CoreError::FullHashUnavailable`.
            return Err(CoreError::NotFound(format!(
                "no files row for file_uuid={}",
                file_uuid.0
            )));
        }
        Ok(())
    }

    fn list_quick_hash_collisions(&self) -> Result<Vec<CollisionGroup>, CoreError> {
        // WHY pool-only: pure SELECT.
        let conn = self.reads.get()?;
        list_quick_hash_collisions_sql(&conn)
    }

    fn mark_verified_distinct(
        &self,
        file_uuids: Vec<FileUuid>,
        device: DeviceId,
    ) -> Result<(), CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
        self.writer
            .send(WriteCmd::File(FileWriteCmd::MarkVerifiedDistinct {
                file_uuids,
                device,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        let _rows = reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))??;
        Ok(())
    }

    fn list_locations_for_verify(&self, device: DeviceId) -> Result<VerifyCandidates, CoreError> {
        // WHY a pool checkout (no writer hop): pure SELECT, same as
        // `list_file_locations` and `VolumeRepository::list`.
        let conn = self.reads.get()?;
        list_locations_for_verify_sql(&conn, device)
    }

    fn update_location_statuses(
        &self,
        updates: &[LocationStatusUpdate],
        device: DeviceId,
    ) -> Result<u64, CoreError> {
        // WHY short-circuit before the writer hop: an unchanged sweep is
        // the steady state, and a no-op round-trip through the actor
        // would still serialise behind every queued write.
        if updates.is_empty() {
            return Ok(0);
        }
        let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
        self.writer
            .send(WriteCmd::File(FileWriteCmd::UpdateLocationStatuses {
                updates: updates.to_vec(),
                device,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
    }

    fn count_missing_locations(&self) -> Result<u64, CoreError> {
        let conn = self.reads.get()?;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_locations
                 WHERE status = 'missing' AND deleted_at IS NULL",
                [],
                |r| r.get(0),
            )
            .map_err(Error::from)?;
        u64::try_from(n).map_err(|_| CoreError::Internal(format!("count {n} is negative")))
    }

    fn soft_delete_missing_locations(&self, device: DeviceId) -> Result<u64, CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
        self.writer
            .send(WriteCmd::File(FileWriteCmd::SoftDeleteMissingLocations {
                device,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
    }

    fn list_files_pending_full_hash(&self, limit: usize) -> Result<Vec<FileUuid>, CoreError> {
        // WHY pool-only: pure SELECT; no write needed.
        let conn = self.reads.get()?;
        list_files_pending_full_hash_sql(&conn, limit)
    }
}

/// SELECT body for [`SqliteFileRepository::list_files_needing_backfill`].
///
/// Joins `files` → `file_locations` → `volume_mounts` to build absolute
/// paths for the backfill worker. Only non-deleted file rows with
/// `quick_hash IS NULL` are returned. The LEFT JOIN on `volume_mounts`
/// means rows on unmounted volumes produce `mount_path = NULL`; those
/// rows surface with `active_path = None` so the worker can skip them.
///
/// WHY one-row-per-file (GROUP BY): `file_locations` may have multiple
/// active rows for the same hash (e.g. duplicates on the same volume).
/// We want exactly one `BackfillFileRow` per `files` row — GROUP BY
/// plus MIN aggregates ensure that. This avoids a lateral join (not
/// supported in older `SQLite`) while remaining indexable.
fn list_files_needing_backfill_sql(
    conn: &Connection,
    limit: u32,
) -> Result<Vec<BackfillFileRow>, perima_core::CoreError> {
    let sql = "
        SELECT f.blake3_hash, f.file_size,
               MIN(vm.mount_path) AS mount_path,
               MIN(fl.relative_path) AS rel_path
        FROM files f
        LEFT JOIN file_locations fl
          ON fl.blake3_hash = f.blake3_hash
         AND fl.deleted_at IS NULL
         AND fl.status = 'active'
        LEFT JOIN volume_mounts vm
          ON vm.volume_id = fl.volume_id
         AND vm.deleted_at IS NULL
        WHERE f.quick_hash IS NULL
          AND f.deleted_at IS NULL
        GROUP BY f.blake3_hash
        ORDER BY f.blake3_hash
        LIMIT ?1
    ";
    let mut stmt = conn
        .prepare(sql)
        .map_err(Error::from)
        .map_err(perima_core::CoreError::from)?;
    let rows = stmt
        .query_map(rusqlite::params![limit], |row| {
            let hash_hex: String = row.get(0)?;
            let size_i64: i64 = row.get(1)?;
            let mount_path: Option<String> = row.get(2)?;
            let rel_path: Option<String> = row.get(3)?;
            Ok((hash_hex, size_i64, mount_path, rel_path))
        })
        .map_err(Error::from)
        .map_err(perima_core::CoreError::from)?;

    let mut out = Vec::new();
    for row in rows {
        let (hash_hex, size_i64, mount_path, rel_path) = row
            .map_err(Error::from)
            .map_err(perima_core::CoreError::from)?;
        let hash = BlakeHash::parse_hex(&hash_hex)?;
        // WHY unwrap_or(0): a negative size in the DB indicates corruption;
        // 0 causes `quick_hash_prefix_suffix` to hash an empty prefix which
        // is benign — the writer's COALESCE guard preserves any race-winning
        // real value.
        let size_bytes = u64::try_from(size_i64).unwrap_or(0);

        // Build the absolute path only if both mount_path and rel_path exist.
        let active_path = match (mount_path, rel_path) {
            (Some(mp), Some(rp)) => {
                let mut p = std::path::PathBuf::from(mp);
                p.push(rp);
                Some(p)
            }
            _ => None,
        };

        out.push(BackfillFileRow {
            hash,
            size_bytes,
            active_path,
        });
    }
    Ok(out)
}

/// SELECT body for [`SqliteFileRepository::lookup_by_file_uuid`].
///
/// Returns the `(blake3_hash, absolute_path, file_size)` triple for the
/// non-deleted `files` row identified by `file_uuid`. The absolute path is
/// resolved by joining `file_locations` → `volume_mounts` and selecting
/// the lex-smallest active mount path (deterministic across calls).
///
/// Returns `Ok(None)` when no row matches OR when no active mount is
/// available (the volume is not mounted). The caller (`ComputeFullHashUseCase`)
/// maps `None` to `CoreError::FullHashUnavailable` with the appropriate
/// reason.
/// Row type for [`lookup_by_file_uuid_sql`]'s query output.
///
/// WHY type alias: `clippy::type_complexity` fires on four-element tuples
/// with multiple Option arms; a named alias keeps the function body clean.
type LookupByUuidRow = (Option<String>, i64, Option<String>, Option<String>);

fn lookup_by_file_uuid_sql(
    conn: &Connection,
    file_uuid: FileUuid,
) -> Result<Option<(Option<BlakeHash>, std::path::PathBuf, u64)>, CoreError> {
    // WHY GROUP BY + MIN(...) for path: a file may have multiple active
    // locations (true duplicates on the same volume); we only need one to
    // hash the bytes. MIN keeps the result deterministic so two calls in a
    // row return the same `active_path`.
    //
    // WHY JOIN on `fl.file_uuid = f.file_uuid` instead of `fl.blake3_hash`:
    // when `files.blake3_hash IS NULL` (a pending file whose full_hash has
    // not yet been computed), the hash-based join produces no rows because
    // SQLite NULL equality is always NULL. The V011 migration backfills
    // `file_locations.file_uuid`, so the surrogate key is the stable join
    // axis for both hashed and pending rows (spec §4.8).
    //
    // WHY `blake3_hash` returned as `Option<String>`: V011 makes
    // `files.blake3_hash` nullable for pending rows. The trait signature
    // returns `Option<BlakeHash>` so callers can distinguish "no hash yet"
    // from "file not found". See `FileRepository::lookup_by_file_uuid` doc.
    let sql = "
        SELECT f.blake3_hash, f.file_size,
               MIN(vm.mount_path) AS mount_path,
               MIN(fl.relative_path) AS rel_path
        FROM files f
        LEFT JOIN file_locations fl
          ON fl.file_uuid = f.file_uuid
         AND fl.deleted_at IS NULL
         AND fl.status = 'active'
        LEFT JOIN volume_mounts vm
          ON vm.volume_id = fl.volume_id
         AND vm.deleted_at IS NULL
        WHERE f.file_uuid = ?1
          AND f.deleted_at IS NULL
        GROUP BY f.file_uuid
    ";
    let uuid_str = file_uuid.0.to_string();
    let mut stmt = conn.prepare(sql).map_err(Error::from)?;
    let row: Option<LookupByUuidRow> = stmt
        .query_row(rusqlite::params![uuid_str], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()
        .map_err(Error::from)?;

    let Some((hash_hex_opt, size_i64, mount_path, rel_path)) = row else {
        return Ok(None);
    };

    let hash = match hash_hex_opt {
        Some(hex) => Some(BlakeHash::parse_hex(&hex)?),
        None => None,
    };
    let size_bytes = u64::try_from(size_i64)
        .map_err(|_| CoreError::Internal(format!("stored file_size {size_i64} is negative")))?;

    // Build the absolute path only if both mount_path and rel_path exist.
    let abs_path = match (mount_path, rel_path) {
        (Some(mp), Some(rp)) => {
            let mut p = std::path::PathBuf::from(mp);
            p.push(rp);
            p
        }
        _ => return Ok(None),
    };

    Ok(Some((hash, abs_path, size_bytes)))
}

/// SELECT body for [`SqliteFileRepository::list_locations_for_verify`].
///
/// Returns one row per active location that resolves to a real absolute
/// path on `device`.
///
/// # Two joins that must not be relaxed
///
/// **`vm.machine_id = ?1`** — `volume_mounts` is keyed on
/// `(volume_id, machine_id)`; the same volume mounts at different paths
/// on different machines. Without this predicate the join can pair a
/// local row with another computer's mount path, and the sweep would
/// stat a path that means nothing here. The index
/// `idx_volume_mounts_volume_machine (volume_id, machine_id)` exists for
/// exactly this access pattern. Three older queries in this file omit
/// the predicate — that is #195, deliberately not fixed here so the
/// trait-signature change lands as its own reviewable commit.
///
/// **`INNER JOIN volume_mounts`** (not `LEFT JOIN`) — an unmounted
/// volume must drop out of the result set entirely rather than surface
/// with a NULL `mount_path`. The sweep marks every row it receives and
/// cannot stat as `Missing`; a row for an unplugged external drive that
/// leaks through becomes a false `Missing`, and prune then deletes a
/// catalogue whose files are intact. Making absence structural means no
/// caller can forget the rule. Sibling queries use `LEFT JOIN` because
/// they want the row regardless and filter later — that is correct for
/// them and wrong here.
fn list_locations_for_verify_sql(
    conn: &Connection,
    device: DeviceId,
) -> Result<VerifyCandidates, perima_core::CoreError> {
    let sql = "
        SELECT fl.volume_id, fl.relative_path, fl.status, vm.mount_path
        FROM file_locations fl
        JOIN volume_mounts vm
          ON vm.volume_id = fl.volume_id
         AND vm.machine_id = ?1
         AND vm.deleted_at IS NULL
        WHERE fl.deleted_at IS NULL
        ORDER BY fl.volume_id, fl.relative_path
    ";
    let mut stmt = conn
        .prepare(sql)
        .map_err(Error::from)
        .map_err(perima_core::CoreError::from)?;
    let rows = stmt
        .query_map(rusqlite::params![device.0.to_string()], |row| {
            let vol: String = row.get(0)?;
            let rel: String = row.get(1)?;
            let status: String = row.get(2)?;
            let mount: String = row.get(3)?;
            Ok((vol, rel, status, mount))
        })
        .map_err(Error::from)
        .map_err(perima_core::CoreError::from)?;

    let mut out = Vec::new();
    for row in rows {
        let (vol, rel, status, mount) = row
            .map_err(Error::from)
            .map_err(perima_core::CoreError::from)?;
        let volume_uuid = uuid::Uuid::parse_str(&vol)
            .map_err(|e| perima_core::CoreError::Internal(format!("parse volume_id: {e}")))?;
        // WHY PathBuf::push rather than string concatenation: this must
        // match the reconstruction the read path performs in
        // `crates/desktop/src/payloads.rs`, which is a platform-sensitive
        // push. Concatenating with '/' would produce a path that works on
        // Unix and fails the stat on Windows.
        let mut absolute_path = std::path::PathBuf::from(mount);
        absolute_path.push(&rel);
        out.push(LocationToVerify {
            volume: VolumeId(volume_uuid),
            path: MediaPath::new(&rel),
            absolute_path,
            status: str_to_status(&status),
        });
    }

    // WHY a second COUNT rather than deriving the number from the row
    // set: the excluded rows are excluded by the JOIN, so they are not
    // observable from `out`. Counting all non-deleted locations and
    // subtracting is the only way to report what the sweep could not
    // look at. Both statements run on the same pooled read connection
    // inside the same implicit read snapshot.
    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_locations WHERE deleted_at IS NULL",
            [],
            |r| r.get(0),
        )
        .map_err(Error::from)
        .map_err(perima_core::CoreError::from)?;
    let total = usize::try_from(total).unwrap_or(0);
    let skipped_unmounted = total.saturating_sub(out.len());

    Ok(VerifyCandidates {
        locations: out,
        skipped_unmounted,
    })
}

/// Parse a `file_locations.status` string back into [`LocationStatus`].
///
/// WHY `Active` for an unrecognised value rather than an error: the
/// column is written only by `status_to_str` in the writer, so an
/// unknown string means either a hand-edited database or a future
/// variant this build predates. Failing the whole sweep over one odd row
/// would be worse than treating it as a normal row — the sweep's own
/// stat decides the outcome regardless, so a wrong guess here is
/// self-correcting on the next pass.
fn str_to_status(s: &str) -> perima_core::LocationStatus {
    match s {
        "missing" => perima_core::LocationStatus::Missing,
        "moved" => perima_core::LocationStatus::Moved,
        "stale" => perima_core::LocationStatus::Stale,
        _ => perima_core::LocationStatus::Active,
    }
}

/// SELECT body for [`SqliteFileRepository::list_files_pending_full_hash`].
///
/// Returns `file_uuid` values for every non-deleted `files` row whose
/// `blake3_hash` (== `full_hash`) is `NULL` AND that has at least one active
/// mounted location on this device. Rows without an active mount are skipped
/// because `ComputeFullHashUseCase::execute_single` would immediately return
/// `FullHashUnavailable::NotMounted` for them, making the iteration useless.
///
/// WHY JOIN on `fl.file_uuid = f.file_uuid` instead of `fl.blake3_hash`:
/// when `files.blake3_hash IS NULL` the hash-based join produces no rows.
/// The V011 migration backfills `file_locations.file_uuid`, so the surrogate
/// key is the correct join axis for pending rows (spec §4.8).
///
/// WHY DISTINCT: a single `files` row may have multiple active `file_locations`
/// entries (true duplicates on the same volume). DISTINCT ensures one `file_uuid`
/// per logical file.
fn list_files_pending_full_hash_sql(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<FileUuid>, CoreError> {
    let sql = "
        SELECT DISTINCT f.file_uuid
        FROM files f
        JOIN file_locations fl
          ON fl.file_uuid = f.file_uuid
         AND fl.deleted_at IS NULL
         AND fl.status = 'active'
        JOIN volume_mounts vm
          ON vm.volume_id = fl.volume_id
         AND vm.deleted_at IS NULL
        WHERE f.blake3_hash IS NULL
          AND f.deleted_at IS NULL
        ORDER BY f.file_uuid
        LIMIT ?1
    ";
    let limit_i64 = limit_to_i64(limit);
    let mut stmt = conn.prepare(sql).map_err(Error::from)?;
    let rows = stmt
        .query_map(rusqlite::params![limit_i64], |row| row.get::<_, String>(0))
        .map_err(Error::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::from)?;

    rows.into_iter().map(|s| parse_file_uuid(&s)).collect()
}

/// SELECT body for [`SqliteFileRepository::list_quick_hash_collisions`].
///
/// Groups `files` rows by `quick_hash`, returning every group with `COUNT > 1`
/// where at least one row has not been marked `verified_distinct`. For each
/// surviving group, fetches the active `file_locations` rows so the frontend
/// can render path / volume info per file.
///
/// WHY two-step (group, then per-group locations): `SQLite`'s `GROUP_CONCAT`
/// would force string-parsing on the Rust side; preparing a per-group SELECT
/// keeps the typing clean and the result Vec sized correctly.
fn list_quick_hash_collisions_sql(conn: &Connection) -> Result<Vec<CollisionGroup>, CoreError> {
    // Step 1: find the colliding quick_hash values.
    let group_sql = "
        SELECT quick_hash
        FROM files
        WHERE quick_hash IS NOT NULL
          AND deleted_at IS NULL
          AND verified_distinct = 0
        GROUP BY quick_hash
        HAVING COUNT(*) > 1
        ORDER BY quick_hash
    ";
    let mut stmt = conn.prepare(group_sql).map_err(Error::from)?;
    let quick_hashes: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(Error::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Error::from)?;
    drop(stmt);

    if quick_hashes.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: for each colliding quick_hash, fetch the per-file location rows.
    // WHY one SELECT per group: clean typing, bounded result set (collision
    // groups are sparse in real-world libraries), avoids a complex multi-key
    // JOIN.
    let loc_sql = "
        SELECT f.file_uuid, f.blake3_hash, f.file_size, fl.volume_id, fl.relative_path,
               fl.status, fl.first_seen
        FROM file_locations fl
        JOIN files f ON f.blake3_hash = fl.blake3_hash
        WHERE f.quick_hash = ?1
          AND f.deleted_at IS NULL
          AND fl.deleted_at IS NULL
        ORDER BY fl.relative_path
    ";
    let mut loc_stmt = conn.prepare(loc_sql).map_err(Error::from)?;

    let mut groups = Vec::with_capacity(quick_hashes.len());
    for qh_hex in &quick_hashes {
        let quick_hash = BlakeHash::parse_hex(qh_hex)?;
        let rows = loc_stmt
            .query_map(rusqlite::params![qh_hex], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(Error::from)?;

        let mut files: Vec<FileLocationRecord> = Vec::new();
        for r in rows {
            let (file_uuid_str, hash_hex_opt, size, vol_str, rel_path, status_str, first_seen) =
                r.map_err(Error::from)?;
            let file_uuid = parse_file_uuid(&file_uuid_str)?;
            let hash = parse_optional_hash(hash_hex_opt.as_deref())?;
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
            files.push(FileLocationRecord {
                file_uuid,
                hash,
                size: i64_to_size(size)?,
                volume_id,
                relative_path: MediaPath::new(&rel_path),
                status,
                first_seen,
            });
        }

        // Skip groups that lost all their location rows between the two
        // SELECTs (deletion race). The frontend would render an empty group.
        if files.is_empty() {
            continue;
        }

        groups.push(CollisionGroup {
            quick_hash,
            files,
            // WHY VerifiedState::Unverified for v1: the schema doesn't yet
            // distinguish per-group verification states. Once Task 14 wires
            // up "Compute full hash" UI per file, the group's state becomes a
            // function of the per-file `full_hash` results — folded in then.
            verified_state: VerifiedState::Unverified,
        });
    }

    Ok(groups)
}

/// Shared SELECT body for `list_file_locations`.
///
/// WHY separate function: factored out for clarity and potential reuse.
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
    // WHY `f.file_uuid` first column + `f.blake3_hash` typed as `Option<String>`:
    // Task 11 surfaces `file_uuid` as the stable surrogate key on the IPC
    // boundary; `blake3_hash` becomes nullable for rows whose `full_hash`
    // has not yet been computed (spec §4.8).
    let sql: &str = if vol_filter.is_some() {
        "SELECT f.file_uuid, f.blake3_hash, f.file_size, fl.volume_id, fl.relative_path,
                fl.status, fl.first_seen
         FROM file_locations fl
         JOIN files f ON f.blake3_hash = fl.blake3_hash
         WHERE fl.deleted_at IS NULL AND fl.volume_id = ?1
         ORDER BY fl.relative_path
         LIMIT ?2"
    } else {
        "SELECT f.file_uuid, f.blake3_hash, f.file_size, fl.volume_id, fl.relative_path,
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
            let file_uuid_str: String = row.get(0)?;
            let hash_hex: Option<String> = row.get(1)?;
            let size: i64 = row.get(2)?;
            let vol_str: String = row.get(3)?;
            let rel_path: String = row.get(4)?;
            let status_str: String = row.get(5)?;
            let first_seen: String = row.get(6)?;
            Ok((
                file_uuid_str,
                hash_hex,
                size,
                vol_str,
                rel_path,
                status_str,
                first_seen,
            ))
        })
        .map_err(Error::from)?;

    let mut out = Vec::new();
    for row in rows {
        let (file_uuid_str, hash_hex_opt, size, vol_str, rel_path, status_str, first_seen) =
            row.map_err(Error::from)?;
        let file_uuid = parse_file_uuid(&file_uuid_str)?;
        let hash = parse_optional_hash(hash_hex_opt.as_deref())?;
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
            file_uuid,
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

    use perima_core::EventBus;
    use tempfile::TempDir;

    use super::*;
    use crate::pool::ReadPool;
    use crate::test_utils::NoopBus;
    use crate::writer::{SqliteWriter, SqliteWriterHandle};

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
            rows[0].hash.map(|h| h.to_hex()),
            Some(f_dest.hash.to_hex()),
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
