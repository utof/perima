//! `perima transcribe` subcommand.
//!
//! Runs a transcription via the active provider, prints transcript text to
//! stdout (or `--output <FILE>`), exits 0 on success.
//!
//! # Concurrency model
//!
//! The CLI shell-side handler builds an `AppContainer`, registers a
//! [`TerminalEventHandler`] (extra handler) that funnels terminal
//! `AppEvent::Transcription{Completed,Failed,Cancelled}` events into a
//! `flume` channel, sends `TranscribeCommand::Start`, and blocks on the
//! channel receive. The pre-completion `AppEvent::TranscriptionProgress`
//! frames stream as one-line-per-event status updates on stderr
//! (best-effort `writeln!`; cloud adapters in v1 emit Started/Finished
//! only, so the per-segment overwrite would be wasted complexity).
//!
//! # Why a synthetic `file_uuid`
//!
//! The CLI's one-shot flow has no row in the `files` table for the
//! source path (no scan happened first). The `transcript` schema's
//! `file_uuid` column is FK-without-cascade and does NOT enforce
//! existence at the DB level, so a fresh `UUIDv7` per CLI invocation is
//! safe and avoids leaking a fake `files` row.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, ValueEnum};
use perima_app::{AppContainer, EventHandler, TranscribeCommand, TranscribeOutput};
use perima_core::transcription::TranscriptionError;
use perima_core::{AppEvent, CoreError};

use crate::signals::Cancellation;

/// Output format for the transcribed text.
#[derive(Copy, Clone, Debug, ValueEnum)]
pub(crate) enum OutputFormat {
    /// Plain text — one line per segment.
    Text,
    /// JSON — `{ "language", "duration_ms", "segments": [...] }`.
    Json,
}

/// Arguments for `perima transcribe`.
#[derive(Args, Debug)]
pub(crate) struct TranscribeArgs {
    /// Path to the source media file (audio or video).
    pub source: PathBuf,

    /// Optional BCP-47 language hint (e.g. `en`, `es`, `zh-Hans`).
    ///
    /// Loose RFC-5646-subset validation: 2-3 letter language with
    /// optional 4-letter script and optional 2-letter or 3-digit region.
    #[arg(long, value_parser = parse_bcp47)]
    pub language: Option<String>,

    /// Write transcript to FILE instead of stdout.
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Output format: `text` (default) or `json`.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

/// CLI exit-code categories.
///
/// 0 = success, 1 = generic error, 2 = auth error,
/// 3 = queue full, 130 = cancelled (POSIX SIGINT convention).
pub(crate) const EXIT_AUTH: u8 = 2;
pub(crate) const EXIT_QUEUE_FULL: u8 = 3;
pub(crate) const EXIT_CANCELLED: u8 = 130;

/// Loose BCP-47 shape validator. Promote to `unic-langid` for full
/// RFC 5646 handling — tracked as out-of-scope per spec.
fn parse_bcp47(s: &str) -> Result<String, String> {
    static BCP47_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        // ^lang(2-3 alpha) (-script(4 alpha))? (-region(2 alpha or 3 digit))? $
        regex::Regex::new(r"^[A-Za-z]{2,3}(-[A-Za-z]{4})?(-[A-Za-z]{2}|-[0-9]{3})?$")
            .expect("static regex compiles")
    });
    if BCP47_RE.is_match(s) {
        Ok(s.to_owned())
    } else {
        Err(format!("invalid BCP-47 language code: {s}"))
    }
}

/// Map a [`TranscriptionError`] variant to the matching CLI exit code.
#[must_use]
pub(crate) const fn exit_code_for(err: &TranscriptionError) -> u8 {
    match err {
        TranscriptionError::Auth | TranscriptionError::QuotaExceeded => EXIT_AUTH,
        TranscriptionError::QueueFull { .. } => EXIT_QUEUE_FULL,
        TranscriptionError::Cancelled => EXIT_CANCELLED,
        _ => 1,
    }
}

// ---------------------------------------------------------------------------
// Terminal-event handler
// ---------------------------------------------------------------------------

/// One terminal outcome for a transcription request.
#[derive(Debug)]
pub(crate) enum Terminal {
    /// Successful completion. `transcript_id` lets the dispatcher load the
    /// freshly-persisted segments via a short-lived read connection.
    Completed { transcript_id: String },
    /// Cancelled by the user (Ctrl-C).
    Cancelled,
    /// Failed with a typed [`TranscriptionError`].
    Failed(TranscriptionError),
}

