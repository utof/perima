//! Integration test: perima ls after scan.

use std::io::Write;
use std::path::Path;
use std::process::Command;

fn mk_fixture(dir: &Path) {
    for (name, content) in [
        ("alpha.txt", b"alpha" as &[u8]),
        ("sub/beta.txt", b"beta"),
        ("sub/gamma.bin", b"\x00\x01\x02\x03"),
    ] {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::File::create(&path)
            .expect("create")
            .write_all(content)
            .expect("write");
    }
}

const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

fn scan_first(td: &Path, env_dir: &Path) {
    let output = Command::new(bin())
        .arg("scan")
        .arg(td)
        .env("PERIMA_CONFIG_DIR", env_dir)
        .env("PERIMA_DATA_DIR", env_dir)
        .output()
        .expect("scan");
    assert!(
        output.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ls_shows_three_files() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    scan_first(td.path(), env_dir.path());

    let output = Command::new(bin())
        .arg("ls")
        .arg("--limit")
        .arg("10")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("ls");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let lines: Vec<&str> = stdout.lines().collect();
    // Header + 3 data lines.
    assert_eq!(lines.len(), 4, "expected header + 3 lines, got: {lines:?}");
}

#[test]
fn ls_json_deserializes() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    scan_first(td.path(), env_dir.path());

    let output = Command::new(bin())
        .arg("ls")
        .arg("--json")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("ls --json");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let records: Vec<perima_core::FileLocationRecord> =
        serde_json::from_str(&stdout).expect("deserialize");
    assert_eq!(records.len(), 3);
}
