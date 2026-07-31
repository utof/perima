//! `OpenAI`-compatible cloud transcription adapter.
//!
//! Single Rust adapter covers all providers serving the `OpenAI`
//! `/v1/audio/transcriptions` wire format (Groq, `OpenAI`,
//! faster-whisper-server, `vLLM`, Whisperfile, `OpenWebUI`, vox-box,
//! Azure-via-LiteLLM).
//! See spec § "The cloud adapter — `OpenAICompatibleTranscriber`".

use std::path::PathBuf;
use std::sync::Arc;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::error::OpenAIError;
use async_openai::types::audio::{
    AudioResponseFormat, CreateTranscriptionRequestArgs, TimestampGranularity,
};
use tokio::runtime::{Handle, RuntimeFlavor};
use uuid::Uuid;

use perima_core::CoreError;
use perima_core::transcription::{
    BackendId, TranscribeRequest, Transcriber, TranscriptSegment, TranscriptionError,
    TranscriptionProgress, TranscriptionResult,
};

use crate::audio::AudioPipeline;
use crate::providers::{AuthScheme, ProviderPreset};

/// One OpenAI-compatible cloud transcription backend.
///
/// Wraps an `async-openai` client with a configurable `base_url`,
/// `auth_scheme`, and `model`. The sync [`Transcriber::transcribe`] method
/// bridges into the adapter's async machinery via
/// `tokio::task::block_in_place + Handle::block_on`, so the calling
/// thread blocks for the duration of the request without spawning a
/// second tokio runtime.
pub struct OpenAICompatibleTranscriber {
    id: BackendId,
    client: Client<OpenAIConfig>,
    model: String,
    runtime: Handle,
    audio: Arc<dyn AudioPipeline>,
    file_size_limit_bytes: u64,
}

// WHY manual Debug: `Client<OpenAIConfig>` does not derive `Debug` and
// `Arc<dyn AudioPipeline>` is also not `Debug`. Elide both so the
// surrounding container types (e.g. `AppContainer`) can still derive Debug.
impl std::fmt::Debug for OpenAICompatibleTranscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAICompatibleTranscriber")
            .field("id", &self.id)
            .field("model", &self.model)
            .field("file_size_limit_bytes", &self.file_size_limit_bytes)
            .finish_non_exhaustive()
    }
}

impl OpenAICompatibleTranscriber {
    /// Build a new adapter.
    ///
    /// # Panics
    /// Panics if the supplied tokio runtime handle is `current_thread`.
    /// `block_in_place` requires the multi-thread scheduler — it would
    /// panic on first transcribe otherwise. See spec § "Sync→async bridge
    /// contract".
    ///
    /// # Errors
    /// Currently infallible; `Result` reserved for future preset validation
    /// (e.g. `AuthScheme::None` + non-localhost `base_url`).
    pub fn new(
        preset: &ProviderPreset,
        api_key: String,
        model: Option<String>,
        runtime: Handle,
        audio: Arc<dyn AudioPipeline>,
    ) -> Result<Self, CoreError> {
        // Fail loud + early on current_thread runtime — block_in_place would
        // panic on first transcribe otherwise. Per spec § "Sync→async bridge
        // contract".
        match runtime.runtime_flavor() {
            RuntimeFlavor::MultiThread => {}
            other => panic!(
                "OpenAICompatibleTranscriber requires a multi-thread tokio runtime; \
                 got {other:?}. block_in_place + block_on would deadlock on \
                 current_thread. See spec § 'Sync→async bridge contract'."
            ),
        }

        let model = model.unwrap_or_else(|| preset.default_model.to_owned());
        let id = BackendId(format!("{}:{}", preset.name, model));

        // Per AuthScheme:
        // - Bearer: async-openai's default; pass api_key through.
        // - XApiKey { header }: emit `<header>: <api_key>` via with_header.
        //   async-openai 0.36 still emits an `Authorization: Bearer ` line
        //   with an empty key alongside, but custom-header-using providers
        //   (Azure, LiteLLM) ignore the unrecognized auth.
        // - None: pass an empty key (some local servers ignore the header).
        let mut config = OpenAIConfig::new().with_api_base(preset.base_url);
        match preset.auth_scheme {
            AuthScheme::Bearer => {
                config = config.with_api_key(api_key);
            }
            AuthScheme::XApiKey { header } => {
                config = config.with_header(header, api_key).map_err(|e| {
                    CoreError::Transcription(TranscriptionError::Internal(format!(
                        "invalid XApiKey header value for provider {}: {e}",
                        preset.name
                    )))
                })?;
            }
            AuthScheme::None => {}
        }

        let client = Client::with_config(config);
        Ok(Self {
            id,
            client,
            model,
            runtime,
            audio,
            file_size_limit_bytes: preset.file_size_limit_bytes,
        })
    }

