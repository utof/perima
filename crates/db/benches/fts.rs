//! `SqliteSearchRepository::search` FTS5 latency benchmark.
//!
//! Seeds a `SQLite` DB with 100 rows in `search_content` via raw rusqlite
//! (mirroring `crates/app/src/search.rs::seed_via_conn` lines 202-223 —
//! the canonical working pattern). The V007 FTS5 trigger on
//! `search_content` fans out to `search_index`, which is what
//! `SqliteSearchRepository::search` queries.
//!
//! Slice 4 of T1 test-architecture decomposition. Observability-only —
//! the workflow prints results; no threshold gate.

// WHY allow(missing_docs): workspace `#![warn(missing_docs)]` plus the
// `-D warnings` clippy gate trips on `criterion_group!`'s macro
// expansion (the generated `benches` const + helpers are undocumented
// by design). Bench files are not part of the public API.
#![allow(missing_docs)]

use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
// WHY both trait imports: `SearchRepository::search` is a trait method;
// without `SearchRepository` in scope, `state.search_repo.search(...)`
// fails method-resolution. `EventBus` import same reason for
// `Arc<dyn EventBus>`.
use perima_core::{EventBus, SearchRepository};
use perima_db::SqliteSearchRepository;
use perima_db::pool::ReadPool;
use perima_db::test_utils::noop_bus::NoopBus;
use perima_db::writer::{SqliteWriter, SqliteWriterHandle};

const SEED_ROW_COUNT: usize = 100;

fn bench_fts(c: &mut Criterion) {
    let mut group = c.benchmark_group("fts/search");

    group.bench_function("search_q_match_half_rows", |b| {
        // WHY iter_batched + LargeInput: setup_db is expensive (writer
        // spawn + 100 INSERTs + refinery migration). LargeInput batches
        // multiple iterations per setup, amortizing the spawn cost.
        b.iter_batched(
            setup_db,
            |state| {
                let _ = state.search_repo.search("tag_42", 20);
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

struct BenchState {
    _td: tempfile::TempDir,
    _writer: SqliteWriterHandle,
    search_repo: SqliteSearchRepository,
}

fn setup_db() -> BenchState {
    let td = tempfile::tempdir().expect("tempdir");
    let db_path = td.path().join("bench.db");
    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);

    // Spawn the writer — its `start()` runs the refinery migrations,
    // installs the FTS5 triggers, and is the sole writable Connection
    // owner per Batch C invariants.
    let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
    let reads = ReadPool::open(&db_path).expect("pool open");
    let search_repo = SqliteSearchRepository::new(writer.sender(), reads);

    seed_search_content(&db_path);

    BenchState {
        _td: td,
        _writer: writer,
        search_repo,
    }
}

#[allow(clippy::disallowed_methods)] // WHY: bench-only seed path, mirrors
// crates/app/src/search.rs::seed_via_conn.
fn seed_search_content(db_path: &std::path::Path) {
    use rusqlite::{Connection, params};
    // WHY Connection::open (NOT open_with_flags + RW|NO_MUTEX): mirrors
    // seed_via_conn line 212 verbatim — Connection::open opens RW by
    // default and is the proven pattern for this seed shape.
    let conn = Connection::open(db_path).expect("open rw");

    // V007 search_content schema:
    //   (blake3_hash, filename, relative_path, mime_type,
    //    camera_model, captured_at, tags)
    // The FTS5 trigger on search_content fans out to search_index;
    // SearchRepository::search queries search_index. Sprinkle "tag_42"
    // into the tags column for ~half the rows so the query has a
    // non-trivial result set.
    let mut stmt = conn
        .prepare(
            "INSERT INTO search_content \
             (blake3_hash, filename, relative_path, mime_type, camera_model, captured_at, tags) \
             VALUES (?1, ?2, ?3, ?4, '', '', ?5)",
        )
        .expect("prepare");
    for i in 0..SEED_ROW_COUNT {
        let hash = format!("{i:064x}");
        let name = format!("file_{i}.jpg");
        let rel = format!("photos/{name}");
        let mime = "image/jpeg";
        let tags = if i % 2 == 0 {
            "tag_42 other"
        } else {
            "different"
        };
        stmt.execute(params![hash, name, rel, mime, tags])
            .expect("insert search_content");
    }
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(30);
    targets = bench_fts
);
criterion_main!(benches);
