//! Cache-hit re-scan throughput benchmark.
//!
//! Validates that Task 7's Tier-0 identity cache delivers the ≥100× re-scan
//! speedup claimed in the v0.6.x spec: a re-scan over 1 000 unchanged 64 KiB
//! files should complete in ≈10 ms total (≈10 µs / file) vs ≈944 ms for a
//! cold full-hash scan (≈944 µs / file per the Task 1 baseline).
//!
//! **Observability-only** — print-only mode per the Batch J precedent
//! (`resize_only_bench_baseline_vs_reused_proves_amortization`). The
//! `eprintln!` lines at the end of `bench_rescan` let reviewers verify
//! the ≥100× ratio against the Task 1 baseline without a hard assertion
//! that would flake under cold-cache or noisy-neighbour CI conditions.
//! Once a stable threshold is calibrated, GH #166 tracks converting
//! to a hard assertion.

// WHY allow(missing_docs): workspace `#![warn(missing_docs)]` plus the
// `-D warnings` clippy gate trips on `criterion_group!`'s macro expansion
// (the generated `benches` const + helpers are undocumented by design).
// Bench files are not part of the public API.
#![allow(missing_docs)]
// WHY allow(clippy::unwrap_used): bench setup panics on unexpected errors
// rather than propagating Results; the same convention used in `benches/fts.rs`.
#![allow(clippy::unwrap_used)]
// WHY allow(print_stderr): observability-only bench intentionally eprintln!s
// per-file timing so reviewers can compare against the Task 1 baseline.
// Batch J's resize_only_bench uses the same pattern.
#![allow(clippy::print_stderr)]
// WHY allow(cast_precision_loss): duration_ms is a u64 representing milliseconds;
// at bench scale (< 60 000 ms) the f64 mantissa is exact enough for display.
// FILE_COUNT is 1 000, also exactly representable.
#![allow(clippy::cast_precision_loss)]
// WHY allow(cast_possible_truncation): `i % 256` fits in u8 by construction;
// the modulo guarantees the value is 0..=255. Annotating the site would add
// noise without safety gain.
#![allow(clippy::cast_possible_truncation)]
// WHY allow(significant_drop_tightening): the bench group must stay alive
// through `group.finish()` which is called mid-function before the
// post-bench eprintln! summary. Clippy's suggested merge would restructure
// the function flow in a way that loses the summary prints.
#![allow(clippy::significant_drop_tightening)]

use std::io::Write as _;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use perima_app::{FullScan, ScanCommand, ScanUseCase};
use perima_core::{EventBus, FileRepository, HashService, IdentityCacheRepository, Scanner};
use perima_db::{
    ReadPool, SqliteFileRepository, SqliteIdentityCacheRepository, SqliteMetadataRepository,
    SqliteSearchRepository, SqliteTagRepository, SqliteVolumeRepository, SqliteWriter,
    SqliteWriterHandle, test_utils::noop_bus::NoopBus,
};
use perima_fs::WalkdirScanner;
use perima_hash::Blake3Service;
use perima_media::ThumbnailGenerator;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

/// Number of synthetic files in the fixture.
const FILE_COUNT: usize = 1_000;
/// Size of each synthetic file in bytes (64 KiB).
const FILE_SIZE_BYTES: usize = 64 * 1_024;

/// Synthesise `FILE_COUNT` × `FILE_SIZE_BYTES` files in `dir`.
///
/// Files are named `file_0000.bin` … `file_0999.bin`.
/// Each file is filled with `(i % 256)` so every file has a distinct hash —
/// avoids accidental dedup short-circuits in the cache or file repo.
fn synth_fixture(dir: &std::path::Path) {
    for i in 0..FILE_COUNT {
        let path = dir.join(format!("file_{i:04}.bin"));
        let mut f = std::fs::File::create(&path).expect("create fixture file");
        let buf: Vec<u8> = vec![(i % 256) as u8; FILE_SIZE_BYTES];
        f.write_all(&buf).expect("write fixture file");
    }
}

/// All resources kept alive across the bench run.
struct Bench {
    _db_tmp: TempDir,
    _fixture_tmp: TempDir,
    fixture_path: std::path::PathBuf,
    uc: ScanUseCase,
    /// Writer handle must outlive all adapter `Sender` handles; drop last.
    _writer: SqliteWriterHandle,
    device_id: perima_core::DeviceId,
    rt: Runtime,
}

