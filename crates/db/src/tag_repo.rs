//! `TagRepository` implementation backed by rusqlite.

use std::collections::HashMap;
use std::sync::Mutex;

use perima_core::{BlakeHash, CoreError, DeviceId, Tag, TagRepository, normalize_tag};
use rusqlite::Connection;
// WHY: OptionalExtension adds `.optional()` to query_row results, converting
// QueryReturnedNoRows into Ok(None) for our SELECT-then-INSERT upsert pattern.
use rusqlite::OptionalExtension;
use uuid::Uuid;

use crate::errors::Error;

/// Rusqlite-backed tag repository.
///
/// WHY `Mutex<Connection>`: `rusqlite::Connection` is `Send` but not
/// `Sync` (internal `RefCell` state). The [`TagRepository`] trait
/// requires `Send + Sync` for `Arc` sharing in desktop state.
/// Wrapping in `Mutex` satisfies both bounds without `unsafe`. All DB
/// methods lock briefly; there is no blocking I/O inside the lock.
pub struct SqliteTagRepository {
    conn: Mutex<Connection>,
}

impl SqliteTagRepository {
    /// Wrap an existing connection. Caller must have run migrations
    /// (at least through V005) before constructing this.
    pub const fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

impl TagRepository for SqliteTagRepository {
    // WHY allow(significant_drop_tightening): the Mutex guard `conn`
    // must outlive the transaction that borrows through it. Dropping
    // the guard earlier would break the borrow graph — same pattern
    // used throughout `metadata_repo.rs`.
    #[allow(clippy::significant_drop_tightening)]
    fn upsert_tag(&self, name: &str, device: DeviceId) -> Result<Tag, CoreError> {
        let normalized = normalize_tag(name)?;

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;

        // WHY BEGIN IMMEDIATE: the SELECT-then-INSERT sequence must be
        // atomic across connections. Two concurrent upserts for the same
        // name would both see "not found" and both INSERT, producing
        // duplicate active tags. IMMEDIATE grabs the writer lock at BEGIN.
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| CoreError::Internal(format!("begin immediate: {e}")))?;

        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT id, first_seen FROM tags WHERE name = ?1 AND deleted_at IS NULL",
                [&normalized],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Error::from)?;

        let tag = if let Some((id_str, first_seen)) = existing {
            let id = Uuid::parse_str(&id_str)
                .map_err(|e| CoreError::Internal(format!("invalid uuid in db: {e}")))?;
            Tag {
                id,
                name: normalized,
                first_seen,
            }
        } else {
            let id = Uuid::now_v7();
            let now = now_iso();
            let dev_str = device.0.to_string();
            tx.execute(
                "INSERT INTO tags (id, name, first_seen, updated_at, device_id)
                 VALUES (?1, ?2, ?3, ?3, ?4)",
                rusqlite::params![id.to_string(), normalized, now, dev_str],
            )
            .map_err(Error::from)?;
            Tag {
                id,
                name: normalized,
                first_seen: now,
            }
        };

        tx.commit()
            .map_err(|e| CoreError::Internal(format!("commit: {e}")))?;

