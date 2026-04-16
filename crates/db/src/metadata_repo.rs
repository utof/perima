//! `MetadataRepository` implementation backed by rusqlite.

use std::sync::Mutex;

use perima_core::{
    BlakeHash, CoreError, DeviceId, FileLocationRecord, FileSize, LocationStatus, MediaMetadata,
    MediaPath, MetadataRepository, UpsertOutcome, VolumeId,
};
use rusqlite::Connection;
// WHY: OptionalExtension adds `.optional()` to query_row results, converting
// QueryReturnedNoRows into Ok(None) for our two-statement SELECT-then-upsert
// pattern (mirrors `SqliteFileRepository::upsert_file`).
use rusqlite::OptionalExtension;

use crate::errors::Error;

/// Rusqlite-backed media-metadata repository.
///
/// WHY `Mutex<Connection>`: `rusqlite::Connection` is `Send` but not
/// `Sync` (internal `RefCell` state). The [`MetadataRepository`] trait
/// requires `Send + Sync` so callers can share implementations via
/// `Arc<dyn MetadataRepository>` — e.g. the desktop `AppState` and the
/// background `MetadataQueue` worker need the same handle. Wrapping in
/// `Mutex` satisfies both bounds without `unsafe`. All DB methods lock
/// briefly; there is no blocking I/O inside the lock.
///
/// WHY `&self` throughout (unlike `SqliteFileRepository`'s `&mut self`):
/// the trait is declared with `&self` so `Arc`-sharing works without
/// `Mutex<Arc<..>>` contortions at call sites. `FileRepository`'s
/// `&mut self` legacy is tracked for migration in v0.5.x.
pub struct SqliteMetadataRepository {
    conn: Mutex<Connection>,
}

