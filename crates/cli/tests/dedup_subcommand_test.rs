//! Integration tests: `perima dedup` subcommand.
//!
//! WHY subprocess-based: verifies the full dispatch path including DB schema
//! migrations (V011 `quick_hash` + `verified_distinct` columns) and clap argument
//! parsing through the real binary entry point.

use std::io::Write;
use std::path::Path;
use std::process::Command;

const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

fn run_scan(td: &Path, env_dir: &Path) {
    let out = Command::new(bin())
        .args(["scan", "--no-thumbnails"])
        .arg(td)
        .env("PERIMA_CONFIG_DIR", env_dir)
        .env("PERIMA_DATA_DIR", env_dir)
        .output()
        .expect("spawn perima scan");
    assert!(
        out.status.success(),
        "scan failed\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `perima dedup --check` on an empty database exits 0 and reports no
/// candidate groups.
#[test]
fn dedup_check_empty_db_prints_no_candidates() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");

    // Create + scan a single unique file.
    let file1 = td.path().join("unique.txt");
    std::fs::File::create(&file1)
        .expect("create file")
        .write_all(b"this content is unique and produces no collision")
        .expect("write");

    run_scan(td.path(), env_dir.path());

    let out = Command::new(bin())
        .args(["dedup", "check"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima dedup check");

    assert!(
        out.status.success(),
        "dedup check must exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        stdout.contains("no duplicate") || stdout.contains("0 candidate"),
        "expected 'no duplicate' or '0 candidate' in: {stdout}"
    );
}

/// `perima dedup check` exits 0 when there are no indexed files at all.
#[test]
fn dedup_check_no_indexed_files_exits_zero() {
    // Don't scan any files — empty DB.
    let env_dir = tempfile::tempdir().expect("env dir");

    let out = Command::new(bin())
        .args(["dedup", "check"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima dedup check");

    assert!(
        out.status.success(),
        "dedup check on empty db must exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `perima dedup verify` exits 0 when there are no candidates.
#[test]
fn dedup_verify_no_candidates_exits_zero() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");

    let file1 = td.path().join("solo.txt");
    std::fs::File::create(&file1)
        .expect("create")
        .write_all(b"completely unique content XYZ")
        .expect("write");

    run_scan(td.path(), env_dir.path());

    let out = Command::new(bin())
        .args(["dedup", "verify"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima dedup verify");

    assert!(
        out.status.success(),
        "dedup verify must exit 0 with no candidates\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `perima dedup mark-distinct` with a valid UUID exits 0.
///
/// WHY use a random UUID: `mark_verified_distinct` in the port is a write
/// that the default adapter accepts even for non-existent UUIDs (the SQL
/// UPDATE rows-affected = 0 is not an error). This tests the happy-path
/// plumbing from CLI → dispatcher → `DedupUseCase` → port.
#[test]
fn dedup_mark_distinct_valid_uuid_exits_zero() {
    let env_dir = tempfile::tempdir().expect("env dir");

    // Need a minimal DB to exist — scan an empty dir to trigger migrations.
    let scan_dir = tempfile::tempdir().expect("scan dir");
    run_scan(scan_dir.path(), env_dir.path());

    // Use a valid UUID v4 (not v7 — either is accepted by the parser).
    let test_uuid = uuid::Uuid::new_v4().to_string();

    let out = Command::new(bin())
        .args(["dedup", "mark-distinct", &test_uuid])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima dedup mark-distinct");

    assert!(
        out.status.success(),
        "dedup mark-distinct with valid UUID must exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        stdout.contains("marked 1 file"),
        "must confirm 1 file marked, got: {stdout}"
    );
}

/// `perima dedup mark-distinct` with a bad UUID string exits non-zero.
#[test]
fn dedup_mark_distinct_bad_uuid_exits_nonzero() {
    let env_dir = tempfile::tempdir().expect("env dir");
    let scan_dir = tempfile::tempdir().expect("scan dir");
    run_scan(scan_dir.path(), env_dir.path());

    let out = Command::new(bin())
        .args(["dedup", "mark-distinct", "not-a-valid-uuid"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima dedup mark-distinct bad");

    assert!(
        !out.status.success(),
        "dedup mark-distinct with bad UUID must exit non-zero"
    );
}
