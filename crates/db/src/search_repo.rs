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

    const DEV: &str = "dev";
    const TS: &str = "2026-01-01T00:00:00Z";

    /// Produce a deterministic 64-hex-char hash from a small integer.
    fn hash_n(n: u8) -> String {
        // WHY format: first two chars encode `n`; remaining 62 are '0'.
        // Gives 256 distinct valid-length hashes without hand-writing literals.
        format!("{:02x}{}", n, "0".repeat(62))
    }

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
             VALUES (?1, 1024, ?2, ?2, ?3)",
            rusqlite::params![hash, TS, DEV],
        )
        .expect("insert file");
        conn.execute(
            "INSERT OR IGNORE INTO file_locations
                 (id, blake3_hash, volume_id, relative_path, status,
                  first_seen, updated_at, device_id)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, ?6)",
            rusqlite::params![
                uuid::Uuid::now_v7().to_string(),
                hash,
                volume,
                path,
                TS,
                DEV
            ],
        )
        .expect("insert file_location");
    }

    /// Insert a minimal `file_metadata` row directly.
    fn insert_metadata(conn: &Connection, hash: &str, mime: &str, camera: &str, captured: &str) {
        conn.execute(
            "INSERT OR REPLACE INTO file_metadata
                 (blake3_hash, mime_type, camera_model, captured_at,
                  extracted_at, updated_at, device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
            rusqlite::params![hash, mime, camera, captured, TS, DEV],
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
        // Metadata insert has already fired `search_after_metadata_insert`;
        // now attach a tag to fire `search_after_file_tags_insert`.
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
             VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![uuid::Uuid::now_v7().to_string(), tag_name, TS, DEV],
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
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            rusqlite::params![uuid::Uuid::now_v7().to_string(), hash, tag_id, TS, DEV],
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
        let hash_owned = hash_n(1);
        let HASH = hash_owned.as_str();
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
        let hash_owned = hash_n(2);
        let HASH = hash_owned.as_str();
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
        let hash_owned = hash_n(3);
        let HASH = hash_owned.as_str();
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, HASH, VOL, "oldname_22.jpg");
            insert_metadata(&conn, HASH, "image/jpeg", "", "");
        }
        // Rename: same hash, new path. V006 has no UPDATE trigger on
        // file_locations, so FTS index is not updated.
        {
            let conn = repo.conn.lock().expect("lock");
            update_path(&conn, HASH, "oldname_22.jpg", "newname_22.jpg");
        }
        let old_hits = repo.search("oldname_22", 50).expect("search old");
        let new_hits = repo.search("newname_22", 50).expect("search new");
        // V006 bug: old path 'oldname_22' still matches; new path 'newname_22' does not.
        assert!(
            old_hits.is_empty(),
            "#22: old path 'oldname_22' still matches after rename (V006 bug)"
        );
        assert_eq!(
            new_hits.len(),
            1,
            "#22: new path 'newname_22' does not match after rename (V006 bug)"
        );
    }

    /// T42: no blake3_hash-change trigger in V006 — replace-in-place
    /// leaves stale FTS doc for the old hash's content.
    #[test]
    #[allow(non_snake_case)]
    fn test_T42_hash_change_retires_old_doc() {
        let hash_old_owned = hash_n(4);
        let hash_new_owned = hash_n(5);
        let HASH_OLD = hash_old_owned.as_str();
        let HASH_NEW = hash_new_owned.as_str();
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
                 VALUES (?1, 2048, ?2, ?2, ?3)",
                rusqlite::params![HASH_NEW, TS, DEV],
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

    // ── Task 4: regression-pin tests (post-V007 behaviour) ──────────────────
    // WHY regression-pin: all six pass against V007 with no impl changes.
    // They lock multi-surface invariants that no single-bug regression test
    // covers. Labelled regression-pin (not TDD red→green) per the plan's
    // bundling justification.

    const VOL2: &str = "00000000-0000-0000-0000-000000000002";

    /// Soft-delete a `file_locations` row by setting its `deleted_at` column.
    ///
    /// WHY helper: three tests share the same two-step pattern of updating
    /// `deleted_at` on a specific `(hash, relative_path)` pair.
    fn soft_delete_location(conn: &Connection, hash: &str, path: &str) {
        conn.execute(
            "UPDATE file_locations SET deleted_at = ?1
             WHERE blake3_hash = ?2 AND relative_path = ?3",
            rusqlite::params![TS, hash, path],
        )
        .expect("soft_delete_location");
    }

    /// Insert a secondary `file_locations` row on a specific volume for an
    /// existing `files` hash. Unlike [`insert_file`] this does NOT INSERT into
    /// `files` — caller has already seeded that row via `insert_file` for the
    /// representative location.
    ///
    /// WHY helper: the I4 test needs two locations for the same hash on
    /// different volumes; `insert_file` alone insists on `INSERT OR IGNORE`
    /// into `files` which is fine, but the location insert benefits from an
    /// explicit volume-aware helper.
    fn insert_file_at_volume(conn: &Connection, hash: &str, path: &str, volume: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO files
                 (blake3_hash, file_size, first_seen, updated_at, device_id)
             VALUES (?1, 1024, ?2, ?2, ?3)",
            rusqlite::params![hash, TS, DEV],
        )
        .expect("insert files (secondary location)");
        conn.execute(
            "INSERT OR IGNORE INTO file_locations
                 (id, blake3_hash, volume_id, relative_path, status,
                  first_seen, updated_at, device_id)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, ?6)",
            rusqlite::params![
                uuid::Uuid::now_v7().to_string(),
                hash,
                volume,
                path,
                TS,
                DEV
            ],
        )
        .expect("insert secondary file_location");
    }

    /// Volume-scoped rename helper: UPDATE `file_locations.relative_path` for
    /// a specific `(hash, volume_id, old_path)` triple. Needed when two
    /// locations share the same `relative_path` on different volumes and the
    /// caller wants to rename exactly one.
    fn update_path_at_volume(
        conn: &Connection,
        hash: &str,
        old_path: &str,
        new_path: &str,
        volume: &str,
    ) {
        conn.execute(
            "UPDATE file_locations SET relative_path = ?1
             WHERE blake3_hash = ?2 AND relative_path = ?3 AND volume_id = ?4",
            rusqlite::params![new_path, hash, old_path, volume],
        )
        .expect("update_path_at_volume");
    }

    /// Hit-count wrapper over `SearchRepository::search(q, 50)`.
    fn search_count(repo: &SqliteSearchRepository, q: &str) -> usize {
        repo.search(q, 50).expect("search_count").len()
    }

    /// I4: "multi-location rename preserves findability."
    ///
    /// Per the v0.6.3 spec §Non-goals: the representative FTS doc is
    /// one-per-hash, indexed under the first-seen active location. This test
    /// verifies that the co-existence of multiple locations does NOT break
    /// the rename trigger — the file remains findable via its current
    /// representative-path tokens across both a non-representative rename
    /// (no-op on FTS) and a representative rename (updates FTS).
    ///
    /// WHY not "secondary location's path is separately searchable": the spec
    /// explicitly scopes that as out-of-scope multi-volume awareness; the
    /// representative is authoritative.
    #[test]
    fn test_multi_location_rename_preserves_findability() {
        let hash = hash_n(10);
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            // Representative (first-seen) location on VOL.
            insert_file(&conn, &hash, VOL, "shared_mlr.jpg");
            // Second location on VOL2, same relative_path.
            insert_file_at_volume(&conn, &hash, "shared_mlr.jpg", VOL2);
        }

        // Rename the non-representative (VOL2) location. Trigger 2b is gated
        // on NEW being the representative, so the rename should NOT affect
        // search_content; the representative's "shared_mlr" token still matches.
        {
            let conn = repo.conn.lock().expect("lock");
            update_path_at_volume(&conn, &hash, "shared_mlr.jpg", "renamed_mlr.jpg", VOL2);
        }
        assert_eq!(
            search_count(&repo, "shared_mlr"),
            1,
            "non-rep rename must not affect FTS — representative path still matches"
        );

        // Rename the representative (VOL) location. Trigger 2b fires and
        // updates search_content.relative_path + filename.
        {
            let conn = repo.conn.lock().expect("lock");
            update_path_at_volume(&conn, &hash, "shared_mlr.jpg", "alpha_mlr.jpg", VOL);
        }
        assert_eq!(
            search_count(&repo, "shared_mlr"),
            0,
            "rep rename retires old path token from FTS"
        );
        assert_eq!(
            search_count(&repo, "alpha_mlr"),
            1,
            "rep rename indexes new path token in FTS"
        );
    }

    /// C1: soft-deleting the representative location of a two-location file
    /// must re-point `search_content` to the surviving sibling, not retire the doc.
    ///
    /// After the delete:
    /// - search("vol1") → 0 (representative was on vol1; path retired)
    /// - search("vol2") → 1 (surviving sibling on vol2 is now indexed)
    #[test]
    fn test_representative_location_soft_delete_repoints() {
        let hash = hash_n(11);
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            // First (representative) location on VOL / vol1 path.
            insert_file(&conn, &hash, VOL, "vol1/repfile_c1.jpg");
            // Second location on VOL2 / vol2 path — same hash.
            conn.execute(
                "INSERT OR IGNORE INTO files
                     (blake3_hash, file_size, first_seen, updated_at, device_id)
                 VALUES (?1, 1024, ?2, ?2, ?3)",
                rusqlite::params![hash, TS, DEV],
            )
            .expect("insert files");
            conn.execute(
                "INSERT OR IGNORE INTO file_locations
                     (id, blake3_hash, volume_id, relative_path, status,
                      first_seen, updated_at, device_id)
                 VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, ?6)",
                rusqlite::params![
                    uuid::Uuid::now_v7().to_string(),
                    hash,
                    VOL2,
                    "vol2/repfile_c1.jpg",
                    TS,
                    DEV
                ],
            )
            .expect("insert second location");
        }
        // Soft-delete the first (representative) location.
        {
            let conn = repo.conn.lock().expect("lock");
            soft_delete_location(&conn, &hash, "vol1/repfile_c1.jpg");
        }
        // vol1 token must no longer match (representative retired from index).
        let vol1_hits = repo.search("vol1", 50).expect("search vol1");
        assert_eq!(
            vol1_hits.len(),
            0,
            "C1: search on deleted representative's path must return zero"
        );
        // vol2 token must still match (search_content re-pointed to sibling).
        let vol2_hits = repo.search("vol2", 50).expect("search vol2");
        assert_eq!(
            vol2_hits.len(),
            1,
            "C1: sibling location must be discoverable after representative soft-delete"
        );
        assert_eq!(vol2_hits[0].blake3_hash, hash);
    }

    /// Soft-deleting the *only* location of a file must retire both the
    /// `search_content` row and the FTS doc.
    ///
    /// After the delete:
    /// - `search_content` row count for this hash → 0
    /// - `search_index` query for any term → 0 results for this hash
    #[test]
    fn test_last_location_soft_delete_retires_doc() {
        let hash = hash_n(12);
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, &hash, VOL, "solo_retire_lsd.jpg");
            insert_metadata(&conn, &hash, "image/jpeg", "RetireCamera", "");
        }
        // Trigger must have indexed via metadata insert; verify first.
        let pre = repo.search("RetireCamera", 50).expect("pre-search");
        assert_eq!(pre.len(), 1, "file must be indexed before soft-delete");

        // Soft-delete the only location.
        {
            let conn = repo.conn.lock().expect("lock");
            soft_delete_location(&conn, &hash, "solo_retire_lsd.jpg");
        }

        // FTS search must return empty.
        let hits = repo.search("RetireCamera", 50).expect("post-search");
        assert!(
            hits.is_empty(),
            "last-location soft-delete must remove the file from FTS search"
        );

        // search_content row must be gone.
        let sc_count: i64 = {
            let conn = repo.conn.lock().expect("lock");
            conn.query_row(
                "SELECT COUNT(*) FROM search_content WHERE blake3_hash = ?1",
                rusqlite::params![hash],
                |r| r.get(0),
            )
            .expect("count search_content")
        };
        assert_eq!(
            sc_count, 0,
            "search_content row must be deleted after last-location soft-delete"
        );
    }

    /// I5: calling `SearchRepository::rebuild()` twice produces an identical
    /// result set; no row-count drift in `search_content`.
    ///
    /// Stricter than the earlier `rebuild_is_idempotent` test: asserts the
    /// exact `search_content` row count is stable across two rebuilds, not
    /// just that searches still return results.
    #[test]
    fn test_rebuild_idempotence_post_v007() {
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            for i in 20u8..23u8 {
                let h = hash_n(i);
                insert_file(&conn, &h, VOL, &format!("idempotent_{i}.jpg"));
                insert_metadata(&conn, &h, "image/jpeg", "", "");
            }
        }
        repo.rebuild().expect("rebuild 1");
        let count_after_first: i64 = {
            let conn = repo.conn.lock().expect("lock");
            conn.query_row("SELECT COUNT(*) FROM search_content", [], |r| r.get(0))
                .expect("count after first rebuild")
        };

        repo.rebuild().expect("rebuild 2");
        let count_after_second: i64 = {
            let conn = repo.conn.lock().expect("lock");
            conn.query_row("SELECT COUNT(*) FROM search_content", [], |r| r.get(0))
                .expect("count after second rebuild")
        };

        assert_eq!(
            count_after_first, count_after_second,
            "I5: search_content row count must be stable across two rebuilds (no drift)"
        );
        assert_eq!(
            count_after_first, 3,
            "I5: exactly 3 rows expected (one per file)"
        );

        // FTS search results must also be identical in content.
        let hits_first = repo
            .search("idempotent", 50)
            .expect("search after rebuild 2");
        assert_eq!(
            hits_first.len(),
            3,
            "I5: all 3 files must be discoverable after double rebuild"
        );
    }

    /// I6: a single `BEGIN…COMMIT` updating `file_metadata.camera_model` +
    /// attaching a new tag + renaming `file_locations.relative_path` must
    /// produce FTS docs that reflect ALL three changes after commit.
    ///
    /// WHY fire-order independence: the three business triggers (2b, 3b, 4a)
    /// all update `search_content` from joined live state, so regardless of
    /// `SQLite`'s trigger-fire order the final `search_content` row converges.
    #[test]
    fn test_combined_transaction_update() {
        let hash = hash_n(13);
        let (_td, repo) = test_db();
        {
            let conn = repo.conn.lock().expect("lock");
            insert_file(&conn, &hash, VOL, "combined_old_ctx.jpg");
            insert_metadata(&conn, &hash, "image/jpeg", "OldCamera", "");
        }

        // Pre-condition: old tokens indexed.
        let pre = repo.search("OldCamera", 50).expect("pre OldCamera");
        assert_eq!(pre.len(), 1, "pre-condition: OldCamera must be indexed");

        // Execute all three mutations in one transaction.
        {
            let conn = repo.conn.lock().expect("lock");
            conn.execute_batch("BEGIN;").expect("begin");
            // 1. Rename the location (trigger 2b if it is the representative).
            conn.execute(
                "UPDATE file_locations SET relative_path = 'combined_new_ctx.jpg'
                 WHERE blake3_hash = ?1 AND relative_path = 'combined_old_ctx.jpg'",
                rusqlite::params![hash],
            )
            .expect("rename");
            // 2. Update camera_model (trigger 3b).
            conn.execute(
                "UPDATE file_metadata SET camera_model = 'NewCamera'
                 WHERE blake3_hash = ?1",
                rusqlite::params![hash],
            )
            .expect("update metadata");
            // 3. Attach a new tag (trigger 4a).
            attach_tag_raw(&conn, &hash, "combined_tag_ctx");
            conn.execute_batch("COMMIT;").expect("commit");
        }

        // FTS must reflect NEW camera_model.
        let new_cam = repo.search("NewCamera", 50).expect("NewCamera");
        assert_eq!(
            new_cam.len(),
            1,
            "I6: NewCamera must be indexed post-commit"
        );

        // FTS must no longer contain OLD camera_model.
        let old_cam = repo.search("OldCamera", 50).expect("OldCamera after");
        assert!(
            old_cam.is_empty(),
            "I6: OldCamera must not appear after metadata update in combined tx"
        );

        // FTS must reflect the new tag.
        let tag_hits = repo.search("combined_tag_ctx", 50).expect("tag");
        assert_eq!(tag_hits.len(), 1, "I6: new tag must be indexed post-commit");

        // FTS must reflect the new path token.
        let new_path = repo.search("combined_new_ctx", 50).expect("new path");
        assert_eq!(
            new_path.len(),
            1,
            "I6: new relative_path token must be indexed post-commit"
        );

        // FTS must no longer match the old path token.
        let old_path = repo.search("combined_old_ctx", 50).expect("old path");
        assert!(
            old_path.is_empty(),
            "I6: old relative_path token must not appear after rename in combined tx"
        );
    }

    // ── Task 5: proptest — FTS5 invariant across tag churn ──────────────────

    /// Soft-delete a `file_tags` row by setting `deleted_at` (tag detach).
    ///
    /// WHY raw SQL: proptest body has only a single `Connection` from the
    /// `SqliteSearchRepository`'s mutex; using `SqliteTagRepository` would
    /// require a second open connection and a `TempDir` with WAL on, which
    /// adds noise without adding coverage. The trigger fires on the SQL UPDATE
    /// regardless of which layer issues it.
    fn detach_tag_raw(conn: &Connection, hash: &str, tag_name: &str) {
        conn.execute(
            "UPDATE file_tags SET deleted_at = ?1, updated_at = ?1, device_id = ?2
             WHERE blake3_hash = ?3
               AND tag_id = (SELECT id FROM tags WHERE name = ?4 AND deleted_at IS NULL)
               AND deleted_at IS NULL",
            rusqlite::params![TS, DEV, hash, tag_name],
        )
        .expect("detach_tag_raw");
    }

    /// Trigger 5: renaming a tag must update every `search_content` row that
    /// references it. After `UPDATE tags SET name = 'holiday'` for the tag
    /// previously named "vacation":
    /// - search("vacation") → 0
    /// - search("holiday")  → 3 (all files that had the tag attached)
    #[test]
    fn test_tag_name_rename_propagates() {
        let (_td, repo) = test_db();
        let hashes: Vec<String> = (30u8..33u8).map(hash_n).collect();
        {
            let conn = repo.conn.lock().expect("lock");
            for (i, h) in hashes.iter().enumerate() {
                insert_file(&conn, h, VOL, &format!("trnp_{i}.jpg"));
                // Attach tag "vacation" to all three files.
                attach_tag_raw(&conn, h, "vacation");
            }
        }

        // Pre-condition: all 3 files discoverable under "vacation".
        let pre = repo.search("vacation", 50).expect("pre-vacation");
        assert_eq!(
            pre.len(),
            3,
            "pre-condition: all 3 files must be indexed under 'vacation'"
        );

        // Rename the tag — trigger 5 must update all three search_content rows.
        {
            let conn = repo.conn.lock().expect("lock");
            conn.execute(
                "UPDATE tags SET name = 'holiday' WHERE name = 'vacation'",
                [],
            )
            .expect("rename tag");
        }

        // Old name must yield zero results.
        let old_hits = repo.search("vacation", 50).expect("vacation after rename");
        assert_eq!(
            old_hits.len(),
            0,
            "trigger 5: 'vacation' must return zero after tag rename"
        );

        // New name must match all three files.
        let new_hits = repo.search("holiday", 50).expect("holiday");
        assert_eq!(
            new_hits.len(),
            3,
            "trigger 5: 'holiday' must match all 3 files after tag rename"
        );
    }

    // ── Task 5: proptest — FTS5 trigger invariant across random tag churn ───

    /// Operations exercised by the property: Attach or Detach a (file, tag) pair.
    #[derive(Debug, Clone)]
    enum TagOp {
        Attach(usize, usize),
        Detach(usize, usize),
    }

    /// Small universe: 3 files × 3 tags — large enough to generate
    /// interesting interactions, small enough that the invariant check
    /// (O(files × tags) FTS queries) completes inside the default proptest
    /// timeout.
    const PROP_FILES: &[&str] = &[
        "7100000000000000000000000000000000000000000000000000000000000000",
        "7200000000000000000000000000000000000000000000000000000000000000",
        "7300000000000000000000000000000000000000000000000000000000000000",
    ];
    const PROP_TAGS: &[&str] = &["alpha", "beta", "gamma"];
    const PROP_VOL: &str = "00000000-0000-0000-0000-000000000099";

    proptest::proptest! {
        /// **Invariant:** after every Attach / Detach operation, for every
        /// `(file, tag)` pair, `MATCH tag_name` returns the file iff
        /// `file_tags.deleted_at IS NULL` for that pair.
        ///
        /// WHY random sequences: fixed-input tests (Task 4) cover specific
        /// triggers; randomised churn covers interaction effects — e.g.
        /// double-attach, detach-never-attached, attach-after-detach-after-attach.
        #[test]
        fn fts_consistent_under_tag_churn(
            ops in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::strategy::Strategy::prop_map(
                        (0..PROP_FILES.len(), 0..PROP_TAGS.len()),
                        |(f, t)| TagOp::Attach(f, t),
                    ),
                    proptest::strategy::Strategy::prop_map(
                        (0..PROP_FILES.len(), 0..PROP_TAGS.len()),
                        |(f, t)| TagOp::Detach(f, t),
                    ),
                ],
                0..30,
            ),
        ) {
            // Fresh DB per proptest case — each case is independent.
            let (_td, repo) = test_db();

            // Seed all three files (no metadata needed; trigger 4a covers
            // metadata-less files, which T41 already pins as a fixed test).
            {
                let conn = repo.conn.lock().expect("lock");
                for (i, hash) in PROP_FILES.iter().enumerate() {
                    insert_file(
                        &conn,
                        hash,
                        PROP_VOL,
                        &format!("prop_file_{i}.jpg"),
                    );
                }
            }

            // Shadow state: (file_idx, tag_idx) → currently attached?
            let mut attached: std::collections::HashMap<(usize, usize), bool> =
                std::collections::HashMap::new();

            for op in &ops {
                {
                    let conn = repo.conn.lock().expect("lock");
                    match *op {
                        TagOp::Attach(f, t) => {
                            // attach_tag_raw is idempotent (INSERT OR IGNORE +
                            // existing file_tags rows with deleted_at non-NULL
                            // are ignored); re-attaching after a detach creates
                            // a fresh row, so we unconditionally set attached=true.
                            attach_tag_raw(&conn, PROP_FILES[f], PROP_TAGS[t]);
                            attached.insert((f, t), true);
                        }
                        TagOp::Detach(f, t) => {
                            // Only detach when the shadow state says the pair is
                            // active; otherwise the UPDATE is a harmless no-op
                            // and the shadow stays false.
                            if *attached.get(&(f, t)).unwrap_or(&false) {
                                detach_tag_raw(&conn, PROP_FILES[f], PROP_TAGS[t]);
                                attached.insert((f, t), false);
                            }
                        }
                    }
                } // mutex guard dropped before the invariant queries below.

                // Invariant check: for every (file, tag) pair the FTS result
                // must agree with the shadow state.
                for (f_idx, &file_hash) in PROP_FILES.iter().enumerate() {
                    for (t_idx, &tag_name) in PROP_TAGS.iter().enumerate() {
                        let is_attached =
                            *attached.get(&(f_idx, t_idx)).unwrap_or(&false);
                        let hits = repo
                            .search(tag_name, 50)
                            .expect("proptest search");
                        let found = hits
                            .iter()
                            .any(|h| h.blake3_hash == file_hash);
                        proptest::prop_assert_eq!(
                            found,
                            is_attached,
                            "FTS invariant violated: file={} tag={} \
                             attached={} found={}",
                            file_hash,
                            tag_name,
                            is_attached,
                            found
                        );
                    }
                }
            }
        }
    }
}
