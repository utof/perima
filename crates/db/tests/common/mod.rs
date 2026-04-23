#![allow(clippy::unwrap_used)] // WHY: integration test helpers; unwrap panics signal bugs.
#![allow(unreachable_pub)]
// WHY: pub fn in test-binary-local mod; unreachable from any wider crate is by design.
#![allow(dead_code)]
// WHY: helpers are consumed by 3 sibling integration-test binaries
// (search_semantics, search_triggers, search_proptests); each binary
// compiles common/mod.rs but only uses a subset. Without this, dead_code
// (-D warnings) fires per binary for helpers used elsewhere. Workaround
// per rust-lang/rust#46379. Keep it permanent — a 4th test binary later
// means the same problem recurs.

//! Shared raw-SQL helpers for `crates/db/tests/search_*.rs` integration
//! tests. Extracted from `crates/db/src/search_repo.rs::tests` in Batch G
//! (audit §A9 disposition — production split superseded by Batch C).
//!
//! Sectioning: connection setup → entity setup → mutation → ground-truth
//! projection → search assertions.

use std::path::Path;
use std::sync::Arc;

use perima_core::{DeviceId, EventBus, SearchRepository};
use rusqlite::Connection;
use tempfile::TempDir;

use perima_db::SqliteSearchRepository;
use perima_db::pool::ReadPool;
use perima_db::tag_repo::SqliteTagRepository;
use perima_db::test_utils::NoopBus;
use perima_db::writer::{SqliteWriter, SqliteWriterHandle};

// ---------------------------------------------------------------------------
// Cross-cluster constants
// ---------------------------------------------------------------------------

pub const DEV: &str = "dev";
pub const TS: &str = "2026-01-01T00:00:00Z";
pub const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const VOL: &str = "00000000-0000-0000-0000-000000000001";
pub const VOL2: &str = "00000000-0000-0000-0000-000000000002";

// ---------------------------------------------------------------------------
// Connection / writer setup
// ---------------------------------------------------------------------------

