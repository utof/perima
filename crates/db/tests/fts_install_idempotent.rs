//! Idempotency check: `install_fts_triggers` can be called multiple times
//! without changing `sqlite_master` state. Pins the contract that boot-time
//! re-install is a no-op on subsequent boots.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use perima_db::schema::install_fts_triggers;
use perima_db::{open_and_migrate, schema::FTS_AGGREGATIONS};

fn fts_triggers(conn: &rusqlite::Connection) -> Vec<(String, String)> {
    conn.prepare(
        "SELECT name, sql FROM sqlite_master \
         WHERE type='trigger' AND (name LIKE 'sc_%' OR name LIKE 'search_after_%') \
         ORDER BY name",
    )
    .unwrap()
    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

#[test]
fn install_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("idempotent.db");
    let conn = open_and_migrate(&path).unwrap();

    install_fts_triggers(&conn).unwrap();
    let after_first = fts_triggers(&conn);

    install_fts_triggers(&conn).unwrap();
    let after_second = fts_triggers(&conn);

    assert_eq!(
        after_first, after_second,
        "second install_fts_triggers call must be a no-op on sqlite_master"
    );
    assert_eq!(
        after_first.len(),
        FTS_AGGREGATIONS.len(),
        "expected {} FTS triggers; got {}",
        FTS_AGGREGATIONS.len(),
        after_first.len()
    );
}
