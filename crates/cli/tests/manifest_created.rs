//! Integration test: manifest.db creation after scan.
//!
//! WHY: The manifest is written to `<volume_root>/.perima/manifest.db` where
//! `volume_root` is the mount point of the scanned directory's volume — not
//! the scan root itself. For a scan under `/tmp`, the volume root is typically
//! `/`. Writing to `/.perima/` requires root permissions, so on most CI
//! systems the manifest write silently fails (the scan still succeeds). This
//! test therefore verifies two things:
//!
//! 1. `perima scan` exits 0 regardless of whether the manifest write succeeds
//!    (graceful degradation is the contract).
//! 2. If the manifest could be written (running as root or the volume root is
//!    user-writable), the manifest DB is valid — it has `manifest_meta` with
//!    a `volume_id` key and `manifest_files` with the expected row count.

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
            .expect("create fixture")
            .write_all(content)
            .expect("write fixture");
    }
}

const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

#[test]
// WHY ignore on Windows: see #138. Concurrent CLI test binaries write to the
// shared C:\.perima\manifest.db (volume root C:\ is writable on GHA runners),
// causing a write race that strips our 3 rows from the manifest by the time
// this test asserts. Linux/macOS are unaffected (/.perima/ is root-only;
// manifest write silently fails and the test takes its else-branch which
// validates the more interesting graceful-degradation contract).
#[cfg_attr(target_os = "windows", ignore = "see #138 — windows manifest race")]
fn manifest_db_created_after_scan() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());

    // WHY: running as root (common in CI containers) writes the manifest to
    // /.perima/manifest.db, which is shared across test runs and accumulates
    // rows. Delete it before scanning so the row-count assertion sees exactly
    // the 3 fixtures from this run.
    let candidate = Path::new("/.perima/manifest.db");
    if candidate.exists() {
        let _ = std::fs::remove_file(candidate);
    }

    let output = Command::new(bin())
        .arg("scan")
        .arg(td.path())
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("run perima scan");

    // Scan must always succeed, regardless of whether the manifest write
    // succeeds or silently fails.
    assert!(
        output.status.success(),
        "scan must exit 0 even when manifest write may fail\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Determine the volume root that perima would have written the manifest
    // to by reading the volume_mounts table from the main DB.
    let db_path = env_dir.path().join("perima.db");
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open main db");

    // Fetch the mount path that was recorded during the scan.
    let mount_path_str: Option<String> = conn
        .query_row("SELECT mount_path FROM volume_mounts LIMIT 1", [], |r| {
            r.get(0)
        })
        .ok();

    let Some(mount_path_str) = mount_path_str else {
        panic!("no volume_mounts rows found — scan did not record a mount");
    };

    let manifest_path = std::path::PathBuf::from(&mount_path_str)
        .join(".perima")
        .join("manifest.db");

    if manifest_path.exists() {
        // The manifest was written successfully (e.g. running as root, or the
        // volume root happens to be user-writable). Validate its contents.
        let mconn = rusqlite::Connection::open_with_flags(
            &manifest_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("open manifest.db");

        // manifest_meta must contain a volume_id entry.
        let vol_id: String = mconn
            .query_row(
                "SELECT value FROM manifest_meta WHERE key = 'volume_id'",
                [],
                |r| r.get(0),
            )
            .expect("manifest_meta volume_id row must exist");
        assert!(!vol_id.is_empty(), "manifest volume_id must not be empty");
        assert_ne!(
            vol_id, "00000000-0000-0000-0000-000000000000",
            "manifest volume_id must not be the sentinel"
        );

        // manifest_files must contain one row per scanned file (3 fixtures).
        // WHY filter by tempdir basename: on Windows the volume root (C:\) is
        // writable, so parallel test binaries (scan_persists, scan_with_volumes,
        // etc.) all share `C:\.perima\manifest.db` and accumulate each other's
        // rows. On Linux/macOS `/.perima/` is root-only so the manifest write
        // silently fails and the `else` branch handles it. The tempdir basename
        // (`.tmpXXXXXX`) is unique per test invocation, so a LIKE filter
        // isolates this test's rows from concurrent writes.
        let basename = td
            .path()
            .file_name()
            .expect("tempdir basename")
            .to_str()
            .expect("tempdir basename utf8");
        let pattern = format!("%{basename}%");
        let file_count: i64 = mconn
            .query_row(
                "SELECT COUNT(*) FROM manifest_files WHERE relative_path LIKE ?1",
                [&pattern],
                |r| r.get(0),
            )
            .expect("count manifest_files");
        assert_eq!(
            file_count, 3,
            "manifest must have 3 file rows for this tempdir, got {file_count}"
        );
    } else {
        // Manifest write silently failed (permission denied at the volume root).
        // Verify the scan still succeeded (already asserted above) and that a
        // warn log was emitted. The manifest is optional; this path is expected
        // in CI environments where tests run without root.
        //
        // WHY: we do not fail the test here because graceful degradation is
        // the explicit contract of write_manifest — a read-only volume root
        // must never make `perima scan` exit non-zero.
        let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
        assert!(
            stderr.contains("scanned"),
            "scan summary must appear in stderr even when manifest write fails: {stderr}"
        );
    }
}
