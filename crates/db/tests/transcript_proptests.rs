//! FTS5 ground-truth proptest for the `transcript_search` virtual table.
//!
//! Mirrors `crates/db/tests/search_proptests.rs::fts_matches_ground_truth_under_soft_delete_churn`
//! shape: a small randomized op universe runs against a fresh tempfile DB
//! per case, and after EVERY op the contents of `transcript_search`
//! (incrementally maintained by codegen-installed FTS5 triggers) must
//! equal a ground-truth set computed directly from joined live state.
//!
//! Cases capped at 64 per CLAUDE.md "FTS5 proptests" rule (#124).
//!
//! Seeding uses a single raw `seed_conn` (writer is idle on the flume
//! channel); follows the same per-case writer + per-case raw-conn pattern
//! the sibling proptest uses.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::collections::BTreeSet;
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags, params};
use tempfile::TempDir;

use perima_core::EventBus;
use perima_db::test_utils::NoopBus;
use perima_db::writer::{SqliteWriter, SqliteWriterHandle};

// ---------------------------------------------------------------------------
// Proptest universe constants
// ---------------------------------------------------------------------------

const TRANSCRIPT_COUNT: usize = 2;
const SEGMENT_TEXTS: &[&str] = &[
    "alpha bravo",
    "charlie delta",
    "echo foxtrot",
    "golf hotel",
    "india juliet",
];
const PROP_DEV: &str = "prop-dev";
const PROP_TS: &str = "2026-01-01T00:00:00Z";
const PROP_HLC: i64 = 1;

// ---------------------------------------------------------------------------
// Setup helpers (test-binary-local; no shared common/ surface added)
// ---------------------------------------------------------------------------

fn scratch_writer() -> (TempDir, std::path::PathBuf, SqliteWriterHandle) {
    let td = tempfile::tempdir().expect("tempdir");
    let db_path = td.path().join("transcripts.db");
    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
    (td, db_path, writer)
}

#[allow(clippy::disallowed_methods)] // WHY: proptest seeding via raw conn — see GH #131 + #124 (sibling search_proptests pattern).
fn seed_conn(db_path: &std::path::Path) -> Connection {
    Connection::open(db_path).expect("seed conn open")
}

fn ro_conn(db_path: &std::path::Path) -> Connection {
    Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("ro conn open")
}

/// Seed `TRANSCRIPT_COUNT` parent transcript rows. Returns their ids.
fn seed_transcripts(conn: &Connection) -> Vec<String> {
    let mut ids = Vec::with_capacity(TRANSCRIPT_COUNT);
    for i in 0..TRANSCRIPT_COUNT {
        let id = format!("t{i:063}"); // deterministic 64-char id
        conn.execute(
            "INSERT INTO transcript
                (id, file_uuid, backend, language, duration_ms,
                 completed_at, created_at, updated_at, device_id, hlc)
             VALUES (?1, ?2, 'groq:test', 'en', 1000,
                     ?3, ?3, ?3, ?4, ?5)",
            params![&id, format!("file-{i}"), PROP_TS, PROP_DEV, PROP_HLC],
        )
        .expect("seed transcript");
        ids.push(id);
    }
    ids
}

fn insert_segment(conn: &Connection, seg_id: &str, transcript_id: &str, text: &str) {
    conn.execute(
        "INSERT INTO transcript_segment
            (id, transcript_id, start_ms, end_ms, text, confidence,
             created_at, updated_at, device_id, hlc)
         VALUES (?1, ?2, 0, 1000, ?3, NULL, ?4, ?4, ?5, ?6)",
        params![seg_id, transcript_id, text, PROP_TS, PROP_DEV, PROP_HLC],
    )
    .expect("insert segment");
}

fn soft_delete_segment(conn: &Connection, seg_id: &str) {
    conn.execute(
        "UPDATE transcript_segment
            SET deleted_at = ?1, updated_at = ?1
          WHERE id = ?2 AND deleted_at IS NULL",
        params![PROP_TS, seg_id],
    )
    .expect("soft delete segment");
}

fn restore_segment(conn: &Connection, seg_id: &str) {
    conn.execute(
        "UPDATE transcript_segment
            SET deleted_at = NULL, updated_at = ?1
          WHERE id = ?2 AND deleted_at IS NOT NULL",
        params![PROP_TS, seg_id],
    )
    .expect("restore segment");
}