/// Produce a deterministic 64-hex-char hash from a small integer.
pub fn hash_n(n: u8) -> String {
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
pub fn test_db() -> (
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
pub fn test_db_with_tag_repo() -> (
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
pub fn seed_conn(db_path: &Path) -> Connection {
    Connection::open(db_path).expect("seed conn open")
}

/// Return a fresh [`DeviceId`].
pub fn device() -> DeviceId {
    DeviceId::new()
}

// ---------------------------------------------------------------------------
// Entity insert helpers
// ---------------------------------------------------------------------------

/// Insert a `files` row + a `file_locations` row into the DB.
pub fn insert_file(conn: &Connection, hash: &str, volume: &str, path: &str) {
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
pub fn insert_metadata(conn: &Connection, hash: &str, mime: &str, camera: &str, captured: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO file_metadata
             (blake3_hash, mime_type, camera_model, captured_at,
              extracted_at, updated_at, device_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
        rusqlite::params![hash, mime, camera, captured, TS, DEV],
    )
    .expect("insert metadata");
}

/// Insert a secondary `file_locations` row on a specific volume for an
/// existing `files` hash. Unlike [`insert_file`] this does NOT INSERT into
/// `files` — caller has already seeded that row via `insert_file` for the
/// representative location.
pub fn insert_file_at_volume(conn: &Connection, hash: &str, path: &str, volume: &str) {
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

// ---------------------------------------------------------------------------
// Mutation helpers
// ---------------------------------------------------------------------------

/// UPDATE a `file_locations` row's `relative_path` (simulates rename).
pub fn update_path(conn: &Connection, hash: &str, old_path: &str, new_path: &str) {
    conn.execute(
        "UPDATE file_locations SET relative_path = ?1
         WHERE blake3_hash = ?2 AND relative_path = ?3",
        rusqlite::params![new_path, hash, old_path],
    )
    .expect("update_path");
}

/// Volume-scoped rename helper: UPDATE `file_locations.relative_path` for
/// a specific `(hash, volume_id, old_path)` triple.
pub fn update_path_at_volume(
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

/// Attach a tag by name to a hash using raw SQL (bypasses `tag_repo` for
/// metadata-less-file tests where only one connection is available).
pub fn attach_tag_raw(conn: &Connection, hash: &str, tag_name: &str) {
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

/// Soft-delete a `file_tags` row by setting `deleted_at` (tag detach).
pub fn detach_tag_raw(conn: &Connection, hash: &str, tag_name: &str) {
    conn.execute(
        "UPDATE file_tags SET deleted_at = ?1, updated_at = ?1, device_id = ?2
         WHERE blake3_hash = ?3
           AND tag_id = (SELECT id FROM tags WHERE name = ?4 AND deleted_at IS NULL)
           AND deleted_at IS NULL",
        rusqlite::params![TS, DEV, hash, tag_name],
    )
    .expect("detach_tag_raw");
}

/// Soft-delete a `file_locations` row by setting its `deleted_at` column.
///
/// WHY helper: three tests share the same two-step pattern of updating
/// `deleted_at` on a specific `(hash, relative_path)` pair.
pub fn soft_delete_location(conn: &Connection, hash: &str, path: &str) {
    conn.execute(
        "UPDATE file_locations SET deleted_at = ?1
         WHERE blake3_hash = ?2 AND relative_path = ?3",
        rusqlite::params![TS, hash, path],
    )
    .expect("soft_delete_location");
}

/// Clear `deleted_at` on a soft-deleted `file_locations` row (restore).
pub fn restore_location(conn: &Connection, hash: &str, path: &str) {
    conn.execute(
        "UPDATE file_locations SET deleted_at = NULL, updated_at = ?1
         WHERE blake3_hash = ?2 AND relative_path = ?3",
        rusqlite::params![TS, hash, path],
    )
    .expect("restore_location");
}

/// Soft-delete a `file_metadata` row.
pub fn soft_delete_metadata(conn: &Connection, hash: &str) {
    conn.execute(
        "UPDATE file_metadata SET deleted_at = ?1, updated_at = ?1
         WHERE blake3_hash = ?2 AND deleted_at IS NULL",
        rusqlite::params![TS, hash],
    )
    .expect("soft_delete_metadata");
}

/// Restore a soft-deleted `file_metadata` row.
pub fn restore_metadata_raw(conn: &Connection, hash: &str) {
    conn.execute(
        "UPDATE file_metadata SET deleted_at = NULL, updated_at = ?1
         WHERE blake3_hash = ?2",
        rusqlite::params![TS, hash],
    )
    .expect("restore_metadata_raw");
}

/// Soft-delete a tag row (simulates `SqliteTagRepository::delete_tag`).
pub fn soft_delete_tag_raw(conn: &Connection, tag_name: &str) {
    conn.execute(
        "UPDATE tags SET deleted_at = ?1, updated_at = ?1, device_id = ?2
         WHERE name = ?3 AND deleted_at IS NULL",
        rusqlite::params![TS, DEV, tag_name],
    )
    .expect("soft_delete_tag_raw");
}

/// Restore a soft-deleted tag.
pub fn restore_tag_raw(conn: &Connection, tag_name: &str) {
    conn.execute(
        "UPDATE tags SET deleted_at = NULL, updated_at = ?1
         WHERE name = ?2",
        rusqlite::params![TS, tag_name],
    )
    .expect("restore_tag_raw");
}

/// Insert or replace a metadata row for `hash` with a deterministic camera
/// token derived from `variant`.
pub fn set_metadata_variant(conn: &Connection, hash: &str, variant: u8) {
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

// ---------------------------------------------------------------------------
// Read helpers
// ---------------------------------------------------------------------------

/// Hit-count wrapper over `SearchRepository::search(q, 50)`.
pub fn search_count(repo: &SqliteSearchRepository, q: &str) -> usize {
    repo.search(q, 50).expect("search_count").len()
}

/// Read actual `search_content` into [`GroundTruthRow`] shape.
pub fn read_search_content(conn: &Connection) -> Vec<GroundTruthRow> {
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

/// Compute expected `search_content` from joined live state.
pub fn compute_ground_truth(conn: &Connection) -> Vec<GroundTruthRow> {
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

// ---------------------------------------------------------------------------
// Ground-truth types (used by search_proptests)
// ---------------------------------------------------------------------------

/// A single expected `search_content` row computed from joined live state.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct GroundTruthRow {
    pub blake3_hash: String,
    pub relative_path: String,
    pub mime_type: String,
    pub camera_model: String,
    pub captured_at: String,
    pub tags: String,
}
