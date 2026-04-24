//! Shell-agnostic observability handlers.
//!
//! WHY this module exists: `LogEventHandler` was duplicated across the
//! CLI (`crates/cli/src/cmd/watch.rs`) and Desktop
//! (`crates/desktop/src/commands.rs`) shells pre-Batch-B. Task 10 of the
//! Batch B plan hoists it alongside [`crate::AppContainer`] so both
//! shells (and future `api` / `ffi` shells) construct the container
//! uniformly — the canonical home for shell-agnostic event handlers.
//!
//! `DbEventHandler` is deliberately NOT hoisted here: it touches
//! `SqliteFileRepository` which lives in `crates/db`, so hoisting it
//! would force `perima-app` to depend on a concrete adapter. It stays
//! shell-local in both CLI and Desktop.

use crate::events::EventHandler;
use perima_core::AppEvent;

/// Logs every application event at INFO level via `tracing`.
///
/// WHY `Default` + `Debug`: wire-up sites construct this with
/// `Box::new(LogEventHandler)` today; derived `Default` keeps the
/// zero-field shape future-proof, and `Debug` makes the handler
/// printable inside `AppContainer` diagnostics without special-casing.
#[derive(Debug, Default)]
pub struct LogEventHandler;

#[async_trait::async_trait]
impl EventHandler for LogEventHandler {
    fn name(&self) -> &'static str {
        "log_event_handler"
    }

    async fn handle(&mut self, event: AppEvent) {
        // WHY match on outer kind first: log statement varies per
        // variant; the existing FileEvent log shape is preserved
        // inside the AppEvent::File arm.
        match event {
            AppEvent::File(file_event) => {
                // WHY `event = ?file_event` field name: preserves the
                // log schema the pre-Batch-E `EventBus::emit` impl
                // established. Downstream log consumers may rely on it.
                tracing::info!(event = ?file_event, "file event");
            }
            AppEvent::ScanCompleted {
                volume,
                files_new,
                files_seen,
                duration_ms,
            } => {
                tracing::info!(
                    ?volume,
                    files_new,
                    files_seen,
                    duration_ms,
                    "scan completed"
                );
            }
            AppEvent::IndexInvalidated { reason } => {
                tracing::info!(?reason, "index invalidated");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use perima_core::{FileEvent, InvalidationReason, MediaPath, VolumeId};
    use uuid::Uuid;

    #[tokio::test]
    async fn log_handler_handles_file_created() {
        let mut handler = LogEventHandler;
        let event = AppEvent::File(FileEvent::Created {
            path: MediaPath::new("foo.txt"),
            volume: VolumeId(Uuid::nil()),
        });
        // `handle` returns (); success = no panic.
        handler.handle(event).await;
    }

    #[tokio::test]
    async fn log_handler_handles_file_renamed() {
        // WHY `LogEventHandler` (not `::default()`): clippy's
        // `default_constructed_unit_structs` flags the latter on zero-field
        // structs. The derived `Default` still matters for symmetry with
        // future non-unit shapes and for `#[derive]` consumers.
        let mut handler = LogEventHandler;
        let event = AppEvent::File(FileEvent::Renamed {
            from: MediaPath::new("a.txt"),
            to: MediaPath::new("b.txt"),
            volume: VolumeId(Uuid::nil()),
        });
        handler.handle(event).await;
    }

    #[tokio::test]
    async fn log_handler_handles_scan_completed() {
        let mut handler = LogEventHandler;
        let event = AppEvent::ScanCompleted {
            volume: VolumeId(Uuid::nil()),
            files_seen: 10,
            files_new: 3,
            duration_ms: 500,
        };
        handler.handle(event).await;
    }

    #[tokio::test]
    async fn log_handler_handles_index_invalidated() {
        let mut handler = LogEventHandler;
        let event = AppEvent::IndexInvalidated {
            reason: InvalidationReason::TagsChanged,
        };
        handler.handle(event).await;
    }
}

// ===== Batch I additions: subscriber init + log-dir resolver =====

use directories::ProjectDirs;
use perima_core::CoreError;
use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Configuration for `init_subscriber`.
///
/// WHY a struct (not multiple bare args): five orthogonal knobs
/// (filter, verbosity bump, format toggle, log dir override, rotation,
/// file prefix) — bare-arg signatures break readability.
#[derive(Debug)]
pub struct SubscriberOpts {
    /// Base env-filter directive. Default: `std::env::var("PERIMA_LOG").unwrap_or_else(|_| "info".into())`.
    pub env_filter_base: String,
    /// Bump perima crates' filter level by N (CLI -v -vv). 0 = no bump.
    pub verbosity_bump: u8,
    /// Force JSON format. Some(true) = JSON; Some(false) = pretty;
    /// None = build-profile default (JSON in release, pretty in debug).
    pub force_json: Option<bool>,
    /// Override log dir. None = `directories::ProjectDirs::from("dev","perima","perima")`-resolved.
    pub log_dir: Option<PathBuf>,
    /// File rotation policy. Default: HOURLY.
    pub rotation: rolling::Rotation,
    /// File-name prefix for the rolling appender. Default: "perima".
    pub file_prefix: String,
}

impl SubscriberOpts {
    /// Inner constructor — pure-string interface for tests (no env reads).
    fn from_filter_base(
        env_filter_base: String,
        verbosity_bump: u8,
        force_json: Option<bool>,
    ) -> Self {
        Self {
            env_filter_base,
            verbosity_bump,
            force_json,
            log_dir: None,
            rotation: rolling::Rotation::HOURLY,
            file_prefix: "perima".into(),
        }
    }

    /// Defaults for the CLI binary (env-filter from `PERIMA_LOG`, JSON
    /// per-build-profile default, hourly rotation, "perima" prefix).
    #[must_use]
    pub fn cli_default(verbosity_bump: u8) -> Self {
        Self::from_filter_base(
            std::env::var("PERIMA_LOG").unwrap_or_else(|_| "info".into()),
            verbosity_bump,
            parse_force_json_env(),
        )
    }

    /// Defaults for the desktop binary. Identical to `cli_default(0)` —
    /// desktop has no `-v` flag so verbosity bump is always 0.
    #[must_use]
    pub fn desktop_default() -> Self {
        Self::cli_default(0)
    }
}

/// Pure helper — testable without env mutation (crates/app/src/lib.rs has
/// `#![forbid(unsafe_code)]` which makes `unsafe { std::env::set_var(...) }`
/// a hard compile error; tests parse known strings instead).
fn parse_force_json_str(s: Option<&str>) -> Option<bool> {
    match s {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    }
}

fn parse_force_json_env() -> Option<bool> {
    // WHY: PERIMA_LOG_JSON=1 → force JSON; =0 → force pretty; unset → build-profile default.
    parse_force_json_str(std::env::var("PERIMA_LOG_JSON").ok().as_deref())
}

/// Resolve the log directory (creates it if missing).
///
/// Linux: `~/.local/state/perima/logs/` (`XDG_STATE_HOME`).
/// macOS: `~/Library/Application Support/dev.perima.perima/logs/`.
/// Windows: `%LOCALAPPDATA%\perima\perima\logs\` (qualifier "dev" is dropped on Windows per `directories` v6).
///
/// # Errors
/// - [`CoreError::Internal`] if no home directory is found.
/// - [`CoreError::Io`] if the log directory cannot be created.
pub fn resolve_log_dir() -> Result<PathBuf, CoreError> {
    let proj = ProjectDirs::from("dev", "perima", "perima")
        .ok_or_else(|| CoreError::Internal("no home dir for log path".into()))?;
    // state_dir() is Linux-only (XDG_STATE_HOME). Fall back to data_dir()
    // — durable across runs; cache_dir CAN be auto-purged by macOS.
    let dir = proj.state_dir().unwrap_or_else(|| proj.data_dir());
    let logs = dir.join("logs");
    std::fs::create_dir_all(&logs)?;
    Ok(logs)
}

/// Init `tracing-subscriber` with stderr + rolling-file layers.
///
/// Returns a `WorkerGuard` that MUST be held for the process lifetime;
/// dropping it terminates the background-flush thread and loses any
/// buffered log lines.
///
/// # Errors
/// - [`CoreError::Internal`] if `EnvFilter` parse fails.
/// - [`CoreError::Internal`] if subscriber is already set (caller decides
///   whether to ignore — tests typically do).
/// - [`CoreError::Io`] if log dir creation fails.
pub fn init_subscriber(opts: SubscriberOpts) -> Result<WorkerGuard, CoreError> {
    let bump = match opts.verbosity_bump {
        0 => None,
        1 => Some("debug"),
        _ => Some("trace"),
    };
    let filter_str = match bump {
        Some(lvl) => format!("{},perima={}", opts.env_filter_base, lvl),
        None => opts.env_filter_base.clone(),
    };
    let filter = EnvFilter::try_new(&filter_str)
        .map_err(|e| CoreError::Internal(format!("env filter: {e}")))?;

    let log_dir = match opts.log_dir.as_ref() {
        Some(p) => p.clone(),
        None => resolve_log_dir()?,
    };
    let file_appender =
        rolling::RollingFileAppender::new(opts.rotation, &log_dir, &opts.file_prefix);
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let use_json = opts.force_json.unwrap_or(!cfg!(debug_assertions));

    // WHY two if/else chains (vs .boxed() type-erase): per spec §6.3 D-8 — simpler error
    // stacks; current crates/cli/src/logging.rs already uses this pattern. JSON layers and
    // pretty layers have different concrete types; we'd need .boxed() to share a chain.
    let registry = tracing_subscriber::registry().with(filter);
    let init_result = if use_json {
        registry
            .with(fmt::layer().json().with_writer(std::io::stderr))
            .with(fmt::layer().json().with_writer(file_writer))
            .try_init()
    } else {
        registry
            .with(fmt::layer().with_writer(std::io::stderr))
            .with(fmt::layer().with_writer(file_writer))
            .try_init()
    };
    init_result.map_err(|e| CoreError::Internal(format!("subscriber: {e}")))?;
    Ok(guard)
}

/// Truncate a string to `n` characters (UTF-8 safe). Used by
/// `#[tracing::instrument(fields(query = %truncated(...)))]` so user-input
/// strings can't bloat span fields.
pub(crate) fn truncated(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod batch_i_tests {
    use super::*;

    // WHY pure-helper tests (no env mutation): crates/app/src/lib.rs:26 has
    // `#![forbid(unsafe_code)]` — `unsafe { std::env::set_var(...) }` would
    // be a hard compile error here. We test the pure helpers (`from_filter_base`,
    // `parse_force_json_str`) and rely on `cli_default()`/`desktop_default()`
    // being thin wrappers we can integration-test elsewhere if needed.

    #[test]
    fn from_filter_base_preserves_inputs() {
        let opts = SubscriberOpts::from_filter_base("info".into(), 0, None);
        assert_eq!(opts.env_filter_base, "info");
        assert_eq!(opts.verbosity_bump, 0);
        assert_eq!(opts.force_json, None);
        assert_eq!(opts.file_prefix, "perima");
    }

    #[test]
    fn from_filter_base_with_verbosity() {
        let opts = SubscriberOpts::from_filter_base("info".into(), 2, Some(true));
        assert_eq!(opts.verbosity_bump, 2);
        assert_eq!(opts.force_json, Some(true));
    }

    #[test]
    fn parse_force_json_str_variants() {
        assert_eq!(parse_force_json_str(Some("1")), Some(true));
        assert_eq!(parse_force_json_str(Some("0")), Some(false));
        assert_eq!(parse_force_json_str(Some("yes")), None); // unknown → None
        assert_eq!(parse_force_json_str(None), None);
    }

    #[test]
    fn truncated_handles_unicode() {
        // 5 emojis = 5 chars but >5 bytes; ensure char-count truncation, not byte slicing.
        assert_eq!(truncated("😀😀😀😀😀😀😀", 5).chars().count(), 5);
    }

    #[test]
    fn truncated_under_limit_is_identity() {
        assert_eq!(truncated("hi", 5), "hi");
    }

    #[test]
    fn resolve_log_dir_creates_directory() {
        let dir = resolve_log_dir().expect("resolve");
        assert!(
            dir.exists(),
            "log dir should be created at {}",
            dir.display()
        );
        assert!(dir.ends_with("logs"));
    }
}
