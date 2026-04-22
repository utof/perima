//! Integration test for Batch C Task 5 acceptance criterion A4.4:
//! every write that touches an HLC-bearing row must populate `hlc`.
//!
//! Run `SqliteFileRepository::{upsert_file, upsert_location,
//! update_location_status, update_location_path, migrate_sentinel_row}`
//! via the writer actor, then open a raw read-only
//! [`rusqlite::Connection`] against the same tempfile-backed DB and
//! assert `hlc IS NOT NULL` and strictly increasing on every write.
//!
//! Covers:
//!
//! - INSERT path of `upsert_file` → `files.hlc` populated + > 0.
//! - UPDATE path of `upsert_file` (size flip) → `files.hlc` strictly
//!   greater than the INSERT value.
//! - `Unchanged` arm of `upsert_file` → `files.hlc` MUST stay equal
//!   (no write happened).
//! - INSERT path of `upsert_location` → `file_locations.hlc` populated.
//! - UPDATE path of `upsert_location` (hash flip) → `file_locations.hlc`
//!   strictly greater than INSERT value.
//! - `Unchanged` arm of `upsert_location` → `file_locations.hlc` must
//!   stay equal.
//! - `update_location_status` → `file_locations.hlc` strictly greater.
//! - `update_location_path` (rename) → `file_locations.hlc` strictly
//!   greater.
//! - `migrate_sentinel_row` → `file_locations.hlc` strictly greater.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::path::PathBuf;
use std::sync::Arc;

use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, EventBus, FileRepository, FileSize, HashedFile,
    LocationStatus, MediaPath, UpsertOutcome, VolumeId,
};
use perima_db::{ReadPool, SqliteFileRepository, SqliteWriter};
use rusqlite::{Connection, OpenFlags};

struct NoopBus;
impl EventBus for NoopBus {
    fn emit(&self, _: &AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

fn sample_hashed_file(content: &[u8], rel_path: &str) -> HashedFile {
    let hash = BlakeHash::from_bytes(*blake3::hash(content).as_bytes());
    HashedFile {
        discovered: perima_core::DiscoveredFile {
            absolute_path: PathBuf::from("/tmp/fake"),
            relative_path: MediaPath::new(rel_path),
            size: FileSize(content.len() as u64),
        },
        hash,
    }
}

fn read_files_hlc(ro: &Connection, hash_hex: &str) -> Option<i64> {
    ro.query_row(
        "SELECT hlc FROM files WHERE blake3_hash = ?1",
        [hash_hex],
        |row| row.get(0),
    )
    .expect("select files.hlc")
}

fn read_location_hlc(ro: &Connection, vol_str: &str, path_str: &str) -> Option<i64> {
    ro.query_row(
        "SELECT hlc FROM file_locations
         WHERE volume_id = ?1 AND relative_path = ?2 AND deleted_at IS NULL",
        [vol_str, path_str],
        |row| row.get(0),
    )
    .expect("select file_locations.hlc")
}

#[test]
fn upsert_file_populates_hlc() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let f = sample_hashed_file(b"hlc_file_test", "test.jpg");
    let hash_hex = f.hash.to_hex();

    // Raw read-only connection to verify column values directly.
    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    // INSERT path → files.hlc must be populated + > 0.
    let out = repo.upsert_file(&f, dev).unwrap();
    assert_eq!(out, UpsertOutcome::Inserted);
    let inserted_hlc = read_files_hlc(&ro, &hash_hex).expect("hlc must NOT be NULL after insert");
    assert!(inserted_hlc > 0, "packed HLC must be positive i64");

    // Unchanged arm (repeat with same size + device) → hlc MUST stay equal.
    let out2 = repo.upsert_file(&f, dev).unwrap();
    assert_eq!(out2, UpsertOutcome::Unchanged);
    let unchanged_hlc = read_files_hlc(&ro, &hash_hex).expect("hlc present");
    assert_eq!(
        unchanged_hlc, inserted_hlc,
        "Unchanged arm must NOT bump files.hlc (no write happened)"
    );

    // UPDATE path (size flip) → hlc strictly greater.
    let mut f2 = f;
    f2.discovered.size = FileSize(9999);
    let out3 = repo.upsert_file(&f2, dev).unwrap();
    assert_eq!(out3, UpsertOutcome::Updated);
    let updated_hlc = read_files_hlc(&ro, &hash_hex).expect("hlc after update");
    assert!(
        updated_hlc > inserted_hlc,
        "Updated arm must refresh files.hlc to strictly greater value \
         (got {updated_hlc} <= {inserted_hlc})"
    );

    drop(repo);
    writer.join();
}

#[test]
fn upsert_location_populates_hlc() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let vol = VolumeId::new();
    let f1 = sample_hashed_file(b"loc_hlc_v1", "loc.jpg");
    let f2 = sample_hashed_file(b"loc_hlc_v2", "loc.jpg");
    let path = MediaPath::new("loc.jpg");
    let vol_str = vol.0.to_string();

    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    // Seed the files rows.
    repo.upsert_file(&f1, dev).unwrap();
    repo.upsert_file(&f2, dev).unwrap();

