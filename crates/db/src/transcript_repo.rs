//! `SQLite` adapter for transcripts. Owns the writer-cmd reply types,
//! row newtypes, and the `SqliteTranscriptRepository` reads-side.
//!
//! The matching writer-side handler lives at
//! `crates/db/src/writer/transcript.rs` (a private writer-actor module
//! whose entry point is invoked from the dispatch loop in
//! [`crate::writer::SqliteWriter`]).
//!
//! Introduced by the transcription v1 slice — see
//! `docs/superpowers/specs/2026-05-02-transcription-v1-design.md`.

use flume::Sender;
use uuid::Uuid;

use perima_core::CoreError;
use perima_core::transcription::TranscriptSegment;

use crate::cmd::{ReplyTx, TranscriptWriteCmd, WriteCmd};
use crate::pool::ReadPool;

/// Transcript ID newtype. The inner `String` is the `UUIDv7`'s
/// lowercase-hex `simple` form, matching the existing `Tag::id`,
/// `Volume::volume_id` conventions.
///
/// WHY no `serde` / `specta` derives on this side: the type is db-adapter
/// internal in the transcription v1 slice. T7 (Tauri commands) will mint
/// a separate IPC-side newtype derived from `perima_core` that carries
/// `serde::Serialize + specta::Type` once the read-path is wired.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TranscriptId(
    /// Lowercase-hex `UUIDv7`.
    pub String,
);

impl TranscriptId {
    /// Generate a fresh `UUIDv7` transcript ID.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7().simple().to_string())
    }
}

impl Default for TranscriptId {
    fn default() -> Self {
        Self::new()
    }
}

/// Row payload for inserting a `transcript` row. The writer stamps
/// `created_at`, `updated_at`, `hlc`; the caller provides everything else.
#[derive(Debug, Clone)]
pub struct TranscriptRow {
    /// `UUIDv7` (lowercase hex string).
    pub id: TranscriptId,
    /// FK to `files.file_uuid` (immutable surrogate per V011).
    pub file_uuid: String,
    /// Backend identifier in `provider:model` form
    /// (e.g. `groq:whisper-large-v3-turbo`).
    pub backend: String,
    /// Detected language (BCP-47 short code or `None`).
    pub language: Option<String>,
    /// Total duration of the source media in milliseconds.
    pub duration_ms: u32,
}

/// Row payload for inserting a `transcript_segment` row.
// WHY id: TranscriptId (not a separate TranscriptSegmentId): same UUIDv7-hex
// shape as the parent's id, single newtype keeps the SQL boundary simple. A
// future slice may split into a dedicated `TranscriptSegmentId` newtype if the
// type-confusion at construction sites starts biting.
#[derive(Debug, Clone)]
pub struct TranscriptSegmentRow {
    /// `UUIDv7`.
    pub id: TranscriptId,
    /// FK to `transcript.id`. The writer overrides this with the
    /// canonical parent transcript id from
    /// [`TranscriptWriteCmd::Insert::transcript`] before binding, so a
    /// caller that forgets to set it (e.g. when constructing via
    /// [`From<TranscriptSegment>`]) still produces consistent rows.
    pub transcript_id: TranscriptId,
    /// Start time in milliseconds since file start.
    pub start_ms: u32,
    /// End time in milliseconds since file start.
    pub end_ms: u32,
    /// Transcribed text.
    pub text: String,
    /// Confidence score `[0.0, 1.0]` if the backend exposes it.
    pub confidence: Option<f32>,
}

impl From<TranscriptSegment> for TranscriptSegmentRow {
    /// Convert a domain segment into a row, leaving `transcript_id`
    /// empty for the writer to populate from the canonical parent.
    fn from(seg: TranscriptSegment) -> Self {
        Self {
            id: TranscriptId(seg.id.simple().to_string()),
            // transcript_id is overridden by the writer at INSERT time.
            transcript_id: TranscriptId(String::new()),
            start_ms: seg.start_ms,
            end_ms: seg.end_ms,
            text: seg.text,
            confidence: seg.confidence,
        }
    }
}

/// Reads-side adapter for transcripts.
///
/// Mirrors the post-Batch-C `Sqlite*Repository` shape:
/// `(flume::Sender<WriteCmd>, ReadPool)`, both cheap to clone. Writes
/// build a [`TranscriptWriteCmd`] variant with a `flume::bounded(1)`
/// reply channel and block on the reply.
#[derive(Clone)]
pub struct SqliteTranscriptRepository {
    writer: Sender<WriteCmd>,
    #[allow(dead_code)]
    // WHY: reads-side queries land in T7 (Tauri commands); kept now to mirror sibling repo shape.
    reads: ReadPool,
}

impl std::fmt::Debug for SqliteTranscriptRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteTranscriptRepository")
            .finish_non_exhaustive()
    }
}

impl SqliteTranscriptRepository {
    /// Construct an adapter from a writer-command sender + a read pool.
    ///
    /// Migrations have already run inside [`crate::SqliteWriter::start`]
    /// before the read pool opens (spec §3.6).
    #[must_use]
    pub const fn new(writer: Sender<WriteCmd>, reads: ReadPool) -> Self {
        Self { writer, reads }
    }

    /// Insert a transcript + segments atomically via the writer.
    ///
    /// One HLC value per command stamped on every row (per Batch C
    /// "one HLC per user-visible logical event"). The writer uses
    /// `request_uuid` to populate `AppEvent::TranscriptionCompleted`'s
    /// correlation field after a successful `COMMIT`.
    ///
    /// # Errors
    /// Returns [`CoreError::Transcription`] (`Cancelled`) if `cancel`
    /// fires before or during the writer's transaction; returns
    /// `CoreError::Internal` for I/O / channel failures and rusqlite
    /// errors propagated as `Internal` from the writer.
    pub fn insert_with_request_uuid(
        &self,
        transcript: TranscriptRow,
        segments: Vec<TranscriptSegmentRow>,
        device: String,
        cancel: Option<tokio_util::sync::CancellationToken>,
        request_uuid: String,
    ) -> Result<TranscriptId, CoreError> {
        let (reply_tx, reply_rx): (ReplyTx<TranscriptId>, _) = flume::bounded(1);
        let cmd = WriteCmd::Transcript(TranscriptWriteCmd::Insert {
            transcript,
            segments,
            device,
            cancel,
            request_uuid,
            reply: reply_tx,
        });
        self.writer
            .send(cmd)
            .map_err(|_| CoreError::Internal("transcript writer disconnected".into()))?;
        reply_rx
            .recv()
            .map_err(|_| CoreError::Internal("transcript writer reply lost".into()))?
    }
}
