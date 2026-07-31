//! End-to-end-ish tests for [`OpenAICompatibleTranscriber`] using wiremock
//! to stub the upstream HTTP server.
//!
//! Multi-thread runtime required: the adapter uses `block_in_place` +
//! `Handle::block_on` to bridge the sync `Transcriber` trait to the async
//! `async-openai` API; `block_in_place` panics on the current-thread
//! scheduler, and the constructor's flavor check rejects it loudly.
//!
//! Because the trait method itself is sync, we wrap each call in
//! `tokio::task::spawn_blocking` so the adapter's internal `block_on`
//! has its own thread to park.

#![allow(clippy::unwrap_used)]
#![allow(clippy::print_stdout)]

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use perima_core::CoreError;
use perima_core::transcription::{
    TranscribeRequest, Transcriber, TranscriptionError, TranscriptionProgress,
};
use perima_transcribe::audio::{AudioError, AudioPipeline};
use perima_transcribe::openai_compat::OpenAICompatibleTranscriber;
use perima_transcribe::providers::{AuthScheme, ProviderPreset};

/// Audio pipeline that just copies its input to a tempfile. Lets us assert
/// the wiremock body without actually invoking ffmpeg.
struct StubAudioPipeline;

impl AudioPipeline for StubAudioPipeline {
    fn remux_for_upload(
        &self,
        input: &std::path::Path,
        _cancel: &CancellationToken,
    ) -> Result<tempfile::NamedTempFile, AudioError> {
        let mut output = tempfile::NamedTempFile::with_suffix(".opus").unwrap();
        std::io::copy(&mut std::fs::File::open(input).unwrap(), &mut output).unwrap();
        Ok(output)
    }
}

/// Synthesize a 100 ms 16 kHz mono silent WAV via hound. Returned path
/// lives in the OS temp dir; tests that care about cleanup can remove it,
/// but for a 3.2 KiB file it's not worth the ceremony.
fn small_test_audio() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("transcribe-test-{}.wav", uuid::Uuid::now_v7()));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&path, spec).unwrap();
    for _ in 0..1_600 {
        writer.write_sample(0_i16).unwrap();
    }
    writer.finalize().unwrap();
    path
}

fn verbose_json_response() -> serde_json::Value {
    serde_json::json!({
        "task": "transcribe",
        "language": "en",
        "duration": 0.1_f32,
        "text": "test",
        "segments": [{
            "id": 0,
            "seek": 0,
            "start": 0.0_f32,
            "end": 0.1_f32,
            "text": "test",
            "tokens": [],
            "temperature": 0.0_f32,
            "avg_logprob": -0.1_f32,
            "compression_ratio": 1.0_f32,
            "no_speech_prob": 0.0_f32,
        }],
        // WHY usage block: async-openai 0.36's
        // CreateTranscriptionResponseVerboseJson types `usage` as
        // non-Option (TranscriptTextUsageDuration). Servers older than
        // that schema may omit it, but the typed wrapper requires it.
        "usage": {
            "type": "duration",
            "seconds": 0.1_f32,
        }
    })
}

fn preset_for(server: &MockServer) -> ProviderPreset {
    // WHY String::leak: ProviderPreset uses `&'static str` for base_url to
    // keep the public preset table allocation-free. Tests need a runtime
    // base_url; leaking the small URL string for the duration of the
    // process is acceptable in the test binary (process exits in seconds).
    ProviderPreset {
        name: "test",
        base_url: server.uri().leak(),
        default_model: "whisper-1",
        file_size_limit_bytes: 25_000_000,
        auth_scheme: AuthScheme::Bearer,
    }
}

