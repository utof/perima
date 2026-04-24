//! `perima debug-report` subcommand.
//!
//! Bundles the active rolling log + last K rotated files + a header
//! (perima version, `GIT_SHA`, OS, env config, timestamp) into a single
//! file the user (or AI agent triaging) can attach to a bug report.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use perima_app::telemetry::resolve_log_dir;
use perima_core::CoreError;

const HEADER_DIVIDER: &str = "=== perima debug report ===";
const ACTIVE_DIVIDER_PREFIX: &str = "=== active log: ";
const FOOTER_DIVIDER: &str = "=== end of report ===";
const MISSING_ACTIVE_PLACEHOLDER: &str = "(no active log file present)";

// WHY: tracing-appender 0.2.5 with Rotation::HOURLY + prefix "perima"
// writes filenames like "perima.YYYY-MM-DD-HH" (NOT "perima.log" or
// "perima.log.YYYY..."). The active log is the lex-greatest file
// matching this prefix; rotated files are everything below it.
const LOG_FILE_PREFIX: &str = "perima.";

/// Write a bundled debug report to `path` (default: `./perima-debug-report-<ts>.log`).
/// Includes the active log + last `include_rotated` rotated files (default 2).
///
/// WHY no `.map_err(CoreError::from)`: per CLAUDE.md Batch D section,
/// `From<io::Error> for CoreError` is implemented; `?` propagates io
/// errors directly through `Result<(), CoreError>`. Keeps the code lean.
pub(crate) fn run(path: Option<PathBuf>, include_rotated: usize) -> Result<(), CoreError> {
    let log_dir = resolve_log_dir()?;
    let dest = path.unwrap_or_else(|| {
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
        PathBuf::from(format!("./perima-debug-report-{ts}.log"))
    });

    let mut out = fs::File::create(&dest)?;

    // ---- Header ----
    writeln!(out, "{HEADER_DIVIDER}")?;
    writeln!(out, "Generated: {}", chrono::Utc::now().to_rfc3339())?;
    writeln!(out, "perima version: {}", env!("CARGO_PKG_VERSION"))?;
    writeln!(out, "Git SHA: {}", env!("GIT_SHA"))?;
    writeln!(out, "OS: {}", std::env::consts::OS)?;
    writeln!(out, "Arch: {}", std::env::consts::ARCH)?;
    writeln!(
        out,
        "PERIMA_LOG: {}",
        std::env::var("PERIMA_LOG").unwrap_or_else(|_| "(unset)".into())
    )?;
    writeln!(
        out,
        "PERIMA_LOG_JSON: {}",
        std::env::var("PERIMA_LOG_JSON").unwrap_or_else(|_| "(unset)".into())
    )?;
    writeln!(out, "Log dir: {}", log_dir.display())?;
    writeln!(out)?;

    // Collect all log files matching the rolling-appender prefix, then
    // sort lex-descending — the lex-greatest is the most recent
    // (filenames are "perima.YYYY-MM-DD-HH"; lex order == chronological).
    let mut log_files: Vec<PathBuf> = fs::read_dir(&log_dir)?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_str()?;
            if name.starts_with(LOG_FILE_PREFIX) {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    log_files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

    // ---- Active log (most recent) ----
    if let Some(active) = log_files.first() {
        let name = active.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        writeln!(out, "{ACTIVE_DIVIDER_PREFIX}{name} ===")?;
        let body = fs::read_to_string(active)?;
        out.write_all(body.as_bytes())?;
    } else {
        writeln!(out, "{ACTIVE_DIVIDER_PREFIX}(none) ===")?;
        writeln!(out, "{MISSING_ACTIVE_PLACEHOLDER}")?;
    }
    writeln!(out)?;

    // ---- Last K rotated files (skip the active one, take next K) ----
    if include_rotated > 0 {
        for p in log_files.into_iter().skip(1).take(include_rotated) {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            writeln!(out, "=== rotated: {name} ===")?;
            let body = fs::read_to_string(&p)?;
            out.write_all(body.as_bytes())?;
            writeln!(out)?;
        }
    }

    writeln!(out, "{FOOTER_DIVIDER}")?;

    eprintln!("Wrote debug report to: {}", dest.display());
    Ok(())
}
