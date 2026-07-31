//! Transcription port: speech-to-text over audio or video files.
//!
//! Implementations live outside this crate (cloud HTTP, future local
//! whisper.cpp, future plugin sidecars) and bridge to async or blocking
//! work internally.
//!
//! Mirrors the shape of [`crate::metadata::MetadataExtractor`]: sync trait,
//! `&self`, `Send + Sync`, callable from the writer-actor without spawning
//! a second tokio runtime.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::CoreError;

/// Stable backend identifier in `provider:model` form.
///
/// Examples: `groq:whisper-large-v3-turbo`, `openai:whisper-1`,
/// `custom:my-self-hosted-server:large-v3-turbo`.
///
/// Newtype kept distinct from `String` so callers cannot accidentally
/// pass a free-form display name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct BackendId(pub String);

impl std::fmt::Display for BackendId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Single output segment. Times in milliseconds since file start.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TranscriptSegment {
    /// `UUIDv7` assigned by the use-case (NOT the adapter), so segments
    /// keep stable identity across re-runs against different backends.
    pub id: Uuid,
    /// Segment start time in milliseconds since file start.
    pub start_ms: u32,
    /// Segment end time in milliseconds since file start.
    pub end_ms: u32,
    /// The transcribed text for this segment.
    pub text: String,
    /// Roughly `[0.0, 1.0]` (1.0 = high confidence). `None` when the
    /// backend does not expose a confidence signal.
    pub confidence: Option<f32>,
}

/// Result of a successful transcription.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct TranscriptionResult {
    /// BCP-47 short code where possible (`en`, `fr`, `zh`).
    pub language: Option<String>,
    /// Total duration of the source media in milliseconds.
    pub duration_ms: u32,
    /// Segments with their timestamps.
    pub segments: Vec<TranscriptSegment>,
    /// Identifier of the backend that produced this result.
    pub backend: BackendId,
}

/// Caller hint about what the adapter should do.
#[derive(Clone)]
pub struct TranscribeRequest {
    /// Path to a media file (video or audio container) the adapter must
    /// handle. Cloud adapters typically remux + multipart-upload.
    /// Local adapters extract PCM via ffmpeg first.
    pub source: PathBuf,

    /// Optional language hint. `None` = autodetect.
    pub language_hint: Option<String>,

    /// Cancel by triggering this token. Adapters MUST check at least
    /// every segment boundary AND on every HTTP poll.
    pub cancel: CancellationToken,

    /// Per-segment / heartbeat progress hook. Called from arbitrary
    /// threads; must be `Send + Sync`. `Arc<dyn Fn>` keeps the trait
    /// object-safe AND lets `TranscribeRequest: Clone` share one closure
    /// across clones (cloning the request shares — does NOT duplicate —
    /// the callback).
    // WHY Arc<dyn Fn> over channel/Box: long-lived progress hook needs both
    // Send + Sync + trait-object safety; a channel would force the request
    // struct to own a tx and entangle cancellation; Box<dyn Fn> would block
    // Clone of TranscribeRequest, which the queue needs to dispatch jobs.
    pub on_progress: Arc<dyn Fn(TranscriptionProgress) + Send + Sync>,

    /// Soft timeout for HTTP-backed adapters. Local adapters ignore.
    /// `None` defers to the adapter's default (typically 600s).
    pub timeout: Option<Duration>,
}

// WHY manual Debug: `Arc<dyn Fn(...)>` does not implement `Debug`; the
// closure is an opaque callback. We elide it from the debug output so the
// derived `Debug` on surrounding types still works.
impl std::fmt::Debug for TranscribeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscribeRequest")
            .field("source", &self.source)
            .field("language_hint", &self.language_hint)
            .field("cancel", &self.cancel)
            .field("on_progress", &"<callback>")
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Progress events emitted by the adapter.
#[derive(Debug, Clone)]
pub enum TranscriptionProgress {
    /// Emitted once when transcription begins.
    Started {
        /// Best-effort estimate of total duration in milliseconds.
        estimated_duration_ms: Option<u32>,
    },
    /// Emitted as segments are finalized (cloud streaming) or at segment
    /// boundaries (local incremental decode).
    Segment {
        /// The segment that was just finalized.
        segment: TranscriptSegment,
        /// Cumulative milliseconds of source media processed so far.
        processed_ms: u32,
        /// Total source duration if known.
        total_ms: Option<u32>,
    },
    /// Periodic heartbeat for backends that don't stream segments
    /// (e.g., `openai:whisper-1` batch mode). Drives UI spinners.
    Heartbeat {
        /// Time since the request started.
        elapsed: Duration,
    },
    /// Emitted once when transcription completes successfully.
    Finished,
}

/// All transcription failure modes.
///
/// `#[non_exhaustive]` so new variants can land in patch releases without
/// breaking downstream `match` arms. Downstream code should either match
/// every variant explicitly or use a wildcard arm and re-audit on bumps.
///
/// `Serialize` only (NOT `Deserialize`) — matches the [`CoreError`]
/// shape. The frontend parses the JSON via the typed-IPC contract;
/// Rust never deserializes its own errors.
// WHY no workspace-wide wildcard_enum_match_arm clippy lint: the lint is
// in the restriction group (not enabled today). Enabling it across the
// workspace is tracked as its own slice. In this crate the no-wildcard
// discipline is reviewer-enforced.
#[derive(Debug, Clone, Error, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", content = "data")]
#[non_exhaustive]
pub enum TranscriptionError {
    /// Network error contacting the backend (DNS, TLS, connection reset, timeout).
    #[error("network error contacting backend: {0}")]
    Network(String),

