//! Integration test for Batch C Task 4 acceptance criterion A4.4:
//! every write that touches an HLC-bearing row must populate `hlc`.
//!
//! Run `SqliteMetadataRepository::{upsert_metadata, update_thumbnail}`
//! via the writer actor, then open a raw read-only
//! [`rusqlite::Connection`] against the same tempfile-backed DB and
//! assert `hlc IS NOT NULL` on every write. Covers:
//!
//! - INSERT path of `upsert_metadata` → `hlc` populated.
//! - UPDATE path (triggered by a `mime_type` flip on the same hash) →
//!   `hlc` strictly greater than the insert value.
//! - `update_thumbnail` (independent logical event) → `hlc` strictly
//!   greater than the prior value.
//! - `Unchanged` arm (second upsert with identical inputs) → `hlc`
//!   MUST stay equal to the prior value (no write happened).

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::sync::Arc;

use perima_core::{
    BlakeHash, CoreError, DeviceId, EventBus, FileEvent, MediaMetadata, MetadataRepository,
};
use perima_db::{ReadPool, SqliteMetadataRepository, SqliteWriter};
use rusqlite::{Connection, OpenFlags};

struct NoopBus;
impl EventBus for NoopBus {
    fn emit(&self, _: &FileEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

fn sample_metadata() -> MediaMetadata {
    let hash = BlakeHash::parse_hex(&"a".repeat(64)).expect("hash");
    MediaMetadata {
        hash,
        width: Some(1920),
        height: Some(1080),
        duration_ms: None,
        captured_at: Some("2024-06-01T12:34:56Z".into()),
        camera_make: Some("Canon".into()),
        camera_model: Some("EOS R5".into()),
        codec: None,
        bitrate_bps: None,
        mime_type: Some("image/jpeg".into()),
        thumbnail_path: None,
        thumbnail_status: None,
    }
}

fn read_hlc(ro: &Connection, hash_hex: &str) -> Option<i64> {
    ro.query_row(
        "SELECT hlc FROM file_metadata WHERE blake3_hash = ?1 AND deleted_at IS NULL",
        [hash_hex],
        |row| row.get(0),
    )
    .expect("select")
}

#[test]
fn upsert_and_update_thumbnail_populate_hlc() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
    let reads = ReadPool::open(&db_path).expect("pool open");
    let repo = SqliteMetadataRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let meta = sample_metadata();
    let hash_hex = meta.hash.to_hex();

    repo.upsert_metadata(&meta, dev).expect("insert");

    // Raw read-only Connection to sidestep the adapter + pool so we
    // verify the column was actually written.
    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("readonly open");

    let inserted_hlc =
        read_hlc(&ro, &hash_hex).expect("hlc must be NOT NULL after upsert_metadata insert");
    assert!(inserted_hlc > 0, "packed HLC must be positive i64");

    // Second upsert with identical inputs → Unchanged arm; hlc must
    // remain exactly equal (no write happened).
    repo.upsert_metadata(&meta, dev).expect("unchanged");
    let unchanged_hlc = read_hlc(&ro, &hash_hex).expect("hlc present");
    assert_eq!(
        unchanged_hlc, inserted_hlc,
        "Unchanged upsert must NOT bump hlc (no write happened)"
    );

    // Third upsert with a flipped mime_type → UPDATE arm; hlc must
    // refresh to a strictly greater value.
    let mut meta2 = meta.clone();
    meta2.mime_type = Some("image/png".into());
    repo.upsert_metadata(&meta2, dev).expect("update");
    let updated_hlc = read_hlc(&ro, &hash_hex).expect("hlc after update");
    assert!(
        updated_hlc > inserted_hlc,
        "Updated upsert should refresh hlc to a strictly greater value \
         (got {updated_hlc} <= {inserted_hlc})"
    );

    // Thumbnail flip: independent logical event → hlc strictly greater
    // than the prior UPDATE value.
    repo.update_thumbnail(&meta.hash, Some("/data/t/ab/cd.webp"), "ready", dev)
        .expect("update_thumbnail");
    let thumbnail_hlc = read_hlc(&ro, &hash_hex).expect("hlc after thumbnail");
    assert!(
        thumbnail_hlc > updated_hlc,
        "update_thumbnail should refresh hlc to a strictly greater value \
         (got {thumbnail_hlc} <= {updated_hlc})"
    );

    // Tear down explicitly — drops the repo's sender + reaps the
    // writer thread cleanly before the tempdir is removed.
    drop(repo);
    writer.join();
}
