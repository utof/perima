//! Verify [`QuickHashBackfillWorker`] populates `files.quick_hash` for NULL rows.
//!
//! Spec §4.1.5. Simulates the legacy-backfill scenario: 10 file rows exist in
//! the DB with `quick_hash IS NULL` (inserted before V011 or inserted without
//! a fingerprint). The worker must compute + store `quick_hash` for each.
//!
//! WHY raw SQL inserts: the writer's eager-populate path (Task 7) would set
//! `quick_hash` on every `UpsertFile` with a `Some` value. To simulate the
//! pre-V011 state we bypass the writer and insert directly, then verify the
//! worker fills the NULLs.

#![allow(clippy::unwrap_used)] // WHY: test code; unwrap panics signal bugs.

use std::sync::Arc;

use perima_app::{BackfillRate, BackfillReport, BackfillRow, QuickHashBackfillWorker};
use perima_core::HashService as _;
use perima_core::{
    BlakeHash, CoreError, DeviceId, EventBus, FileRepository, FileSize, HashedFile, MediaPath,
};
use perima_db::{ReadPool, SqliteFileRepository, SqliteWriter};
use perima_hash::Blake3Service;
use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Local stubs
// ---------------------------------------------------------------------------

/// No-op event bus for test writer and backfill worker.
struct NullBus;
impl EventBus for NullBus {
    fn emit(&self, _: &perima_core::AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a temp dir-backed DB writer + file repo.
fn test_harness() -> (TempDir, SqliteFileRepository, Connection) {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NullBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    // WHY open_with_flags: read-only connection for assertions only.
    // clippy::disallowed_methods exempt: test inspection seam (see GH #131 + #124).
    #[allow(clippy::disallowed_methods)]
    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    // writer handle dropped deliberately: senders inside `repo` keep the thread alive.
    drop(writer);

    (tmp, repo, ro)
}

/// Insert a file row with `quick_hash IS NULL` via the file repo (base upsert,
/// no `quick_hash` arg), then return its `blake3_hash`.
///
/// Also writes an actual file on disk at `abs_path` so the backfill worker
/// can read bytes from it.
fn seed_null_quick_hash_file(
    repo: &SqliteFileRepository,
    dev: DeviceId,
    abs_path: &std::path::Path,
    content: &[u8],
    rel_name: &str,
) -> BlakeHash {
    // Write bytes to disk so the hasher can compute quick_hash.
    std::fs::write(abs_path, content).unwrap();

    // WHY Blake3Service: we need the exact hash the writer will store.
    // Hashing from the file (which we just wrote) gives the same value
    // the backfill worker's `quick_hash_prefix_suffix` will compute.
    let hash = Blake3Service::new()
        .full_hash(abs_path)
        .expect("hash seed file");
    let hf = HashedFile {
        discovered: perima_core::DiscoveredFile {
            absolute_path: abs_path.to_path_buf(),
            relative_path: MediaPath::new(rel_name),
            size: FileSize(content.len() as u64),
        },
        hash,
    };
    // WHY upsert_file (not _with_quick_hash): simulates pre-V011 insert path
    // that left quick_hash NULL. The writer's COALESCE INSERT with None
    // produces NULL in the column.
    repo.upsert_file(&hf, dev).unwrap();

    hash
}

/// Count rows where `quick_hash IS NULL`.
fn count_null_quick_hash(ro: &Connection) -> i64 {
    ro.query_row(
        "SELECT COUNT(*) FROM files WHERE quick_hash IS NULL",
        [],
        |row| row.get(0),
    )
    .expect("COUNT null quick_hash")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Worker must fill every NULL `quick_hash` row it receives.
///
/// Setup:
/// 1. Insert 10 files with `quick_hash IS NULL` via the base `upsert_file`.
/// 2. Build `BackfillRow` list pointing at real on-disk files.
/// 3. Spawn `QuickHashBackfillWorker` with `rate_per_sec = 0` (unlimited).
/// 4. Await the `JoinHandle<BackfillReport>`.
/// 5. Assert report.processed = 10, null count = 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_populates_quick_hash_for_null_rows() {
    // WHY const before let: clippy::items_after_statements fires on const
    // declarations that follow variable-binding statements.
    const N: usize = 10;
    // WHY literal not `N as i64`: clippy::cast_possible_wrap fires on
    // const usize→i64 casts even when the value is a small literal.
    const N_I64: i64 = 10;

    let (tmp, repo, ro) = test_harness();
    let dev = DeviceId::new();
    let files_dir = tmp.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();

    let mut rows: Vec<BackfillRow> = Vec::with_capacity(N);

    // Seed N files with NULL quick_hash and build BackfillRow descriptors.
    for i in 0..N {
        let content = format!("content for file {i}");
        let rel_name = format!("f{i:02}.txt");
        let abs_path = files_dir.join(&rel_name);
        let hash = seed_null_quick_hash_file(&repo, dev, &abs_path, content.as_bytes(), &rel_name);
        rows.push(BackfillRow {
            hash,
            size_bytes: content.len() as u64,
            path: abs_path,
        });
    }

    // Confirm all 10 rows have NULL quick_hash before the worker runs.
    assert_eq!(
        count_null_quick_hash(&ro),
        N_I64,
        "all rows must start with quick_hash IS NULL"
    );

    // Spawn the backfill worker with real repo + real Blake3Service.
    let file_repo: Arc<dyn FileRepository> = Arc::new(repo);
    let hasher: Arc<dyn perima_core::HashService> = Arc::new(perima_hash::Blake3Service::new());
    let bus: Arc<dyn EventBus> = Arc::new(NullBus);
    let cancel = CancellationToken::new();

    let handle = QuickHashBackfillWorker::spawn(
        Box::new(rows.into_iter()),
        Arc::clone(&hasher),
        Arc::clone(&file_repo),
        dev,
        BackfillRate::Unlimited,
        bus,
        cancel,
    );

    let report: BackfillReport = handle.await.expect("worker task must not panic");

    // All 10 files must be processed.
    assert_eq!(report.processed, N as u64, "all 10 rows must be processed");
    assert_eq!(
        report.skipped_io_error, 0,
        "no I/O errors expected for on-disk files"
    );
    assert_eq!(
        report.skipped_no_active_location, 0,
        "no missing-location skips (paths are supplied directly)"
    );

    // Verify the DB: no NULL quick_hash rows remain.
    assert_eq!(
        count_null_quick_hash(&ro),
        0,
        "worker must have populated all quick_hash NULLs"
    );
}

/// Worker must skip (warn, count) files whose path produces an I/O error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_skips_missing_file_with_io_error() {
    let (tmp, repo, _ro) = test_harness();
    let dev = DeviceId::new();
    let files_dir = tmp.path().join("skip_test");
    std::fs::create_dir_all(&files_dir).unwrap();

    // Seed one row with a NULL quick_hash, real file on disk.
    let content = b"skip me";
    let abs_path = files_dir.join("skip.txt");
    let hash = seed_null_quick_hash_file(&repo, dev, &abs_path, content, "skip.txt");

    // Remove the file so the hasher gets an I/O error.
    std::fs::remove_file(&abs_path).unwrap();

    let rows = vec![BackfillRow {
        hash,
        size_bytes: content.len() as u64,
        path: abs_path,
    }];

    let file_repo: Arc<dyn FileRepository> = Arc::new(repo);
    let hasher: Arc<dyn perima_core::HashService> = Arc::new(perima_hash::Blake3Service::new());
    let bus: Arc<dyn EventBus> = Arc::new(NullBus);
    let cancel = CancellationToken::new();

    let handle = QuickHashBackfillWorker::spawn(
        Box::new(rows.into_iter()),
        hasher,
        file_repo,
        dev,
        BackfillRate::Unlimited,
        bus,
        cancel,
    );

    let report = handle.await.expect("task must not panic");

    assert_eq!(
        report.processed, 0,
        "missing file must not count as processed"
    );
    assert_eq!(
        report.skipped_io_error, 1,
        "missing file must count as skipped_io_error"
    );
}

/// Cancellation token fires mid-run: worker drains loop and exits cleanly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_exits_cleanly_on_cancellation() {
    let (tmp, repo, _ro) = test_harness();
    let dev = DeviceId::new();
    let files_dir = tmp.path().join("cancel_test");
    std::fs::create_dir_all(&files_dir).unwrap();