fn soft_delete_transcript(conn: &Connection, transcript_id: &str) {
    conn.execute(
        "UPDATE transcript
            SET deleted_at = ?1, updated_at = ?1, hlc = ?2
          WHERE id = ?3 AND deleted_at IS NULL",
        params![PROP_TS, PROP_HLC, transcript_id],
    )
    .expect("soft delete transcript");
}

fn restore_transcript(conn: &Connection, transcript_id: &str) {
    conn.execute(
        "UPDATE transcript
            SET deleted_at = NULL, updated_at = ?1, hlc = ?2
          WHERE id = ?3 AND deleted_at IS NOT NULL",
        params![PROP_TS, PROP_HLC, transcript_id],
    )
    .expect("restore transcript");
}

fn edit_segment_text(conn: &Connection, seg_id: &str, new_text: &str) {
    conn.execute(
        "UPDATE transcript_segment
            SET text = ?1, updated_at = ?2
          WHERE id = ?3",
        params![new_text, PROP_TS, seg_id],
    )
    .expect("edit segment text");
}

// ---------------------------------------------------------------------------
// Ground-truth derivation
// ---------------------------------------------------------------------------

/// `(rowid, text)` pairs that SHOULD currently be present in
/// `transcript_search`. Computed directly from joined live state.
fn ground_truth(conn: &Connection) -> BTreeSet<(i64, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT s.rowid, s.text
             FROM transcript_segment s
             WHERE s.deleted_at IS NULL",
        )
        .expect("prepare ground truth");
    stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .expect("query ground truth")
        .filter_map(Result::ok)
        .collect()
}

