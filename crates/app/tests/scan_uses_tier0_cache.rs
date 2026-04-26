//! Verify that `ScanUseCase::execute_full` consults the Tier-0 identity cache
//! BEFORE hashing each file.
//!
//! Spec §4.3 (Tier-0 cache logic) + plan Task 7. On a cache hit the use case
//! MUST skip the hash compute entirely — no `quick_hash`, `quick_hash_prefix_suffix`,
//! or `full_hash` invocation.
//!
//! WHY this is the canonical performance landing test: the whole rationale for
//! the V011 cache table is "skip the file read on re-scans of unchanged files".
//! A regression that re-introduces the hash call (e.g. someone refactors the
//! cache lookup back into a no-op) would silently undo the v0.6.x perf goal;
//! this test catches that immediately by counting hash invocations on the mock.

#![allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use perima_app::{FullScan, ScanCommand, ScanUseCase};
use perima_core::{
    AppEvent, BlakeHash, CacheEntry, CacheKey, CoreError, DeviceId, EventBus, FileRepository,
    HashService, IdentityCacheRepository, MetadataRepository, Scanner, VolumeRepository,
};
use perima_db::{
    ReadPool, SqliteFileRepository, SqliteIdentityCacheRepository, SqliteMetadataRepository,
    SqliteVolumeRepository, SqliteWriter,
};
use perima_fs::WalkdirScanner;
use perima_media::ThumbnailGenerator;
use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// `HashService` mock that counts every method call.
///
/// Tier-0 cache hits MUST skip every hash call; a non-zero count after a
/// cache-pre-populated scan is a regression.
#[derive(Default)]
#[allow(clippy::struct_field_names)] // every field IS a per-method counter; the suffix is intentional
struct CountingHasher {
    quick_calls: AtomicUsize,
    quick_ps_calls: AtomicUsize,
    full_calls: AtomicUsize,
    full_dispatched_calls: AtomicUsize,
}

impl CountingHasher {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn total_calls(&self) -> usize {
        self.quick_calls.load(Ordering::SeqCst)
            + self.quick_ps_calls.load(Ordering::SeqCst)
            + self.full_calls.load(Ordering::SeqCst)
            + self.full_dispatched_calls.load(Ordering::SeqCst)
    }
}

impl HashService for CountingHasher {
    fn quick_hash(&self, _path: &Path) -> Result<BlakeHash, CoreError> {
        self.quick_calls.fetch_add(1, Ordering::SeqCst);
        Ok(BlakeHash::from_bytes([0u8; 32]))
    }

    fn full_hash(&self, _path: &Path) -> Result<BlakeHash, CoreError> {
        self.full_calls.fetch_add(1, Ordering::SeqCst);
        Ok(BlakeHash::from_bytes([0u8; 32]))
    }

    fn full_hash_dispatched(
        &self,
        _path: &Path,
        _size_bytes: u64,
        _device_kind: perima_core::DeviceKind,
    ) -> Result<BlakeHash, CoreError> {
        self.full_dispatched_calls.fetch_add(1, Ordering::SeqCst);
        Ok(BlakeHash::from_bytes([0u8; 32]))
    }

    fn quick_hash_prefix_suffix(
        &self,
        _path: &Path,
        _size_bytes: u64,
    ) -> Result<BlakeHash, CoreError> {
        self.quick_ps_calls.fetch_add(1, Ordering::SeqCst);
        Ok(BlakeHash::from_bytes([0u8; 32]))
    }
}

