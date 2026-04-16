//! Integration tests: `perima search` subcommand.
//!
//! WHY subprocess-based: verifies the full dispatch path including DB schema
//! migrations (V006 `FTS5` tables + triggers) and `clap` argument parsing.

use std::io::Write;
use std::path::Path;
use std::process::Command;

const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

fn mk_fixture(dir: &Path) {
    for (name, content) in [
        ("alpha.txt", b"alpha content" as &[u8]),
        ("sub/beta.txt", b"beta content"),
        ("sub/gamma.bin", b"\x00\x01\x02\x03"),
    ] {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::File::create(&path)
            .expect("create fixture")
            .write_all(content)
            .expect("write fixture");
    }
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

#[test]
fn search_rebuild_exits_zero() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    run_scan(td.path(), env_dir.path());

    let out = Command::new(bin())
        .args(["search", "--rebuild"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima search --rebuild");
    assert!(
        out.status.success(),
        "search --rebuild must exit 0\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn search_finds_file_by_name_token() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    run_scan(td.path(), env_dir.path());

    // Rebuild index so the FTS5 table has content (trigger fires on metadata
    // insert, but metadata extraction may not run in --no-thumbnails mode for
    // plain text files; rebuild ensures coverage).
    Command::new(bin())
        .args(["search", "--rebuild"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("rebuild");

    let out = Command::new(bin())
        .args(["search", "alpha"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima search alpha");
    assert!(
        out.status.success(),
        "search must exit 0\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("alpha"),
        "output must mention alpha.txt, got: {stdout}"
    );
}

#[test]
fn search_json_output_is_valid() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    run_scan(td.path(), env_dir.path());

    Command::new(bin())
        .args(["search", "--rebuild"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("rebuild");

    let out = Command::new(bin())
        .args(["search", "--json", "beta"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima search --json beta");
    assert!(out.status.success(), "search --json must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Must be a JSON array.
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("output must be valid JSON");
    assert!(
        parsed.is_array(),
        "JSON output must be an array, got: {stdout}"
    );
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 1, "exactly one result for 'beta'");
    let hit = &arr[0];
    assert!(
        hit["relative_path"].as_str().unwrap_or("").contains("beta"),
        "hit path must contain 'beta', got: {hit}"
    );
}

#[test]
fn search_no_results_for_unknown_term() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    run_scan(td.path(), env_dir.path());

    Command::new(bin())
        .args(["search", "--rebuild"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("rebuild");

    let out = Command::new(bin())
        .args(["search", "xyzzy_nonexistent_42"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn search nonexistent");
    assert!(out.status.success(), "search must exit 0 even with no hits");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no results"),
        "must print '(no results)' for empty result set, got: {stdout}"
    );
}
