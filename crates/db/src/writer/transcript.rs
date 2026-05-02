//! Writer-side handler for [`crate::cmd::TranscriptWriteCmd`].
//!
//! Mirrors the [`crate::writer::metadata`] handler shape:
//! `(conn: &mut Connection, cmd: TranscriptWriteCmd, bus: &Arc<dyn EventBus>)`.
//!
//! Atomic transaction structure (per spec § "Writer-cmd handler shape"):
//! 1. `BEGIN IMMEDIATE`.
//! 2. Cancel-token check — if fired, ROLLBACK + reply `Cancelled`.
//! 3. Compute one HLC value via `Hlc::now()` (one HLC per command).
//! 4. INSERT into `transcript` with `device, hlc, now`.
//! 5. For each segment: cancel check; if fired ROLLBACK; else INSERT
//!    with the same `device, hlc, now`.
//! 6. FTS5 maintenance triggers (codegen-installed) fire automatically.
//! 7. COMMIT.
//! 8. Send `Ok(transcript.id)` via `reply`.
//! 9. Emit `AppEvent::IndexInvalidated { reason: SearchIndexRebuilt }`
//!    via `bus.emit`. (T5 will introduce a richer
//!    `AppEvent::TranscriptionCompleted` variant; today we use the
//!    closest existing v1 invalidation reason so query-cache consumers
//!    refresh transcript-search hits.)
//!
//! # Cancellation race
//!
//! The cancel token is the same one passed into `TranscribeRequest`; the
//! use-case threads it through unchanged. Checking inside the writer
//! transaction (after `BEGIN IMMEDIATE`, before each per-segment INSERT)
//! closes the cancel-after-adapter-success window between the
//! transcriber adapter returning `Ok` and the writer committing.

use std::sync::Arc;

use rusqlite::Connection;

use perima_core::transcription::TranscriptionError;
use perima_core::{AppEvent, CoreError, EventBus, Hlc, InvalidationReason};

use crate::cmd::TranscriptWriteCmd;
use crate::errors::Error;
use crate::transcript_repo::{TranscriptId, TranscriptRow, TranscriptSegmentRow};

/// Writer-side dispatch for [`TranscriptWriteCmd`]. Consumes the command
/// (the reply channel lives inside each variant) and sends the result
/// back on the caller's reply channel.
///
/// After a successful `COMMIT`, emits
/// [`AppEvent::IndexInvalidated`] with
/// [`InvalidationReason::SearchIndexRebuilt`] (the v1 invalidation
/// signal used until T5 adds `TranscriptionCompleted`).
#[allow(clippy::needless_pass_by_value)]
pub(super) fn handle(conn: &mut Connection, cmd: TranscriptWriteCmd, bus: &Arc<dyn EventBus>) {
    match cmd {
        TranscriptWriteCmd::Insert {
            transcript,
            segments,
            device,
            cancel,
            reply,
        } => {
            let result = insert_impl(conn, &transcript, &segments, &device, cancel.as_ref());
            let send_id = match &result {
                Ok((id, _events)) => Ok(id.clone()),
                Err(e) => Err(e.clone()),
            };
            // Send reply first; even if events fail to emit, the writer-cmd is settled.
            if reply.send(send_id).is_err() {
                tracing::debug!("transcript insert reply channel closed before send");
            }
            // Emit events after the COMMIT, only on success.
            if let Ok((_id, events)) = result {
                for event in &events {
                    if let Err(e) = bus.emit(event) {
                        tracing::warn!(?e, "post-commit emit failed for transcript insert");
                    }
                }
            }
        }
    }
}

/// Inner helper: opens `BEGIN IMMEDIATE`, INSERTs the transcript header,
/// loops over segments with cancel checks, COMMITs, and returns the
/// transcript id + events to emit.
fn insert_impl(
    conn: &mut Connection,
    transcript: &TranscriptRow,
    segments: &[TranscriptSegmentRow],
    device: &str,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(TranscriptId, Vec<AppEvent>), CoreError> {
    // WHY BEGIN IMMEDIATE: writes always grab the WRITE lock at
    // statement-start to avoid a SHARED→RESERVED upgrade race under
    // WAL. Consistent with every other write path in this crate.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    // Post-BEGIN cancel check. Closes the cancel-after-adapter-success
    // window — the adapter may have returned Ok before the use-case
    // observed a downstream cancel; we honour that cancel here even
    // though we hold the writer transaction.
    if let Some(token) = cancel
        && token.is_cancelled()
    {
        // Drop the transaction without commit (rollback on Drop is
        // safe; explicit rollback would also work but Drop is the
        // sibling-handler convention).
        drop(tx);
        return Err(CoreError::Transcription(TranscriptionError::Cancelled));
    }

    // One HLC value per command — same value stamped on every row
    // (spec §3.7 / Batch C "one HLC per user-visible logical event").
    let hlc = Hlc::now().pack();
    let now = chrono::Utc::now().to_rfc3339();

    tx.execute(
        "INSERT INTO transcript
            (id, file_uuid, backend, language, duration_ms,
             completed_at, created_at, updated_at, device_id, hlc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?6, ?7, ?8)",
        rusqlite::params![
            &transcript.id.0,
            &transcript.file_uuid,
            &transcript.backend,
            &transcript.language,
            transcript.duration_ms,
            &now,
            device,
            hlc,
        ],
    )
    .map_err(Error::from)?;

    for seg in segments {
        if let Some(token) = cancel
            && token.is_cancelled()
        {
            drop(tx);
            return Err(CoreError::Transcription(TranscriptionError::Cancelled));
        }
        tx.execute(
            "INSERT INTO transcript_segment
                (id, transcript_id, start_ms, end_ms, text, confidence,
                 created_at, updated_at, device_id, hlc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)",
            rusqlite::params![
                &seg.id.0,
                // Override seg.transcript_id with the canonical parent id —
                // domain `From<TranscriptSegment>` cannot know the eventual
                // transcript header id; the writer is the canonical source.
                &transcript.id.0,
                seg.start_ms,
                seg.end_ms,
                &seg.text,
                seg.confidence,
                &now,
                device,
                hlc,
            ],
        )
        .map_err(Error::from)?;
    }

    tx.commit().map_err(Error::from)?;

    // WHY SearchIndexRebuilt + not yet TranscriptionCompleted: the
    // dedicated AppEvent variant lands in T5 (along with its
    // bindings.ts + frontend invalidation hook). For T2, populating
    // SearchIndexRebuilt is the closest existing query-invalidation
    // signal that consumers of `transcript_search` rely on.
    let events = vec![AppEvent::IndexInvalidated {
        reason: InvalidationReason::SearchIndexRebuilt,
    }];

    Ok((transcript.id.clone(), events))
}
