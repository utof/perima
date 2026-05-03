//! Use-case tests for [`perima_app::TranscriptionUseCase`].
//!
//! Strategy: build a `TranscriberRegistry` containing a `FakeTranscriber`
//! whose behaviour the test harness controls (Ok / Err / sleep + observe
//! cancel). All four scenarios from the plan are covered:
//!
//! - **Happy path** — Ok returned; events fire `Started` → (writer-emitted)
//!   `Completed`; rows persist.
//! - **Cancel mid-flight** — adapter sleeps; test fires the cancel token;
//!   adapter returns `Cancelled`; `TranscriptionCancelled` is observed.
//! - **Queue overflow** — fire `QUEUE_DEPTH + 1` jobs; the (n+1)-th
//!   `Start` returns typed `QueueFull`.
//! - **Error mapping** — adapter returns `Auth`; `TranscriptionFailed`
//!   carries the same discriminant.
//!
//! The use-case spawns its worker with `tokio::spawn`, so every test runs on
//! `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]` per the
//! plan's standing constraint (`block_in_place` requires multi-thread).

#![allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::TempDir;
use tokio::time::timeout;

use perima_app::{TranscribeCommand, TranscribeOutput, TranscriptionUseCase, transcription};
use perima_core::transcription::{
    BackendId, TranscribeRequest, Transcriber, TranscriptSegment, TranscriptionError,
    TranscriptionResult,
};
use perima_core::{AppEvent, CoreError, EventBus};
use perima_db::transcript_repo::SqliteTranscriptRepository;
use perima_db::{ReadPool, SqliteWriter, SqliteWriterHandle};
use perima_transcribe::registry::TranscriberRegistry;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

/// Records every emitted [`AppEvent`] in insertion order.
#[derive(Default)]
struct RecordingBus {
    events: Mutex<Vec<AppEvent>>,
}

impl EventBus for RecordingBus {
    fn emit(&self, e: &AppEvent) -> Result<(), CoreError> {
        self.events.lock().unwrap().push(e.clone());
        Ok(())
    }
}

impl RecordingBus {
    fn snapshot(&self) -> Vec<AppEvent> {
        self.events.lock().unwrap().clone()
    }
}

/// Behaviour the [`FakeTranscriber`] follows on each `transcribe` call.
#[derive(Clone)]
enum Behaviour {
    /// Return a single-segment success result immediately.
    Ok,
    /// Return the given error.
    Err(TranscriptionError),
    /// Sleep up to `max` checking `cancel.is_cancelled()` every 25 ms;
    /// if cancel fires, return `Cancelled`. If the sleep finishes
    /// without cancel, return Ok (the test should not let this happen).
    SleepThenCancel { max: Duration },
}

/// Stub Transcriber whose response is fixed at construction time.
struct FakeTranscriber {
    id: BackendId,
    behaviour: Behaviour,
}

impl FakeTranscriber {
    fn new(provider: &str, behaviour: Behaviour) -> Self {
        Self {
            id: BackendId(format!("{provider}:fake-model")),
            behaviour,
        }
    }
}

impl Transcriber for FakeTranscriber {
    fn id(&self) -> &BackendId {
        &self.id
    }

    fn accepts(&self, _mime: &str) -> bool {
        true
    }

