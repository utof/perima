//! `SearchRepository` implementation backed by rusqlite FTS5.

use std::sync::Mutex;

use perima_core::{CoreError, SearchHit, SearchRepository};
use rusqlite::Connection;

use crate::errors::Error;

/// Rusqlite-backed full-text search repository.
///
/// WHY `Mutex<Connection>`: same rationale as `SqliteTagRepository` —
/// `Connection` is `Send` but not `Sync`; wrapping satisfies the
/// `Send + Sync` bound required by the `SearchRepository` trait without
/// `unsafe`.
pub struct SqliteSearchRepository {
    conn: Mutex<Connection>,
}

impl SqliteSearchRepository {
    /// Wrap an existing connection. Caller must have run migrations
    /// through V006 before constructing this.
    pub const fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }
}

impl SearchRepository for SqliteSearchRepository {
    #[allow(clippy::significant_drop_tightening)]
    fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, CoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT srm.blake3_hash, srm.volume_id, srm.relative_path, rank
                 FROM search_index
                 JOIN search_rowid_map srm ON srm.rowid = search_index.rowid
                 WHERE search_index MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(Error::from)?;

        let hits = stmt
            .query_map(rusqlite::params![query, limit], |row| {
                Ok(SearchHit {
                    blake3_hash: row.get(0)?,
                    volume_id: row.get(1)?,
                    relative_path: row.get(2)?,
                    rank: row.get(3)?,
                })
            })
            .map_err(Error::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::from)?;

