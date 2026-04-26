//! Integration tests for spec §4.1.1: `files.quick_hash` populated eagerly
//! at scan time.
//!
//! Task 7 fix: `FileWriteCmd::UpsertFile` carries `quick_hash: Option<BlakeHash>`;
//! the writer INSERT populates `files.quick_hash` when `Some`; the UPDATE path
//! uses `COALESCE(quick_hash, ?)` to preserve any previously-stored value.
//!
//! Covers:
//! - INSERT with `quick_hash = Some(h)` → `files.quick_hash` IS populated.
//! - INSERT with `quick_hash = None` → `files.quick_hash` IS NULL.
//! - UPDATE (re-upsert same hash, different size) with `quick_hash = Some(new)`
//!   when existing row already has a non-NULL `quick_hash` → original is preserved
//!   (COALESCE semantics).
//! - UPDATE with `quick_hash = None` when existing row has a non-NULL `quick_hash`
//!   → original is preserved (COALESCE with NULL arg leaves stored value intact).
//! - UPDATE with `quick_hash = Some(v)` when existing row has `quick_hash = NULL`
//!   → NULL row gets filled in on subsequent update.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::path::PathBuf;
use std::sync::Arc;

use perima_core::{
    BlakeHash, DeviceId, EventBus, FileRepository, FileSize, HashedFile, MediaPath, UpsertOutcome,
};
use perima_db::{ReadPool, SqliteFileRepository, SqliteWriter, test_utils::NoopBus};
use rusqlite::{Connection, OpenFlags};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

fn read_quick_hash(ro: &Connection, hash_hex: &str) -> Option<String> {
    ro.query_row(
        "SELECT quick_hash FROM files WHERE blake3_hash = ?1",
        [hash_hex],
        |row| row.get(0),
    )
    .expect("SELECT quick_hash")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// INSERT with a `Some(quick_hash)` populates `files.quick_hash`.
#[test]
fn insert_with_some_quick_hash_populates_column() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let f = sample_hashed_file(b"quick_hash_insert_test", "test.jpg");
    let hash_hex = f.hash.to_hex();
    let qh = BlakeHash::from_bytes([0xBBu8; 32]);

    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    let out = repo.upsert_file_with_quick_hash(&f, dev, Some(qh)).unwrap();
    assert_eq!(out, UpsertOutcome::Inserted);

    let stored = read_quick_hash(&ro, &hash_hex)
        .expect("quick_hash must NOT be NULL after INSERT with Some(qh)");
    assert_eq!(
        stored,
        qh.to_hex(),
        "stored quick_hash must match the value passed at INSERT"
    );

    drop(repo);
    writer.join();
}

/// INSERT with `quick_hash = None` leaves `files.quick_hash` NULL.
#[test]
fn insert_with_none_quick_hash_leaves_null() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let f = sample_hashed_file(b"quick_hash_none_insert", "none.jpg");
    let hash_hex = f.hash.to_hex();

    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    // Use base upsert_file which sends None for quick_hash.
    let out = repo.upsert_file(&f, dev).unwrap();
    assert_eq!(out, UpsertOutcome::Inserted);

    let stored = read_quick_hash(&ro, &hash_hex);
    assert!(
        stored.is_none(),
        "quick_hash must be NULL when inserted without a fingerprint, got {stored:?}"
    );

    drop(repo);
    writer.join();
}

/// COALESCE: re-upserting the SAME file with a different `quick_hash` must
/// NOT overwrite an already-stored non-NULL `quick_hash`.
#[test]
fn update_preserves_existing_quick_hash_via_coalesce() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let f1 = sample_hashed_file(b"coalesce_test_file", "coalesce.jpg");
    let hash_hex = f1.hash.to_hex();
    let original_qh = BlakeHash::from_bytes([0xCCu8; 32]);
    let new_qh = BlakeHash::from_bytes([0xDDu8; 32]);

    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    // INSERT with original quick_hash.
    let out1 = repo
        .upsert_file_with_quick_hash(&f1, dev, Some(original_qh))
        .unwrap();
    assert_eq!(out1, UpsertOutcome::Inserted);
    let after_insert = read_quick_hash(&ro, &hash_hex).expect("quick_hash after insert");
    assert_eq!(after_insert, original_qh.to_hex());

    // UPDATE path: change size to force an UPDATE (not Unchanged).
    let mut f2 = f1;
    f2.discovered.size = FileSize(9_999);

    // Re-upsert with a DIFFERENT quick_hash — COALESCE must keep the original.
    let out2 = repo
        .upsert_file_with_quick_hash(&f2, dev, Some(new_qh))
        .unwrap();
    assert_eq!(out2, UpsertOutcome::Updated);
    let after_update = read_quick_hash(&ro, &hash_hex).expect("quick_hash after update");
    assert_eq!(
        after_update,
        original_qh.to_hex(),
        "COALESCE must preserve the original quick_hash on UPDATE; \
         got {after_update} (expected {})",
        original_qh.to_hex()
    );

    drop(repo);
    writer.join();
}

/// COALESCE: re-upserting with `quick_hash = None` after a stored non-NULL
/// value still preserves the stored value.
#[test]
fn update_with_none_preserves_existing_quick_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let f1 = sample_hashed_file(b"coalesce_none_test", "cn.jpg");
    let hash_hex = f1.hash.to_hex();
    let original_qh = BlakeHash::from_bytes([0xEEu8; 32]);

    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    // INSERT with Some quick_hash.
    repo.upsert_file_with_quick_hash(&f1, dev, Some(original_qh))
        .unwrap();

    // UPDATE via base upsert_file (sends None for quick_hash).
    let mut f2 = f1;
    f2.discovered.size = FileSize(1_111);
    let out = repo.upsert_file(&f2, dev).unwrap();
    assert_eq!(out, UpsertOutcome::Updated);

    let after_update = read_quick_hash(&ro, &hash_hex).expect("quick_hash after update");
    assert_eq!(
        after_update,
        original_qh.to_hex(),
        "COALESCE(quick_hash, NULL) must preserve the stored value; \
         got {after_update} (expected {})",
        original_qh.to_hex()
    );

    drop(repo);
    writer.join();
}

/// NULL row gets filled in on a subsequent UPDATE when `quick_hash = Some(v)`.
///
/// Scenario: a row inserted without a `quick_hash` (pre-Task-7-fix backfill
/// scenario) later receives one through a re-scan that carries `Some(qh)`.
#[test]
fn update_fills_in_null_quick_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let f1 = sample_hashed_file(b"fill_null_qh_test", "fill.jpg");
    let hash_hex = f1.hash.to_hex();
    let qh = BlakeHash::from_bytes([0xFFu8; 32]);

    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    // INSERT without quick_hash (NULL row).
    repo.upsert_file(&f1, dev).unwrap();
    assert!(
        read_quick_hash(&ro, &hash_hex).is_none(),
        "quick_hash must be NULL after base insert"
    );

    // UPDATE with Some(qh) → COALESCE(NULL, qh) = qh.
    let mut f2 = f1;
    f2.discovered.size = FileSize(2_222);
    let out = repo
        .upsert_file_with_quick_hash(&f2, dev, Some(qh))
        .unwrap();
    assert_eq!(out, UpsertOutcome::Updated);

    let after_update = read_quick_hash(&ro, &hash_hex).expect("quick_hash must be populated now");
    assert_eq!(
        after_update,
        qh.to_hex(),
        "COALESCE(NULL, qh) must fill in the NULL row; \
         got {after_update} (expected {})",
        qh.to_hex()
    );

    drop(repo);
    writer.join();
}
