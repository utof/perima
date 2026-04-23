//! `SearchRepository` implementation backed by rusqlite FTS5.
//!
//! Post-Batch-C Task 7. The struct holds two cheap-to-clone handles:
//! a [`flume::Sender<WriteCmd>`] connected to the single writer actor
//! (spec §3.1) and a [`ReadPool`] of read-only `r2d2_sqlite`
//! connections (spec §3.4). Writes build a [`SearchWriteCmd`] variant
//! with a `flume::bounded(1)` reply channel and block on the reply.
//! Reads run SQL directly against a pooled connection.
//!
//! No `Mutex<Connection>`. Every caller now supplies
//! `(writer_sender, read_pool)` via `SqliteSearchRepository::new`.

use flume::Sender;
use perima_core::{CoreError, SearchHit, SearchRepository};
use rusqlite::Connection;

use crate::cmd::{SearchWriteCmd, WriteCmd};
use crate::errors::Error;
use crate::pool::ReadPool;

/// Writer-actor + read-pool backed full-text search repository.
///
/// Cheap to [`Clone`]: both fields (`flume::Sender`, `ReadPool`) are
/// internally refcounted.
#[derive(Clone)]
pub struct SqliteSearchRepository {
    writer: Sender<WriteCmd>,
    reads: ReadPool,
}

impl std::fmt::Debug for SqliteSearchRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSearchRepository")
            .finish_non_exhaustive()
    }
}

impl SqliteSearchRepository {
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
// SearchRepository trait impl
// ---------------------------------------------------------------------------

impl SearchRepository for SqliteSearchRepository {
    fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, CoreError> {
        let conn = self.reads.get()?;
        search_impl(&conn, query, limit)
    }