    // INSERT → file_locations.hlc populated + > 0.
    let out = repo.upsert_location(&f1.hash, vol, &path, dev).unwrap();
    assert_eq!(out, UpsertOutcome::Inserted);
    let inserted_hlc =
        read_location_hlc(&ro, &vol_str, "loc.jpg").expect("hlc must NOT be NULL after insert");
    assert!(inserted_hlc > 0, "packed HLC must be positive i64");

    // Unchanged arm (repeat) → hlc stays equal.
    let out2 = repo.upsert_location(&f1.hash, vol, &path, dev).unwrap();
    assert_eq!(out2, UpsertOutcome::Unchanged);
    let unchanged_hlc = read_location_hlc(&ro, &vol_str, "loc.jpg").expect("hlc present");
    assert_eq!(
        unchanged_hlc, inserted_hlc,
        "Unchanged arm must NOT bump file_locations.hlc"
    );

    // UPDATE path (hash flip) → hlc strictly greater.
    let out3 = repo.upsert_location(&f2.hash, vol, &path, dev).unwrap();
    assert_eq!(out3, UpsertOutcome::Updated);
    let updated_hlc = read_location_hlc(&ro, &vol_str, "loc.jpg").expect("hlc after update");
    assert!(
        updated_hlc > inserted_hlc,
        "Updated arm must refresh file_locations.hlc to strictly greater value \
         (got {updated_hlc} <= {inserted_hlc})"
    );

    drop(repo);
    writer.join();
}

#[test]
fn update_location_status_bumps_hlc() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let vol = VolumeId::new();
    let f = sample_hashed_file(b"status_hlc", "status.jpg");
    let path = MediaPath::new("status.jpg");
    let vol_str = vol.0.to_string();

    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    repo.upsert_file(&f, dev).unwrap();
    repo.upsert_location(&f.hash, vol, &path, dev).unwrap();
    let prior_hlc = read_location_hlc(&ro, &vol_str, "status.jpg").expect("prior hlc");

    let n = repo
        .update_location_status(vol, &path, LocationStatus::Missing, dev)
        .unwrap();
    assert_eq!(n, 1);

    let after_hlc =
        read_location_hlc(&ro, &vol_str, "status.jpg").expect("hlc after status update");
    assert!(
        after_hlc > prior_hlc,
        "update_location_status must refresh file_locations.hlc to strictly greater value \
         (got {after_hlc} <= {prior_hlc})"
    );

    drop(repo);
    writer.join();
}

#[test]
fn update_location_path_bumps_hlc() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let vol = VolumeId::new();
    let f = sample_hashed_file(b"path_hlc", "old.jpg");
    let old_path = MediaPath::new("old.jpg");
    let new_path = MediaPath::new("new.jpg");
    let vol_str = vol.0.to_string();

    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    repo.upsert_file(&f, dev).unwrap();
    repo.upsert_location(&f.hash, vol, &old_path, dev).unwrap();
    let prior_hlc = read_location_hlc(&ro, &vol_str, "old.jpg").expect("prior hlc");

    let n = repo
        .update_location_path(vol, &old_path, &new_path, dev)
        .unwrap();
    assert_eq!(n, 1);

    // Row moved to new_path — check hlc there.
    let after_hlc = read_location_hlc(&ro, &vol_str, "new.jpg").expect("hlc after path update");
    assert!(
        after_hlc > prior_hlc,
        "update_location_path must refresh file_locations.hlc to strictly greater value \
         (got {after_hlc} <= {prior_hlc})"
    );

    drop(repo);
    writer.join();
}

#[test]
fn migrate_sentinel_row_bumps_hlc() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let sentinel_vol = VolumeId(uuid::Uuid::nil());
    let real_vol = VolumeId::new();
    let f = sample_hashed_file(b"sentinel_hlc", "sentinel.jpg");
    let path = MediaPath::new("sentinel.jpg");
    let real_vol_str = real_vol.0.to_string();

    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    repo.upsert_file(&f, dev).unwrap();
    // Insert with sentinel volume.
    repo.upsert_location(&f.hash, sentinel_vol, &path, dev)
        .unwrap();

    // Read hlc before sentinel migration (row currently has sentinel vol UUID).
    let sentinel_vol_str = sentinel_vol.0.to_string();
    let prior_hlc = ro
        .query_row(
            "SELECT hlc FROM file_locations
             WHERE volume_id = ?1 AND relative_path = ?2 AND deleted_at IS NULL",
            [&sentinel_vol_str, "sentinel.jpg"],
            |row| row.get::<_, Option<i64>>(0),
        )
        .expect("select sentinel hlc")
        .expect("sentinel hlc must be set");

    let n = repo.migrate_sentinel_row(&path, real_vol, dev).unwrap();
    assert_eq!(n, 1);

    // After migration the row lives under the real_vol UUID.
    let after_hlc =
        read_location_hlc(&ro, &real_vol_str, "sentinel.jpg").expect("hlc after sentinel migrate");
    assert!(
        after_hlc > prior_hlc,
        "migrate_sentinel_row must refresh file_locations.hlc to strictly greater value \
         (got {after_hlc} <= {prior_hlc})"
    );

    drop(repo);
    writer.join();
}
