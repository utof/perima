//! V011 migration smoke test.
//!
//! Verifies that after `SqliteWriter::start` runs all migrations up to V011,
//! the expected columns (`files.file_uuid`, `files.quick_hash`,
//! `files.verified_distinct`), the `file_identity_cache` table, and the
//! `file_uuid` FK column on every dependent table are all present.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

mod common;
use perima_db::SqliteWriter;
use perima_db::test_utils::NoopBus;
use rusqlite::{Connection, OpenFlags};

#[test]
fn v011_adds_file_uuid_quick_hash_columns_and_cache_table() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("perima.db");
    let writer = SqliteWriter::start(&db_path, std::sync::Arc::new(NoopBus)).unwrap();
    drop(writer);

    // WHY open_with_flags + READ_ONLY: clippy.toml bans rusqlite::Connection::open
    // (CLAUDE.md "Second-Connection bug class (GH #131) prevention"). RO inspection
    // is the canonical pattern for post-migration assertions.
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    // file_uuid + quick_hash + verified_distinct on files
    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(files)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        cols.contains(&"file_uuid".into()),
        "files.file_uuid missing; got: {cols:?}"
    );
    assert!(
        cols.contains(&"quick_hash".into()),
        "files.quick_hash missing; got: {cols:?}"
    );
    assert!(
        cols.contains(&"verified_distinct".into()),
        "files.verified_distinct missing; got: {cols:?}"
    );

    // file_identity_cache table exists
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='file_identity_cache'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "file_identity_cache table missing");

    // FK columns added to 4 tables (search_rowid_map was replaced by
    // search_content in V007; only 4 tables + search_content exist now).
    for tbl in [
        "file_locations",
        "file_metadata",
        "file_tags",
        "search_content",
    ] {
        let stmt = format!("PRAGMA table_info({tbl})");
        let cols: Vec<String> = conn
            .prepare(&stmt)
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(
            cols.contains(&"file_uuid".into()),
            "{tbl}.file_uuid missing; got: {cols:?}"
        );
    }
}