    fn rebuild(&self) -> Result<(), CoreError> {
        let (tx, rx) = flume::bounded::<Result<(), CoreError>>(1);
        self.writer
            .send(WriteCmd::Search(SearchWriteCmd::Rebuild { reply: tx }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        rx.recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
    }
}

// ---------------------------------------------------------------------------
// Read-path helpers (pool variant)
// ---------------------------------------------------------------------------

/// SELECT body for [`SearchRepository::search`].
///
/// V007: `search_content` has `(rowid, blake3_hash, relative_path, ...)`
/// but [`perima_core::SearchHit`] requires `volume_id` which lives only
/// on `file_locations`. Pick the first-seen active location per hash to
/// populate `volume_id`. The subquery ordering (`first_seen ASC, id ASC`)
/// mirrors the trigger representative-selection rule, so the `volume_id`
/// returned here agrees with the path indexed in `search_content`.
fn search_impl(conn: &Connection, query: &str, limit: u32) -> Result<Vec<SearchHit>, CoreError> {
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[allow(dead_code, unused_imports)] // WHY: helpers remain for the 2 proptests; Task 5 deletes this mod wholesale.
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use perima_core::{DeviceId, EventBus, TagRepository};
    use tempfile::TempDir;

    use crate::pool::ReadPool;
    use crate::tag_repo::SqliteTagRepository;
    use crate::test_utils::NoopBus;
    use crate::writer::{SqliteWriter, SqliteWriterHandle};

    const DEV: &str = "dev";
    const TS: &str = "2026-01-01T00:00:00Z";

    /// Produce a deterministic 64-hex-char hash from a small integer.
    fn hash_n(n: u8) -> String {
        // WHY format: first two chars encode `n`; remaining 62 are '0'.
        // Gives 256 distinct valid-length hashes without hand-writing literals.
        format!("{:02x}{}", n, "0".repeat(62))
    }

    /// Build a tempfile-on-disk DB, writer actor, read pool, and search repo.
    ///
    /// WHY tempfile-on-disk (not in-memory): writer + pool must share
    /// the same DB file; `:memory:` is per-connection private.
    ///
    /// Returns `(TempDir, db_path, SqliteSearchRepository, SqliteWriterHandle)`.
    /// Keep the `TempDir` alive for the test duration; the `db_path` is needed
    /// by seeding helpers that open a direct raw connection.
    fn test_db() -> (
        TempDir,
        std::path::PathBuf,
        SqliteSearchRepository,
        SqliteWriterHandle,
    ) {
        let td = tempfile::tempdir().expect("tempdir");
        let db_path = td.path().join("test.db");
        let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
        let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
        let reads = ReadPool::open(&db_path).expect("pool open");
        let repo = SqliteSearchRepository::new(writer.sender(), reads);
        (td, db_path, repo, writer)
    }

    /// Harness for search + tag tests.
    ///
    /// WHY returns `SqliteWriterHandle`: post-Batch-C Task 3,
    /// `SqliteTagRepository` holds `(flume::Sender<WriteCmd>, ReadPool)`.
    /// Tests must keep the writer handle alive so the writer thread
    /// outlives the tag repo.
    fn test_db_with_tag_repo() -> (
        TempDir,
        std::path::PathBuf,
        SqliteSearchRepository,
        SqliteTagRepository,
        SqliteWriterHandle,
    ) {
        let td = tempfile::tempdir().expect("tempdir");
        let db_path = td.path().join("test.db");

        // Writer runs the migration sweep. WAL mode lets the two connections coexist.
        let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
        let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
        let reads = ReadPool::open(&db_path).expect("pool open");

        (
            td,
            db_path,
            SqliteSearchRepository::new(writer.sender(), reads.clone()),
            SqliteTagRepository::new(writer.sender(), reads),
            writer,
        )
    }

    /// Open a direct raw connection for seeding raw SQL in tests.
    ///
    /// WHY raw connection: test seeding inserts rows directly (bypassing
    /// the writer actor) to exercise `SQLite` triggers in isolation. The
    /// writer actor is idle (blocked on `flume` channel) while tests seed,
    /// so a second connection in WAL mode does not conflict. Post-GH #131
    /// (rusqlite 0.39 / `SQLite` 3.51.3) the lock-order-inversion close race is fixed
    /// upstream; the proptest seeding pattern is tracked under #124 for
    /// a longer-term writer-routed rewrite.
    #[allow(clippy::disallowed_methods)]
    fn seed_conn(db_path: &Path) -> Connection {
        Connection::open(db_path).expect("seed conn open")
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
    /// a specific `(hash, volume_id, old_path)` triple.
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

    // ── v0.6.4 RED regression tests (codex-surfaced bugs in V007) ───────────

    /// Soft-delete a tag row (simulates `SqliteTagRepository::delete_tag`).
    fn soft_delete_tag_raw(conn: &Connection, tag_name: &str) {
        conn.execute(
            "UPDATE tags SET deleted_at = ?1, updated_at = ?1, device_id = ?2
             WHERE name = ?3 AND deleted_at IS NULL",
            rusqlite::params![TS, DEV, tag_name],
        )
        .expect("soft_delete_tag_raw");
    }

    /// Clear `deleted_at` on a soft-deleted `file_locations` row (restore).
    fn restore_location(conn: &Connection, hash: &str, path: &str) {
        conn.execute(
            "UPDATE file_locations SET deleted_at = NULL, updated_at = ?1
             WHERE blake3_hash = ?2 AND relative_path = ?3",
            rusqlite::params![TS, hash, path],
        )
        .expect("restore_location");
    }

    /// Soft-delete a `file_metadata` row.
    fn soft_delete_metadata(conn: &Connection, hash: &str) {
        conn.execute(
            "UPDATE file_metadata SET deleted_at = ?1, updated_at = ?1
             WHERE blake3_hash = ?2 AND deleted_at IS NULL",
            rusqlite::params![TS, hash],
        )
        .expect("soft_delete_metadata");
    }

    // ── Task 5: proptest — FTS5 invariant across tag churn ──────────────────

    /// Soft-delete a `file_tags` row by setting `deleted_at` (tag detach).
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

    // ── Task 5: proptest — FTS5 trigger invariant across random tag churn ───

    /// Operations exercised by the property: Attach or Detach a (file, tag) pair.
    #[derive(Debug, Clone)]
    enum TagOp {
        Attach(usize, usize),
        Detach(usize, usize),
    }

    /// Small universe: 3 files × 3 tags.
    const PROP_FILES: &[&str] = &[
        "7100000000000000000000000000000000000000000000000000000000000000",
        "7200000000000000000000000000000000000000000000000000000000000000",
        "7300000000000000000000000000000000000000000000000000000000000000",
    ];
    const PROP_TAGS: &[&str] = &["alpha", "beta", "gamma"];
    const PROP_VOL: &str = "00000000-0000-0000-0000-000000000099";

    proptest::proptest! {
        // WHY cases=64 (down from the 256 default): post-Batch-C each proptest
        // case creates a writer-actor thread + `r2d2` read pool + a single
        // `seed_conn` on a fresh tempdir DB — ~5x the per-case cost of the
        // pre-Task-7 single-`Mutex<Connection>` fixture (#124). The seed
        // connection is already hoisted below to case scope so per-op
        // `Connection::open` churn is gone; the residual per-case cost is
        // the writer-thread + pool init itself. At 256 cases the cumulative
        // overhead exceeds the 80s terminate-after window on VM filesystems
        // even though no individual case contends for the write lock. 64
        // cases × up to 30 ops = ~1 920 ops per proptest, still strong
        // combinatorial coverage for FTS trigger invariants.
        #![proptest_config(proptest::test_runner::Config {
            cases: 64,
            ..proptest::test_runner::Config::default()
        })]

        /// **Invariant:** after every Attach / Detach operation, for every
        /// `(file, tag)` pair, `MATCH tag_name` returns the file iff
        /// `file_tags.deleted_at IS NULL` for that pair.
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
            let (_td, db, repo, _writer) = test_db();

            // WHY single seed_conn hoisted to case scope: each `Connection::open`
            // on a WAL file does several syscalls (open, SHARED lock, -shm/-wal
            // handshake, header read). With 30 ops × 256 default cases × 2
            // proptests = ~15k opens, that cost compounded to >80s on VM
            // filesystems (#124). Reusing one connection per case keeps all
            // writes as auto-commit statements — no transaction state crosses
            // ops, so test semantics are identical to the per-op-scope version.
            let conn = seed_conn(&db);

            // Seed all three files.
            for (i, hash) in PROP_FILES.iter().enumerate() {
                insert_file(
                    &conn,
                    hash,
                    PROP_VOL,
                    &format!("prop_file_{i}.jpg"),
                );
            }

            let mut attached: std::collections::HashMap<(usize, usize), bool> =
                std::collections::HashMap::new();

            for op in &ops {
                match *op {
                    TagOp::Attach(f, t) => {
                        attach_tag_raw(&conn, PROP_FILES[f], PROP_TAGS[t]);
                        attached.insert((f, t), true);
                    }
                    TagOp::Detach(f, t) => {
                        if *attached.get(&(f, t)).unwrap_or(&false) {
                            detach_tag_raw(&conn, PROP_FILES[f], PROP_TAGS[t]);
                            attached.insert((f, t), false);
                        }
                    }
                }

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

    // ── v0.6.4 proptest — ground-truth invariant over full soft-delete op universe ──

    #[derive(Debug, Clone)]
    enum SoftOp {
        AttachTag(usize, usize),
        DetachTag(usize, usize),
        SoftDeleteTag(usize),
        RestoreTag(usize),
        SetMetadata(usize, u8),
        SoftDeleteMetadata(usize),
        RestoreMetadata(usize),
        SoftDeleteLocation(usize),
        RestoreLocation(usize),
    }

    /// Restore a soft-deleted tag.
    fn restore_tag_raw(conn: &Connection, tag_name: &str) {
        conn.execute(
            "UPDATE tags SET deleted_at = NULL, updated_at = ?1
             WHERE name = ?2",
            rusqlite::params![TS, tag_name],
        )
        .expect("restore_tag_raw");
    }

    /// Restore a soft-deleted `file_metadata` row.
    fn restore_metadata_raw(conn: &Connection, hash: &str) {
        conn.execute(
            "UPDATE file_metadata SET deleted_at = NULL, updated_at = ?1
             WHERE blake3_hash = ?2",
            rusqlite::params![TS, hash],
        )
        .expect("restore_metadata_raw");
    }

    /// Insert or replace a metadata row for `hash` with a deterministic camera
    /// token derived from `variant`.
    fn set_metadata_variant(conn: &Connection, hash: &str, variant: u8) {
        let cam = format!("cam_{variant}");
        let mime = format!("image/type{variant}");
        conn.execute(
            "INSERT INTO file_metadata
                 (blake3_hash, mime_type, camera_model, captured_at,
                  extracted_at, updated_at, device_id)
             VALUES (?1, ?2, ?3, '', ?4, ?4, ?5)
             ON CONFLICT(blake3_hash) DO UPDATE SET
                 mime_type = excluded.mime_type,
                 camera_model = excluded.camera_model,
                 updated_at = excluded.updated_at,
                 deleted_at = NULL",
            rusqlite::params![hash, mime, cam, TS, DEV],
        )
        .expect("set_metadata_variant");
    }

    /// A single expected `search_content` row computed from joined live state.
    #[derive(Debug, PartialEq, Eq, Hash, Clone)]
    struct GroundTruthRow {
        blake3_hash: String,
        relative_path: String,
        mime_type: String,
        camera_model: String,
        captured_at: String,
        tags: String,
    }

    /// Compute expected `search_content` from joined live state.
    fn compute_ground_truth(conn: &Connection) -> Vec<GroundTruthRow> {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT fl.blake3_hash
                 FROM file_locations fl
                 WHERE fl.deleted_at IS NULL
                 ORDER BY fl.blake3_hash",
            )
            .expect("prepare hashes");
        let hashes: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query hashes")
            .filter_map(Result::ok)
            .collect();

        let mut out = Vec::new();
        for h in hashes {
            let path: String = conn
                .query_row(
                    "SELECT fl.relative_path FROM file_locations fl
                     WHERE fl.blake3_hash = ?1 AND fl.deleted_at IS NULL
                     ORDER BY fl.first_seen ASC, fl.id ASC LIMIT 1",
                    rusqlite::params![h],
                    |r| r.get(0),
                )
                .expect("rep path");
            let (mime, camera, captured): (String, String, String) = conn
                .query_row(
                    "SELECT COALESCE(mime_type, ''),
                            COALESCE(camera_model, ''),
                            COALESCE(captured_at, '')
                     FROM file_metadata
                     WHERE blake3_hash = ?1 AND deleted_at IS NULL",
                    rusqlite::params![h],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap_or_else(|_| (String::new(), String::new(), String::new()));
            let mut tag_names: Vec<String> = {
                let mut s = conn
                    .prepare(
                        "SELECT t.name FROM file_tags ft
                         JOIN tags t ON t.id = ft.tag_id
                         WHERE ft.blake3_hash = ?1
                           AND ft.deleted_at IS NULL
                           AND t.deleted_at IS NULL",
                    )
                    .expect("prepare tags");
                s.query_map(rusqlite::params![h], |r| r.get::<_, String>(0))
                    .expect("query tags")
                    .filter_map(Result::ok)
                    .collect()
            };
            tag_names.sort();
            out.push(GroundTruthRow {
                blake3_hash: h,
                relative_path: path.clone(),
                mime_type: mime,
                camera_model: camera,
                captured_at: captured,
                tags: tag_names.join(" "),
            });
        }
        out
    }

    /// Read actual `search_content` into the same shape.
    fn read_search_content(conn: &Connection) -> Vec<GroundTruthRow> {
        let mut stmt = conn
            .prepare(
                "SELECT blake3_hash, relative_path, mime_type, camera_model,
                        captured_at, tags
                 FROM search_content ORDER BY blake3_hash",
            )
            .expect("prepare sc");
        stmt.query_map([], |r| {
            let tags_raw: String = r.get(5)?;
            let mut toks: Vec<&str> = tags_raw.split_whitespace().collect();
            toks.sort_unstable();
            Ok(GroundTruthRow {
                blake3_hash: r.get(0)?,
                relative_path: r.get(1)?,
                mime_type: r.get(2)?,
                camera_model: r.get(3)?,
                captured_at: r.get(4)?,
                tags: toks.join(" "),
            })
        })
        .expect("query sc")
        .filter_map(Result::ok)
        .collect()
    }

    const SOFT_FILES: &[&str] = &[
        "a100000000000000000000000000000000000000000000000000000000000000",
        "a200000000000000000000000000000000000000000000000000000000000000",
    ];
    const SOFT_TAGS: &[&str] = &["alpha", "beta"];
    const SOFT_VOL: &str = "00000000-0000-0000-0000-0000000000aa";

    proptest::proptest! {
        // See `fts_consistent_under_tag_churn` for the cases-reduction
        // rationale (#124). This proptest is set to cases=32 (half the other
        // one) because each op runs `compute_ground_truth` — up to 8
        // per-hash SELECTs plus a `read_search_content` scan — on top of the
        // mutation. With 25 ops × 9 queries ≈ 225 DB ops per case, the
        // per-case cost is ~2x the tag-churn proptest.
        #![proptest_config(proptest::test_runner::Config {
            cases: 32,
            ..proptest::test_runner::Config::default()
        })]

