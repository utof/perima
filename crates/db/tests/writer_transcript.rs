//! Writer-cmd integration test for `WriteCmd::Transcript(TranscriptWriteCmd::Insert)`.
//!
//! Verifies:
//! - **Atomicity** — transcript + segments commit together; FTS5
//!   maintenance triggers fire and `transcript_search` is queryable.
//! - **Cancel rollback** — pre-cancelled token causes the writer to
//!   roll back without writing anything; reply is
//!   `Err(CoreError::Transcription(Cancelled))`.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use perima_core::transcription::TranscriptionError;
use perima_core::{CoreError, EventBus};
use perima_db::cmd::{ReplyTx, TranscriptWriteCmd, WriteCmd};
use perima_db::test_utils::NoopBus;
use perima_db::transcript_repo::{TranscriptId, TranscriptRow, TranscriptSegmentRow};
use perima_db::writer::SqliteWriter;

// ---------------------------------------------------------------------------
// Local helpers (kept here rather than in tests/common/mod.rs because no
// other transcript test exists yet — the proptest below seeds via raw SQL,
// not via the writer, so it doesn't need this scaffolding).
// ---------------------------------------------------------------------------

/// Build a tempfile-on-disk DB + writer actor. Mirrors `common::test_db()`
/// but without a `SearchRepository` (transcripts have their own search
/// surface; this test reads via a raw RO connection).
fn scratch_writer() -> (TempDir, std::path::PathBuf, perima_db::SqliteWriterHandle) {
    let td = tempfile::tempdir().expect("tempdir");
    let db_path = td.path().join("transcripts.db");
    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
    (td, db_path, writer)
}

/// Open a fresh read-only connection on the scratch DB. Avoids a second
/// writable handle (clippy.toml bans `Connection::open` outside a curated
/// allow-list per GH #131).
fn ro_conn(db_path: &std::path::Path) -> Connection {
    Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("ro conn open")
}

fn make_segment(start_ms: u32, end_ms: u32, text: &str) -> TranscriptSegmentRow {
    TranscriptSegmentRow {
        id: TranscriptId::new(),
        // transcript_id is overridden by the writer with the parent id;
        // the empty string here documents that intent.
        transcript_id: TranscriptId(String::new()),
        start_ms,
        end_ms,
        text: text.to_owned(),
        confidence: Some(0.9),
    }
}

/// Happy path: header + 3 segments commit together; the codegen FTS5
/// trigger fires on each segment INSERT (verified by `MATCH 'hello'`).
/// Atomicity-under-fault is verified by the cancel-rollback test below
/// and by `transcript_proptests::transcript_search_matches_ground_truth_under_soft_delete_churn`.
#[test]
fn insert_persists_transcript_and_segments_atomically() {
    let (_td, db_path, writer) = scratch_writer();

    let transcript_id = TranscriptId::new();
    let transcript = TranscriptRow {
        id: transcript_id.clone(),
        // FK to files.file_uuid is structurally NOT NULL but unenforced
        // (no FK constraint in the v1 schema). A fresh UUIDv7 is fine.
        file_uuid: uuid::Uuid::now_v7().to_string(),
        backend: "groq:whisper-large-v3-turbo".to_owned(),
        language: Some("en".to_owned()),
        duration_ms: 5_000,
    };
    let segments = vec![
        make_segment(0, 1500, "hello world"),
        make_segment(1500, 3000, "this is a test"),
        make_segment(3000, 5000, "of transcription"),
    ];

    let (reply_tx, reply_rx): (ReplyTx<TranscriptId>, _) = flume::bounded(1);
    writer
        .sender()
        .send(WriteCmd::Transcript(TranscriptWriteCmd::Insert {
            transcript,
            segments,
            device: "test-device-001".to_owned(),
            cancel: None,
            request_uuid: uuid::Uuid::now_v7().simple().to_string(),
            reply: reply_tx,
        }))
        .unwrap();

    let id = reply_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(id, transcript_id);

    // Verify rows exist + FTS index was populated by the codegen-installed
    // `transcript_search_after_segment_insert` trigger.
    let conn = ro_conn(&db_path);
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM transcript WHERE id = ?1",
            rusqlite::params![&transcript_id.0],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "transcript header row missing");

    let seg_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM transcript_segment WHERE transcript_id = ?1",
            rusqlite::params![&transcript_id.0],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(seg_count, 3, "expected 3 segment rows");

    // Verify the codegen FTS5 trigger fired: query `hello` should find one
    // segment containing "hello world".
    let fts_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM transcript_search WHERE text MATCH 'hello'",
            rusqlite::params![],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fts_count, 1, "FTS5 trigger did not fire");

    drop(conn);
    writer.join();
}

#[test]
fn insert_rolls_back_when_cancel_fires_before_first_insert() {
    let (_td, db_path, writer) = scratch_writer();

    let transcript = TranscriptRow {
        id: TranscriptId::new(),
        file_uuid: uuid::Uuid::now_v7().to_string(),
        backend: "groq:whisper-large-v3-turbo".to_owned(),
        language: None,
        duration_ms: 1_000,
    };
    let segments = vec![make_segment(0, 1000, "should not land")];

    // Pre-fire cancel so the post-BEGIN check trips immediately.
    let cancel = CancellationToken::new();
    cancel.cancel();

    let (reply_tx, reply_rx): (ReplyTx<TranscriptId>, _) = flume::bounded(1);
    writer
        .sender()
        .send(WriteCmd::Transcript(TranscriptWriteCmd::Insert {
            transcript,
            segments,
            device: "test-device-002".to_owned(),
            cancel: Some(cancel),
            request_uuid: uuid::Uuid::now_v7().simple().to_string(),
            reply: reply_tx,
        }))
        .unwrap();

    let result = reply_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        matches!(
            result,
            Err(CoreError::Transcription(TranscriptionError::Cancelled))
        ),
        "expected Err(Transcription(Cancelled)), got {result:?}",
    );

    // Verify nothing landed.
    let conn = ro_conn(&db_path);
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM transcript",
            rusqlite::params![],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "transcript row leaked despite cancel rollback");
    let seg_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM transcript_segment",
            rusqlite::params![],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        seg_count, 0,
        "transcript_segment row leaked despite cancel rollback"
    );

    drop(conn);
    writer.join();
}