fn build_request(
    source: std::path::PathBuf,
    progress_calls: &Arc<std::sync::Mutex<Vec<String>>>,
) -> TranscribeRequest {
    let progress_clone = Arc::clone(progress_calls);
    let on_progress: Arc<dyn Fn(TranscriptionProgress) + Send + Sync> =
        Arc::new(move |p| progress_clone.lock().unwrap().push(format!("{p:?}")));
    TranscribeRequest {
        source,
        language_hint: Some("en".to_owned()),
        cancel: CancellationToken::new(),
        on_progress,
        timeout: Some(Duration::from_secs(10)),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn happy_path_returns_segments_and_progress_lifecycle() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(verbose_json_response()))
        .mount(&server)
        .await;

    let preset = preset_for(&server);
    let runtime = tokio::runtime::Handle::current();
    let audio: Arc<dyn AudioPipeline> = Arc::new(StubAudioPipeline);
    let transcriber =
        OpenAICompatibleTranscriber::new(&preset, "test-key".to_owned(), None, runtime, audio)
            .unwrap();

    let progress_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let req = build_request(small_test_audio(), &progress_calls);

    let result = tokio::task::spawn_blocking(move || transcriber.transcribe(&req))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(result.segments.len(), 1);
    assert_eq!(result.segments[0].text, "test");
    assert_eq!(result.segments[0].start_ms, 0);
    assert_eq!(result.segments[0].end_ms, 100);
    assert_eq!(result.language.as_deref(), Some("en"));
    assert_eq!(result.duration_ms, 100);
    assert_eq!(result.backend.0, "test:whisper-1");

    // Snapshot the progress vec out from under the mutex so the lock guard
    // drops immediately (clippy::significant_drop_tightening).
    let progress: Vec<String> = progress_calls.lock().unwrap().clone();
    // Started + Finished, in that order.
    assert!(
        progress.iter().any(|s| s.contains("Started")),
        "missing Started; got {progress:?}"
    );
    assert!(
        progress.iter().any(|s| s.contains("Finished")),
        "missing Finished; got {progress:?}"
    );
    let started_idx = progress.iter().position(|s| s.contains("Started")).unwrap();
    let finished_idx = progress
        .iter()
        .position(|s| s.contains("Finished"))
        .unwrap();
    assert!(
        started_idx < finished_idx,
        "Started must precede Finished; got {progress:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_failure_maps_to_typed_auth_error() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "error": {
            "message": "Incorrect API key provided",
            "type": "invalid_request_error",
            "param": null,
            "code": "invalid_api_key",
        }
    });
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(body))
        .mount(&server)
        .await;

    let preset = preset_for(&server);
    let runtime = tokio::runtime::Handle::current();
    let audio: Arc<dyn AudioPipeline> = Arc::new(StubAudioPipeline);
    let transcriber =
        OpenAICompatibleTranscriber::new(&preset, "wrong-key".to_owned(), None, runtime, audio)
            .unwrap();

    let progress_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let req = build_request(small_test_audio(), &progress_calls);

    let result = tokio::task::spawn_blocking(move || transcriber.transcribe(&req))
        .await
        .unwrap();

    match result {
        Err(CoreError::Transcription(TranscriptionError::Auth)) => {}
        other => panic!("expected Auth, got {other:?}"),
    }
}

// WHY no end-to-end 5xx test: async-openai 0.36's default
// `ExponentialBackoff` retries 5xx responses indefinitely (~15 min total
// wall-clock before giving up). Constructing a per-test client with a
// no-retry backoff would require leaking a `with_backoff` override into
// the production `OpenAICompatibleTranscriber::new` API surface, which
// is not justified for v1. The `tests/error_mapping.rs` unit tests cover
// the 5xx → BackendUnavailable mapping comprehensively against the typed
// `ApiError` shape — which is exactly what the cloud client surfaces
// after a successful read_response. The end-to-end 4xx (Auth) test above
// already proves the wiremock + adapter wiring is correct.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deserialization_failure_maps_to_internal_error() {
    // Server returns 200 + non-JSON body; async-openai's JSONDeserialize
    // path fires (no retry on 2xx malformed bodies).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json at all"))
        .mount(&server)
        .await;

    let preset = preset_for(&server);
    let runtime = tokio::runtime::Handle::current();
    let audio: Arc<dyn AudioPipeline> = Arc::new(StubAudioPipeline);
    let transcriber =
        OpenAICompatibleTranscriber::new(&preset, "test-key".to_owned(), None, runtime, audio)
            .unwrap();

    let progress_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let req = build_request(small_test_audio(), &progress_calls);

    let result = tokio::task::spawn_blocking(move || transcriber.transcribe(&req))
        .await
        .unwrap();

    match result {
        Err(CoreError::Transcription(TranscriptionError::Internal(msg))) => {
            assert!(
                msg.contains("deserialize") || msg.contains("not json"),
                "got {msg}"
            );
        }
        other => panic!("expected Internal, got {other:?}"),
    }
}