/// No-op event bus for the writer + use case.
struct NullBus;
impl EventBus for NullBus {
    fn emit(&self, _e: &AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

/// Fixture: write a single small file so the walker yields exactly one entry.
fn mk_one_file(dir: &Path) {
    let p = dir.join("alpha.txt");
    std::fs::File::create(&p)
        .unwrap()
        .write_all(b"alpha-content")
        .unwrap();
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

/// Tier-0 cache hit must short-circuit all hash calls.
///
/// Setup: pre-populate `file_identity_cache` with a row whose lookup tuple
/// `(device, volume, fs_file_id, size, mtime_ns)` matches the on-disk file.
/// Run a full scan with a `CountingHasher`; assert zero hash invocations.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_hit_skips_all_hash_calls() {
    // ---- wire-up ------------------------------------------------------------
    let db_tmp = TempDir::new().unwrap();
    let fixture = TempDir::new().unwrap();
    mk_one_file(fixture.path());

    let db_path = db_tmp.path().join("perima.db");
    let writer = SqliteWriter::start(&db_path, Arc::new(NullBus)).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();

    let files: Arc<dyn FileRepository> =
        Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
    let volumes: Arc<dyn VolumeRepository> =
        Arc::new(SqliteVolumeRepository::new(writer.sender(), reads.clone()));
    let metadata: Arc<dyn MetadataRepository> = Arc::new(SqliteMetadataRepository::new(
        writer.sender(),
        reads.clone(),
    ));
    let cache: Arc<dyn IdentityCacheRepository> =
        Arc::new(SqliteIdentityCacheRepository::new(writer.sender(), reads));

    let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());
    let counting = CountingHasher::new();
    let hasher: Arc<dyn HashService> = counting.clone();
    let thumbnailer = Arc::new(ThumbnailGenerator::disabled());
    let events: Arc<dyn EventBus> = Arc::new(NullBus);

    let device_id = DeviceId::new();

    // Resolve the volume up-front so we can pre-populate the cache row with the
    // SAME volume_id that ScanUseCase will derive at scan time. `find_or_create`
    // is idempotent — the second resolution at scan time returns the same id.
    let detected = perima_fs::detect_volume(fixture.path()).unwrap();
    let volume_id = volumes
        .find_or_create(&detected.identifiers, device_id)
        .unwrap();

    // Stat the fixture file via `Scanner::stat_with_id` so the cache row's
    // lookup tuple (size, mtime_ns, fs_file_id) matches what the use case
    // computes during the scan.
    let file_path = fixture.path().join("alpha.txt");
    let stat = scanner.stat_with_id(&file_path).unwrap();

    // Pre-populate the cache. WHY arbitrary non-zero hash bytes: the cached
    // BlakeHash content doesn't matter for this test — we only assert that
    // ScanUseCase skipped the hasher. Use a recognizable pattern (0xAB) so a
    // future debug print is unambiguous.
    let cached_quick = BlakeHash::from_bytes([0xABu8; 32]);
    let key = CacheKey {
        device_id,
        volume_id,
        fs_file_id: stat.fs_file_id,
        size_bytes: stat.size_bytes,
        mtime_ns: stat.mtime_ns,
    };
    let entry = CacheEntry {
        quick_hash: cached_quick,
        full_hash: None,
    };
    cache.upsert(&key, &entry).unwrap();

    let uc = ScanUseCase::new(
        files,
        volumes,
        metadata,
        cache,
        scanner,
        hasher,
        thumbnailer,
        events,
    );

    // ---- execute ------------------------------------------------------------
    let cmd = ScanCommand::Full(FullScan {
        path: fixture.path().to_path_buf(),
        device_id,
        with_metadata: false,
        dry_run: false,
        no_wait_metadata: true,
        no_thumbnails: true,
        cancel: CancellationToken::new(),
        on_persist: None,
    });

    let report = uc.execute(cmd).await.expect("scan succeeds");

    // ---- assert -------------------------------------------------------------
    assert_eq!(report.files_seen, 1, "fixture has exactly one file");
    assert_eq!(report.files_errored, 0, "no file errors expected");

    // The crux of Task 7: a cache HIT must skip every hash invocation.
    assert_eq!(
        counting.total_calls(),
        0,
        "Tier-0 cache hit must skip every HashService call (got quick={} quick_ps={} full={} full_dispatched={})",
        counting.quick_calls.load(Ordering::SeqCst),
        counting.quick_ps_calls.load(Ordering::SeqCst),
        counting.full_calls.load(Ordering::SeqCst),
        counting.full_dispatched_calls.load(Ordering::SeqCst),
    );

    // Spec §4.1.1 + Task 7 fix (commit d7161f0): cache-HIT path must also
    // populate `files.quick_hash` (using the cached entry's quick_hash),
    // not just `file_identity_cache.quick_hash`. Without this assertion a
    // regression dropping the hit-side `Some(entry.quick_hash)` to `None`
    // would silently slip past — the writer-level tests don't exercise
    // the scan-loop wiring. Mirror of the analogous assertion in
    // `cache_miss_calls_quick_hash_prefix_suffix`.
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE quick_hash IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "cache-HIT path must populate files.quick_hash for the row it inserts"
    );
    drop(conn);