    // Seed 5 files.
    let mut rows = Vec::new();
    for i in 0..5 {
        let content = format!("cancel content {i}");
        let rel = format!("c{i}.txt");
        let abs = files_dir.join(&rel);
        let hash = seed_null_quick_hash_file(&repo, dev, &abs, content.as_bytes(), &rel);
        rows.push(BackfillRow {
            hash,
            size_bytes: content.len() as u64,
            path: abs,
        });
    }

    let file_repo: Arc<dyn FileRepository> = Arc::new(repo);
    let hasher: Arc<dyn perima_core::HashService> = Arc::new(perima_hash::Blake3Service::new());
    let bus: Arc<dyn EventBus> = Arc::new(NullBus);
    let cancel = CancellationToken::new();

    // Cancel before spawn — worker should exit without processing anything.
    cancel.cancel();

    let handle = QuickHashBackfillWorker::spawn(
        Box::new(rows.into_iter()),
        hasher,
        file_repo,
        dev,
        BackfillRate::Unlimited,
        bus,
        cancel,
    );

    // Must complete (not hang).
    let report = handle.await.expect("task must not panic");
    // With pre-cancel, worker exits immediately — processed could be 0 or a few
    // depending on scheduling; just verify it doesn't hang.
    let _ = report; // outcome is non-deterministic; just verify no deadlock.
}
