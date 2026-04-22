//! `MetadataRepository` adapter — writer-actor + read-pool backed.
//!
//! Post-Batch-C Task 4. The struct holds two cheap-to-clone handles:
//! a [`flume::Sender<WriteCmd>`] connected to the single writer actor
//! (spec §3.1) and a [`ReadPool`] of read-only `r2d2_sqlite`
//! connections (spec §3.4). Writes build a [`MetadataWriteCmd`] variant
//! with a `flume::bounded(1)` reply channel and block on the reply.
//! Reads run SQL directly against a pooled connection.
//!
//! No `Mutex<Connection>`. The legacy `::new(conn)` constructor is
//! deleted; every caller now supplies `(writer_sender, read_pool)`.

use flume::Sender;
use perima_core::{
    BlakeHash, CoreError, DeviceId, FileLocationRecord, FileSize, LocationStatus, MediaMetadata,
    MediaPath, MetadataRepository, UpsertOutcome, VolumeId,
};
// WHY: OptionalExtension adds `.optional()` to query_row results, converting
// QueryReturnedNoRows into Ok(None) for the two-statement SELECT-then-upsert
// pattern preserved on the read path (mirrors `SqliteFileRepository`).
use rusqlite::OptionalExtension;

use crate::cmd::{MetadataWriteCmd, WriteCmd};
use crate::errors::Error;
use crate::pool::ReadPool;

/// Writer-actor + read-pool backed media-metadata repository.
///
/// Cheap to [`Clone`]: both fields (`flume::Sender`, `ReadPool`) are
/// internally refcounted.
#[derive(Clone)]
pub struct SqliteMetadataRepository {
    writer: Sender<WriteCmd>,
    reads: ReadPool,
}

impl std::fmt::Debug for SqliteMetadataRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteMetadataRepository")
            .finish_non_exhaustive()
    }
}

