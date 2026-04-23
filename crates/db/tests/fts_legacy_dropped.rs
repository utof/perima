#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

//! Pre-seed a fresh DB with every name in `LEGACY_TRIGGER_NAMES`, run
//! `install_fts_triggers`, assert each legacy trigger is absent. Pins
//! the `LEGACY_TRIGGER_NAMES` contract — a regression here means an
//! existing dev DB silently retains a stale trigger.

use perima_db::{
    open_and_migrate,
    schema::{LEGACY_TRIGGER_NAMES, install_fts_triggers},
};
use rusqlite::Connection;

fn trigger_count(conn: &Connection, name: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name=?1",
        [name],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn legacy_trigger_names_are_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("legacy.db");
    let conn = open_and_migrate(&path).unwrap();

    // Pre-seed every legacy name with a minimal valid body.
    // WHY: DROP TRIGGER matches by name only — the body just has to be
    // syntactically valid SQLite, not a faithful copy of the original V007
    // body. Keeping it minimal keeps the test resilient to schema drift.
    for name in LEGACY_TRIGGER_NAMES {
        conn.execute_batch(&format!(
            "CREATE TRIGGER {name} \
             AFTER UPDATE OF blake3_hash ON file_locations \
             WHEN OLD.blake3_hash != NEW.blake3_hash \
             BEGIN \
                 DELETE FROM search_content WHERE blake3_hash = OLD.blake3_hash; \
             END;"
        ))
        .unwrap_or_else(|e| panic!("pre-seed legacy trigger {name}: {e}"));
        assert_eq!(
            trigger_count(&conn, name),
            1,
            "pre-seed must register legacy trigger {name}"
        );
    }

    install_fts_triggers(&conn).expect("install_fts_triggers");

    for name in LEGACY_TRIGGER_NAMES {
        assert_eq!(
            trigger_count(&conn, name),
            0,
            "install_fts_triggers must DROP legacy trigger {name}"
        );
    }
}
