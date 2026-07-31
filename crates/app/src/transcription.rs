//! Transcription use-case: orchestrates provider selection, audio
//! extraction, transcription, and atomic persistence. Owns the
//! single-worker FIFO queue.
//!
//! See spec § `UseCase — crates/app::transcription`.
//!
//! # Concurrency model
//!
//! - One [`tokio::spawn`]ed worker task lives for the lifetime of the
//!   use-case (and therefore the [`crate::AppContainer`]).
//! - Producer side: `execute(Start { .. })` builds an internal
//!   `QueueItem` and pushes it through `flume::bounded(32)`. Queue full
//!   → typed [`TranscriptionError::QueueFull`].
//! - Worker side: pops FIFO, emits [`AppEvent::TranscriptionStarted`],
//!   calls the active backend (sync — adapter bridges to async via
//!   `block_in_place` + [`tokio::runtime::Handle::block_on`]), stamps
//!   `UUIDv7` ids on segments, submits to the writer through
//!   `SqliteTranscriptRepository`, and relies on the writer to emit
//!   [`AppEvent::TranscriptionCompleted`] after `COMMIT`. Cancel /
//!   failure paths emit [`AppEvent::TranscriptionCancelled`] /
//!   [`AppEvent::TranscriptionFailed`] from the worker.
//!
//! WHY [`tokio::spawn`] inside [`TranscriptionUseCase::new`]: the use-case
//! is constructed by [`crate::AppContainer::new`], which both shells call
//! from inside an existing tokio runtime (Tauri's runtime in desktop;
//! `#[tokio::main]` in CLI). Tests constructing a use-case directly
//! MUST run on `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`
//! — `block_in_place` inside the cloud adapter requires the multi-thread
//! flavour.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use perima_core::CoreError;
use perima_core::events::{AppEvent, EventBus};
use perima_core::transcription::{TranscribeRequest, TranscriptionError, TranscriptionProgress};

use perima_db::transcript_repo::{
    SqliteTranscriptRepository, TranscriptId, TranscriptRow, TranscriptSegmentRow,
};
use perima_transcribe::registry::TranscriberRegistry;

/// Bound on the in-flight + queued job count.
///
/// Past this threshold the producer side returns
/// [`TranscriptionError::QueueFull`] so the UI can surface "queue is full"
/// as a typed error rather than a silent retry.
///
/// WHY 32: Cloud transcription typically runs 2-10s per request, so 32
/// buffers about 1-5 minutes of FIFO work without overwhelming a single
/// worker. The bound matches the spec's `flume::bounded(32)` figure.
pub const QUEUE_DEPTH: usize = 32;

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Caller commands for the use-case.
#[derive(Debug, Clone)]
pub enum TranscribeCommand {
    /// Start a new transcription job. The use-case mints a fresh
    /// `request_uuid` (`UUIDv7`) and returns it via
    /// [`TranscribeOutput::Started`].
    Start {
        /// File UUID (immutable surrogate per V011).
        file_uuid: String,
        /// Display name for UI surfacing (typically the file basename).
        file_name: String,
        /// Absolute path to the source media file.
        source: PathBuf,
        /// Optional language hint (BCP-47 short code, e.g. `"en"`).
        language_hint: Option<String>,
    },
    /// Cancel an in-flight or queued job by `request_uuid`.
    /// Idempotent — cancelling an unknown id is a no-op.
    Cancel {
        /// Request UUID returned by [`TranscribeOutput::Started`].
        request_uuid: String,
    },
}

/// Use-case output.
#[derive(Debug, Clone)]
pub enum TranscribeOutput {
    /// Job enqueued. `queue_position` is `1` when this is the only job.
    Started {
        /// Per-request `UUIDv7` (lowercase-hex simple form). Pairs with
        /// every `AppEvent::Transcription*` variant for this job.
        request_uuid: String,
        /// 1-based position in the queue at the moment of enqueue.
        queue_position: u32,
    },
    /// Cancel acknowledged (token fired). The job may still be mid-
    /// adapter-call — the worker observes the token and emits
    /// [`AppEvent::TranscriptionCancelled`] on the next checkpoint.
    Cancelled {
        /// Per-request `UUIDv7` echoed back from the request.
        request_uuid: String,
    },
}

// ---------------------------------------------------------------------------
// Internal queue item
// ---------------------------------------------------------------------------

