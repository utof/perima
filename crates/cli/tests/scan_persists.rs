//! Integration test: scan without --dry-run persists to DB.

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

#[test]
fn scan_persists_three_files() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());

    let output = Command::new(bin())
        .arg("scan")
        .arg(td.path())
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("run");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("3 new"), "expected '3 new' in: {stderr}");

    // Open the DB directly and count rows.
    let db_path = env_dir.path().join("perima.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let file_count: i64 = conn
        .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
        .expect("count files");
    assert_eq!(file_count, 3);
    let loc_count: i64 = conn
        .query_row("SELECT count(*) FROM file_locations", [], |r| r.get(0))
        .expect("count locations");
    assert_eq!(loc_count, 3);
}

#[test]
fn rescan_produces_zero_new() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());

    let run = || {
        Command::new(bin())
            .arg("scan")
            .arg(td.path())
            .env("PERIMA_CONFIG_DIR", env_dir.path())
            .env("PERIMA_DATA_DIR", env_dir.path())
            .output()
            .expect("run")
    };

    let first = run();
    assert!(first.status.success());

    let second = run();
    assert!(second.status.success());
    let stderr = String::from_utf8(second.stderr).expect("utf8");
    assert!(
        stderr.contains("0 new"),
        "expected '0 new' in second scan: {stderr}"
    );
}
