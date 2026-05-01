//! Shared test helpers for `crates/app/tests/*.rs`.
//!
//! WHY load-bearing `#![allow(unreachable_pub)]` + `#![allow(dead_code)]`
//! headers: every integration-test binary compiles `common/mod.rs` but
//! only uses a subset of the helpers. Without these allows, each binary
//! sees the unused helpers as warnings → CI's `-D warnings` fails.
//! Per CLAUDE.md "Test architecture (Batch F + G)" + rust-lang/rust#46379.
#![allow(unreachable_pub)]
#![allow(dead_code)]
#![allow(clippy::unwrap_used)] // WHY: test code; unwrap panics signal bugs.

use std::path::Path;
use std::sync::Arc;

use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, DiscoveredFile, EventBus, FileRepository, FileSize,
    HashedFile, MediaPath, ports::DatabaseAdmin,
};
use perima_db::{ReadPool, SqliteFileRepository, SqliteWriter, SqliteWriterHandle};
use rusqlite::OpenFlags;
use tempfile::TempDir;

/// `EventBus` stub that drops all emissions.
pub struct NoopBus;

impl EventBus for NoopBus {
    fn emit(&self, _event: &AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

#[must_use]
pub fn noop_bus() -> Arc<dyn EventBus> {
    Arc::new(NoopBus)
}

/// Bundle of (tempdir, writer-handle, read-pool, bus) for backup tests.
///
/// WHY a single fresh-env helper (vs. independent tempdir + writer init
/// per test): a prior plan revision dropped the writer after schema-init
/// then re-started it inside each test, racing migrations under WAL.
/// Tests now reuse the same handle through `TestEnv`.
pub struct TestEnv {
    pub tmp: TempDir,
    pub writer: SqliteWriterHandle,
    pub reads: ReadPool,
    pub bus: Arc<dyn EventBus>,
}

impl TestEnv {
    pub fn db_path(&self) -> std::path::PathBuf {
        self.tmp.path().join("perima.db")
    }
}

/// Initialise a fresh tempdir + perima.db + writer + read pool.
#[must_use]
pub fn fresh_env() -> TestEnv {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("perima.db");
    let bus = noop_bus();
    let writer = SqliteWriter::start(&db_path, Arc::clone(&bus)).expect("writer start");
    let reads = ReadPool::open(&db_path).expect("pool open");
    TestEnv {
        tmp,
        writer,
        reads,
        bus,
    }
}

/// Insert one fake file row.
pub fn insert_one_file_row(env: &TestEnv) {
    insert_n_file_rows(env, 1);
}

/// Insert `n` fake file rows into the `files` table.
///
/// WHY synthesise hashes deterministically (not via `Blake3Service`):
/// backup tests only need rows to land in `files`; the actual content
/// of `blake3_hash` is never inspected. Deterministic byte-fill keeps
/// the helper allocation-free and makes the seeded-row count the only
/// observable signal.
pub fn insert_n_file_rows(env: &TestEnv, n: usize) {
    let repo = SqliteFileRepository::new(env.writer.sender(), env.reads.clone());
    let device = DeviceId::default();

    for i in 0..n {
        // WHY u32 stride + repeat byte: gives a unique 32-byte hash per
        // row without needing a hasher dep. usize → u32 → 4-byte LE +
        // tail-pad with the same byte produces 256 distinct values for
        // small N (the only tests using this seed at most insert 100).
        let lo = u32::try_from(i).unwrap_or(u32::MAX);
        let mut bytes = [0u8; 32];
        bytes[0..4].copy_from_slice(&lo.to_le_bytes());
        // Pad tail with the low byte so two consecutive indices differ
        // even after the leading 4 bytes are zeroed (i < 256 case).
        let pad = u8::try_from(lo & 0xFF).unwrap_or(0);
        for slot in bytes.iter_mut().skip(4) {
            *slot = pad;
        }
        let hash = BlakeHash::from_bytes(bytes);

        let rel = format!("seed-{i}.bin");
        let hf = HashedFile {
            discovered: DiscoveredFile {
                absolute_path: env.tmp.path().join(&rel),
                relative_path: MediaPath::new(&rel),
                size: FileSize(64),
            },
            hash,
        };

        repo.upsert_file_with_quick_hash(&hf, device, None)
            .expect("upsert seed row");
    }
}

/// Open a sqlite file read-only and return COUNT(*) FROM files.
///
/// WHY allow `clippy::disallowed_methods`: backup verification needs to
/// read a non-managed sqlite file (the produced backup) — exactly the
/// legitimate read-only-inspection case carved out by GH #131.
#[allow(clippy::disallowed_methods)]
pub fn count_files(db_path: &Path) -> i64 {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open ro");
    conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .expect("count")
}

/// Test-double `DatabaseAdmin` that holds the in-flight slot for
/// a controlled duration before producing an empty backup file —
/// used by `backup_concurrent_returns_already_in_progress` to make
/// the concurrency window deterministic (not VACUUM-INTO-timing-dependent).
pub struct SlowAdmin {
    pub sleep_ms: u64,
}

impl DatabaseAdmin for SlowAdmin {
    fn backup(&self, target: &Path) -> Result<u64, CoreError> {
        std::thread::sleep(std::time::Duration::from_millis(self.sleep_ms));
        std::fs::write(target, b"fake-backup")
            .map_err(|e| CoreError::Internal(format!("write fake backup: {e}")))?;
        Ok(11)
    }
}