    fn transcribe(&self, req: &TranscribeRequest) -> Result<TranscriptionResult, CoreError> {
        match &self.behaviour {
            Behaviour::Ok => Ok(TranscriptionResult {
                language: Some("en".to_owned()),
                duration_ms: 1_500,
                segments: vec![TranscriptSegment {
                    // Nil; the use-case stamps a real UUIDv7.
                    id: Uuid::nil(),
                    start_ms: 0,
                    end_ms: 1_500,
                    text: "hello world".to_owned(),
                    confidence: Some(0.95),
                }],
                backend: self.id.clone(),
            }),
            Behaviour::Err(e) => Err(CoreError::Transcription(e.clone())),
            Behaviour::SleepThenCancel { max } => {
                let deadline = std::time::Instant::now() + *max;
                while std::time::Instant::now() < deadline {
                    if req.cancel.is_cancelled() {
                        return Err(CoreError::Transcription(TranscriptionError::Cancelled));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                // Should not happen under the cancel test (max is 5s, the
                // test fires cancel within 100 ms).
                Ok(TranscriptionResult {
                    language: None,
                    duration_ms: 0,
                    segments: vec![],
                    backend: self.id.clone(),
                })
            }
        }
    }
}

/// Build a fresh on-disk DB + writer + transcript repo. The writer handle
/// is returned so the test can keep it alive past the use-case construction.
///
/// `writer_bus` is also handed to `SqliteWriter::start` so post-COMMIT
/// `TranscriptionCompleted` events surface on it (every test passes the
/// same bus it later inspects).
fn db_harness_with_bus(
    writer_bus: Arc<dyn EventBus>,
) -> (TempDir, SqliteWriterHandle, Arc<SqliteTranscriptRepository>) {
    let td = tempfile::tempdir().unwrap();
    let db_path = td.path().join("perima.db");
    let writer = SqliteWriter::start(&db_path, writer_bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = Arc::new(SqliteTranscriptRepository::new(writer.sender(), reads));
    (td, writer, repo)
}

/// Local `EventBus` that drops every event. Mirrors
/// `perima_db::test_utils::NoopBus` (which is gated behind the
/// `test-utils` feature, not enabled in `perima-app`).
struct LocalNoopBus;

impl EventBus for LocalNoopBus {
    fn emit(&self, _: &AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

/// Convenience: writer-bus is a fresh no-op bus (drops all events).
fn db_harness() -> (TempDir, SqliteWriterHandle, Arc<SqliteTranscriptRepository>) {
    db_harness_with_bus(Arc::new(LocalNoopBus))
}

/// Build a registry pre-loaded with one fake under `provider:fake-model`,
/// set as active.
fn registry_with_fake(provider: &str, behaviour: Behaviour) -> Arc<TranscriberRegistry> {
    let mut registry = TranscriberRegistry::new();
    let fake = Arc::new(FakeTranscriber::new(provider, behaviour));
    let id = fake.id().clone();
    registry.register(fake);
    registry.set_active(id).unwrap();
    Arc::new(registry)
}

/// Wait up to `timeout_ms` for the recording bus to surface an event matching
/// `pred`. Returns the matched event (cloned).
async fn wait_for_event<F>(bus: &RecordingBus, pred: F, timeout_ms: u64) -> Option<AppEvent>
where
    F: Fn(&AppEvent) -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if let Some(e) = bus.snapshot().into_iter().find(|e| pred(e)) {
            return Some(e);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Happy path: Start a job; the worker emits `TranscriptionStarted`, the
/// writer emits `TranscriptionCompleted` after persistence.
///
/// WHY pass `bus` to BOTH the writer AND the use-case: the
/// `TranscriptionCompleted` event is emitted from inside the writer thread
/// post-COMMIT (carries the use-case's `request_uuid` threaded via
/// `WriteCmd::Transcript::Insert::request_uuid`). To observe both
/// `TranscriptionStarted` (worker emit) and `TranscriptionCompleted`
/// (writer emit) on a single recording surface, both sites use the same
/// `RecordingBus`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_path_emits_started_then_completed() {
    let bus: Arc<RecordingBus> = Arc::new(RecordingBus::default());
    let (_td, _writer, repo) = db_harness_with_bus(Arc::clone(&bus) as Arc<dyn EventBus>);
    let registry = registry_with_fake("groq", Behaviour::Ok);

    let use_case = TranscriptionUseCase::new(
        registry,
        repo,
        Arc::clone(&bus) as Arc<dyn EventBus>,
        "test-device-001".to_owned(),
    );

    let out = use_case
        .execute(TranscribeCommand::Start {
            file_uuid: Uuid::now_v7().simple().to_string(),
            file_name: "happy.m4a".to_owned(),
            source: std::path::PathBuf::from("/dev/null"),
            language_hint: None,
        })
        .await
        .unwrap();
    let request_uuid = match out {
        TranscribeOutput::Started {
            request_uuid,
            queue_position,
        } => {
            assert_eq!(queue_position, 1, "first job queue_position should be 1");
            request_uuid
        }
        other => panic!("expected Started, got {other:?}"),
    };

    let started = wait_for_event(
        &bus,
        |e| matches!(e, AppEvent::TranscriptionStarted { request_uuid: r, .. } if r == &request_uuid),
        2_000,
    )
    .await;
    assert!(started.is_some(), "TranscriptionStarted not observed");

    let completed = wait_for_event(
        &bus,
        |e| matches!(e, AppEvent::TranscriptionCompleted { request_uuid: r, .. } if r == &request_uuid),
        5_000,
    )
    .await;
    let Some(AppEvent::TranscriptionCompleted {
        segment_count,
        language,
        ..
    }) = completed
    else {
        panic!(
            "TranscriptionCompleted not observed; bus saw {:?}",
            bus.snapshot()
        );
    };
    assert_eq!(segment_count, 1, "expected 1 segment");
    assert_eq!(language.as_deref(), Some("en"));
}

/// Cancel mid-flight: the adapter sleeps observing the cancel token; the
/// test fires Cancel; the adapter returns Cancelled; the worker emits
/// `TranscriptionCancelled`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_mid_flight_emits_transcription_cancelled() {
    let (_td, _writer, repo) = db_harness();
    let bus: Arc<RecordingBus> = Arc::new(RecordingBus::default());
    let registry = registry_with_fake(
        "groq",
        Behaviour::SleepThenCancel {
            max: Duration::from_secs(5),
        },
    );

    let use_case = TranscriptionUseCase::new(
        registry,
        repo,
        Arc::clone(&bus) as Arc<dyn EventBus>,
        "test-device-002".to_owned(),
    );

    let out = use_case
        .execute(TranscribeCommand::Start {
            file_uuid: Uuid::now_v7().simple().to_string(),
            file_name: "cancel.m4a".to_owned(),
            source: std::path::PathBuf::from("/dev/null"),
            language_hint: None,
        })
        .await
        .unwrap();
    let request_uuid = match out {
        TranscribeOutput::Started { request_uuid, .. } => request_uuid,
        other => panic!("expected Started, got {other:?}"),
    };

    // Wait for the worker to emit Started before cancelling so we know it
    // entered the adapter.
    wait_for_event(
        &bus,
        |e| matches!(e, AppEvent::TranscriptionStarted { request_uuid: r, .. } if r == &request_uuid),
        2_000,
    )
    .await
    .expect("Started event should fire");

    // Fire the cancel.
    use_case
        .execute(TranscribeCommand::Cancel {
            request_uuid: request_uuid.clone(),
        })
        .await
        .unwrap();

    let cancelled = wait_for_event(
        &bus,
        |e| matches!(e, AppEvent::TranscriptionCancelled { request_uuid: r } if r == &request_uuid),
        5_000,
    )
    .await;
    assert!(
        cancelled.is_some(),
        "TranscriptionCancelled not observed; bus saw {:?}",
        bus.snapshot()
    );
}

/// Queue overflow: enqueue `QUEUE_DEPTH + 1` jobs as fast as possible. The
/// (n+1)-th `Start` must return `Err(Transcription(QueueFull))`.
///
/// Strategy: use a fake transcriber that BLOCKS the worker by holding it
/// in a sync wait until a test-controlled `Arc<AtomicBool>` flips. This
/// keeps the worker pinned long enough for the producer to fill the
/// queue without sleeping for an arbitrary wall-clock duration.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_overflow_returns_queue_full() {
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Block until `release` flips to true, observing `cancel`.
    struct BlockingTranscriber {
        id: BackendId,
        release: Arc<AtomicBool>,
    }

    impl Transcriber for BlockingTranscriber {
        fn id(&self) -> &BackendId {
            &self.id
        }
        fn accepts(&self, _: &str) -> bool {
            true
        }
        fn transcribe(&self, req: &TranscribeRequest) -> Result<TranscriptionResult, CoreError> {
            while !self.release.load(Ordering::SeqCst) {
                if req.cancel.is_cancelled() {
                    return Err(CoreError::Transcription(TranscriptionError::Cancelled));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(TranscriptionResult {
                language: None,
                duration_ms: 0,
                segments: vec![],
                backend: self.id.clone(),
            })
        }
    }

    let (_td, _writer, repo) = db_harness();
    let bus: Arc<RecordingBus> = Arc::new(RecordingBus::default());
    let release = Arc::new(AtomicBool::new(false));
    let mut registry = TranscriberRegistry::new();
    let backend: Arc<dyn Transcriber> = Arc::new(BlockingTranscriber {
        id: BackendId("groq:fake-model".to_owned()),
        release: Arc::clone(&release),
    });
    let id = backend.id().clone();
    registry.register(backend);
    registry.set_active(id).unwrap();

    let use_case = TranscriptionUseCase::new(
        Arc::new(registry),
        repo,
        Arc::clone(&bus) as Arc<dyn EventBus>,
        "test-device-003".to_owned(),
    );

    // Producer-side: hammer Start until QueueFull surfaces. With one job
    // in-flight (worker blocked) + QUEUE_DEPTH queued, the
    // QUEUE_DEPTH + 2 attempt must trip QueueFull.
    let mut queue_full_seen = false;
    let mut last_queued = 0;
    for i in 0..(transcription::QUEUE_DEPTH + 4) {
        let r = use_case
            .execute(TranscribeCommand::Start {
                file_uuid: Uuid::now_v7().simple().to_string(),
                file_name: format!("job-{i}.m4a"),
                source: std::path::PathBuf::from("/dev/null"),
                language_hint: None,
            })
            .await;
        match r {
            Ok(_) => {
                last_queued += 1;
            }
            Err(CoreError::Transcription(TranscriptionError::QueueFull { .. })) => {
                queue_full_seen = true;
                break;
            }
            Err(other) => panic!("unexpected error from Start: {other:?}"),
        }
    }
    assert!(
        queue_full_seen,
        "QueueFull never surfaced; only {last_queued} jobs accepted (QUEUE_DEPTH={})",
        transcription::QUEUE_DEPTH
    );

    // Cleanup: release the blocking worker so test shutdown is fast.
    release.store(true, Ordering::SeqCst);
    // Give the worker a beat to drain any remaining queued items.
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(use_case);
}

/// Error mapping: the adapter returns `Auth`; the worker emits
/// `TranscriptionFailed { error: Auth }`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_error_emits_transcription_failed_with_same_discriminant() {
    let (_td, _writer, repo) = db_harness();
    let bus: Arc<RecordingBus> = Arc::new(RecordingBus::default());
    let registry = registry_with_fake("groq", Behaviour::Err(TranscriptionError::Auth));

    let use_case = TranscriptionUseCase::new(
        registry,
        repo,
        Arc::clone(&bus) as Arc<dyn EventBus>,
        "test-device-004".to_owned(),
    );

    let out = use_case
        .execute(TranscribeCommand::Start {
            file_uuid: Uuid::now_v7().simple().to_string(),
            file_name: "failing.m4a".to_owned(),
            source: std::path::PathBuf::from("/dev/null"),
            language_hint: None,
        })
        .await
        .unwrap();
    let request_uuid = match out {
        TranscribeOutput::Started { request_uuid, .. } => request_uuid,
        other => panic!("expected Started, got {other:?}"),
    };

    let failed = wait_for_event(
        &bus,
        |e| matches!(e, AppEvent::TranscriptionFailed { request_uuid: r, .. } if r == &request_uuid),
        5_000,
    )
    .await;
    let Some(AppEvent::TranscriptionFailed { error, .. }) = failed else {
        panic!(
            "TranscriptionFailed not observed; bus saw {:?}",
            bus.snapshot()
        );
    };
    assert!(
        matches!(error, TranscriptionError::Auth),
        "expected Auth, got {error:?}"
    );
}

/// Smoke test: cancelling an unknown request_uuid is idempotent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_unknown_request_uuid_is_idempotent() {
    let (_td, _writer, repo) = db_harness();
    let bus: Arc<RecordingBus> = Arc::new(RecordingBus::default());
    let registry = registry_with_fake("groq", Behaviour::Ok);

    let use_case = TranscriptionUseCase::new(
        registry,
        repo,
        Arc::clone(&bus) as Arc<dyn EventBus>,
        "test-device-005".to_owned(),
    );

    let out = timeout(
        Duration::from_secs(2),
        use_case.execute(TranscribeCommand::Cancel {
            request_uuid: "deadbeef".to_owned(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(out, TranscribeOutput::Cancelled { .. }));
}