    // WHY explicit drop order: writer outlives repo handles; tempdirs last.
    drop(writer);
    drop(db_tmp);
    drop(fixture);
}

/// Tier-0 cache MISS must compute `quick_hash_prefix_suffix` exactly once per
/// file and insert a fresh cache row.
///
/// WHY pair this test with the hit: alone, the hit test would silently pass if
/// `ScanUseCase` started returning early on every file (e.g. a `return Ok(report)`
/// placed before the loop). The miss test confirms the use case actually
/// processes files when there's no cached entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_miss_calls_quick_hash_prefix_suffix() {
    let db_tmp = TempDir::new().unwrap();
    let fixture = TempDir::new().unwrap();
    mk_one_file(fixture.path());

    let db_path = db_tmp.path().join("perima.db");
    let writer = SqliteWriter::start(&db_path, Arc::new(NullBus)).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();

    let files: Arc<dyn FileRepository> =
        Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
    let volumes: Arc<dyn VolumeRepository> =
        Arc::new(SqliteVolumeRepository::new(writer.sender(), reads.clone()));
    let metadata: Arc<dyn MetadataRepository> = Arc::new(SqliteMetadataRepository::new(
        writer.sender(),
        reads.clone(),
    ));
    let cache: Arc<dyn IdentityCacheRepository> =
        Arc::new(SqliteIdentityCacheRepository::new(writer.sender(), reads));

    let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());
    let counting = CountingHasher::new();
    let hasher: Arc<dyn HashService> = counting.clone();
    let thumbnailer = Arc::new(ThumbnailGenerator::disabled());
    let events: Arc<dyn EventBus> = Arc::new(NullBus);

    let uc = ScanUseCase::new(
        files,
        volumes,
        metadata,
        Arc::clone(&cache),
        scanner,
        hasher,
        thumbnailer,
        events,
    );

    let device_id = DeviceId::new();
    let cmd = ScanCommand::Full(FullScan {
        path: fixture.path().to_path_buf(),
        device_id,
        with_metadata: false,
        dry_run: false,
        no_wait_metadata: true,
        no_thumbnails: true,
        cancel: CancellationToken::new(),
        on_persist: None,
    });

    let report = uc.execute(cmd).await.expect("scan succeeds");
    assert_eq!(report.files_seen, 1);

    // Miss path: exactly one quick_hash_prefix_suffix call. Other hash methods
    // must remain at 0 so a future regression that swaps in `full_hash` (or
    // the legacy `quick_hash`) under the miss path lights up here.
    assert_eq!(
        counting.quick_ps_calls.load(Ordering::SeqCst),
        1,
        "miss path must call quick_hash_prefix_suffix exactly once",
    );
    assert_eq!(
        counting.quick_calls.load(Ordering::SeqCst),
        0,
        "miss path must not fall back to legacy quick_hash",
    );
    assert_eq!(
        counting.full_calls.load(Ordering::SeqCst)
            + counting.full_dispatched_calls.load(Ordering::SeqCst),
        0,
        "miss path must not full-hash in v0.6.x — full_hash is Task 9 work",
    );

    // Spec §4.1.1: files.quick_hash must be populated after a cache-miss scan.
    // WHY raw SQL: the port-trait read path does not expose quick_hash yet
    // (Task 9 adds list_quick_hash_collisions). A direct SELECT verifies the
    // writer actually wrote the column.
    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    let quick_hash_count: i64 = ro
        .query_row(
            "SELECT COUNT(*) FROM files WHERE quick_hash IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        quick_hash_count, 1,
        "spec §4.1.1: files.quick_hash must be non-NULL after a cache-miss scan \
         (got {quick_hash_count} rows with quick_hash IS NOT NULL)"
    );

    drop(writer);
    drop(db_tmp);
    drop(fixture);
}
