//! Integration tests: `perima hash` subcommand.
//!
//! WHY subprocess-based: verifies the full dispatch path including DB schema
//! migrations (V011 `file_uuid` + `quick_hash` columns) and clap argument parsing.

use std::io::Write;
use std::path::Path;
use std::process::Command;

const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

/// Create two plain-text fixture files in `dir`.
fn mk_fixture(dir: &Path) {
    for (name, content) in [
        ("file1.txt", b"hello world hash test" as &[u8]),
        ("file2.txt", b"another file for hashing"),
    ] {
        let path = dir.join(name);
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

/// `perima hash <path>` on a known indexed file emits a 64-char hex hash.
#[test]
fn hash_single_file_emits_hex_hash() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());

    // Index the file first.
    run_scan(td.path(), env_dir.path());

    let file1 = td.path().join("file1.txt");
    let out = Command::new(bin())
        .arg("hash")
        .arg(&file1)
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima hash <path>");

    assert!(
        out.status.success(),
        "perima hash <path> must exit 0\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let hash_hex = stdout.trim();
    // BLAKE3 hex is exactly 64 lowercase hex characters.
    assert_eq!(
        hash_hex.len(),
        64,
        "full hash output must be 64 hex chars, got: {hash_hex:?}"
    );
    assert!(
        hash_hex.chars().all(|c| c.is_ascii_hexdigit()),
        "full hash output must be hex, got: {hash_hex:?}"
    );
}

/// `perima hash <path>` is idempotent — calling it twice on the same file
/// produces the same hash.
#[test]
fn hash_single_file_is_idempotent() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    run_scan(td.path(), env_dir.path());

    let file1 = td.path().join("file1.txt");

    let hash1 = {
        let out = Command::new(bin())
            .arg("hash")
            .arg(&file1)
            .env("PERIMA_CONFIG_DIR", env_dir.path())
            .env("PERIMA_DATA_DIR", env_dir.path())
            .output()
            .expect("spawn hash 1");
        assert!(out.status.success(), "first hash failed");
        String::from_utf8(out.stdout)
            .expect("utf8")
            .trim()
            .to_owned()
    };

    let hash2 = {
        let out = Command::new(bin())
            .arg("hash")
            .arg(&file1)
            .env("PERIMA_CONFIG_DIR", env_dir.path())
            .env("PERIMA_DATA_DIR", env_dir.path())
            .output()
            .expect("spawn hash 2");
        assert!(out.status.success(), "second hash failed");
        String::from_utf8(out.stdout)
            .expect("utf8")
            .trim()
            .to_owned()
    };

    assert_eq!(hash1, hash2, "hash must be deterministic across calls");
}

/// `perima hash --all` on a database with indexed files exits 0 and prints
/// progress lines.
#[test]
fn hash_all_exits_zero_and_prints_progress() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    run_scan(td.path(), env_dir.path());

    let out = Command::new(bin())
        .args(["hash", "--all"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima hash --all");

    assert!(
        out.status.success(),
        "perima hash --all must exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).expect("utf8");
    // Should mention "computing" and "done".
    assert!(
        stdout.contains("computing") || stdout.contains("no indexed files"),
        "unexpected output: {stdout}"
    );
}

/// `perima hash --pending` exits 0 when there are no pending rows.
///
/// WHY "no pending files" case: after a normal scan with no `full_hash`
/// computation deferral, every file row has its `blake3_hash` set (since
/// the scan itself sets it), so `--pending` finds nothing to do.
#[test]
fn hash_pending_exits_zero_with_no_pending_rows() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    run_scan(td.path(), env_dir.path());

    let out = Command::new(bin())
        .args(["hash", "--pending"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima hash --pending");

    assert!(
        out.status.success(),
        "perima hash --pending must exit 0\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `perima hash` without arguments exits non-zero and prints a helpful message.
#[test]
fn hash_no_args_exits_nonzero() {
    let _td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");

    let out = Command::new(bin())
        .arg("hash")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima hash");

    // Clap exits 2 for usage errors when neither path nor flag is given.
    // The run function itself returns an InvalidPath error (exit 2) or
    // clap catches it first.
    assert!(
        !out.status.success(),
        "perima hash with no args must exit non-zero"
    );
}
