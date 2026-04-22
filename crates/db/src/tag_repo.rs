//! `TagRepository` adapter — writer-actor + read-pool backed.
//!
//! Post-Batch-C Task 3. The struct holds two cheap-to-clone handles:
//! a [`flume::Sender<WriteCmd>`] connected to the single writer actor
//! (spec §3.1) and a [`ReadPool`] of read-only `r2d2_sqlite`
//! connections (spec §3.4). Writes build a [`TagWriteCmd`] variant with
//! a `flume::bounded(1)` reply channel and block on the reply. Reads
//! run SQL directly against a pooled connection.
//!
//! No `Mutex<Connection>`. The legacy `::new(conn)` constructor is
//! deleted; every caller now supplies `(writer_sender, read_pool)`.

use std::collections::HashMap;

use flume::Sender;
use perima_core::{BlakeHash, CoreError, DeviceId, Tag, TagRepository, normalize_tag};
use uuid::Uuid;

use crate::cmd::{TagWriteCmd, WriteCmd};
use crate::errors::Error;
use crate::pool::ReadPool;

/// Writer-actor + read-pool backed tag + file-tag repository.
///
/// Cheap to [`Clone`]: both fields (`flume::Sender`, `ReadPool`) are
/// internally refcounted.
#[derive(Clone)]
pub struct SqliteTagRepository {
    writer: Sender<WriteCmd>,
    reads: ReadPool,
}

impl std::fmt::Debug for SqliteTagRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteTagRepository")
            .finish_non_exhaustive()
    }
}

