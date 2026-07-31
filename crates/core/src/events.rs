//! Filesystem event types and the `EventBus` trait.

use serde::Serialize;

use crate::{
    CoreError, FileUuid, MediaPath, VolumeId,
    dedup::{BatchId, FullHashOutcome},
    transcription::TranscriptionError,
};

/// A filesystem event detected by the watcher.
///
/// WHY `tag = "type"` (inline, no content key): matches the pre-Batch-D
/// `FileEventPayload` mirror in `crates/desktop/src/events.rs`, keeping the
/// frontend's `'file-event'` channel listener byte-compatible. `CoreError` uses
/// `tag = "kind", content = "data"` — a different shape intentionally, because
/// `CoreError` is a Result error type while `FileEvent` is a v1-frozen channel payload.
///
/// WHY `file_uuid: Option<FileUuid>` (Task 11, spec §4.8): the watcher emits
/// `Created` events BEFORE the file enters the DB (no `file_uuid` exists yet),
/// so the field is optional. `Modified` / `Deleted` / `Renamed` events also
/// pass `None` from the current emitter — consumers do their own `(volume,
/// path)` lookup to find the row. A future enhancement may populate the field
/// from a DB lookup at emission time. Consumers migrate at their own pace.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum FileEvent {
    /// A new file appeared at this path.
    Created {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
        /// Stable file id, when known. `None` for newly-discovered files.
        file_uuid: Option<FileUuid>,
    },
    /// An existing file's content was modified.
    Modified {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
        /// Stable file id, when known.
        file_uuid: Option<FileUuid>,
    },
    /// A file was deleted from this path.
    Deleted {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
        /// Stable file id, when known.
        file_uuid: Option<FileUuid>,
    },
    /// A file was renamed/moved within the same volume.
    Renamed {
        /// Previous relative path.
        from: MediaPath,
        /// New relative path.
        to: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
        /// Stable file id, when known.
        file_uuid: Option<FileUuid>,
    },
}

/// Application-level event broadcast through the bus.
///
/// Wraps the existing [`FileEvent`] for filesystem-watcher events and
/// adds two domain events emitted by the writer / use-case layer.
///
/// Wire shape: `#[serde(tag = "kind", content = "data")]` produces a
/// TypeScript discriminated union the frontend pattern-matches on.
/// Inner `FileEvent` keeps its own `#[serde(tag = "type")]` inline
/// shape (Batch D D-4 invariant) — wraps cleanly inside `AppEvent::File`.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", content = "data")]
pub enum AppEvent {
    /// Filesystem-watcher event (Created/Modified/Deleted/Renamed).
    File(FileEvent),

    /// Emitted by `ScanUseCase::execute` after a successful scan.
    /// Frontend triggers an immediate refetch (no debounce).
    ScanCompleted {
        /// Volume the scan ran against.
        volume: VolumeId,
        /// Total files seen by the walker (incl. existing).
        files_seen: u64,
        /// New files inserted since last scan.
        files_new: u64,
        /// Wall-clock duration of the scan.
        duration_ms: u64,
    },

    /// Emitted by the writer actor after a successful `WriteCmd` that
    /// changes a query-relevant index.
    IndexInvalidated {
        /// Which index category was invalidated.
        reason: InvalidationReason,
    },

    /// Emitted by the full-hash worker after each file completes.
    /// Frontend uses `batch_id` to correlate with the `BatchHandle`
    /// returned by `compute_full_hash_batch`.
    VerifyProgress {
        /// The batch this event belongs to.
        batch_id: BatchId,
        /// Number of files completed so far (including this one).
        files_done: u32,
        /// Total files in the batch.
        files_total: u32,
        /// Outcome for the file just processed.
        latest_outcome: FullHashOutcome,
    },

    /// Emitted by the full-hash worker when all files in a batch have
    /// been processed (successfully or not).
    VerifyComplete {
        /// The batch that has finished.
        batch_id: BatchId,
    },

