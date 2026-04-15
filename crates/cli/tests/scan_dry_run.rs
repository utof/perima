//! End-to-end `perima scan --dry-run` via the built binary.

use std::io::Write;
use std::path::Path;
use std::process::Command;

fn mk_fixture(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = vec![
        ("alpha.txt".to_string(), b"alpha".to_vec()),
        ("sub/beta.txt".to_string(), b"beta".to_vec()),
        ("sub/gamma.bin".to_string(), b"\x00\x01\x02\x03".to_vec()),
    ];
    for (name, bytes) in &files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::File::create(&path)
            .expect("create")
            .write_all(bytes)
            .expect("write");
    }
    files.sort();
    files
}

const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

#[test]
fn dry_run_prints_hashes_and_summary() {
    let td = tempfile::tempdir().expect("tempdir");
    let _fixture = mk_fixture(td.path());

    let tmp_env = tempfile::tempdir().expect("env dir");
    let output = Command::new(bin())
        .arg("scan")
        .arg("--dry-run")
        .arg(td.path())
        .env("PERIMA_CONFIG_DIR", tmp_env.path())
        .env("PERIMA_DATA_DIR", tmp_env.path())
        .output()
        .expect("run perima");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines.len(), 3, "expected 3 hashed files, got: {lines:?}");

    for line in &lines {
        let mut parts = line.splitn(3, "  ");
        let hash = parts.next().expect("hash field");
        let size = parts.next().expect("size field");
        let path = parts.next().expect("path field");
        assert_eq!(hash.len(), 64, "bad hash length in: {line}");
        assert!(
            hash.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "hash not lowercase hex: {line}"
        );
        assert!(size.bytes().all(|b| b.is_ascii_digit()), "bad size: {line}");
        assert!(!path.is_empty(), "empty path: {line}");
    }

    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("scanned 3 files (dry-run; DB not yet wired)"),
        "stderr missing summary; got: {stderr}"
    );
}

#[test]
fn dry_run_is_deterministic_across_runs() {
    let td = tempfile::tempdir().expect("tempdir");
    mk_fixture(td.path());
    let tmp_env = tempfile::tempdir().expect("env dir");

    let run = || {
        let output = Command::new(bin())
            .arg("scan")
            .arg("--dry-run")
            .arg("--quiet")
            .arg(td.path())
            .env("PERIMA_CONFIG_DIR", tmp_env.path())
            .env("PERIMA_DATA_DIR", tmp_env.path())
            .output()
            .expect("run perima");
        assert!(output.status.success());
        String::from_utf8(output.stdout).expect("utf8")
    };

    // With --quiet, stdout is empty on both runs.
    let a = run();
    let b = run();
    assert_eq!(a, b);
}

#[test]
fn real_scan_refused_in_phase_1a() {
    let td = tempfile::tempdir().expect("tempdir");
    mk_fixture(td.path());
    let tmp_env = tempfile::tempdir().expect("env dir");

    let output = Command::new(bin())
        .arg("scan")
        .arg(td.path())
        .env("PERIMA_CONFIG_DIR", tmp_env.path())
        .env("PERIMA_DATA_DIR", tmp_env.path())
        .output()
        .expect("run perima");

    assert_eq!(output.status.code(), Some(2), "expected exit 2");
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("phase 1a ships only 'scan --dry-run'"),
        "stderr missing guard message; got: {stderr}"
    );
}