impl SqliteMetadataRepository {
    /// Wrap an existing connection. The caller must have run
    /// migrations (at least V001 + V002) before constructing this.
    pub const fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
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

/// Convert `Option<u64>` to `Option<i64>` for binding as `INTEGER`.
///
/// WHY: `rusqlite`'s `ToSql` impl does not cover `u64` (`SQLite`
/// integers are signed 64-bit). Values originating from media
/// containers (duration in ms) fit comfortably in `i64` on any
/// real-world asset; we propagate overflow as `Internal` rather than
/// truncating.
fn u64_opt_to_i64(v: Option<u64>) -> Result<Option<i64>, CoreError> {
    v.map(|raw| {
        i64::try_from(raw).map_err(|_| {
            CoreError::Internal(format!("duration_ms {raw} overflows SQLite INTEGER (i64)"))
        })
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
    // WHY allow(significant_drop_tightening): the Mutex guard `conn`
    // must outlive the transaction that borrows through it. Dropping
    // the guard earlier would break the borrow graph — same pattern
    // used throughout `file_repo.rs`.
    #[allow(clippy::significant_drop_tightening)]
    fn upsert_metadata(
        &self,
        meta: &MediaMetadata,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;

        // WHY BEGIN IMMEDIATE: the SELECT-then-INSERT/UPDATE sequence
        // must serialize across connections. Two concurrent extractor
        // workers SELECTing "not found" for the same hash would both
        // INSERT otherwise — SQLite's statement-level atomicity is not
        // enough for a read-modify-write cycle. IMMEDIATE grabs the
        // writer lock at BEGIN; the busy_timeout installed by
        // `open_and_migrate` makes the second writer wait instead of
        // erroring with SQLITE_BUSY.
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| CoreError::Internal(format!("begin immediate: {e}")))?;

        let hash_hex = meta.hash.to_hex();
        let now = now_iso();
        let dev_str = device.0.to_string();
        let duration_ms_i64 = u64_opt_to_i64(meta.duration_ms)?;

        // Mirror `SqliteFileRepository::upsert_file`'s SELECT-then-
        // INSERT/UPDATE on the content-addressed PK (blake3_hash). We
        // fetch the existing row's device_id + mime_type for a cheap
        // equivalence proxy to classify Unchanged vs Updated.
        let existing: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT device_id, mime_type FROM file_metadata
                 WHERE blake3_hash = ?1 AND deleted_at IS NULL",
                [&hash_hex],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Error::from)?;

        let outcome = match existing {
            None => {
                tx.execute(
                    "INSERT INTO file_metadata
                     (blake3_hash, width, height, duration_ms, captured_at,
                      camera_make, camera_model, codec, bitrate_bps, mime_type,
                      thumbnail_path, thumbnail_status,
                      extracted_at, updated_at, device_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                             ?11, ?12,
                             ?13, ?13, ?14)",
                    rusqlite::params![
                        hash_hex,
                        meta.width,
                        meta.height,
                        duration_ms_i64,
                        meta.captured_at,
                        meta.camera_make,
                        meta.camera_model,
                        meta.codec,
                        meta.bitrate_bps,
                        meta.mime_type,
                        meta.thumbnail_path,
                        meta.thumbnail_status,
                        now,
                        dev_str,
                    ],
                )
                .map_err(Error::from)?;
                UpsertOutcome::Inserted
            }
            Some((existing_dev, existing_mime))
                if existing_dev == dev_str && existing_mime == meta.mime_type =>
            {
                // WHY cheap equality proxy: comparing every Option field
                // would bloat this method and still miss changes hidden
                // in (say) camera_model alone. mime_type + device_id is
                // the coarsest check that classifies "new extraction
                // run" vs "repeat call with identical inputs". v0.4.0
                // accepts occasional false-Updated over false-Unchanged
                // as the safe default.
                UpsertOutcome::Unchanged
            }
            Some(_) => {
                tx.execute(
                    "UPDATE file_metadata
                     SET width = ?2, height = ?3, duration_ms = ?4,
                         captured_at = ?5, camera_make = ?6, camera_model = ?7,
                         codec = ?8, bitrate_bps = ?9, mime_type = ?10,
                         thumbnail_path = ?11, thumbnail_status = ?12,
                         updated_at = ?13, device_id = ?14
                     WHERE blake3_hash = ?1",
                    rusqlite::params![
                        hash_hex,
                        meta.width,
                        meta.height,
                        duration_ms_i64,
                        meta.captured_at,
                        meta.camera_make,
                        meta.camera_model,
                        meta.codec,
                        meta.bitrate_bps,
                        meta.mime_type,
                        meta.thumbnail_path,
                        meta.thumbnail_status,
                        now,
                        dev_str,
                    ],
                )
                .map_err(Error::from)?;
                UpsertOutcome::Updated
            }
        };

        tx.commit()
            .map_err(|e| CoreError::Internal(format!("commit: {e}")))?;
        Ok(outcome)
    }

    // WHY allow(significant_drop_tightening): the Mutex guard must
    // outlive the query borrow.
    #[allow(clippy::significant_drop_tightening)]
    fn find_by_hash(&self, hash: &BlakeHash) -> Result<Option<MediaMetadata>, CoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;
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

    // WHY allow(significant_drop_tightening): `stmt` + `rows` borrow
    // through the Mutex guard; dropping `conn` earlier breaks the
    // borrow graph (same pattern as `list_file_locations`).
    #[allow(clippy::significant_drop_tightening)]
    #[allow(clippy::too_many_lines)]
    fn list_with_metadata(
        &self,
        limit: usize,
        volume: Option<VolumeId>,
    ) -> Result<Vec<(FileLocationRecord, Option<MediaMetadata>)>, CoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;
        let vol_filter = volume.map(|v| v.0.to_string());

        // WHY single LEFT JOIN (no N+1): pairing file locations with
        // optional metadata in one statement keeps large `ls
        // --with-metadata` listings cheap. `fm.updated_at IS NULL` is
        // the sentinel that distinguishes "no metadata row at all"
        // from "row present with all-NULL optional fields" — the
        // latter is a legitimate state (extractor ran on an
        // unsupported MIME and wrote an empty row).
        let mut stmt = conn
            .prepare(
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
                   AND (?1 IS NULL OR fl.volume_id = ?1)
                 ORDER BY fl.relative_path
                 LIMIT ?2",
            )
            .map_err(Error::from)?;

        let rows = stmt
            .query_map(rusqlite::params![vol_filter, limit_to_i64(limit)], |row| {
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
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use perima_core::{DiscoveredFile, FileRepository, HashedFile};

    use super::*;
    use crate::connection::open_and_migrate;
    use crate::file_repo::SqliteFileRepository;

    fn metadata_repo() -> (tempfile::TempDir, SqliteMetadataRepository) {
        let td = tempfile::tempdir().expect("tempdir");
        let conn = open_and_migrate(&td.path().join("test.db")).expect("open");
        (td, SqliteMetadataRepository::new(conn))
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

    #[test]
    fn upsert_metadata_inserts_new() {
        let (_td, repo) = metadata_repo();
        let dev = device();
        let hash = BlakeHash::from_bytes(*blake3::hash(b"payload").as_bytes());
        let meta = sample_metadata(hash);
        let out = repo.upsert_metadata(&meta, dev).expect("upsert");
        assert_eq!(out, UpsertOutcome::Inserted);
    }

    #[test]
    fn upsert_metadata_unchanged_on_repeat() {
        let (_td, repo) = metadata_repo();
        let dev = device();
        let hash = BlakeHash::from_bytes(*blake3::hash(b"payload").as_bytes());
        let meta = sample_metadata(hash);
        repo.upsert_metadata(&meta, dev).expect("first");
        let out = repo.upsert_metadata(&meta, dev).expect("second");
        assert_eq!(out, UpsertOutcome::Unchanged);
    }

    #[test]
    fn upsert_metadata_updated_on_change() {
        let (_td, repo) = metadata_repo();
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
        let td = tempfile::tempdir().expect("tempdir");
        let db_path = td.path().join("test.db");
        let dev = device();
        let vol = VolumeId::new();
        let f = sample_hashed_file(b"joinsnull", "no_meta.txt");

        {
            let conn = open_and_migrate(&db_path).expect("open");
            let mut file_repo = SqliteFileRepository::new(conn);
            file_repo.upsert_file(&f, dev).expect("upsert file");
            file_repo
                .upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
                .expect("upsert location");
        }

        // Act: open a second connection and call list_with_metadata.
        let meta_conn = open_and_migrate(&db_path).expect("reopen");
        let meta_repo = SqliteMetadataRepository::new(meta_conn);
        let rows = meta_repo
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
        let td = tempfile::tempdir().expect("tempdir");
        let db_path = td.path().join("test.db");
        let dev = device();
        let vol = VolumeId::new();
        let f = sample_hashed_file(b"joinspop", "has_meta.jpg");
        let meta = sample_metadata(f.hash);

        {
            let conn = open_and_migrate(&db_path).expect("open");
            let mut file_repo = SqliteFileRepository::new(conn);
            file_repo.upsert_file(&f, dev).expect("upsert file");
            file_repo
                .upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
                .expect("upsert location");
        }
        {
            let conn = open_and_migrate(&db_path).expect("reopen for metadata");
            let meta_repo = SqliteMetadataRepository::new(conn);
            meta_repo
                .upsert_metadata(&meta, dev)
                .expect("upsert metadata");
        }

        // Act
        let meta_conn = open_and_migrate(&db_path).expect("reopen for list");
        let meta_repo = SqliteMetadataRepository::new(meta_conn);
        let rows = meta_repo
            .list_with_metadata(100, None)
            .expect("list_with_metadata");

        // Assert
        assert_eq!(rows.len(), 1);
        let (loc, got_meta) = &rows[0];
        assert_eq!(loc.hash, f.hash);
        let got = got_meta
            .as_ref()
            .expect("metadata present for file with file_metadata row");
        assert_eq!(got, &meta, "round-tripped metadata must match");
    }
}
