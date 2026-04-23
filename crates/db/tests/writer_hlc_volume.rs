//! Integration test for Batch C Task 2 acceptance criterion A4.4:
//! every write that touches an HLC-bearing row must populate `hlc`.
//!
//! Run `SqliteVolumeRepository::find_or_create` via the writer actor,
//! then open a raw read-only [`rusqlite::Connection`] against the same
//! tempfile-backed DB and assert `hlc IS NOT NULL` on the inserted
//! row. Also exercises the UPDATE path (second `find_or_create` for
//! the same identifiers) and asserts the updated row's `hlc` is
//! strictly greater than the first — `Hlc::now()` is monotonically
//! non-decreasing.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::sync::Arc;

use perima_core::{DeviceId, EventBus, VolumeIdentifiers, VolumeRepository};
use perima_db::{ReadPool, SqliteVolumeRepository, SqliteWriter, test_utils::NoopBus};
use rusqlite::{Connection, OpenFlags};

fn ident(label: &str, cap: u64) -> VolumeIdentifiers {
    VolumeIdentifiers {
        gpt_partition_guid: None,
        fs_uuid: None,
        label: Some(label.to_owned()),
        capacity_bytes: cap,
        is_removable: false,
    }
}

#[test]
fn find_or_create_populates_hlc_on_insert_and_update() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
    let reads = ReadPool::open(&db_path).expect("pool open");
    let repo = SqliteVolumeRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let id = repo
        .find_or_create(&ident("HLC_TEST", 1_024), dev)
        .expect("insert");

    // Raw read-only Connection to sidestep the adapter + pool so we
    // verify the column was actually written.
    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("readonly open");

    let hlc_after_insert: Option<i64> = ro
        .query_row(
            "SELECT hlc FROM volumes WHERE volume_id = ?1 AND deleted_at IS NULL",
            [id.0.to_string()],
            |row| row.get(0),
        )
        .expect("select");
    let inserted_hlc = hlc_after_insert.expect("hlc must be NOT NULL after find_or_create insert");
    assert!(inserted_hlc > 0, "packed HLC must be positive i64");

    // Second find_or_create on same identifiers → UPDATE path. The
    // row's `hlc` must refresh to a strictly greater value (Hlc::now()
    // is monotonically non-decreasing; within-ms the counter bumps).
    let id2 = repo
        .find_or_create(&ident("HLC_TEST", 1_024), dev)
        .expect("update");
    assert_eq!(id, id2, "same identifiers must resolve to same VolumeId");

    let hlc_after_update: Option<i64> = ro
        .query_row(
            "SELECT hlc FROM volumes WHERE volume_id = ?1 AND deleted_at IS NULL",
            [id.0.to_string()],
            |row| row.get(0),
        )
        .expect("select");
    let updated_hlc = hlc_after_update.expect("hlc must be NOT NULL after UPDATE");
    assert!(
        updated_hlc > inserted_hlc,
        "second find_or_create should refresh hlc to a strictly greater value \
         (got {updated_hlc} <= {inserted_hlc})"
    );

    // Tear down explicitly — drops the writer handle's sender + reaps
    // the writer thread cleanly before the tempdir is removed.
    drop(repo);
    writer.join();
}