impl SqliteMetadataRepository {
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

/// Cast a `usize` limit to `i64` for `SQLite`'s `LIMIT ?` parameter.
///
/// WHY: `LIMIT` accepts a signed 64-bit integer in `SQLite`. A `usize`
/// larger than `i64::MAX` is capped to `i64::MAX` — effectively
/// unlimited, and the safest fallback for a caller asking "everything".
fn limit_to_i64(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

/// Convert an `i64` stored in the DB back to `FileSize`.
///
/// WHY: values we wrote originated as `u64` that fit in `i64`, so a
/// negative value indicates data corruption and is propagated as
/// `Internal` rather than silently wrapping.
fn i64_to_size(v: i64) -> Result<FileSize, CoreError> {
    u64::try_from(v)
        .map(FileSize)
        .map_err(|_| CoreError::Internal(format!("stored file_size {v} is negative")))
}

/// Convert a `u32` column value from a nullable `INTEGER` via `i64`.
///
/// WHY: `rusqlite` reads `INTEGER` as `i64`. Dimensions/bitrate are
/// `u32` in the domain type. Negative or too-large stored values mean
/// data corruption; we propagate as `Internal`.
fn i64_to_u32_opt(v: Option<i64>) -> Result<Option<u32>, CoreError> {
    v.map(|raw| {
        u32::try_from(raw)
            .map_err(|_| CoreError::Internal(format!("stored u32 column value {raw} out of range")))
    })
    .transpose()
}

/// Convert a nullable `INTEGER` column to `u64`.
///
/// WHY: `duration_ms` is `u64` in the domain type. Negative values
/// indicate data corruption.
fn i64_to_u64_opt(v: Option<i64>) -> Result<Option<u64>, CoreError> {
    v.map(|raw| {
        u64::try_from(raw)
            .map_err(|_| CoreError::Internal(format!("stored u64 column value {raw} out of range")))
    })
    .transpose()
}

/// Raw tuple mirroring the optional columns of a `file_metadata` row.
///
/// WHY type alias: the 11-tuple is repeated in `find_by_hash` and keeps
/// clippy's `type_complexity` wall satisfied without suppression.
/// Field order matches the SELECT clause: `width, height, duration_ms,
/// captured_at, camera_make, camera_model, codec, bitrate_bps,
/// mime_type, thumbnail_path, thumbnail_status`.
type MetadataRowCols = (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Deserialize a `LocationStatus` from its DB string representation.
///
/// WHY: mirrors `SqliteFileRepository`'s string mapping so the two
/// adapters agree on the enum encoding. Unknown values propagate as
/// `Internal` rather than defaulting silently.
fn status_from_str(s: &str) -> Result<LocationStatus, CoreError> {
    match s {
        "active" => Ok(LocationStatus::Active),
        "missing" => Ok(LocationStatus::Missing),
        "moved" => Ok(LocationStatus::Moved),
        "stale" => Ok(LocationStatus::Stale),
        other => Err(CoreError::Internal(format!(
            "unknown location status: {other}"
        ))),
    }
}

impl MetadataRepository for SqliteMetadataRepository {
    fn upsert_metadata(
        &self,
        meta: &MediaMetadata,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<UpsertOutcome, CoreError>>(1);
        // WHY clone `meta`: the command crosses a thread boundary via
        // `flume::Sender::send`, which requires `'static`. `MediaMetadata`
        // is `Clone`, so this is a shallow clone (the only heap payloads
        // are the `Option<String>` fields — metadata rows are small).
        self.writer
            .send(WriteCmd::Metadata(MetadataWriteCmd::UpsertMetadata {
                record: meta.clone(),
                device,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
    }

    fn find_by_hash(&self, hash: &BlakeHash) -> Result<Option<MediaMetadata>, CoreError> {
        // WHY a pool checkout here (no writer hop): `find_by_hash` is a
        // pure SELECT. Reads go directly through the `r2d2_sqlite` pool
        // (spec §3.5). `PooledConnection` derefs to
        // `rusqlite::Connection`, so the SQL body is lifted verbatim
        // from the pre-Batch-C impl.
        let conn = self.reads.get()?;
        let hash_hex = hash.to_hex();
        let row: Option<MetadataRowCols> = conn
            .query_row(
                "SELECT width, height, duration_ms, captured_at,
                        camera_make, camera_model, codec, bitrate_bps, mime_type,
                        thumbnail_path, thumbnail_status
                 FROM file_metadata
                 WHERE blake3_hash = ?1 AND deleted_at IS NULL",
                [&hash_hex],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get(10)?,
                    ))
                },
            )
            .optional()
            .map_err(Error::from)?;

        match row {
            None => Ok(None),
            Some((
                width,
                height,
                duration_ms,
                captured_at,
                camera_make,
                camera_model,
                codec,
                bitrate_bps,
                mime_type,
                thumbnail_path,
                thumbnail_status,
            )) => Ok(Some(MediaMetadata {
                hash: *hash,
                width: i64_to_u32_opt(width)?,
                height: i64_to_u32_opt(height)?,
                duration_ms: i64_to_u64_opt(duration_ms)?,
                captured_at,
                camera_make,
                camera_model,
                codec,
                bitrate_bps: i64_to_u32_opt(bitrate_bps)?,
                mime_type,
                thumbnail_path,
                thumbnail_status,
            })),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn list_with_metadata(
        &self,
        limit: usize,
        volume: Option<VolumeId>,
    ) -> Result<Vec<(FileLocationRecord, Option<MediaMetadata>)>, CoreError> {
        let conn = self.reads.get()?;
        let vol_filter = volume.map(|v| v.0.to_string());

        // WHY single LEFT JOIN (no N+1): pairing file locations with
        // optional metadata in one statement keeps large `ls
        // --with-metadata` listings cheap. `fm.updated_at IS NULL` is
        // the sentinel that distinguishes "no metadata row at all"
        // from "row present with all-NULL optional fields" — the
        // latter is a legitimate state (extractor ran on an
        // unsupported MIME and wrote an empty row).
        //
        // WHY separate SQL strings per branch: the `(?1 IS NULL OR fl.volume_id = ?1)`
        // OR-with-NULL predicate defeats index use on `idx_file_locations_volume_path`
        // even when a concrete volume_id is supplied (SQLite's planner cannot
        // factor NULL out of the disjunction). Branching at Rust level keeps
        // both shapes index-eligible.
        let sql: &str = if vol_filter.is_some() {
            "SELECT f.blake3_hash, f.file_size, fl.volume_id, fl.relative_path,
                    fl.status, fl.first_seen,
                    fm.updated_at,
                    fm.width, fm.height, fm.duration_ms, fm.captured_at,
                    fm.camera_make, fm.camera_model, fm.codec, fm.bitrate_bps,
                    fm.mime_type, fm.thumbnail_path, fm.thumbnail_status
             FROM file_locations fl
             JOIN files f ON f.blake3_hash = fl.blake3_hash
             LEFT JOIN file_metadata fm
               ON fm.blake3_hash = fl.blake3_hash
              AND fm.deleted_at IS NULL
             WHERE fl.deleted_at IS NULL AND fl.volume_id = ?1
             ORDER BY fl.relative_path
             LIMIT ?2"
        } else {
            "SELECT f.blake3_hash, f.file_size, fl.volume_id, fl.relative_path,
                    fl.status, fl.first_seen,
                    fm.updated_at,
                    fm.width, fm.height, fm.duration_ms, fm.captured_at,
                    fm.camera_make, fm.camera_model, fm.codec, fm.bitrate_bps,
                    fm.mime_type, fm.thumbnail_path, fm.thumbnail_status
             FROM file_locations fl
             JOIN files f ON f.blake3_hash = fl.blake3_hash
             LEFT JOIN file_metadata fm
               ON fm.blake3_hash = fl.blake3_hash
              AND fm.deleted_at IS NULL
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
                let fm_updated_at: Option<String> = row.get(6)?;
                let width: Option<i64> = row.get(7)?;
                let height: Option<i64> = row.get(8)?;
                let duration_ms: Option<i64> = row.get(9)?;
                let captured_at: Option<String> = row.get(10)?;
                let camera_make: Option<String> = row.get(11)?;
                let camera_model: Option<String> = row.get(12)?;
                let codec: Option<String> = row.get(13)?;
                let bitrate_bps: Option<i64> = row.get(14)?;
                let mime_type: Option<String> = row.get(15)?;
                let thumbnail_path: Option<String> = row.get(16)?;
                let thumbnail_status: Option<String> = row.get(17)?;
                Ok((
                    hash_hex,
                    size,
                    vol_str,
                    rel_path,
                    status_str,
                    first_seen,
                    fm_updated_at,
                    width,
                    height,
                    duration_ms,
                    captured_at,
                    camera_make,
                    camera_model,
                    codec,
                    bitrate_bps,
                    mime_type,
                    thumbnail_path,
                    thumbnail_status,
                ))
            })
            .map_err(Error::from)?;

        let mut out = Vec::new();
        for row in rows {
            let (
                hash_hex,
                size,
                vol_str,
                rel_path,
                status_str,
                first_seen,
                fm_updated_at,
                width,
                height,
                duration_ms,
                captured_at,
                camera_make,
                camera_model,
                codec,
                bitrate_bps,
                mime_type,
                thumbnail_path,
                thumbnail_status,
            ) = row.map_err(Error::from)?;
            let hash = BlakeHash::parse_hex(&hash_hex)?;
            let volume_id = VolumeId(
                uuid::Uuid::parse_str(&vol_str)
                    .map_err(|e| CoreError::Internal(format!("bad volume uuid: {e}")))?,
            );
            let status = status_from_str(&status_str)?;
            let location = FileLocationRecord {
                hash,
                size: i64_to_size(size)?,
                volume_id,
                relative_path: MediaPath::new(&rel_path),
                status,
                first_seen,
            };

            // WHY `fm_updated_at` as the NULL sentinel: under LEFT JOIN,
            // SQLite synthesizes all right-side columns as NULL when
            // there is no match. `updated_at` is NOT NULL in
            // file_metadata, so NULL here unambiguously means "no row"
            // rather than "row with an unset optional field".
            let metadata = if fm_updated_at.is_none() {
                None
            } else {
                Some(MediaMetadata {
                    hash,
                    width: i64_to_u32_opt(width)?,
                    height: i64_to_u32_opt(height)?,
                    duration_ms: i64_to_u64_opt(duration_ms)?,
                    captured_at,
                    camera_make,
                    camera_model,
                    codec,
                    bitrate_bps: i64_to_u32_opt(bitrate_bps)?,
                    mime_type,
                    thumbnail_path,
                    thumbnail_status,
                })
            };

            out.push((location, metadata));
        }
        Ok(out)
    }

    fn update_thumbnail(
        &self,
        hash: &BlakeHash,
        path: Option<&str>,
        status: &str,
        device: DeviceId,
    ) -> Result<u64, CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
        // WHY clone `path` / `status`: same rationale as `upsert_metadata`
        // above — commands cross a thread boundary via `flume::Sender::send`
        // (`'static` lifetime contract).
        self.writer
            .send(WriteCmd::Metadata(MetadataWriteCmd::UpdateThumbnail {
                hash: *hash,
                path: path.map(str::to_owned),
                status: status.to_owned(),
                device,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
    }
}

#[allow(
    clippy::unwrap_used,
    reason = "tests: unwrap is the assertion — a panic is a failing test by design"
)]
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use perima_core::{DiscoveredFile, EventBus, FileEvent, FileRepository, HashedFile};
    use tempfile::TempDir;

    use super::*;
    use crate::connection::open_and_migrate;
    use crate::file_repo::SqliteFileRepository;
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
    fn metadata_repo() -> (TempDir, SqliteMetadataRepository, SqliteWriterHandle) {
        let td = tempfile::tempdir().expect("tempdir");
        let db_path = td.path().join("test.db");
        let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
        let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
        let reads = ReadPool::open(&db_path).expect("pool open");
        let repo = SqliteMetadataRepository::new(writer.sender(), reads);
        (td, repo, writer)
    }

    /// WHY duplicated from `file_repo::tests`: those helpers are
    /// `#[cfg(test)]` inside the `file_repo` module, so they are
    /// inaccessible from a sibling module's test submodule. Keeping
    /// the duplicated helpers small is cheaper than re-exposing them
    /// via `pub(crate)` test plumbing.
    fn sample_hashed_file(content: &[u8], rel_path: &str) -> HashedFile {
        let hash = BlakeHash::from_bytes(*blake3::hash(content).as_bytes());
        HashedFile {
            discovered: DiscoveredFile {
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

    fn sample_metadata(hash: BlakeHash) -> MediaMetadata {
        MediaMetadata {
            hash,
            width: Some(1920),
            height: Some(1080),
            duration_ms: None,
            captured_at: Some("2024-06-01T12:34:56Z".into()),
            camera_make: Some("Canon".into()),
            camera_model: Some("EOS R5".into()),
            codec: None,
            bitrate_bps: None,
            mime_type: Some("image/jpeg".into()),
            thumbnail_path: None,
            thumbnail_status: None,
        }
    }

    /// Regression: the V004 backfill SQL must flip NULL
    /// `thumbnail_status` rows to 'pending' without touching rows that
    /// already have a real status. Models a v0.4.1 DB upgraded to
    /// v0.4.2 — rows persisted before V004 carried NULL; the partial
    /// index `idx_file_metadata_thumbnail_pending` excluded them.
    /// See `utof/perima#15` HIGH #3.
    ///
    /// WHY direct `open_and_migrate` here (not writer+pool): this test
    /// exercises the raw migration SQL, not the adapter API. Running
    /// a one-shot owned connection keeps the assertion scope tight —
    /// no writer actor / pool required.
    #[test]
    fn v004_backfills_null_thumbnail_status_to_pending() {
        let td = tempfile::tempdir().expect("tempdir");
        let db_path = td.path().join("test.db");
        let conn = open_and_migrate(&db_path).expect("open");

        // Insert a row with thumbnail_status = NULL directly via raw
        // SQL, simulating a row that existed pre-V004.
        // WHY raw insert: the production `upsert_metadata` path now
        // seeds 'pending' on INSERT, so we cannot reproduce a NULL
        // via the safe API. `extracted_at` is NOT NULL per V002 —
        // supply an ISO timestamp.
        conn.execute(
            "INSERT INTO file_metadata
             (blake3_hash, thumbnail_status, extracted_at, updated_at, device_id)
             VALUES (?1, NULL, ?2, ?2, ?3)",
            rusqlite::params![
                "deadbeef".repeat(8), // 64 hex chars
                "2026-04-16T00:00:00Z",
                DeviceId::new().0.to_string(),
            ],
        )
        .expect("insert legacy NULL row");

        // Sanity: the row is indeed NULL.
        let before: Option<String> = conn
            .query_row(
                "SELECT thumbnail_status FROM file_metadata
                 WHERE blake3_hash = ?1",
                [&"deadbeef".repeat(8)],
                |r| r.get(0),
            )
            .expect("read before");
        assert!(before.is_none(), "precondition: row starts at NULL");

        // Re-run the V004 SQL verbatim (refinery only runs once; a
        // fresh execution of the same UPDATE is idempotent and
        // exercises the same statement).
        let rows = conn
            .execute(
                "UPDATE file_metadata
                    SET thumbnail_status = 'pending'
                  WHERE thumbnail_status IS NULL
                    AND deleted_at IS NULL",
                [],
            )
            .expect("run V004 sql");
        assert_eq!(rows, 1, "V004 must update exactly the 1 NULL row");

        let after: Option<String> = conn
            .query_row(
                "SELECT thumbnail_status FROM file_metadata
                 WHERE blake3_hash = ?1",
                [&"deadbeef".repeat(8)],
                |r| r.get(0),
            )
            .expect("read after");
        assert_eq!(
            after.as_deref(),
            Some("pending"),
            "V004 must flip NULL rows to 'pending'",
        );
    }

    /// Regression: fresh INSERTs via `upsert_metadata` must seed
    /// `thumbnail_status = 'pending'` so the partial index stays
    /// populated for future rows. See `utof/perima#15` HIGH #3.
    #[test]
    fn upsert_metadata_insert_seeds_pending_thumbnail_status() {
        let (_td, repo, _writer) = metadata_repo();
        let dev = device();
        let hash = BlakeHash::from_bytes(*blake3::hash(b"seed").as_bytes());
        let meta = sample_metadata(hash);
        repo.upsert_metadata(&meta, dev).expect("upsert insert");

        let got = repo.find_by_hash(&hash).expect("find").expect("present");
        assert_eq!(
            got.thumbnail_status.as_deref(),
            Some("pending"),
            "fresh INSERT must seed thumbnail_status='pending', got {:?}",
            got.thumbnail_status,
        );
        assert!(
            got.thumbnail_path.is_none(),
            "fresh INSERT must leave thumbnail_path = None",
        );
    }

    #[test]
    fn upsert_metadata_inserts_new() {
        let (_td, repo, _writer) = metadata_repo();
        let dev = device();
        let hash = BlakeHash::from_bytes(*blake3::hash(b"payload").as_bytes());
        let meta = sample_metadata(hash);
        let out = repo.upsert_metadata(&meta, dev).expect("upsert");
        assert_eq!(out, UpsertOutcome::Inserted);
    }

    #[test]
    fn upsert_metadata_unchanged_on_repeat() {
        let (_td, repo, _writer) = metadata_repo();
        let dev = device();
        let hash = BlakeHash::from_bytes(*blake3::hash(b"payload").as_bytes());
        let meta = sample_metadata(hash);
        repo.upsert_metadata(&meta, dev).expect("first");
        let out = repo.upsert_metadata(&meta, dev).expect("second");
        assert_eq!(out, UpsertOutcome::Unchanged);
    }

    #[test]
    fn upsert_metadata_updated_on_change() {
        let (_td, repo, _writer) = metadata_repo();
        let dev = device();
        let hash = BlakeHash::from_bytes(*blake3::hash(b"payload").as_bytes());
        let meta1 = sample_metadata(hash);
        repo.upsert_metadata(&meta1, dev).expect("first");
        let mut meta2 = meta1;
        meta2.mime_type = Some("image/png".into());
        let out = repo.upsert_metadata(&meta2, dev).expect("second");
        assert_eq!(out, UpsertOutcome::Updated);
    }

    #[test]
    fn list_with_metadata_joins_null() {
        // Arrange: insert a file + location WITHOUT a metadata row.
        let (td, repo, _writer) = metadata_repo();
        let db_path = td.path().join("test.db");
        let dev = device();
        let vol = VolumeId::new();
        let f = sample_hashed_file(b"joinsnull", "no_meta.txt");

        // WHY new_legacy: Task 5 adds the writer+pool constructor for
        // SqliteFileRepository. This test fixture uses the legacy constructor
        // (deprecated, Task 7 migrates it). Seeding via open_and_migrate on
        // the same DB file works because migrations already ran in
        // SqliteWriter::start.
        {
            let conn = open_and_migrate(&db_path).expect("open");
            #[allow(deprecated)]
            let file_repo = SqliteFileRepository::new_legacy(conn);
            file_repo.upsert_file(&f, dev).expect("upsert file");
            file_repo
                .upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
                .expect("upsert location");
        }

        // Act: call list_with_metadata through the pool.
        let rows = repo
            .list_with_metadata(100, None)
            .expect("list_with_metadata");

        // Assert: one row, metadata is None.
        assert_eq!(rows.len(), 1, "expected exactly one (file_loc, meta) pair");
        let (loc, meta) = &rows[0];
        assert_eq!(loc.hash, f.hash);
        assert_eq!(loc.relative_path.as_str(), "no_meta.txt");
        assert!(
            meta.is_none(),
            "file without file_metadata row must yield None"
        );
    }

    #[test]
    fn list_with_metadata_joins_populated() {
        // Arrange: insert file + location AND a metadata row for it.
        let (td, repo, _writer) = metadata_repo();
        let db_path = td.path().join("test.db");
        let dev = device();
        let vol = VolumeId::new();
        let f = sample_hashed_file(b"joinspop", "has_meta.jpg");
        let meta = sample_metadata(f.hash);

        {
            let conn = open_and_migrate(&db_path).expect("open");
            #[allow(deprecated)]
            let file_repo = SqliteFileRepository::new_legacy(conn);
            file_repo.upsert_file(&f, dev).expect("upsert file");
            file_repo
                .upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
                .expect("upsert location");
        }
        // Seed metadata via the writer-backed repo.
        repo.upsert_metadata(&meta, dev).expect("upsert metadata");

        // Act
        let rows = repo
            .list_with_metadata(100, None)
            .expect("list_with_metadata");

        // Assert
        assert_eq!(rows.len(), 1);
        let (loc, got_meta) = &rows[0];
        assert_eq!(loc.hash, f.hash);
        let got = got_meta
            .as_ref()
            .expect("metadata present for file with file_metadata row");
        // WHY expected-transform: since v0.4.2, `upsert_metadata`'s
        // INSERT seeds `thumbnail_status = 'pending'` as a literal
        // default so the partial index stays populated and the UI
        // can distinguish "not yet attempted" from "nothing to
        // attempt". `sample_metadata` still carries None in the
        // struct (extractors produce None); after round-trip the
        // stored row reads back as 'pending'. See `utof/perima#15`
        // HIGH #3.
        let mut expected = meta;
        expected.thumbnail_status = Some("pending".into());
        assert_eq!(
            got, &expected,
            "round-tripped metadata must match (with insert-seeded 'pending')",
        );
    }

    #[test]
    fn update_thumbnail_marks_ready_with_path() {
        let (_td, repo, _writer) = metadata_repo();
        let dev = device();
        let hash = BlakeHash::from_bytes(*blake3::hash(b"ready").as_bytes());
        let meta = sample_metadata(hash);
        repo.upsert_metadata(&meta, dev).expect("upsert");

        let n = repo
            .update_thumbnail(&hash, Some("/data/t/ab/cd.webp"), "ready", dev)
            .expect("update_thumbnail");
        assert_eq!(n, 1, "exactly 1 row updated");

        let got = repo.find_by_hash(&hash).expect("find").expect("present");
        assert_eq!(got.thumbnail_path.as_deref(), Some("/data/t/ab/cd.webp"));
        assert_eq!(got.thumbnail_status.as_deref(), Some("ready"));
    }

    /// Regression: an `Updated` upsert (triggered by a `mime_type` flip
    /// on the same hash) must NOT clobber the thumbnail state that
    /// `update_thumbnail` already wrote. The queue worker is the sole
    /// writer of thumbnail columns; extractor-sourced upserts must
    /// leave them untouched. See `utof/perima#15` HIGH #4.
    #[test]
    fn upsert_metadata_preserves_thumbnail_state() {
        let (_td, repo, _writer) = metadata_repo();
        let dev = device();
        let hash = BlakeHash::from_bytes(*blake3::hash(b"preserve").as_bytes());
        let mut meta = sample_metadata(hash);
        repo.upsert_metadata(&meta, dev).expect("initial upsert");

        // Simulate thumbnail completion from the worker.
        repo.update_thumbnail(&hash, Some("/tmp/t.webp"), "ready", dev)
            .expect("update_thumbnail");

        // A later metadata upsert with a new mime (forces Updated path)
        // must NOT clear the thumbnail_path / status.
        meta.mime_type = Some("image/jpeg2000".into());
        repo.upsert_metadata(&meta, dev).expect("second upsert");

        let got = repo.find_by_hash(&hash).expect("find").expect("present");
        assert_eq!(
            got.thumbnail_path.as_deref(),
            Some("/tmp/t.webp"),
            "upsert_metadata must not clear thumbnail_path"
        );
        assert_eq!(
            got.thumbnail_status.as_deref(),
            Some("ready"),
            "upsert_metadata must not clear thumbnail_status"
        );
    }

    #[test]
    fn update_thumbnail_marks_failed_keeps_path_none() {
        let (_td, repo, _writer) = metadata_repo();
        let dev = device();
        let hash = BlakeHash::from_bytes(*blake3::hash(b"fail").as_bytes());
        let meta = sample_metadata(hash);
        repo.upsert_metadata(&meta, dev).expect("upsert");

        let n = repo
            .update_thumbnail(&hash, None, "failed", dev)
            .expect("update_thumbnail");
        assert_eq!(n, 1);

        let got = repo.find_by_hash(&hash).expect("find").expect("present");
        assert_eq!(got.thumbnail_path, None);
        assert_eq!(got.thumbnail_status.as_deref(), Some("failed"));
    }
}