/// One unit of work for the worker task.
#[derive(Debug)]
struct QueueItem {
    request_uuid: String,
    file_uuid: String,
    file_name: String,
    source: PathBuf,
    language_hint: Option<String>,
    cancel: CancellationToken,
}

// ---------------------------------------------------------------------------
// The use-case
// ---------------------------------------------------------------------------

/// Orchestrates the transcription pipeline. One per process; lives inside
/// [`crate::AppContainer`].
pub struct TranscriptionUseCase {
    queue: flume::Sender<QueueItem>,
    cancels: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl std::fmt::Debug for TranscriptionUseCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // WHY manual Debug: the `flume::Sender<QueueItem>` and the cancel
        // map are runtime objects with no useful textual representation.
        // The trait derive on the surrounding `AppContainer` needs us to
        // be Debug, so emit a stable type name.
        f.debug_struct("TranscriptionUseCase")
            .field("queue_capacity", &QUEUE_DEPTH)
            .finish_non_exhaustive()
    }
}

impl TranscriptionUseCase {
    /// Construct + spawn the worker. The worker holds its own `Arc`s to
    /// the registry, repo, and event bus.
    ///
    /// # Panics
    ///
    /// Must be called from within a tokio runtime context — `tokio::spawn`
    /// requires it. Both shells (CLI `#[tokio::main]`, Desktop via
    /// Tauri's runtime) satisfy this.
    #[must_use]
    pub fn new(
        registry: Arc<TranscriberRegistry>,
        repo: Arc<SqliteTranscriptRepository>,
        events: Arc<dyn EventBus>,
        device: String,
    ) -> Self {
        let (tx, rx) = flume::bounded(QUEUE_DEPTH);
        let cancels: Arc<Mutex<HashMap<String, CancellationToken>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Worker owns its own Arc clones — the use-case keeps the
        // sender + the cancel map for the producer-side `execute` calls.
        tokio::spawn(worker_loop(
            rx,
            registry,
            repo,
            events,
            Arc::clone(&cancels),
            device,
        ));

        Self { queue: tx, cancels }
    }