        /// **Invariant:** after EVERY op, search_content rows (incrementally
        /// maintained by triggers) must equal the ground-truth rows computed
        /// directly from joined live state via independent per-field subqueries.
        #[test]
        fn fts_matches_ground_truth_under_soft_delete_churn(
            ops in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::strategy::Strategy::prop_map(
                        (0..SOFT_FILES.len(), 0..SOFT_TAGS.len()),
                        |(f, t)| SoftOp::AttachTag(f, t),
                    ),
                    proptest::strategy::Strategy::prop_map(
                        (0..SOFT_FILES.len(), 0..SOFT_TAGS.len()),
                        |(f, t)| SoftOp::DetachTag(f, t),
                    ),
                    proptest::strategy::Strategy::prop_map(
                        0..SOFT_TAGS.len(),
                        SoftOp::SoftDeleteTag,
                    ),
                    proptest::strategy::Strategy::prop_map(
                        0..SOFT_TAGS.len(),
                        SoftOp::RestoreTag,
                    ),
                    proptest::strategy::Strategy::prop_map(
                        (0..SOFT_FILES.len(), 0u8..4u8),
                        |(f, v)| SoftOp::SetMetadata(f, v),
                    ),
                    proptest::strategy::Strategy::prop_map(
                        0..SOFT_FILES.len(),
                        SoftOp::SoftDeleteMetadata,
                    ),
                    proptest::strategy::Strategy::prop_map(
                        0..SOFT_FILES.len(),
                        SoftOp::RestoreMetadata,
                    ),
                    proptest::strategy::Strategy::prop_map(
                        0..SOFT_FILES.len(),
                        SoftOp::SoftDeleteLocation,
                    ),
                    proptest::strategy::Strategy::prop_map(
                        0..SOFT_FILES.len(),
                        SoftOp::RestoreLocation,
                    ),
                ],
                0..25,
            ),
        ) {
            let (_td, db, _repo, _writer) = test_db();

            // See `fts_consistent_under_tag_churn` for the rationale — one
            // seed_conn per case instead of per-op avoids ~15k extra
            // `Connection::open` calls on the WAL-mode DB file (#124).
            let conn = seed_conn(&db);
            for (i, h) in SOFT_FILES.iter().enumerate() {
                insert_file(&conn, h, SOFT_VOL, &format!("soft_{i}.jpg"));
            }

            for op in &ops {
                match *op {
                    SoftOp::AttachTag(f, t) => {
                        attach_tag_raw(&conn, SOFT_FILES[f], SOFT_TAGS[t]);
                    }
                    SoftOp::DetachTag(f, t) => {
                        detach_tag_raw(&conn, SOFT_FILES[f], SOFT_TAGS[t]);
                    }
                    SoftOp::SoftDeleteTag(t) => {
                        soft_delete_tag_raw(&conn, SOFT_TAGS[t]);
                    }
                    SoftOp::RestoreTag(t) => {
                        restore_tag_raw(&conn, SOFT_TAGS[t]);
                    }
                    SoftOp::SetMetadata(f, v) => {
                        set_metadata_variant(&conn, SOFT_FILES[f], v);
                    }
                    SoftOp::SoftDeleteMetadata(f) => {
                        soft_delete_metadata(&conn, SOFT_FILES[f]);
                    }
                    SoftOp::RestoreMetadata(f) => {
                        restore_metadata_raw(&conn, SOFT_FILES[f]);
                    }
                    SoftOp::SoftDeleteLocation(f) => {
                        soft_delete_location(
                            &conn,
                            SOFT_FILES[f],
                            &format!("soft_{f}.jpg"),
                        );
                    }
                    SoftOp::RestoreLocation(f) => {
                        restore_location(
                            &conn,
                            SOFT_FILES[f],
                            &format!("soft_{f}.jpg"),
                        );
                    }
                }

                let (actual, expected) = (
                    read_search_content(&conn),
                    compute_ground_truth(&conn),
                );
                proptest::prop_assert_eq!(
                    actual,
                    expected,
                    "search_content drifted from ground truth after op {:?} in sequence {:?}",
                    op, ops
                );
            }
        }
    }
}