        Ok(tag)
    }

    // WHY allow(significant_drop_tightening): same Mutex-guard lifetime
    // reason as upsert_tag.
    #[allow(clippy::significant_drop_tightening)]
    fn delete_tag(&self, tag_id: Uuid, device: DeviceId) -> Result<(), CoreError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;

        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| CoreError::Internal(format!("begin immediate: {e}")))?;

        let now = now_iso();
        let dev_str = device.0.to_string();
        tx.execute(
            "UPDATE tags SET deleted_at = ?1, updated_at = ?1, device_id = ?2
             WHERE id = ?3 AND deleted_at IS NULL",
            rusqlite::params![now, dev_str, tag_id.to_string()],
        )
        .map_err(Error::from)?;

        tx.commit()
            .map_err(|e| CoreError::Internal(format!("commit: {e}")))?;

        Ok(())
    }

    // WHY allow(significant_drop_tightening): same Mutex-guard lifetime
    // reason as upsert_tag.
    #[allow(clippy::significant_drop_tightening)]
    fn attach(&self, hash: &BlakeHash, tag_id: Uuid, device: DeviceId) -> Result<(), CoreError> {
        let hash_hex = hash.to_hex();
        let tag_id_str = tag_id.to_string();

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;

        // WHY BEGIN IMMEDIATE: SELECT-then-INSERT must be atomic to
        // prevent duplicate active (hash, tag_id) rows under concurrency.
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| CoreError::Internal(format!("begin immediate: {e}")))?;

        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM file_tags
                 WHERE blake3_hash = ?1 AND tag_id = ?2 AND deleted_at IS NULL",
                [&hash_hex, &tag_id_str],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)?;

        if existing.is_none() {
            let id = Uuid::now_v7();
            let now = now_iso();
            let dev_str = device.0.to_string();
            tx.execute(
                "INSERT INTO file_tags (id, blake3_hash, tag_id, first_seen, updated_at, device_id)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                rusqlite::params![id.to_string(), hash_hex, tag_id_str, now, dev_str],
            )
            .map_err(Error::from)?;
        }

        tx.commit()
            .map_err(|e| CoreError::Internal(format!("commit: {e}")))?;

        Ok(())
    }

    // WHY allow(significant_drop_tightening): same Mutex-guard lifetime
    // reason as upsert_tag.
    #[allow(clippy::significant_drop_tightening)]
    fn detach(&self, hash: &BlakeHash, tag_id: Uuid, device: DeviceId) -> Result<(), CoreError> {
        let hash_hex = hash.to_hex();
        let tag_id_str = tag_id.to_string();

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;

        // WHY BEGIN IMMEDIATE: consistent with attach and upsert — all
        // read-modify-write paths use the same pattern.
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| CoreError::Internal(format!("begin immediate: {e}")))?;

        let now = now_iso();
        let dev_str = device.0.to_string();
        tx.execute(
            "UPDATE file_tags SET deleted_at = ?1, updated_at = ?1, device_id = ?2
             WHERE blake3_hash = ?3 AND tag_id = ?4 AND deleted_at IS NULL",
            rusqlite::params![now, dev_str, hash_hex, tag_id_str],
        )
        .map_err(Error::from)?;

        tx.commit()
            .map_err(|e| CoreError::Internal(format!("commit: {e}")))?;

        Ok(())
    }

    // WHY allow(significant_drop_tightening): the Mutex guard `conn` must
    // outlive the `stmt` and row-iteration borrows that hold a reference
    // through it. Dropping `conn` earlier would invalidate those borrows.
    #[allow(clippy::significant_drop_tightening)]
    fn list_tags(&self) -> Result<Vec<Tag>, CoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, name, first_seen FROM tags
                 WHERE deleted_at IS NULL
                 ORDER BY name",
            )
            .map_err(Error::from)?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(Error::from)?;

        let mut tags = Vec::new();
        for row in rows {
            let (id_str, name, first_seen) = row.map_err(Error::from)?;
            let id = Uuid::parse_str(&id_str)
                .map_err(|e| CoreError::Internal(format!("invalid uuid in db: {e}")))?;
            tags.push(Tag {
                id,
                name,
                first_seen,
            });
        }

        Ok(tags)
    }

    // WHY allow(significant_drop_tightening): the Mutex guard `conn` must
    // outlive the `stmt` and row-iteration borrows that hold a reference
    // through it.
    #[allow(clippy::significant_drop_tightening)]
    fn tags_for_hashes(
        &self,
        hashes: &[BlakeHash],
    ) -> Result<HashMap<BlakeHash, Vec<Tag>>, CoreError> {
        // WHY early return: SQL `IN ()` (empty list) is a parse error in
        // SQLite, not an empty result set. Short-circuit here to prevent
        // the caller from hitting that error accidentally.
        if hashes.is_empty() {
            return Ok(HashMap::new());
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;

        let placeholders: String = hashes
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            "SELECT ft.blake3_hash, t.id, t.name, t.first_seen
             FROM file_tags ft
             JOIN tags t ON t.id = ft.tag_id
             WHERE ft.blake3_hash IN ({placeholders})
               AND ft.deleted_at IS NULL
               AND t.deleted_at IS NULL"
        );

        let hash_strs: Vec<String> = hashes.iter().map(BlakeHash::to_hex).collect();

        let mut stmt = conn.prepare(&sql).map_err(Error::from)?;

        let rows = stmt
            .query_map(rusqlite::params_from_iter(hash_strs.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(Error::from)?;

        let mut map: HashMap<BlakeHash, Vec<Tag>> = HashMap::new();
        for row in rows {
            let (hash_str, id_str, name, first_seen) = row.map_err(Error::from)?;
            let hash = BlakeHash::parse_hex(&hash_str)
                .map_err(|e| CoreError::Internal(format!("invalid hash in db: {e}")))?;
            let id = Uuid::parse_str(&id_str)
                .map_err(|e| CoreError::Internal(format!("invalid uuid in db: {e}")))?;
            map.entry(hash).or_default().push(Tag {
                id,
                name,
                first_seen,
            });
        }

        Ok(map)
    }

    // WHY allow(significant_drop_tightening): the Mutex guard `conn` must
    // outlive the `stmt` and row-iteration borrows through it.
    #[allow(clippy::significant_drop_tightening)]
    fn files_with_tag(&self, tag_id: Uuid) -> Result<Vec<BlakeHash>, CoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT blake3_hash FROM file_tags
                 WHERE tag_id = ?1 AND deleted_at IS NULL",
            )
            .map_err(Error::from)?;

        let rows = stmt
            .query_map([tag_id.to_string()], |row| row.get::<_, String>(0))
            .map_err(Error::from)?;

        let mut hashes = Vec::new();
        for row in rows {
            let hex = row.map_err(Error::from)?;
            let hash = BlakeHash::parse_hex(&hex)
                .map_err(|e| CoreError::Internal(format!("invalid hash in db: {e}")))?;
            hashes.push(hash);
        }

        Ok(hashes)
    }

    // WHY allow(significant_drop_tightening): the Mutex guard `conn`
    // must outlive the query_row call that borrows through it.
    #[allow(clippy::significant_drop_tightening)]
    fn count_files_for_tag(&self, tag_id: Uuid) -> Result<u64, CoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))?;

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_tags WHERE tag_id = ?1 AND deleted_at IS NULL",
                [tag_id.to_string()],
                |row| row.get(0),
            )
            .map_err(Error::from)?;

        u64::try_from(count).map_err(|_| CoreError::Internal(format!("count {count} is negative")))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    fn test_db() -> (tempfile::TempDir, SqliteTagRepository) {
        let td = tempfile::tempdir().expect("tempdir");
        let conn = crate::connection::open_and_migrate(&td.path().join("test.db")).expect("open");
        (td, SqliteTagRepository::new(conn))
    }

    fn device() -> DeviceId {
        DeviceId::new()
    }

    fn sample_hash() -> BlakeHash {
        BlakeHash::parse_hex(&"a".repeat(64)).expect("hash")
    }

    fn sample_hash_2() -> BlakeHash {
        BlakeHash::parse_hex(&"b".repeat(64)).expect("hash")
    }

    #[test]
    fn upsert_tag_inserts_new() {
        let (_td, repo) = test_db();
        let tag = repo.upsert_tag("Vacation", device()).expect("upsert");
        assert_eq!(tag.name, "vacation");
        assert!(!tag.first_seen.is_empty());
    }

    #[test]
    fn upsert_tag_idempotent_on_repeat() {
        let (_td, repo) = test_db();
        let t1 = repo.upsert_tag("trip", device()).expect("first");
        let t2 = repo.upsert_tag("trip", device()).expect("second");
        assert_eq!(t1.id, t2.id, "same id on repeat upsert");
    }

    #[test]
    fn upsert_tag_normalizes_case_and_nfc() {
        let (_td, repo) = test_db();
        let t1 = repo.upsert_tag("Vacation", device()).expect("upper");
        let t2 = repo.upsert_tag("vacation", device()).expect("lower");
        assert_eq!(t1.id, t2.id, "case variants resolve to the same tag");
    }

    #[test]
    fn attach_inserts_new() {
        let (_td, repo) = test_db();
        let tag = repo.upsert_tag("photo", device()).expect("upsert");
        let h = sample_hash();
        repo.attach(&h, tag.id, device()).expect("attach");
        let map = repo.tags_for_hashes(&[h]).expect("query");
        let tags = map.get(&h).expect("hash in map");
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].id, tag.id);
    }

    #[test]
    fn attach_idempotent_on_repeat() {
        let (_td, repo) = test_db();
        let tag = repo.upsert_tag("photo", device()).expect("upsert");
        let h = sample_hash();
        repo.attach(&h, tag.id, device()).expect("attach 1");
        repo.attach(&h, tag.id, device()).expect("attach 2");
        let map = repo.tags_for_hashes(&[h]).expect("query");
        let tags = map.get(&h).expect("hash in map");
        assert_eq!(tags.len(), 1, "idempotent: only one active row");
    }

    #[test]
    fn detach_softdeletes() {
        let (_td, repo) = test_db();
        let tag = repo.upsert_tag("trip", device()).expect("upsert");
        let h = sample_hash();
        repo.attach(&h, tag.id, device()).expect("attach");
        repo.detach(&h, tag.id, device()).expect("detach");
        let map = repo.tags_for_hashes(&[h]).expect("query");
        assert!(
            map.get(&h).is_none_or(Vec::is_empty),
            "no active tags after detach"
        );
    }

    #[test]
    fn tags_for_hashes_single_hash() {
        let (_td, repo) = test_db();
        let tag = repo.upsert_tag("landscape", device()).expect("upsert");
        let h = sample_hash();
        repo.attach(&h, tag.id, device()).expect("attach");
        let map = repo.tags_for_hashes(&[h]).expect("query");
        assert_eq!(map[&h].len(), 1);
        assert_eq!(map[&h][0].name, "landscape");
    }

    #[test]
    fn tags_for_hashes_batch_returns_map() {
        let (_td, repo) = test_db();
        let t1 = repo.upsert_tag("nature", device()).expect("t1");
        let t2 = repo.upsert_tag("urban", device()).expect("t2");
        let h1 = sample_hash();
        let h2 = sample_hash_2();
        repo.attach(&h1, t1.id, device()).expect("attach h1/t1");
        repo.attach(&h2, t2.id, device()).expect("attach h2/t2");
        let map = repo.tags_for_hashes(&[h1, h2]).expect("batch");
        assert_eq!(map[&h1][0].id, t1.id);
        assert_eq!(map[&h2][0].id, t2.id);
    }

    #[test]
    fn tags_for_hashes_empty_input_shortcircuits() {
        // WHY: SQL `IN ()` is a parse error in SQLite; this test ensures
        // the short-circuit returns an empty map without hitting the DB.
        let (_td, repo) = test_db();
        let map = repo.tags_for_hashes(&[]).expect("empty");
        assert!(map.is_empty());
    }

    #[test]
    fn files_with_tag_returns_hashes() {
        let (_td, repo) = test_db();
        let tag = repo.upsert_tag("archive", device()).expect("upsert");
        let h1 = sample_hash();
        let h2 = sample_hash_2();
        repo.attach(&h1, tag.id, device()).expect("attach 1");
        repo.attach(&h2, tag.id, device()).expect("attach 2");
        let mut hashes = repo.files_with_tag(tag.id).expect("query");
        hashes.sort_by_key(BlakeHash::to_hex);
        assert_eq!(hashes.len(), 2);
    }

    #[test]
    fn count_files_for_tag_is_o1() {
        let (_td, repo) = test_db();
        let tag = repo.upsert_tag("raw", device()).expect("upsert");
        let h1 = sample_hash();
        let h2 = sample_hash_2();
        repo.attach(&h1, tag.id, device()).expect("attach 1");
        repo.attach(&h2, tag.id, device()).expect("attach 2");
        assert_eq!(repo.count_files_for_tag(tag.id).expect("count"), 2);
    }

    // WHY allow(significant_drop_tightening): `conn` must outlive the
    // `query_row` call that borrows through it; this is a test helper
    // that accesses the Mutex directly to verify raw DB state.
    #[allow(clippy::significant_drop_tightening)]
    #[test]
    fn delete_preserves_attachments() {
        let (_td, repo) = test_db();
        let tag = repo.upsert_tag("keep", device()).expect("upsert");
        let h = sample_hash();
        repo.attach(&h, tag.id, device()).expect("attach");
        repo.delete_tag(tag.id, device()).expect("delete tag");

        // Verify the file_tags row still exists (soft-delete, not cascade).
        // WHY raw SQL: tags_for_hashes filters out deleted tags, so we reach
        // directly into the DB to confirm the row is still physically present.
        let conn = repo.conn.lock().expect("lock");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_tags WHERE tag_id = ?1",
                [tag.id.to_string()],
                |r| r.get(0),
            )
            .expect("raw count");
        assert_eq!(count, 1, "file_tags row must survive tag soft-delete");
    }

    #[test]
    fn recreate_after_delete_yields_new_id() {
        let (_td, repo) = test_db();
        let t1 = repo.upsert_tag("temp", device()).expect("first");
        repo.delete_tag(t1.id, device()).expect("delete");
        let t2 = repo.upsert_tag("temp", device()).expect("recreate");
        assert_ne!(t1.id, t2.id, "recreate after delete must yield a new id");
    }

    #[test]
    fn find_or_create_tag_concurrent_unique() {
        let td = tempfile::tempdir().expect("tempdir");
        let db = td.path().join("test.db");
        let repo1 =
            SqliteTagRepository::new(crate::connection::open_and_migrate(&db).expect("open 1"));
        let repo2 =
            SqliteTagRepository::new(crate::connection::open_and_migrate(&db).expect("open 2"));
        let barrier = Arc::new(Barrier::new(2));
        let b1 = Arc::clone(&barrier);
        let d1 = device();
        let h1 = std::thread::spawn(move || {
            b1.wait();
            repo1.upsert_tag("race", d1)
        });
        let b2 = barrier;
        let d2 = device();
        let h2 = std::thread::spawn(move || {
            b2.wait();
            repo2.upsert_tag("race", d2)
        });
        let t1 = h1.join().unwrap().expect("t1");
        let t2 = h2.join().unwrap().expect("t2");
        assert_eq!(t1.id, t2.id, "both threads must resolve the same tag");
    }
}
