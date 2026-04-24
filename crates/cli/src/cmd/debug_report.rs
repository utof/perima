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
const ACTIVE_DIVIDER: &str = "=== active log: perima.log ===";
const FOOTER_DIVIDER: &str = "=== end of report ===";
const MISSING_ACTIVE_PLACEHOLDER: &str = "(no active log file present)";

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

    // ---- Active log ----
    writeln!(out, "{ACTIVE_DIVIDER}")?;
    let active_path = log_dir.join("perima.log");
    if active_path.exists() {
        let body = fs::read_to_string(&active_path)?;
        out.write_all(body.as_bytes())?;
    } else {
        writeln!(out, "{MISSING_ACTIVE_PLACEHOLDER}")?;
    }
    writeln!(out)?;

    // ---- Last K rotated files (lex-sort descending == chronological descending) ----
    if include_rotated > 0 {
        let mut rotated: Vec<PathBuf> = fs::read_dir(&log_dir)?
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let name = p.file_name()?.to_str()?;
                if name.starts_with("perima.log.") {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();
        rotated.sort_by(|a, b| b.file_name().cmp(&a.file_name())); // descending
        for p in rotated.into_iter().take(include_rotated) {
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