    /// Authentication failed: bad API key, expired token, or wrong scopes.
    #[error("authentication failed (bad API key, expired token, or wrong scopes)")]
    Auth,

    /// Rate limited by backend; retry-after hint may be present.
    #[error("rate limited by backend; retry after {retry_after_secs:?}s")]
    RateLimited {
        /// Seconds until retry is acceptable, parsed from `Retry-After` header.
        retry_after_secs: Option<u64>,
    },

    /// Quota or billing exhausted.
    #[error("quota or billing exhausted")]
    QuotaExceeded,

    /// Requested model not available at the backend.
    #[error("model {model} not available at backend {backend}")]
    ModelNotFound {
        /// Backend ID that rejected the model.
        backend: String,
        /// Model name requested.
        model: String,
    },

    /// Could not decode audio from source.
    #[error("could not decode audio from source: {0}")]
    AudioDecode(String),

    /// Input file too large for backend.
    #[error("input file too large for backend (limit {limit_bytes} bytes)")]
    FileTooLarge {
        /// Backend's file-size ceiling in bytes.
        limit_bytes: u64,
    },

    /// Cancelled by caller via [`TranscribeRequest::cancel`].
    #[error("cancelled by caller")]
    Cancelled,

    /// Backend service unavailable (5xx, maintenance, etc.).
    #[error("backend unavailable: {reason}")]
    BackendUnavailable {
        /// Human-readable reason.
        reason: String,
    },

    /// Transcription queue is full (bounded mpsc reached capacity).
    #[error("transcription queue is full ({queued} jobs queued); try again later")]
    QueueFull {
        /// Number of jobs currently in the queue.
        queued: u32,
    },

    /// Last-resort variant for unexpected adapter-internal failures.
    /// Adapters MUST emit a `tracing::error!` AND include the
    /// underlying message. Do NOT use for any classifiable failure.
    #[error("internal adapter error: {0}")]
    Internal(String),
}

/// The transcription port. Sync, `&self`, `Send + Sync`. Object-safe.
///
/// Implementations bridge to async (HTTP) or blocking CPU (whisper.cpp)
/// **internally** — see crate `transcribe` for the canonical bridges.
///
/// # Why not `async fn`
/// 1. Existing [`crate::metadata::MetadataExtractor`] is sync; consistency wins.
/// 2. `async fn` in object-safe traits still has rough edges in stable
///    Rust (no-`dyn`-without-helpers).
/// 3. The writer-actor pattern this codebase uses needs sync
///    boundaries to keep the actor's lock-order discipline intact.
pub trait Transcriber: Send + Sync {
    /// Stable backend identifier.
    fn id(&self) -> &BackendId;

    /// Whether this backend handles the given MIME type.
    ///
    /// Mirrors [`crate::metadata::MetadataExtractor::accepts`]. Cloud
    /// adapters typically accept `audio/*` and `video/*`; local adapters
    /// that need PCM accept the same and rely on the use-case (or audio
    /// pipeline) to pre-extract.
    fn accepts(&self, mime: &str) -> bool;

    /// Run transcription. Blocks the calling thread for the duration.
    ///
    /// # Errors
    /// All failures map into [`CoreError::Transcription`] via
    /// [`TranscriptionError`]. Adapters do NOT panic on contracted
    /// failures (network, auth, quota, etc.).
    fn transcribe(&self, req: &TranscribeRequest) -> Result<TranscriptionResult, CoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_id_display_round_trips() {
        let id = BackendId("groq:whisper-large-v3-turbo".to_owned());
        assert_eq!(id.to_string(), "groq:whisper-large-v3-turbo");
        assert_eq!(&id.0, "groq:whisper-large-v3-turbo");
    }

    #[test]
    fn transcription_error_serializes_with_kind_data_tag() {
        let err = TranscriptionError::RateLimited {
            retry_after_secs: Some(30),
        };
        let json = serde_json::to_string(&err).expect("serialize");
        // Verify discriminated-union shape per CoreError convention.
        assert!(json.contains("\"kind\":\"RateLimited\""), "got {json}");
        assert!(json.contains("\"data\":{"), "got {json}");
        assert!(json.contains("\"retry_after_secs\":30"), "got {json}");
    }

    #[test]
    fn transcription_error_auth_serializes_without_data() {
        let err = TranscriptionError::Auth;
        let json = serde_json::to_string(&err).expect("serialize");
        assert!(json.contains("\"kind\":\"Auth\""), "got {json}");
        // Auth carries no payload — verify `data` key is genuinely absent so a
        // future refactor that adds a field to Auth doesn't silently slip past.
        assert!(!json.contains("\"data\""), "got {json}");
    }

    #[test]
    fn transcript_segment_round_trip_uuid_v7() {
        let id = Uuid::now_v7();
        let seg = TranscriptSegment {
            id,
            start_ms: 0,
            end_ms: 1500,
            text: "hello world".to_owned(),
            confidence: Some(0.92),
        };
        let json = serde_json::to_string(&seg).expect("serialize");
        let de: TranscriptSegment = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(seg.id, de.id);
        assert_eq!(seg.start_ms, de.start_ms);
        assert_eq!(seg.end_ms, de.end_ms);
        assert_eq!(seg.text, de.text);
        assert_eq!(seg.confidence, de.confidence);
    }
}
