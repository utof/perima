//! Integration: `perima debug-report` produces a file with the expected
//! header structure + active log content.

#![allow(clippy::unwrap_used)] // WHY: integration test; panics are assertion failures, not prod bugs.

use std::process::Command;

use tempfile::TempDir;

#[test]
fn debug_report_writes_header_and_active_log() {
    let tmp = TempDir::new().expect("tempdir");
    let xdg_state = tmp.path().to_path_buf();
    let report_path = tmp.path().join("report.log");

    // Run the CLI under XDG_STATE_HOME=<tmp> so logs go to tmp/perima/logs/
    let status = Command::new(env!("CARGO_BIN_EXE_perima"))
        .args(["debug-report", report_path.to_str().unwrap()])
        .env("XDG_STATE_HOME", &xdg_state)
        .env("PERIMA_LOG", "info")
        .status()
        .expect("run perima debug-report");
    assert!(status.success(), "debug-report exited non-zero");

    let body = std::fs::read_to_string(&report_path).expect("read report");
    assert!(
        body.contains("=== perima debug report ==="),
        "header divider missing"
    );
    assert!(body.contains("perima version:"), "version line missing");
    assert!(body.contains("Git SHA:"), "git sha line missing");
    // WHY: divider is "=== active log: <name> ===" where <name> is the
    // most-recent rolling-appender file ("perima.YYYY-MM-DD-HH") OR "(none)"
    // on a cold-start where no log file exists yet (test runs before any
    // log line is emitted to disk by the non-blocking appender flush).
    assert!(body.contains("=== active log: "), "active divider missing");
    assert!(
        body.contains("=== end of report ==="),
        "footer divider missing"
    );
}