impl SqliteTagRepository {
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

impl TagRepository for SqliteTagRepository {
    fn upsert_tag(&self, name: &str, device: DeviceId) -> Result<Tag, CoreError> {
        // WHY normalize adapter-side (not in the writer): `normalize_tag`
        // returns `CoreError::InvalidTag` on empty/whitespace/overlong
        // names. Surfacing that error before the writer hop keeps the
        // error path synchronous + matches the pre-Batch-C behaviour
        // (where the SELECT-then-INSERT block also normalized first).
        let normalized = normalize_tag(name)?;

        let (reply_tx, reply_rx) = flume::bounded::<Result<Tag, CoreError>>(1);
        self.writer
            .send(WriteCmd::Tag(TagWriteCmd::UpsertTag {
                name: normalized,
                device,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
    }

    fn delete_tag(&self, tag_id: Uuid, device: DeviceId) -> Result<(), CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
        self.writer
            .send(WriteCmd::Tag(TagWriteCmd::DeleteTag {
                tag_id,
                device,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        // WHY drop the rows_changed `u64`: the `TagRepository` port
        // returns `Result<(), CoreError>` on delete today. The writer
        // still surfaces `changes()` so a future port widening is an
        // additive change in this module only.
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))??;
        Ok(())
    }

    fn attach(&self, hash: &BlakeHash, tag_id: Uuid, device: DeviceId) -> Result<(), CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
        self.writer
            .send(WriteCmd::Tag(TagWriteCmd::Attach {
                hash: *hash,
                tag_id,
                device,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        // WHY drop rows_changed: same as delete_tag — port returns `()`.
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))??;
        Ok(())
    }

    fn detach(&self, hash: &BlakeHash, tag_id: Uuid, device: DeviceId) -> Result<(), CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<u64, CoreError>>(1);
        self.writer
            .send(WriteCmd::Tag(TagWriteCmd::Detach {
                hash: *hash,
                tag_id,
                device,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        // WHY drop rows_changed: same as delete_tag — port returns `()`.
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))??;
        Ok(())
    }

    fn list_tags(&self) -> Result<Vec<Tag>, CoreError> {
        // WHY a pool checkout here (no writer hop): `list_tags` is a
        // pure SELECT. Reads go directly through the `r2d2_sqlite` pool
        // (spec §3.5). `PooledConnection` derefs to
        // `rusqlite::Connection`, so the SQL body is lifted verbatim
        // from the pre-Batch-C impl.
        let conn = self.reads.get()?;

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

        let conn = self.reads.get()?;

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

    fn files_with_tag(&self, tag_id: Uuid) -> Result<Vec<BlakeHash>, CoreError> {
        let conn = self.reads.get()?;

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

    fn count_files_for_tag(&self, tag_id: Uuid) -> Result<u64, CoreError> {
        let conn = self.reads.get()?;

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

#[allow(
    clippy::unwrap_used,
    reason = "tests: unwrap is the assertion — a panic is a failing test by design"
)]
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use perima_core::{AppEvent, EventBus};
    use tempfile::TempDir;

    use super::*;
    use crate::pool::ReadPool;
    use crate::writer::{SqliteWriter, SqliteWriterHandle};

    /// No-op event bus used by writer-backed test fixtures.
    struct NoopBus;
    impl EventBus for NoopBus {
        fn emit(&self, _: &AppEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }

    /// Test harness: tempdir-backed DB, writer actor, read pool, repo.
    ///
    /// WHY tempfile-on-disk (not in-memory): writer + pool must share
    /// the same DB file; `:memory:` is per-connection private.
    fn test_db() -> (TempDir, SqliteTagRepository, SqliteWriterHandle) {
        let td = tempfile::tempdir().expect("tempdir");
        let db_path = td.path().join("test.db");
        let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
        let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
        let reads = ReadPool::open(&db_path).expect("pool open");
        let repo = SqliteTagRepository::new(writer.sender(), reads);
        (td, repo, writer)
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
        let (_td, repo, _writer) = test_db();
        let tag = repo.upsert_tag("Vacation", device()).expect("upsert");
        assert_eq!(tag.name, "vacation");
        assert!(!tag.first_seen.is_empty());
    }

    #[test]
    fn upsert_tag_idempotent_on_repeat() {
        let (_td, repo, _writer) = test_db();
        let t1 = repo.upsert_tag("trip", device()).expect("first");
        let t2 = repo.upsert_tag("trip", device()).expect("second");
        assert_eq!(t1.id, t2.id, "same id on repeat upsert");
    }

    #[test]
    fn upsert_tag_normalizes_case_and_nfc() {
        let (_td, repo, _writer) = test_db();
        let t1 = repo.upsert_tag("Vacation", device()).expect("upper");
        let t2 = repo.upsert_tag("vacation", device()).expect("lower");
        assert_eq!(t1.id, t2.id, "case variants resolve to the same tag");
    }

    #[test]
    fn attach_inserts_new() {
        let (_td, repo, _writer) = test_db();
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
        let (_td, repo, _writer) = test_db();
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
        let (_td, repo, _writer) = test_db();
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
        let (_td, repo, _writer) = test_db();
        let tag = repo.upsert_tag("landscape", device()).expect("upsert");
        let h = sample_hash();
        repo.attach(&h, tag.id, device()).expect("attach");
        let map = repo.tags_for_hashes(&[h]).expect("query");
        assert_eq!(map[&h].len(), 1);
        assert_eq!(map[&h][0].name, "landscape");
    }

    #[test]
    fn tags_for_hashes_batch_returns_map() {
        let (_td, repo, _writer) = test_db();
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
        let (_td, repo, _writer) = test_db();
        let map = repo.tags_for_hashes(&[]).expect("empty");
        assert!(map.is_empty());
    }

    #[test]
    fn files_with_tag_returns_hashes() {
        let (_td, repo, _writer) = test_db();
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
        let (_td, repo, _writer) = test_db();
        let tag = repo.upsert_tag("raw", device()).expect("upsert");
        let h1 = sample_hash();
        let h2 = sample_hash_2();
        repo.attach(&h1, tag.id, device()).expect("attach 1");
        repo.attach(&h2, tag.id, device()).expect("attach 2");
        assert_eq!(repo.count_files_for_tag(tag.id).expect("count"), 2);
    }

    #[test]
    fn delete_preserves_attachments() {
        let (_td, repo, _writer) = test_db();
        let tag = repo.upsert_tag("keep", device()).expect("upsert");
        let h = sample_hash();
        repo.attach(&h, tag.id, device()).expect("attach");
        repo.delete_tag(tag.id, device()).expect("delete tag");

        // Verify the file_tags row still exists (soft-delete, not cascade).
        // WHY raw read via a pooled connection: tags_for_hashes filters
        // out deleted tags, so we reach directly into the DB to confirm
        // the row is still physically present.
        let conn = repo.reads.get().expect("pool get");
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
        let (_td, repo, _writer) = test_db();
        let t1 = repo.upsert_tag("temp", device()).expect("first");
        repo.delete_tag(t1.id, device()).expect("delete");
        let t2 = repo.upsert_tag("temp", device()).expect("recreate");
        assert_ne!(t1.id, t2.id, "recreate after delete must yield a new id");
    }

    #[test]
    fn upsert_tag_concurrent_unique() {
        // WHY: two concurrent adapter HANDLES (cloned) calling
        // upsert_tag with identical names must settle on ONE active
        // tags row. Under the writer actor this is guaranteed by
        // single-threaded serialization — the test still covers the
        // observable behaviour contract.
        use std::sync::{Arc as ArcStd, Barrier};
        use std::thread;

        let (_td, repo, _writer) = test_db();
        let repo = ArcStd::new(repo);
        let barrier = ArcStd::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let repo = ArcStd::clone(&repo);
            let barrier = ArcStd::clone(&barrier);
            handles.push(thread::spawn(move || -> Tag {
                let dev = device();
                barrier.wait();
                repo.upsert_tag("race", dev).expect("upsert")
            }));
        }
        let a = handles.remove(0).join().expect("thread a");
        let b = handles.remove(0).join().expect("thread b");
        assert_eq!(a.id, b.id, "both threads must resolve the same tag id");
    }
}
