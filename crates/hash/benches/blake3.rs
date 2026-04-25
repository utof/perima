//! `Blake3Service` hash-throughput benchmark.
//!
//! Runs `quick_hash` + `full_hash` over 3 input sizes (1 KiB, 64 KiB,
//! 1 MiB). Reports per-iteration time + throughput (MiB/s) per criterion.
//!
//! Slice 4 of T1 test-architecture decomposition. Observability-only —
//! the workflow prints results; no threshold gate.

// WHY allow missing_docs: the workspace lint set denies missing_docs, but
// `criterion_group!` expands to an undocumented `pub fn benches()`. The
// macro is the third-party public-API entry point — annotating its
// expansion with rustdoc is impossible. Bench targets are not part of
// the library's public surface either (compiled only with --benches).
#![allow(missing_docs)]

use std::io::Write;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
// WHY HashService trait import: Blake3Service's quick_hash/full_hash methods
// are defined inside `impl HashService for Blake3Service` (NOT inherent
// methods). Without `use perima_core::HashService;` in scope, method
// resolution fails at compile time.
use perima_core::HashService;
use perima_hash::Blake3Service;

const SIZES: &[usize] = &[1024, 64 * 1024, 1024 * 1024];

fn bench_blake3(c: &mut Criterion) {
    let svc = Blake3Service::new();

    for &size in SIZES {
        let mut group = c.benchmark_group(format!("blake3/{}", human_size(size)));
        // WHY u64 cast: Throughput::Bytes takes u64; usize-to-u64 is
        // lossless on 32-bit + 64-bit. clippy::cast_possible_truncation
        // would not fire (lossless cast).
        group.throughput(Throughput::Bytes(size as u64));

        // WHY iter_batched: setup writes a fresh tempfile per batch; the
        // measurement is just the hash call. Keeps file I/O setup out of
        // the timing window.
        group.bench_function("quick_hash", |b| {
            b.iter_batched(
                || setup_tempfile(size),
                |(_td, path)| {
                    let _ = svc.quick_hash(&path);
                },
                BatchSize::SmallInput,
            );
        });
        group.bench_function("full_hash", |b| {
            b.iter_batched(
                || setup_tempfile(size),
                |(_td, path)| {
                    let _ = svc.full_hash(&path);
                },
                BatchSize::SmallInput,
            );
        });

        group.finish();
    }
}

/// Build a tempfile of `size` bytes filled with `0xFF`. Returns the
/// `TempDir` (must outlive the path) + the path.
fn setup_tempfile(size: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let td = tempfile::tempdir().expect("tempdir");
    let path = td.path().join("bench.bin");
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(&vec![0xFFu8; size]).expect("write");
    (td, path)
}

fn human_size(n: usize) -> String {
    if n >= 1024 * 1024 {
        format!("{}MiB", n / (1024 * 1024))
    } else if n >= 1024 {
        format!("{}KiB", n / 1024)
    } else {
        format!("{n}B")
    }
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(30);
    targets = bench_blake3
);
criterion_main!(benches);
