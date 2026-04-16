//! `FileRepository` implementation backed by rusqlite.

use std::sync::Mutex;

use perima_core::{
    BlakeHash, CoreError, DeviceId, FileLocationRecord, FileRepository, FileSize, HashedFile,
    LocationStatus, MediaPath, UpsertOutcome, VolumeId,
};
use rusqlite::Connection;
// WHY: OptionalExtension adds `.optional()` to query_row results, converting
// QueryReturnedNoRows into Ok(None) for our two-statement SELECT-then-upsert pattern.
use rusqlite::OptionalExtension;

use crate::errors::Error;

/// Rusqlite-backed file + location repository.
///
/// WHY `Mutex`: `rusqlite::Connection` is `Send` but not `Sync` (internal
/// `RefCell` state). The `FileRepository` trait requires `Send + Sync` so
/// callers can store implementations in `Arc<dyn FileRepository>`. Wrapping
/// in `Mutex` makes the struct satisfy both bounds without `unsafe`.
/// All DB methods lock briefly; there is no blocking I/O inside the lock.
pub struct SqliteFileRepository {
    conn: Mutex<Connection>,
}

impl SqliteFileRepository {
    /// Wrap an existing connection. The caller must have run
    /// migrations before constructing this.
    pub const fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Convert `FileSize` (`u64`) to the `i64` that `SQLite` stores.
///
/// WHY: `SQLite` integers are signed 64-bit. A file larger than `i64::MAX`
/// (~8 EiB) cannot exist on current hardware; we propagate as `Internal`
/// rather than silently wrapping.
fn size_to_i64(size: FileSize) -> Result<i64, CoreError> {
    i64::try_from(size.0)
        .map_err(|_| CoreError::Internal(format!("file size {} overflows i64", size.0)))
}

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

impl FileRepository for SqliteFileRepository {
    fn upsert_file(
        &mut self,
        file: &HashedFile,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError> {
        // WHY: PoisonError can only occur if a thread panicked while holding
        // the lock. In that case the DB state is unknown; propagate as Internal.
        let conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;
        let hash_hex = file.hash.to_hex();
        let now = now_iso();
        let dev_str = device.0.to_string();
        let size_i64 = size_to_i64(file.discovered.size)?;

        // WHY: two-statement SELECT-then-INSERT/UPDATE because
        // `SQLite`'s changes() cannot distinguish a fresh INSERT from
        // a conflict-triggered UPDATE — both report 1.
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
                    "INSERT INTO files (blake3_hash, file_size, first_seen, updated_at, device_id)
                     VALUES (?1, ?2, ?3, ?3, ?4)",
                    rusqlite::params![hash_hex, size_i64, now, dev_str],
                )
                .map_err(Error::from)?;
                UpsertOutcome::Inserted
            }
            Some((existing_size, existing_dev))
                if existing_size == size_i64 && existing_dev == dev_str =>
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

    fn upsert_location(
        &mut self,
        hash: &BlakeHash,
        volume: VolumeId,
        path: &MediaPath,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;
        let hash_hex = hash.to_hex();
        let vol_str = volume.0.to_string();
        let path_str = path.as_str();
        let dev_str = device.0.to_string();
        let now = now_iso();

        // WHY: app-level uniqueness on (volume_id, relative_path,
        // deleted_at IS NULL) replaces a UNIQUE constraint that
        // CLAUDE.md forbids on mutable columns. The two-statement
        // pattern is safe under `SQLite`'s single-writer model.
        let existing: Option<(String, String, String)> = conn
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
                conn.execute(
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
                conn.execute(
                    "UPDATE file_locations
                     SET blake3_hash = ?1, updated_at = ?2, device_id = ?3
                     WHERE id = ?4",
                    rusqlite::params![hash_hex, now, dev_str, row_id],
                )
                .map_err(Error::from)?;
                UpsertOutcome::Updated
            }
        };
        drop(conn);
        Ok(outcome)
    }

    // WHY allow(significant_drop_tightening): the Mutex guard `conn` must
    // outlive `stmt` and `rows` because they borrow through the guard.
    // Dropping `conn` after `rows` is fully consumed is already optimal;
    // Clippy's suggested rewrite would break the borrow graph.
    #[allow(clippy::significant_drop_tightening)]
    fn list_file_locations(
        &self,
        limit: usize,
        volume: Option<VolumeId>,
    ) -> Result<Vec<FileLocationRecord>, CoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;
        let vol_filter = volume.map(|v| v.0.to_string());
        let mut stmt = conn
            .prepare(
                "SELECT f.blake3_hash, f.file_size, fl.volume_id, fl.relative_path,
                        fl.status, fl.first_seen
                 FROM file_locations fl
                 JOIN files f ON f.blake3_hash = fl.blake3_hash
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
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::connection::open_and_migrate;

    fn test_db() -> (tempfile::TempDir, SqliteFileRepository) {
        let td = tempfile::tempdir().expect("tempdir");
        let conn = open_and_migrate(&td.path().join("test.db")).expect("open");
        (td, SqliteFileRepository::new(conn))
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
        let (_td, mut repo) = test_db();
        let f = sample_hashed_file(b"hello", "a.txt");
        let out = repo.upsert_file(&f, device()).expect("upsert");
        assert_eq!(out, UpsertOutcome::Inserted);
    }

    #[test]
    fn upsert_file_unchanged_on_repeat() {
        let (_td, mut repo) = test_db();
        let dev = device();
        let f = sample_hashed_file(b"hello", "a.txt");
        repo.upsert_file(&f, dev).expect("first");
        let out = repo.upsert_file(&f, dev).expect("second");
        assert_eq!(out, UpsertOutcome::Unchanged);
    }

    #[test]
    fn upsert_file_updated_on_size_change() {
        let (_td, mut repo) = test_db();
        let dev = device();
        let f1 = sample_hashed_file(b"hello", "a.txt");
        repo.upsert_file(&f1, dev).expect("first");
        // Same hash, different size (contrived but tests the branch).
        let mut f2 = f1.clone();
        f2.discovered.size = FileSize(999);
        let out = repo.upsert_file(&f2, dev).expect("second");
        assert_eq!(out, UpsertOutcome::Updated);
    }

    #[test]
    fn upsert_location_inserts_new() {
        let (_td, mut repo) = test_db();
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
        let (_td, mut repo) = test_db();
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
        let (_td, mut repo) = test_db();
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
        let (_td, mut repo) = test_db();
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
        let (_td, mut repo) = test_db();
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
        let (_td, mut repo) = test_db();
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
}
