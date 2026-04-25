//! Integration tests for the `perima migrate-data-dir` command surface.
//!
//! Full path-logic coverage lives in unit tests inside
//! `crates/cli/src/cmd/migrate_data_dir.rs` (the `migrate()` helper accepts
//! explicit paths so it can use `TempDir`-backed fixtures without touching
//! real user data). These process-level tests verify the command wiring and
//! CLI surface only.

#![allow(clippy::unwrap_used)] // WHY: integration test; panics are assertion failures, not prod bugs.

fn perima_bin() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_perima"))
}

/// `perima migrate-data-dir --help` exits 0 (command is registered).
#[test]
fn migrate_data_dir_help_exits_zero() {
    let status = perima_bin()
        .args(["migrate-data-dir", "--help"])
        .status()
        .expect("run perima migrate-data-dir --help");
    assert!(status.success(), "migrate-data-dir --help should exit 0");
}

/// `perima migrate-data-dir --dry-run` with `PERIMA_DATA_DIR` pointing to an
/// empty canonical dir exits 0, even if the legacy path has no DB.
#[test]
fn migrate_data_dir_dry_run_always_exits_zero() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    // Point canonical at the tmp dir. Legacy DB almost certainly absent in CI.
    // If it is present on a dev machine, dry-run still exits 0.
    let status = perima_bin()
        .args(["migrate-data-dir", "--dry-run"])
        .env("PERIMA_DATA_DIR", tmp.path())
        .status()
        .expect("run perima migrate-data-dir --dry-run");
    assert!(status.success(), "migrate-data-dir --dry-run should exit 0");
}