    /// Transcription job has been picked up by the worker and is starting.
    ///
    /// Emitted once per request immediately before the adapter call. The
    /// frontend uses this to flip the per-file UI from "queued" to "running"
    /// without polling.
    TranscriptionStarted {
        /// Per-request `UUIDv7` (lowercase-hex, simple form). Pairs with the
        /// `request_uuid` returned by `TranscribeOutput::Started`.
        request_uuid: String,
        /// File the job is transcribing (immutable surrogate).
        file_uuid: String,
        /// Display name for UI surfacing (file basename or curated label).
        file_name: String,
        /// Current queue size including this job (1 = this is the only job).
        queue_size: u32,
    },

    /// Mid-flight transcription progress.
    ///
    /// Driven by adapter `TranscriptionProgress::Segment` and `Heartbeat`
    /// callbacks. `processed_ms` is the cumulative ms of source media
    /// finalized so far; `total_ms` may be `None` for backends that do not
    /// publish a duration estimate up front.
    TranscriptionProgress {
        /// Per-request `UUIDv7`.
        request_uuid: String,
        /// Cumulative milliseconds of source media processed.
        processed_ms: u32,
        /// Total source duration in milliseconds, when known.
        total_ms: Option<u32>,
    },

    /// Transcription job completed successfully; transcript persisted.
    ///
    /// Emitted by the writer-cmd handler AFTER `COMMIT`. The frontend uses
    /// `transcript_id` to refetch the freshly-inserted row and `request_uuid`
    /// to dismiss the matching in-flight slot in its job map.
    TranscriptionCompleted {
        /// Per-request `UUIDv7` (threaded from the use-case via
        /// `WriteCmd::Transcript(TranscriptWriteCmd::Insert)`).
        request_uuid: String,
        /// Persisted transcript header row id (`UUIDv7` simple-hex).
        transcript_id: String,
        /// File the transcript belongs to (immutable surrogate).
        file_uuid: String,
        /// Number of segment rows written.
        segment_count: u32,
        /// Detected language (BCP-47 short code) when the backend reports one.
        language: Option<String>,
    },

    /// Transcription job cancelled by user (token fired before commit).
    TranscriptionCancelled {
        /// Per-request `UUIDv7`.
        request_uuid: String,
    },

    /// Transcription job failed (non-cancel terminal error).
    ///
    /// Carries the full [`TranscriptionError`] so the frontend can surface
    /// discriminant payloads such as `RateLimited.retry_after_secs` or
    /// `FileTooLarge.limit_bytes` without a lossy re-stringification.
    TranscriptionFailed {
        /// Per-request `UUIDv7`.
        request_uuid: String,
        /// The terminal error that ended the job.
        error: TranscriptionError,
    },
}

/// Categorical reason an index was invalidated.
///
/// Kept coarse in v1 — Batch H may split into per-row variants once
/// `TanStack` Query lands and profiling shows surgical invalidation pays.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum InvalidationReason {
    /// Tag attach/detach.
    TagsChanged,
    /// File location upsert / status change.
    FilesChanged,
    /// Media metadata extraction or attach.
    MetadataChanged,
    /// FTS5 rebuild.
    SearchIndexRebuilt,
    /// Collision groups changed (quick-hash or full-hash dedup result updated).
    CollisionsChanged,
}

/// Consumer of application events.
///
/// Multiple implementations can be composed via a fan-out adapter
/// (e.g., `CompositeEventBus`). The composite logs errors from
/// individual handlers but does not abort — remaining handlers
/// still fire.
pub trait EventBus: Send + Sync {
    /// Publish an event onto the bus. Implementations are expected to
    /// be cheap (clone + push to channel); slow work happens in the
    /// async `EventHandler` tasks spawned by `AppContainer::new`.
    ///
    /// # Errors
    /// Returns `CoreError` if the publish fails. The production `Bus`
    /// (in `crates/app::bus`) returns `Ok(())` on capacity-Full (logs
    /// a warning instead) and on Closed (shutdown path).
    fn emit(&self, event: &AppEvent) -> Result<(), CoreError>;
}