/// Funnel terminal `AppEvent::Transcription*` events into a `flume`
/// channel. The dispatcher constructs one of these BEFORE container
/// build, registers it as an `extra_handler`, then sets the request UUID
/// AFTER `Start` returns (the use-case mints the UUID).
///
/// WHY a shared `Arc<Mutex<Option<String>>>` for the request UUID rather
/// than a per-handler value: the handler must be registered during
/// `AppContainer::new`, but the request UUID does not exist until after
/// `TranscribeCommand::Start` runs. The shared cell lets the dispatcher
/// fill in the UUID retroactively. Until it is set, all transcription
/// events fall through (matches the "ignore until we know our UUID"
/// invariant).
struct TerminalEventHandler {
    /// Set by the dispatcher AFTER `Start` returns. Until then, all
    /// transcription events are ignored.
    request_uuid: Arc<std::sync::Mutex<Option<String>>>,
    tx: flume::Sender<Terminal>,
}

#[async_trait::async_trait]
impl EventHandler for TerminalEventHandler {
    fn name(&self) -> &'static str {
        "cli_transcribe_terminal"
    }

    async fn handle(&mut self, event: AppEvent) {
        let target = match self.request_uuid.lock() {
            Ok(g) => g.clone(),
            Err(_) => return, // poisoned — drop the event silently.
        };
        let Some(target) = target else { return };
        // WHY `let _ = self.tx.send(...)`: the bounded(4) channel's
        // receiver is the dispatcher's terminal-event blocking recv; if
        // it's already gone (caller exited via Ctrl-C), dropping the
        // event is the correct behaviour, not surfacing a SendError.
        match event {
            AppEvent::TranscriptionCompleted {
                request_uuid,
                transcript_id,
                ..
            } if request_uuid == target => {
                let _ = self.tx.send(Terminal::Completed { transcript_id });
            }
            AppEvent::TranscriptionCancelled { request_uuid } if request_uuid == target => {
                let _ = self.tx.send(Terminal::Cancelled);
            }
            AppEvent::TranscriptionFailed {
                request_uuid,
                error,
            } if request_uuid == target => {
                let _ = self.tx.send(Terminal::Failed(error));
            }
            AppEvent::TranscriptionProgress {
                request_uuid,
                processed_ms,
                total_ms,
            } if request_uuid == target => {
                // One-line stderr status. Best-effort — failure to write
                // is silent (broken pipe, redirected stderr).
                let elapsed = ms_to_secs(processed_ms);
                let total =
                    total_ms.map_or_else(|| "?".to_owned(), |ms| format!("{:.1}", ms_to_secs(ms)));
                let _ = writeln!(
                    std::io::stderr(),
                    "transcribing... {elapsed:.1}s / {total}s"
                );
            }
            _ => {}
        }
    }
}

#[allow(clippy::cast_precision_loss)]
fn ms_to_secs(ms: u32) -> f32 {
    ms as f32 / 1000.0
}

/// Shared cell the dispatcher fills with the request UUID after `Start`.
pub(crate) type RequestUuidCell = Arc<std::sync::Mutex<Option<String>>>;

/// Bundle returned by [`make_terminal_handler`].
pub(crate) struct TerminalHandle {
    /// Boxed handler — pass to `build_container` as an `extra_handler`.
    pub handler: Box<dyn EventHandler>,
    /// Receiver the dispatcher reads from.
    pub rx: flume::Receiver<Terminal>,
    /// Shared cell the dispatcher fills with the request UUID after `Start`.
    pub request_uuid: RequestUuidCell,
}

/// Construct the terminal handler + receiver + request-UUID cell.
pub(crate) fn make_terminal_handler() -> TerminalHandle {
    let (tx, rx) = flume::bounded::<Terminal>(4);
    let request_uuid: RequestUuidCell = Arc::new(std::sync::Mutex::new(None));
    let handler = TerminalEventHandler {
        request_uuid: Arc::clone(&request_uuid),
        tx,
    };
    TerminalHandle {
        handler: Box::new(handler),
        rx,
        request_uuid,
    }
}

// ---------------------------------------------------------------------------
// Read transcript text out of the DB
// ---------------------------------------------------------------------------