    /// Execute a [`TranscribeCommand`].
    ///
    /// `Start`: returns immediately with the new `request_uuid` and 1-based
    /// queue position. The worker picks up the job asynchronously and
    /// publishes `AppEvent::TranscriptionStarted` when it begins.
    ///
    /// `Cancel`: fires the cancel token. The cancel signal is checked at
    /// adapter checkpoints (per-segment + HTTP poll) AND inside the
    /// writer's transaction (post-`BEGIN IMMEDIATE`, pre-INSERT). Cancel
    /// for an unknown id is a no-op (idempotent).
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::Transcription`] wrapping
    /// [`TranscriptionError::QueueFull`] when the bounded queue is at
    /// capacity. Surface this to the user as "transcription queue full;
    /// try again later" — never silently retry.
    ///
    /// # Panics
    ///
    /// Panics if the internal `cancels` mutex is poisoned, which only
    /// happens if a previous holder panicked while holding the lock —
    /// treated as an unrecoverable bug rather than papered over.
    #[allow(clippy::cast_possible_truncation)]
    // WHY: queue.len() is bounded above by QUEUE_DEPTH (32) so the
    // usize -> u32 cast is loss-free.
    #[allow(clippy::unused_async)]
    // WHY allow unused_async: matches the established pattern across every
    // other UseCase::execute (search, tag, scan, dedup, …). Keeping the
    // signature async preserves forward-compat with adapters that may move
    // off `SqliteWriter::send` (sync) onto async write paths in a later
    // batch — callers don't need to migrate then.
    pub async fn execute(&self, cmd: TranscribeCommand) -> Result<TranscribeOutput, CoreError> {
        match cmd {
            TranscribeCommand::Start {
                file_uuid,
                file_name,
                source,
                language_hint,
            } => {
                let request_uuid = Uuid::now_v7().simple().to_string();
                let cancel = CancellationToken::new();
                self.cancels
                    .lock()
                    .expect("transcription cancels mutex poisoned")
                    .insert(request_uuid.clone(), cancel.clone());

                let queued = self.queue.len() as u32;
                self.queue
                    .try_send(QueueItem {
                        request_uuid: request_uuid.clone(),
                        file_uuid,
                        file_name,
                        source,
                        language_hint,
                        cancel,
                    })
                    .map_err(|_| {
                        // WHY remove the cancel entry on QueueFull: the job
                        // never enters the worker, so a future Cancel call
                        // for the same id would silently fail without this
                        // cleanup. We also avoid retaining dead entries.
                        let _ = self
                            .cancels
                            .lock()
                            .expect("transcription cancels mutex poisoned")
                            .remove(&request_uuid);
                        CoreError::Transcription(TranscriptionError::QueueFull { queued })
                    })?;
                Ok(TranscribeOutput::Started {
                    request_uuid,
                    queue_position: queued.saturating_add(1),
                })
            }
            TranscribeCommand::Cancel { request_uuid } => {
                // WHY pull the token out of the mutex before the if-let:
                // `clippy::significant_drop_in_scrutinee` flags holding the
                // MutexGuard temporary across the if-let body. Binding the
                // Option here drops the guard before token.cancel() runs.
                let token = self
                    .cancels
                    .lock()
                    .expect("transcription cancels mutex poisoned")
                    .remove(&request_uuid);
                if let Some(token) = token {
                    token.cancel();
                }
                Ok(TranscribeOutput::Cancelled { request_uuid })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

async fn worker_loop(
    rx: flume::Receiver<QueueItem>,
    registry: Arc<TranscriberRegistry>,
    repo: Arc<SqliteTranscriptRepository>,
    events: Arc<dyn EventBus>,
    cancels: Arc<Mutex<HashMap<String, CancellationToken>>>,
    device: String,
) {
    while let Ok(item) = rx.recv_async().await {
        // Snapshot remaining queue depth at dequeue + this job = live queue
        // size as observed by the worker. WHY: AppEvent::TranscriptionStarted.
        // queue_size docstring promises "current queue size including this
        // job"; rx.len() is jobs still waiting behind this one (post-recv).
        #[allow(clippy::cast_possible_truncation)]
        let queue_size = (rx.len() as u32).saturating_add(1);
        process_one(&item, queue_size, &registry, &repo, &events, &device);
        // Always remove from cancels map after terminal state so a stale
        // Cancel for a completed id is a true no-op (instead of firing a
        // dead token uselessly).
        let _ = cancels
            .lock()
            .expect("transcription cancels mutex poisoned")
            .remove(&item.request_uuid);
    }
    tracing::debug!("transcription worker loop exiting (queue closed)");
}

/// Process a single queued item end-to-end (start event → backend call →
/// persist → terminal event).
///
/// WHY a free function rather than a method: the worker only borrows
/// `Arc` clones, never `&self` of the use-case (the use-case is dropped
/// at container shutdown but the worker task may still be draining).
#[allow(clippy::cognitive_complexity)] // WHY: the linear pipeline reads top-to-bottom; splitting harms readability.
fn process_one(
    item: &QueueItem,
    queue_size: u32,
    registry: &Arc<TranscriberRegistry>,
    repo: &Arc<SqliteTranscriptRepository>,
    events: &Arc<dyn EventBus>,
    device: &str,
) {
    // 1. Emit the started event. Build first to dodge the
    //    "&AppEvent::Variant{...}" temporary-lifetime trap (spec §
    //    "UseCase").
    let started_event = AppEvent::TranscriptionStarted {
        request_uuid: item.request_uuid.clone(),
        file_uuid: item.file_uuid.clone(),
        file_name: item.file_name.clone(),
        queue_size,
    };
    if let Err(e) = events.emit(&started_event) {
        tracing::warn!(error = %e, "failed to emit TranscriptionStarted");
    }

    // 2. Resolve the active backend.
    let backend = match registry.active() {
        Ok(b) => b,
        Err(e) => {
            emit_failure(events, &item.request_uuid, &e);
            return;
        }
    };

    // 3. Build the request, threading the cancel token end-to-end.
    let request_uuid_for_progress = item.request_uuid.clone();
    let events_for_progress: Arc<dyn EventBus> = Arc::clone(events);
    let on_progress: Arc<dyn Fn(TranscriptionProgress) + Send + Sync> = Arc::new(move |p| {
        // WHY: only Segment + Heartbeat surface as progress events; the
        // Started/Finished progress signals are absorbed by the worker
        // (it emits its own start/terminal AppEvents).
        let event = match p {
            TranscriptionProgress::Segment {
                processed_ms,
                total_ms,
                ..
            } => AppEvent::TranscriptionProgress {
                request_uuid: request_uuid_for_progress.clone(),
                processed_ms,
                total_ms,
            },
            TranscriptionProgress::Heartbeat { elapsed } => {
                let processed_ms = u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX);
                AppEvent::TranscriptionProgress {
                    request_uuid: request_uuid_for_progress.clone(),
                    processed_ms,
                    total_ms: None,
                }
            }
            TranscriptionProgress::Started { .. } | TranscriptionProgress::Finished => return,
        };
        if let Err(e) = events_for_progress.emit(&event) {
            tracing::warn!(error = %e, "failed to emit TranscriptionProgress");
        }
    });

    let req = TranscribeRequest {
        source: item.source.clone(),
        language_hint: item.language_hint.clone(),
        cancel: item.cancel.clone(),
        on_progress,
        timeout: None,
    };

    // 4. Run the (sync) adapter call.
    let result = backend.transcribe(&req);

    let mut transcription = match result {
        Ok(t) => t,
        Err(e) => {
            emit_terminal_for_error(events, &item.request_uuid, &e);
            return;
        }
    };

    // 5a. Stamp UUIDv7 ids on segments. The adapter sets nil-UUIDs as
    //     placeholders so the use-case can mint stable ids without
    //     introducing a UUID dep in the adapter crate's hot loop.
    for seg in &mut transcription.segments {
        if seg.id == Uuid::nil() {
            seg.id = Uuid::now_v7();
        }
    }

    // 5b. Pre-write cancel check. Narrows the cancel-after-success window.
    //     The writer also re-checks inside its transaction (post-BEGIN
    //     IMMEDIATE, pre-INSERT) — together they fully close the race.
    if item.cancel.is_cancelled() {
        emit_cancelled(events, &item.request_uuid);
        return;
    }

    // 6. Build rows and submit to the writer.
    let transcript_row = TranscriptRow {
        id: TranscriptId::new(),
        file_uuid: item.file_uuid.clone(),
        backend: backend.id().0.clone(),
        language: transcription.language.clone(),
        duration_ms: transcription.duration_ms,
    };
    let segment_rows: Vec<TranscriptSegmentRow> = transcription
        .segments
        .iter()
        .map(|s| TranscriptSegmentRow {
            id: TranscriptId(s.id.simple().to_string()),
            // WHY empty: the writer overrides this with `transcript_row.id`
            // before binding (matches the From<TranscriptSegment> contract).
            transcript_id: TranscriptId(String::new()),
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            text: s.text.clone(),
            confidence: s.confidence,
        })
        .collect();

    match repo.insert_with_request_uuid(
        transcript_row,
        segment_rows,
        device.to_owned(),
        Some(item.cancel.clone()),
        item.request_uuid.clone(),
    ) {
        Ok(_) => {
            // The writer emitted `AppEvent::TranscriptionCompleted` itself
            // post-COMMIT (with the use-case's request_uuid threaded through
            // the writer-cmd payload). Nothing to do here.
        }
        Err(e) => emit_terminal_for_error(events, &item.request_uuid, &e),
    }
}

// ---------------------------------------------------------------------------
// Terminal-event helpers
// ---------------------------------------------------------------------------

fn emit_terminal_for_error(events: &Arc<dyn EventBus>, request_uuid: &str, e: &CoreError) {
    if matches!(e, CoreError::Transcription(TranscriptionError::Cancelled)) {
        emit_cancelled(events, request_uuid);
    } else {
        emit_failure(events, request_uuid, e);
    }
}

fn emit_cancelled(events: &Arc<dyn EventBus>, request_uuid: &str) {
    let event = AppEvent::TranscriptionCancelled {
        request_uuid: request_uuid.to_owned(),
    };
    if let Err(e) = events.emit(&event) {
        tracing::warn!(error = %e, "failed to emit TranscriptionCancelled");
    }
}

fn emit_failure(events: &Arc<dyn EventBus>, request_uuid: &str, e: &CoreError) {
    let event = AppEvent::TranscriptionFailed {
        request_uuid: request_uuid.to_owned(),
        error: extract_transcription_error(e),
    };
    if let Err(emit_err) = events.emit(&event) {
        tracing::warn!(error = %emit_err, original_error = %e, "failed to emit TranscriptionFailed");
    }
}

/// Lift a [`CoreError`] to a [`TranscriptionError`] for inclusion in the
/// `Failed` event. Non-transcription core errors are wrapped under
/// [`TranscriptionError::Internal`] so the frontend has one switch surface.
fn extract_transcription_error(e: &CoreError) -> TranscriptionError {
    match e {
        CoreError::Transcription(t) => t.clone(),
        other => TranscriptionError::Internal(other.to_string()),
    }
}