    async fn transcribe_inner(
        &self,
        upload_path: PathBuf,
        language_hint: Option<String>,
    ) -> Result<TranscriptionResult, CoreError> {
        let mut builder = CreateTranscriptionRequestArgs::default();
        // WHY .file(upload_path): the derive_builder setter uses `into`,
        // and async-openai 0.36 implements `From<P: AsRef<Path>>` for
        // `AudioInput` (see types/impls.rs::impl_input!), so a PathBuf
        // converts straight in.
        builder
            .file(upload_path)
            .model(self.model.clone())
            .response_format(AudioResponseFormat::VerboseJson)
            .timestamp_granularities(vec![TimestampGranularity::Segment]);
        if let Some(lang) = language_hint {
            builder.language(lang);
        }
        let request = builder.build().map_err(|e| {
            CoreError::Transcription(TranscriptionError::Internal(format!("request build: {e}")))
        })?;

        // WHY .audio().transcription().create_verbose_json: async-openai 0.36
        // names this differently than spec/plan (which referenced a 0.32-era
        // `audio().transcribe_verbose_json` shortcut). 0.36 splits the audio
        // group into nested `Audio -> Transcriptions -> create_verbose_json`.
        let response = self
            .client
            .audio()
            .transcription()
            .create_verbose_json(request)
            .await
            .map_err(|e| {
                map_async_openai_error(e, &self.id, &self.model, self.file_size_limit_bytes)
            })?;

        let segments = response
            .segments
            .unwrap_or_default()
            .into_iter()
            .map(|s| {
                // WHY explicit f32 casts: TranscriptionSegment fields are
                // f32 in 0.36 (not f64 as the plan draft assumed); seconds *
                // 1000 fits well within u32 for any realistic media file
                // (u32::MAX ms ≈ 49 days), so the lossy as-cast is safe.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let start_ms = (s.start * 1000.0) as u32;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let end_ms = (s.end * 1000.0) as u32;
                TranscriptSegment {
                    // WHY nil placeholder: the use-case stamps a real UUIDv7
                    // before the row is built (TranscriptionUseCase walks the
                    // returned segments and replaces every nil id). Keeping
                    // the adapter UUID-free avoids an extra `uuid::Uuid::now_v7()`
                    // call per segment in the cloud hot path.
                    id: Uuid::nil(),
                    start_ms,
                    end_ms,
                    text: s.text,
                    // WHY None for v1: avg_logprob is a quality signal not a
                    // calibrated probability; mapping it to a confidence in
                    // [0.0, 1.0] needs separate calibration. Local adapters
                    // will surface real confidences in a later slice.
                    confidence: None,
                }
            })
            .collect();

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let duration_ms = (response.duration * 1000.0) as u32;
        Ok(TranscriptionResult {
            language: Some(response.language),
            duration_ms,
            segments,
            backend: self.id.clone(),
        })
    }
}

impl Transcriber for OpenAICompatibleTranscriber {
    fn id(&self) -> &BackendId {
        &self.id
    }

    fn accepts(&self, mime: &str) -> bool {
        mime.starts_with("audio/") || mime.starts_with("video/")
    }