        Ok(hits)
    }

    #[allow(clippy::significant_drop_tightening)]
    fn rebuild(&self) -> Result<(), CoreError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;

        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(Error::from)?;

        // Wipe both the FTS5 index and the rowid map.
        tx.execute_batch(
            "DELETE FROM search_rowid_map;
             INSERT INTO search_index(search_index) VALUES('delete-all');",
        )
        .map_err(Error::from)?;

        // Populate the rowid map: one representative location per hash.
        // WHY INSERT OR IGNORE: the UNIQUE constraint on blake3_hash in
        // search_rowid_map ensures we pick exactly one file_locations row
        // per hash without requiring a GROUP BY.
        tx.execute_batch(
            "INSERT OR IGNORE INTO search_rowid_map (blake3_hash, volume_id, relative_path)
             SELECT f.blake3_hash, fl.volume_id, fl.relative_path
             FROM files f
             JOIN file_locations fl ON fl.blake3_hash = f.blake3_hash
             WHERE f.deleted_at IS NULL AND fl.deleted_at IS NULL;",
        )
        .map_err(Error::from)?;

        // Populate the FTS5 index from the rowid map.
        // WHY filename = relative_path: SQLite has no built-in REVERSE() for
        // basename extraction; the unicode61 tokenizer splits on '/' and '.'
        // so basenames are discoverable via token match on relative_path.
        tx.execute_batch(
            "INSERT INTO search_index
                 (rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
             SELECT srm.rowid,
                    srm.relative_path,
                    srm.relative_path,
                    COALESCE(m.mime_type, ''),
                    COALESCE(m.camera_model, ''),
                    COALESCE(m.captured_at, ''),
                    COALESCE((
                        SELECT GROUP_CONCAT(t.name, ' ')
                        FROM file_tags ft
                        JOIN tags t ON t.id = ft.tag_id
                        WHERE ft.blake3_hash = srm.blake3_hash
                          AND ft.deleted_at IS NULL
                    ), '')
             FROM search_rowid_map srm
             LEFT JOIN file_metadata m ON m.blake3_hash = srm.blake3_hash;",
        )
        .map_err(Error::from)?;

        tx.commit().map_err(Error::from)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use perima_core::{DeviceId, TagRepository};

    use crate::tag_repo::SqliteTagRepository;

    fn test_db() -> (tempfile::TempDir, SqliteSearchRepository) {
        let td = tempfile::tempdir().expect("tempdir");
        let conn = crate::connection::open_and_migrate(&td.path().join("test.db")).expect("open");
        (td, SqliteSearchRepository::new(conn))
    }

    fn test_db_with_tag_repo() -> (
        tempfile::TempDir,
        SqliteSearchRepository,
        SqliteTagRepository,
    ) {
        let td = tempfile::tempdir().expect("tempdir");
        let db = td.path().join("test.db");
        let conn1 = crate::connection::open_and_migrate(&db).expect("open search");
        let conn2 = crate::connection::open_and_migrate(&db).expect("open tag");
        (
            td,
            SqliteSearchRepository::new(conn1),
            SqliteTagRepository::new(conn2),
        )
    }

    fn device() -> DeviceId {
        DeviceId::new()
    }

    /// Insert a `files` row + a `file_locations` row into the DB.
    fn insert_file(conn: &Connection, hash: &str, volume: &str, path: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO files
                 (blake3_hash, file_size, first_seen, updated_at, device_id)
             VALUES (?1, 1024, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'dev')",
            rusqlite::params![hash],
        )
        .expect("insert file");
        conn.execute(
            "INSERT OR IGNORE INTO file_locations
                 (id, blake3_hash, volume_id, relative_path, status,
                  first_seen, updated_at, device_id)
             VALUES (?1, ?2, ?3, ?4, 'active',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'dev')",
            rusqlite::params![uuid::Uuid::now_v7().to_string(), hash, volume, path],
        )
        .expect("insert file_location");
    }

    /// Insert a minimal `file_metadata` row directly.
    fn insert_metadata(conn: &Connection, hash: &str, mime: &str, camera: &str, captured: &str) {
        conn.execute(
            "INSERT OR REPLACE INTO file_metadata
                 (blake3_hash, mime_type, camera_model, captured_at,
                  extracted_at, updated_at, device_id)
             VALUES (?1, ?2, ?3, ?4,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'dev')",
            rusqlite::params![hash, mime, camera, captured],
        )
        .expect("insert metadata");
    }

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const VOL: &str = "00000000-0000-0000-0000-000000000001";

    #[test]
    fn search_empty_index_returns_empty() {
        let (_td, repo) = test_db();
        let hits = repo.search("vacation", 50).expect("search");
        assert!(hits.is_empty());
    }

    #[test]
    fn search_finds_by_filename() {
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH_A, VOL, "photos/sunset.jpg");
            insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
        }
        repo.rebuild().expect("rebuild");
        let hits = repo.search("sunset", 50).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].blake3_hash, HASH_A);
    }

    #[test]
    fn search_finds_by_mime_type() {
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH_A, VOL, "doc.pdf");
            insert_metadata(&conn, HASH_A, "application/pdf", "", "");
        }
        repo.rebuild().expect("rebuild");
        // FTS5 exact phrase search.
        let hits = repo.search("\"application/pdf\"", 50).expect("search");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn search_finds_by_camera_model() {
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH_A, VOL, "img.jpg");
            insert_metadata(&conn, HASH_A, "image/jpeg", "Canon EOS R5", "");
        }
        repo.rebuild().expect("rebuild");
        let hits = repo.search("Canon", 50).expect("search");
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn search_finds_by_tag() {
        let (_td, repo, tag_repo) = test_db_with_tag_repo();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH_A, VOL, "beach.jpg");
            insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
        }
        let tag = tag_repo
            .upsert_tag("beachlife", device())
            .expect("upsert tag");
        let hash = perima_core::BlakeHash::parse_hex(HASH_A).expect("hash");
        tag_repo.attach(&hash, tag.id, device()).expect("attach");
        repo.rebuild().expect("rebuild");
        let hits = repo.search("beachlife", 50).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].blake3_hash, HASH_A);
    }

    #[test]
    fn rebuild_is_idempotent() {
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH_A, VOL, "a.jpg");
            insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
        }
        repo.rebuild().expect("rebuild 1");
        repo.rebuild().expect("rebuild 2");
        let hits = repo.search("a", 50).expect("search");
        // "a.jpg" filename contains "a".
        assert!(!hits.is_empty());
        // Exactly one doc (idempotent — no duplicates from double rebuild).
        let count: i64 = {
            let conn = repo.conn.lock().expect("lock");
            conn.query_row("SELECT COUNT(*) FROM search_rowid_map", [], |r| r.get(0))
                .expect("count")
        };
        assert_eq!(count, 1);
    }

    #[test]
    fn trigger_sync_on_metadata_insert() {
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH_A, VOL, "photos/trigger_test.jpg");
            // Inserting metadata fires search_after_metadata_insert trigger.
            insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
        }
        // No explicit rebuild — trigger should have synced the index.
        let hits = repo.search("trigger_test", 50).expect("search");
        assert_eq!(hits.len(), 1, "trigger must sync on metadata insert");
    }

    #[test]
    fn trigger_sync_on_tag_attach() {
        let (_td, repo, tag_repo) = test_db_with_tag_repo();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH_A, VOL, "img.jpg");
            insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
        }
        // Wait for metadata insert trigger to run, then attach a tag.
        let tag = tag_repo.upsert_tag("triggertag", device()).expect("upsert");
        let hash = perima_core::BlakeHash::parse_hex(HASH_A).expect("hash");
        tag_repo.attach(&hash, tag.id, device()).expect("attach");
        // No rebuild — file_tags INSERT trigger should have updated the index.
        let hits = repo.search("triggertag", 50).expect("search");
        assert_eq!(hits.len(), 1, "trigger must sync on tag attach");
    }

    #[test]
    fn search_limit_is_respected() {
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            for i in 0..5u8 {
                let hash = format!("{:0<64}", format!("{i:x}"));
                insert_file(&conn, &hash, VOL, &format!("file{i}.jpg"));
                insert_metadata(&conn, &hash, "image/jpeg", "", "");
            }
        }
        repo.rebuild().expect("rebuild");
        // All 5 files have "jpeg" — limit 2 should return exactly 2.
        let hits = repo.search("jpeg", 2).expect("search");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn search_no_results_for_unknown_term() {
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH_A, VOL, "alpha.txt");
            insert_metadata(&conn, HASH_A, "text/plain", "", "");
        }
        repo.rebuild().expect("rebuild");
        let hits = repo
            .search("xyzzy_nonexistent_term_42", 50)
            .expect("search");
        assert!(hits.is_empty());
    }

    #[test]
    fn filename_without_slash_is_indexed_correctly() {
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            // Root-level file: no '/' in path.
            insert_file(&conn, HASH_A, VOL, "rootfile.jpg");
            insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
        }
        repo.rebuild().expect("rebuild");
        let hits = repo.search("rootfile", 50).expect("search");
        assert_eq!(hits.len(), 1);
    }
}
