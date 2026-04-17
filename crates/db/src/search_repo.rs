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
    /// through V007 before constructing this.
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

        // V007: search_content has (rowid, blake3_hash, relative_path, ...),
        // but SearchHit requires volume_id which lives only on file_locations.
        // WHY the LEFT JOIN + representative subquery: pick the first-seen
        // active file_locations row per hash to populate volume_id. The
        // subquery ordering (first_seen ASC, id ASC) mirrors the trigger
        // representative-selection rule, so the volume_id returned here
        // agrees with the path indexed in search_content.
        let mut stmt = conn
            .prepare(
                "SELECT sc.blake3_hash,
                        COALESCE((
                            SELECT fl.volume_id FROM file_locations fl
                            WHERE fl.blake3_hash = sc.blake3_hash
                              AND fl.deleted_at IS NULL
                            ORDER BY fl.first_seen ASC, fl.id ASC
                            LIMIT 1
                        ), ''),
                        sc.relative_path,
                        search_index.rank
                 FROM search_index
                 JOIN search_content sc ON sc.rowid = search_index.rowid
                 WHERE search_index MATCH ?1
                 ORDER BY search_index.rank
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

        // V007 rebuild: wipe search_content, then repopulate from joined
        // live state. The search_content AFTER-INSERT/DELETE triggers keep
        // search_index in sync row-by-row — no explicit FTS5 'rebuild'
        // needed here.
        //
        // WHY not 'INSERT INTO search_index(search_index) VALUES('rebuild')':
        // that primitive is an external-content resync from search_content,
        // but the DELETE + INSERT path above already drives FTS via triggers.
        // Calling 'rebuild' would be redundant (and defensive for a case
        // that doesn't exist here: search_content-out-of-sync-with-index).
        tx.execute_batch("DELETE FROM search_content;")
            .map_err(Error::from)?;

        // Populate search_content: one representative location per hash,
        // joined with metadata + tags. Mirrors V007 migration bulk-insert.
        // WHY filename = relative_path: SQLite has no built-in REVERSE() for
        // basename extraction; the unicode61 tokenizer splits on '/' and '.'
        // so basenames are discoverable via token match on relative_path.
        tx.execute_batch(
            "INSERT INTO search_content
                 (blake3_hash, filename, relative_path, mime_type, camera_model, captured_at, tags)
             SELECT fl.blake3_hash,
                    fl.relative_path,
                    fl.relative_path,
                    COALESCE(m.mime_type, ''),
                    COALESCE(m.camera_model, ''),
                    COALESCE(m.captured_at, ''),
                    COALESCE((
                        SELECT GROUP_CONCAT(t.name, ' ')
                        FROM file_tags ft
                        JOIN tags t ON t.id = ft.tag_id
                        WHERE ft.blake3_hash = fl.blake3_hash
                          AND ft.deleted_at IS NULL
                    ), '')
             FROM file_locations fl
             LEFT JOIN file_metadata m ON m.blake3_hash = fl.blake3_hash
             WHERE fl.deleted_at IS NULL
               AND fl.id = (
                   SELECT id FROM file_locations
                   WHERE blake3_hash = fl.blake3_hash AND deleted_at IS NULL
                   ORDER BY first_seen ASC, id ASC LIMIT 1
               );",
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
            conn.query_row("SELECT COUNT(*) FROM search_content", [], |r| r.get(0))
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
    fn search_rank_orders_better_match_first() {
        // WHY: plan Task 1 Step 2 required this test — the whole point of
        // FTS5 over LIKE is BM25 ranking. Two files both contain "vacation"
        // in their filename; only one also has the matching TAG attached.
        // BM25 weights multi-field matches higher, so the tagged hit must
        // rank before the filename-only hit. In FTS5 lower rank = better
        // match (SQLite convention; default `rank` returns negative BM25
        // score, smaller = better).
        const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let (_td, repo, tag_repo) = test_db_with_tag_repo();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH_A, VOL, "vacation_tagged.jpg");
            insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
            insert_file(&conn, HASH_B, VOL, "vacation_only.jpg");
            insert_metadata(&conn, HASH_B, "image/jpeg", "", "");
        }
        // Attach the matching tag only to HASH_A so the BM25 signal is
        // stronger for that row.
        let tag = tag_repo.upsert_tag("vacation", device()).expect("upsert");
        let hash_a = perima_core::BlakeHash::parse_hex(HASH_A).expect("hash A");
        tag_repo.attach(&hash_a, tag.id, device()).expect("attach");

        repo.rebuild().expect("rebuild");
        let hits = repo.search("vacation", 50).expect("search");
        assert_eq!(hits.len(), 2, "both files should hit on 'vacation'");
        assert_eq!(
            hits[0].blake3_hash,
            HASH_A,
            "tagged file must rank above filename-only file (got order: {:?})",
            hits.iter().map(|h| &h.blake3_hash).collect::<Vec<_>>()
        );
        assert!(
            hits[0].rank <= hits[1].rank,
            "FTS5 BM25 rank must be non-increasing (lower = better); \
             got [0]={}, [1]={}",
            hits[0].rank,
            hits[1].rank
        );
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

    // ── Scaffold helpers for v0.6.3 regression tests ────────────────────────
    // WHY bundled: fewer than 30 lines total; no standalone commit needed.

    /// UPDATE a `file_locations` row's `relative_path` (simulates rename).
    fn update_path(conn: &Connection, hash: &str, old_path: &str, new_path: &str) {
        conn.execute(
            "UPDATE file_locations SET relative_path = ?1
             WHERE blake3_hash = ?2 AND relative_path = ?3",
            rusqlite::params![new_path, hash, old_path],
        )
        .expect("update_path");
    }

    /// Attach a tag by name to a hash using raw SQL (bypasses `tag_repo` for
    /// metadata-less-file tests where only one connection is available).
    fn attach_tag_raw(conn: &Connection, hash: &str, tag_name: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO tags (id, name, first_seen, updated_at, device_id)
             VALUES (?1, ?2, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'dev')",
            rusqlite::params![uuid::Uuid::now_v7().to_string(), tag_name],
        )
        .expect("insert tag");
        let tag_id: String = conn
            .query_row(
                "SELECT id FROM tags WHERE name = ?1",
                rusqlite::params![tag_name],
                |r| r.get(0),
            )
            .expect("get tag id");
        conn.execute(
            "INSERT OR IGNORE INTO file_tags
                 (id, blake3_hash, tag_id, first_seen, updated_at, device_id)
             VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'dev')",
            rusqlite::params![uuid::Uuid::now_v7().to_string(), hash, tag_id],
        )
        .expect("insert file_tag");
    }

    // ── v0.6.3 RED regression tests (bugs in V006) ──────────────────────────

    /// T40: contentless FTS5 'delete' with blank payloads is a no-op.
    /// After updating `camera_model` the old token ("Canon") must not match.
    /// Fails on V006 because `search_after_metadata_update` supplies ''
    /// for every column on the 'delete' command — stale tokens remain.
    #[test]
    #[allow(non_snake_case)]
    fn test_T40_metadata_update_removes_stale_tokens() {
        const HASH: &str = "aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000";
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH, VOL, "cam.jpg");
            insert_metadata(&conn, HASH, "image/jpeg", "Canon EOS R5", "");
        }
        // Trigger: UPDATE file_metadata fires search_after_metadata_update.
        {
            let conn = repo.conn.lock().expect("lock");
            conn.execute(
                "UPDATE file_metadata SET camera_model = ?1 WHERE blake3_hash = ?2",
                rusqlite::params!["Nikon Zf", HASH],
            )
            .expect("update metadata");
        }
        // V006 bug: stale token 'Canon' still matches.
        let hits = repo.search("Canon", 50).expect("search");
        assert!(
            hits.is_empty(),
            "#40: stale token 'Canon' still matches after metadata update (V006 bug)"
        );
    }

    /// T41: under V006, `search_rowid_map` was only seeded on `file_metadata`
    /// INSERT, so attaching a tag to a metadata-less file was a silent no-op.
    /// V007 trigger 4a seeds `search_content` from `file_locations` directly.
    #[test]
    #[allow(non_snake_case)]
    fn test_T41_tag_attach_on_metadata_less_file() {
        const HASH: &str = "bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000bbbb0000";
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH, VOL, "plain.txt"); // NO metadata row
            attach_tag_raw(&conn, HASH, "beach");
        }
        // V006 bug: file_tags INSERT trigger found no search_rowid_map row;
        // the tag was never indexed. V007 trigger 4a fixes this by seeding
        // search_content from file_locations directly on tag attach.
        let hits = repo.search("beach", 50).expect("search");
        assert_eq!(
            hits.len(),
            1,
            "#41: tag-attach on metadata-less file was a no-op (V006 bug)"
        );
    }

    /// T22: no `file_locations` UPDATE trigger in V006 — rename leaves old
    /// path indexed and new path absent.
    #[test]
    #[allow(non_snake_case)]
    fn test_T22_rename_updates_indexed_path() {
        const HASH: &str = "cccc0000cccc0000cccc0000cccc0000cccc0000cccc0000cccc0000cccc0000";
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH, VOL, "A.jpg");
            insert_metadata(&conn, HASH, "image/jpeg", "", "");
        }
        // Rename: same hash, new path. V006 has no UPDATE trigger on
        // file_locations, so FTS index is not updated.
        {
            let conn = repo.conn.lock().expect("lock");
            update_path(&conn, HASH, "A.jpg", "B.jpg");
        }
        let old_hits = repo.search("A", 50).expect("search old");
        let new_hits = repo.search("B", 50).expect("search new");
        // V006 bug: old path 'A' still matches; new path 'B' does not.
        assert!(
            old_hits.is_empty(),
            "#22: old path 'A' still matches after rename (V006 bug)"
        );
        assert_eq!(
            new_hits.len(),
            1,
            "#22: new path 'B' does not match after rename (V006 bug)"
        );
    }

    /// T42: no blake3_hash-change trigger in V006 — replace-in-place
    /// leaves stale FTS doc for the old hash's content.
    #[test]
    #[allow(non_snake_case)]
    fn test_T42_hash_change_retires_old_doc() {
        const HASH_OLD: &str = "dddd0000dddd0000dddd0000dddd0000dddd0000dddd0000dddd0000dddd0000";
        const HASH_NEW: &str = "eeee0000eeee0000eeee0000eeee0000eeee0000eeee0000eeee0000eeee0000";
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH_OLD, VOL, "cam.jpg");
            insert_metadata(&conn, HASH_OLD, "image/jpeg", "Canon EOS R5", "");
        }
        // Replace hash in-place (file content changed at same path).
        // V006 has no trigger on file_locations.blake3_hash change.
        {
            let conn = repo.conn.lock().expect("lock");
            conn.execute(
                "INSERT OR IGNORE INTO files
                     (blake3_hash, file_size, first_seen, updated_at, device_id)
                 VALUES (?1, 2048, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'dev')",
                rusqlite::params![HASH_NEW],
            )
            .expect("insert new files row");
            conn.execute(
                "UPDATE file_locations SET blake3_hash = ?1 WHERE relative_path = 'cam.jpg'",
                rusqlite::params![HASH_NEW],
            )
            .expect("update hash");
            insert_metadata(&conn, HASH_NEW, "image/jpeg", "Nikon Zf", "");
            drop(conn);
        }
        // V006 bug: old FTS doc not retired — "Canon" still matches.
        let hits = repo.search("Canon", 50).expect("search");
        assert!(
            hits.is_empty(),
            "#42: old doc not retired when hash changed at same path (V006 bug)"
        );
    }
}