/// Read every segment for `transcript_id` (ordered by `start_ms`) into a
/// flat `Vec<(start_ms, end_ms, text)>`. Uses a short-lived read-only
/// connection — there is no T7 read API yet.
fn load_segments_for(
    db_path: &std::path::Path,
    transcript_id: &str,
) -> Result<Vec<(u32, u32, String)>, CoreError> {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CoreError::Internal(format!("open transcript db: {e}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT start_ms, end_ms, text FROM transcript_segment \
             WHERE transcript_id = ?1 AND deleted_at IS NULL \
             ORDER BY start_ms ASC",
        )
        .map_err(|e| CoreError::Internal(format!("prepare segments query: {e}")))?;
    // WHY u32::try_from(i64).unwrap_or(u32::MAX): SQLite stores INTEGER
    // as i64; schema values are ms timestamps that fit in u32 for any
    // realistic media file (u32::MAX ms ≈ 49 days). Saturating on the
    // (impossible) overflow is preferable to crashing the render — the
    // segment is still readable and the UX degrades to "very long" rather
    // than aborting the whole transcript print.
    let rows = stmt
        .query_map([transcript_id], |row| {
            let start = row.get::<_, i64>(0)?;
            let end = row.get::<_, i64>(1)?;
            let text = row.get::<_, String>(2)?;
            Ok((
                u32::try_from(start).unwrap_or(u32::MAX),
                u32::try_from(end).unwrap_or(u32::MAX),
                text,
            ))
        })
        .map_err(|e| CoreError::Internal(format!("query segments: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| CoreError::Internal(format!("read segment row: {e}")))?);
    }
    Ok(out)
}

/// Read the transcript header (`language`, `duration_ms`) for the given id.
fn load_transcript_header(
    db_path: &std::path::Path,
    transcript_id: &str,
) -> Result<(Option<String>, u32), CoreError> {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| CoreError::Internal(format!("open transcript db: {e}")))?;
    let row = conn
        .query_row(
            "SELECT language, duration_ms FROM transcript WHERE id = ?1",
            [transcript_id],
            |r| {
                let lang = r.get::<_, Option<String>>(0)?;
                let dur = r.get::<_, i64>(1)?;
                Ok((lang, u32::try_from(dur).unwrap_or(u32::MAX)))
            },
        )
        .map_err(|e| CoreError::Internal(format!("query transcript header: {e}")))?;
    Ok(row)
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

fn render_text<W: Write>(w: &mut W, segments: &[(u32, u32, String)]) -> Result<(), CoreError> {
    for (_, _, text) in segments {
        writeln!(w, "{}", text.trim()).map_err(CoreError::from)?;
    }
    Ok(())
}

fn render_json<W: Write>(
    w: &mut W,
    language: Option<&str>,
    duration_ms: u32,
    segments: &[(u32, u32, String)],
) -> Result<(), CoreError> {
    let segs: Vec<serde_json::Value> = segments
        .iter()
        .map(|(s, e, t)| {
            serde_json::json!({
                "start_ms": s,
                "end_ms": e,
                "text": t,
            })
        })
        .collect();
    let body = serde_json::json!({
        "language": language,
        "duration_ms": duration_ms,
        "segments": segs,
    });
    serde_json::to_writer_pretty(&mut *w, &body)
        .map_err(|e| CoreError::Internal(format!("json: {e}")))?;
    writeln!(w).map_err(CoreError::from)
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Render the persisted transcript to either stdout or `--output`.
fn render_output(
    db_path: &std::path::Path,
    transcript_id: &str,
    args: &TranscribeArgs,
) -> Result<(), CoreError> {
    let segments = load_segments_for(db_path, transcript_id)?;
    let (language, duration_ms) = load_transcript_header(db_path, transcript_id)?;

    if let Some(out_path) = &args.output {
        let mut file = std::fs::File::create(out_path).map_err(CoreError::from)?;
        match args.format {
            OutputFormat::Text => render_text(&mut file, &segments),
            OutputFormat::Json => {
                render_json(&mut file, language.as_deref(), duration_ms, &segments)
            }
        }
    } else {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        match args.format {
            OutputFormat::Text => render_text(&mut handle, &segments),
            OutputFormat::Json => {
                render_json(&mut handle, language.as_deref(), duration_ms, &segments)
            }
        }
    }
}

/// Wait for the terminal event OR Ctrl-C. The cancel token (Ctrl-C)
/// takes precedence.
async fn await_terminal(
    container: &Arc<AppContainer>,
    request_uuid: &str,
    rx: flume::Receiver<Terminal>,
    cancel: &Cancellation,
) -> Result<Terminal, u8> {
    let cancel_token = cancel.token();
    let terminal = tokio::select! {
        recv = rx.recv_async() => recv,
        () = cancel_token.cancelled() => {
            // Best-effort cancel — the use-case removes the entry and
            // fires the token; the worker emits TranscriptionCancelled
            // which our handler funnels back. Bound the wait so a stuck
            // worker can't hang the CLI.
            let _ = container
                .transcription
                .execute(TranscribeCommand::Cancel {
                    request_uuid: request_uuid.to_owned(),
                })
                .await;
            let Ok(r) = tokio::time::timeout(Duration::from_secs(5), rx.recv_async()).await else {
                eprintln!("perima: cancelled (worker did not acknowledge in 5s)");
                return Err(EXIT_CANCELLED);
            };
            r
        }
    };
    terminal.map_err(|_| {
        eprintln!("perima: event bus closed before terminal event");
        1
    })
}

/// Outcome of the dispatcher: the exit-code byte the caller passes to
/// `ExitCode::from`.
pub(crate) async fn run(
    container: Arc<AppContainer>,
    db_path: &std::path::Path,
    args: TranscribeArgs,
    rx: flume::Receiver<Terminal>,
    request_uuid_cell: RequestUuidCell,
    cancel: &Cancellation,
) -> u8 {
    if !args.source.exists() {
        eprintln!("perima: source not found: {}", args.source.display());
        return 1;
    }

    let file_name = args.source.file_name().map_or_else(
        || args.source.display().to_string(),
        |s| s.to_string_lossy().into_owned(),
    );

    let request = TranscribeCommand::Start {
        file_uuid: uuid::Uuid::now_v7().simple().to_string(),
        file_name,
        source: args.source.clone(),
        language_hint: args.language.clone(),
    };

    let started = match container.transcription.execute(request).await {
        Ok(out) => out,
        Err(CoreError::Transcription(t)) => {
            eprintln!("perima: {t}");
            return exit_code_for(&t);
        }
        Err(e) => {
            eprintln!("perima: {e}");
            return 1;
        }
    };

    let TranscribeOutput::Started { request_uuid, .. } = started else {
        eprintln!("perima: unexpected use-case output (Cancelled before Start)");
        return 1;
    };

    // Make the handler aware of our request UUID NOW. Any events that
    // arrived before this assignment were dropped (matches the "ignore
    // until we know our UUID" invariant on `TerminalEventHandler`).
    if let Ok(mut g) = request_uuid_cell.lock() {
        *g = Some(request_uuid.clone());
    }

    let terminal = match await_terminal(&container, &request_uuid, rx, cancel).await {
        Ok(t) => t,
        Err(code) => return code,
    };

    match terminal {
        Terminal::Completed { transcript_id } => {
            if let Err(e) = render_output(db_path, &transcript_id, &args) {
                eprintln!("perima: {e}");
                return 1;
            }
            0
        }
        Terminal::Cancelled => {
            eprintln!("perima: transcription cancelled");
            EXIT_CANCELLED
        }
        Terminal::Failed(err) => {
            eprintln!("perima: {err}");
            exit_code_for(&err)
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bcp47_validator_accepts_common_codes() {
        assert!(parse_bcp47("en").is_ok());
        assert!(parse_bcp47("es").is_ok());
        assert!(parse_bcp47("zh-Hans").is_ok());
        assert!(parse_bcp47("en-US").is_ok());
        assert!(parse_bcp47("es-419").is_ok());
    }

    #[test]
    fn bcp47_validator_rejects_garbage() {
        assert!(parse_bcp47("not-a-real-tag-zzz").is_err());
        assert!(parse_bcp47("12345").is_err());
        assert!(parse_bcp47("").is_err());
    }

    #[test]
    fn exit_code_mapping_matches_spec() {
        assert_eq!(exit_code_for(&TranscriptionError::Auth), EXIT_AUTH);
        assert_eq!(exit_code_for(&TranscriptionError::QuotaExceeded), EXIT_AUTH);
        assert_eq!(
            exit_code_for(&TranscriptionError::QueueFull { queued: 32 }),
            EXIT_QUEUE_FULL
        );
        assert_eq!(
            exit_code_for(&TranscriptionError::Cancelled),
            EXIT_CANCELLED
        );
        assert_eq!(exit_code_for(&TranscriptionError::Network("dns".into())), 1);
    }
}