fn setup_bench() -> Bench {
    // 1. Synthesise fixture.
    let fixture_tmp = tempfile::tempdir().expect("fixture tempdir");
    synth_fixture(fixture_tmp.path());
    let fixture_path = fixture_tmp.path().to_path_buf();

    // 2. Open DB + adapters.
    let db_tmp = tempfile::tempdir().expect("db tempdir");
    let db_path = db_tmp.path().join("bench.db");
    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);

    let writer = SqliteWriter::start(&db_path, Arc::clone(&bus)).expect("writer start");
    let reads = ReadPool::open(&db_path).expect("pool open");

    let files: Arc<dyn FileRepository> =
        Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
    let volumes = Arc::new(SqliteVolumeRepository::new(writer.sender(), reads.clone()));
    let _tags = Arc::new(SqliteTagRepository::new(writer.sender(), reads.clone()));
    let metadata = Arc::new(SqliteMetadataRepository::new(
        writer.sender(),
        reads.clone(),
    ));
    let _search = Arc::new(SqliteSearchRepository::new(writer.sender(), reads.clone()));
    let identity_cache: Arc<dyn IdentityCacheRepository> =
        Arc::new(SqliteIdentityCacheRepository::new(writer.sender(), reads));
    let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
    let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());
    let thumbnailer = Arc::new(ThumbnailGenerator::disabled());

    let device_id = perima_core::DeviceId::new();

    // 3. Build ScanUseCase directly (not via AppContainer::new).
    //    WHY: AppContainer::new calls tokio::spawn for event handlers,
    //    which requires an active runtime. The criterion harness is NOT
    //    inside a tokio context — we drive execute() via Runtime::block_on
    //    instead. Building ScanUseCase directly avoids the spawn.
    let uc = ScanUseCase::new(
        files,
        volumes,
        metadata,
        identity_cache,
        scanner,
        hasher,
        thumbnailer,
        bus,
    );

    // 4. Single-threaded tokio runtime. WHY single-thread (not multi_thread):
    //    this bench binary runs one bench function; multi-thread buys nothing
    //    here and adds scheduler overhead to the per-iteration measurements.
    //    One runtime per binary — this is the only Runtime::new() call.
    let rt = Runtime::new().expect("tokio runtime");

    Bench {
        _db_tmp: db_tmp,
        _fixture_tmp: fixture_tmp,
        fixture_path,
        uc,
        _writer: writer,
        device_id,
        rt,
    }
}

fn bench_rescan(c: &mut Criterion) {
    let b = setup_bench();

    // --- Warm scan: populates DB + Tier-0 cache for every file -----------
    let warm_cmd = ScanCommand::Full(FullScan {
        path: b.fixture_path.clone(),
        device_id: b.device_id,
        with_metadata: false,
        dry_run: false,
        no_wait_metadata: true,
        no_thumbnails: true,
        cancel: CancellationToken::new(),
        on_persist: None,
    });
    let warm = b.rt.block_on(b.uc.execute(warm_cmd)).expect("warm scan");
    assert_eq!(
        warm.files_seen, FILE_COUNT as u64,
        "warm scan must see all {FILE_COUNT} files",
    );
    eprintln!(
        "[rescan bench] warm-scan (cold cache): {FILE_COUNT} files in {}ms ({:.1}µs/file)",
        warm.duration_ms,
        warm.duration_ms as f64 * 1_000.0 / FILE_COUNT as f64,
    );

    // --- Criterion timing loop: cache-hit re-scans -----------------------
    let mut group = c.benchmark_group("rescan");
    group.sample_size(30);

    group.bench_function("rescan_1k_files_cache_hit", |bench| {
        bench.iter(|| {
            let cmd = ScanCommand::Full(FullScan {
                path: b.fixture_path.clone(),
                device_id: b.device_id,
                with_metadata: false,
                dry_run: false,
                no_wait_metadata: true,
                no_thumbnails: true,
                cancel: CancellationToken::new(),
                on_persist: None,
            });
            let report = b.rt.block_on(b.uc.execute(cmd)).expect("re-scan");
            // WHY black_box: prevents the compiler from optimising away the call.
            std::hint::black_box(report.files_seen)
        });
    });

    group.finish();

    // --- Post-bench summary print -----------------------------------------
    // One final timed re-scan so reviewers can compare the per-file cost
    // against the Task 1 baseline without needing to read criterion's output.
    //
    // Task 1 baseline (Task 1 memory + docs/superpowers/plans/
    //   2026-04-25-fast-hashing-baseline.md):
    //   full-hash scan  ≈ 944 ms total ≈ 944 µs / file
    //   warm re-scan    ≈ 140 ms total ≈ 140 µs / file  (pre-Task-7, no cache)
    //
    // Target (spec §7 + Task 16 plan): ≤ ~10 µs / file  (≥ 100× speedup).
    // If this number is >> 10 µs / file, the Tier-0 cache is not hitting.
    let check_cmd = ScanCommand::Full(FullScan {
        path: b.fixture_path.clone(),
        device_id: b.device_id,
        with_metadata: false,
        dry_run: false,
        no_wait_metadata: true,
        no_thumbnails: true,
        cancel: CancellationToken::new(),
        on_persist: None,
    });
    let check =
        b.rt.block_on(b.uc.execute(check_cmd))
            .expect("check re-scan");
    eprintln!(
        "[rescan bench] cache-hit re-scan: {FILE_COUNT} files in {}ms ({:.1}µs/file)",
        check.duration_ms,
        check.duration_ms as f64 * 1_000.0 / FILE_COUNT as f64,
    );
    eprintln!(
        "[rescan bench] expected ≤10 µs/file (≥100× faster than ~944 µs/file cold baseline). \
         Values >> 10 µs/file indicate the Tier-0 cache is not being hit.",
    );
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(30);
    targets = bench_rescan
);
criterion_main!(benches);
