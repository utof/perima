#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

//! Pre-seed a fresh DB with a known V007-only trigger name, run
//! `install_fts_triggers`, assert the legacy trigger is absent. Pins
//! the `LEGACY_TRIGGER_NAMES` contract — a regression here means an
//! existing dev DB silently retains a stale trigger.

use perima_db::{open_and_migrate, schema::install_fts_triggers};

#[test]
fn legacy_trigger_names_are_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("legacy.db");
    let conn = open_and_migrate(&path).unwrap();

    // Pre-seed the V007-only legacy trigger name.
    conn.execute_batch(
        "CREATE TRIGGER search_after_location_hash_change \
         AFTER UPDATE OF blake3_hash ON file_locations \
         WHEN OLD.blake3_hash != NEW.blake3_hash \
         BEGIN \
             DELETE FROM search_content WHERE blake3_hash = OLD.blake3_hash; \
         END;",
    )
    .expect("pre-seed legacy trigger");

    let pre: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type='trigger' AND name='search_after_location_hash_change'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pre, 1, "pre-seed must register the legacy trigger");

    install_fts_triggers(&conn).expect("install_fts_triggers");

    let post: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type='trigger' AND name='search_after_location_hash_change'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        post, 0,
        "install_fts_triggers must DROP the LEGACY trigger 'search_after_location_hash_change'"
    );
}