    fn transcribe(&self, req: &TranscribeRequest) -> Result<TranscriptionResult, CoreError> {
        // 1. Pre-flight remux if file > limit OR has a video MIME.
        let metadata = std::fs::metadata(&req.source).map_err(|e| {
            CoreError::Transcription(TranscriptionError::AudioDecode(format!("stat input: {e}")))
        })?;
        let needs_remux = metadata.len() > self.file_size_limit_bytes
            || mime_guess::from_path(&req.source)
                .first_or_octet_stream()
                .type_()
                == "video";

        // WHY hold the NamedTempFile in a binding rather than mem::forget +
        // manual remove_file: tempfile's Drop is the canonical cleanup path
        // and the plan's manual remove_file/forget pair is a leak-on-panic
        // hazard. Holding `_remux_handle` in scope keeps the file alive until
        // the function returns; Drop fires on both Ok and Err paths.
        let (upload_path, _remux_handle) = if needs_remux {
            let temp = self
                .audio
                .remux_for_upload(&req.source, &req.cancel)
                .map_err(|e| match e {
                    // Preserve the cancel signal end-to-end — folding it into
                    // AudioDecode would surface as a confusing decode error in
                    // the UI even though the user just hit Cancel.
                    crate::audio::AudioError::Cancelled => {
                        CoreError::Transcription(TranscriptionError::Cancelled)
                    }
                    other => CoreError::Transcription(TranscriptionError::AudioDecode(format!(
                        "remux: {other}"
                    ))),
                })?;
            (temp.path().to_owned(), Some(temp))
        } else {
            (req.source.clone(), None)
        };

        (req.on_progress)(TranscriptionProgress::Started {
            estimated_duration_ms: None,
        });

        // 2. block_in_place + block_on bridge. Required because the trait is
        //    sync but async-openai exposes only async methods. block_in_place
        //    is panic-banned on current_thread; the constructor's flavor
        //    check guards that.
        let result = tokio::task::block_in_place(|| {
            self.runtime
                .block_on(self.transcribe_inner(upload_path, req.language_hint.clone()))
        });

        (req.on_progress)(TranscriptionProgress::Finished);
        result
    }
}

/// Map `async-openai` errors to typed [`TranscriptionError`] variants.
///
/// Covered by `crates/transcribe/tests/error_mapping.rs` (variant +
/// field assertions, not insta snapshots).
///
/// The match covers every concrete `OpenAIError` variant in 0.36 (no
/// wildcard arm), so adding a new variant in a future bump becomes a
/// compile error here.
#[must_use]
pub fn map_async_openai_error(
    err: OpenAIError,
    backend: &BackendId,
    model: &str,
    file_size_limit_bytes: u64,
) -> CoreError {
    let te = match err {
        OpenAIError::ApiError(api) => map_api_error(&api, backend, model),
        OpenAIError::Reqwest(e) => TranscriptionError::Network(e.to_string()),
        OpenAIError::JSONDeserialize(e, body) => {
            TranscriptionError::Internal(format!("deserialize response: {e} (body: {body})"))
        }
        OpenAIError::FileSaveError(s) | OpenAIError::FileReadError(s) => {
            TranscriptionError::AudioDecode(s)
        }
        OpenAIError::StreamError(s) => TranscriptionError::Network(s.to_string()),
        // Heuristic: builder/preflight errors mentioning size hit the
        // backend's file-size ceiling. Any other InvalidArgument is an
        // adapter-internal bug (caller passed a malformed request).
        OpenAIError::InvalidArgument(s) if s.to_lowercase().contains("size") => {
            TranscriptionError::FileTooLarge {
                limit_bytes: file_size_limit_bytes,
            }
        }
        OpenAIError::InvalidArgument(s) => TranscriptionError::Internal(s),
    };
    CoreError::Transcription(te)
}

fn map_api_error(
    api: &async_openai::error::ApiError,
    backend: &BackendId,
    model: &str,
) -> TranscriptionError {
    match api.code.as_deref() {
        Some("invalid_api_key" | "unauthorized") => TranscriptionError::Auth,
        Some("quota_exceeded" | "billing_hard_limit_reached" | "insufficient_quota") => {
            TranscriptionError::QuotaExceeded
        }
        Some("model_not_found") => TranscriptionError::ModelNotFound {
            backend: backend.0.clone(),
            model: model.to_owned(),
        },
        Some("rate_limit_exceeded") => TranscriptionError::RateLimited {
            // async-openai 0.36's ApiError doesn't surface response headers,
            // so we cannot read Retry-After here. Adapters that need the
            // hint will have to bypass `Client::audio()` and inspect the
            // raw response — out of scope for v1.
            retry_after_secs: None,
        },
        _ => TranscriptionError::BackendUnavailable {
            reason: format!("API error: {} ({:?})", api.message, api.code),
        },
    }
}