/// Read `transcript_search` content. The FTS5 contentless-shadow tables
/// don't expose a clean `SELECT * FROM transcript_search`; instead we
/// query the join with the source table by rowid where the FTS index
/// MATCH-shaped lookup confirms presence. Simpler: enumerate all
/// `transcript_search` rowids via an FTS-internal query.
///
/// WHY this approach: `transcript_search` uses `content='transcript_segment'`,
/// meaning rows in the FTS5 index are keyed by `transcript_segment.rowid`
/// and the index stores the tokens of `text`. We fan from each segment's
/// rowid and probe whether a query matching ANY token of the original text
/// returns the row. False negatives here (a row's tokens fail to MATCH any
/// of their own words) would indicate a real index inconsistency — the
/// invariant we're testing.
fn actual_index(conn: &Connection) -> BTreeSet<(i64, String)> {
    // Iterate every segment row (live + soft-deleted) and probe whether
    // its rowid is reachable via FTS by matching a unique token from its
    // original text. If reachable, record (rowid, text).
    let mut stmt = conn
        .prepare(
            "SELECT s.rowid, s.text
             FROM transcript_segment s",
        )
        .expect("prepare segments");
    let segs: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .expect("query segments")
        .filter_map(Result::ok)
        .collect();
    drop(stmt);

    let mut out = BTreeSet::new();
    for (rowid, text) in segs {
        // Pick the first whitespace-tokenised word; FTS5 unicode61 tokens
        // are lowercase + alphanumeric subsets of words. If `text` is
        // empty (shouldn't happen given the seeded universe), skip.
        let probe_token = text.split_whitespace().next().unwrap_or("");
        if probe_token.is_empty() {
            continue;
        }
        // Quote token to avoid FTS5 query-syntax collisions on punctuation.
        let q = format!("\"{probe_token}\"");
        let hit: Option<i64> = conn
            .query_row(
                "SELECT rowid FROM transcript_search
                 WHERE rowid = ?1 AND text MATCH ?2",
                params![rowid, &q],
                |r| r.get(0),
            )
            .ok();
        if hit.is_some() {
            out.insert((rowid, text));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Op universe
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum TranscriptOp {
    InsertSegment(usize, usize),   // (transcript_idx, text_idx)
    SoftDeleteSegment(usize),      // segment seq idx
    RestoreSegment(usize),         // segment seq idx
    SoftDeleteTranscript(usize),   // transcript_idx (cascade)
    RestoreTranscript(usize),      // transcript_idx (cascade)
    EditSegmentText(usize, usize), // (segment seq idx, new text idx)
    /// Re-inserts a transcript header is not exercised — we seed all
    /// transcripts at startup. The op universe focuses on per-segment
    /// churn + transcript-level cascade arms, which is where the FTS5
    /// trigger correctness matters.
    Touch,
}

// ---------------------------------------------------------------------------
// Proptest
// ---------------------------------------------------------------------------

proptest::proptest! {
    // WHY cases=64: matches the FTS5 proptest cap from CLAUDE.md (#124).
    // Each case spawns a writer thread + opens 2 raw connections + runs
    // up to 25 ops with a per-op ground-truth comparison.
    #![proptest_config(proptest::test_runner::Config {
        cases: 64,
        ..proptest::test_runner::Config::default()
    })]

    /// **Invariant:** after EVERY op, `transcript_search` rows
    /// (incrementally maintained by FTS5 codegen triggers) MUST equal
    /// the ground-truth set computed directly from joined live state.
    #[test]
    fn transcript_search_matches_ground_truth_under_soft_delete_churn(
        ops in proptest::collection::vec(
            proptest::prop_oneof![
                proptest::strategy::Strategy::prop_map(
                    (0..TRANSCRIPT_COUNT, 0..SEGMENT_TEXTS.len()),
                    |(t, x)| TranscriptOp::InsertSegment(t, x),
                ),
                proptest::strategy::Strategy::prop_map(
                    0..16usize,
                    TranscriptOp::SoftDeleteSegment,
                ),
                proptest::strategy::Strategy::prop_map(
                    0..16usize,
                    TranscriptOp::RestoreSegment,
                ),
                proptest::strategy::Strategy::prop_map(
                    0..TRANSCRIPT_COUNT,
                    TranscriptOp::SoftDeleteTranscript,
                ),
                proptest::strategy::Strategy::prop_map(
                    0..TRANSCRIPT_COUNT,
                    TranscriptOp::RestoreTranscript,
                ),
                proptest::strategy::Strategy::prop_map(
                    (0..16usize, 0..SEGMENT_TEXTS.len()),
                    |(s, x)| TranscriptOp::EditSegmentText(s, x),
                ),
                proptest::strategy::Just(TranscriptOp::Touch),
            ],
            0..25,
        ),
    ) {
        let (_td, db, writer) = scratch_writer();
        let conn = seed_conn(&db);
        let transcript_ids = seed_transcripts(&conn);

        // Stable IDs for segments seeded by InsertSegment ops, in the order
        // they were inserted. Ops referencing a not-yet-existing segment
        // index become no-ops (cleaner than aborting a case mid-sequence).
        let mut segment_ids: Vec<String> = Vec::new();
        let mut next_seg_seq: u64 = 0;

        for op in &ops {
            match op {
                TranscriptOp::InsertSegment(t, x) => {
                    let seg_id = format!("s{next_seg_seq:063}");
                    next_seg_seq += 1;
                    insert_segment(
                        &conn,
                        &seg_id,
                        &transcript_ids[*t],
                        SEGMENT_TEXTS[*x],
                    );
                    segment_ids.push(seg_id);
                }
                TranscriptOp::SoftDeleteSegment(s) => {
                    if let Some(id) = segment_ids.get(*s) {
                        soft_delete_segment(&conn, id);
                    }
                }
                TranscriptOp::RestoreSegment(s) => {
                    if let Some(id) = segment_ids.get(*s) {
                        restore_segment(&conn, id);
                    }
                }
                TranscriptOp::SoftDeleteTranscript(t) => {
                    soft_delete_transcript(&conn, &transcript_ids[*t]);
                }
                TranscriptOp::RestoreTranscript(t) => {
                    restore_transcript(&conn, &transcript_ids[*t]);
                }
                TranscriptOp::EditSegmentText(s, x) => {
                    if let Some(id) = segment_ids.get(*s) {
                        edit_segment_text(&conn, id, SEGMENT_TEXTS[*x]);
                    }
                }
                TranscriptOp::Touch => { /* explicit no-op */ }
            }

            // Re-open RO conn to ensure WAL visibility of the writes
            // performed via the seed_conn — the FTS5 triggers fire
            // synchronously inside the same connection so seed_conn
            // already sees them, but the asserter uses an independent
            // connection to avoid any same-conn statement-cache effects.
            let ro = ro_conn(&db);
            let actual = actual_index(&ro);
            let expected = ground_truth(&ro);
            drop(ro);

            proptest::prop_assert_eq!(
                actual.clone(),
                expected.clone(),
                "transcript_search drifted from ground truth after op {:?} in sequence {:?} \
                 — actual={:?} expected={:?}",
                op, ops, actual, expected
            );
        }

        drop(conn);
        writer.join();
    }
}
