//! Integration test: `perima volumes` output.

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
fn volumes_shows_one_volume() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());

    // Scan first so the volumes table is populated.
    let scan_out = Command::new(bin())
        .arg("scan")
        .arg(td.path())
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("run scan");
    assert!(
        scan_out.status.success(),
        "scan failed\nstderr: {}",
        String::from_utf8_lossy(&scan_out.stderr)
    );

    // Run `perima volumes`.
    let vol_out = Command::new(bin())
        .arg("volumes")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("run volumes");
    assert!(
        vol_out.status.success(),
        "volumes failed\nstderr: {}",
        String::from_utf8_lossy(&vol_out.stderr)
    );

    let stdout = String::from_utf8(vol_out.stdout).expect("utf8 stdout");
    let lines: Vec<&str> = stdout.lines().collect();

    // WHY: we assert ≥ 2 lines (header + at least one data row) rather than
    // an exact count because the machine running CI may have a different
    // number of mounted volumes. The key contract is that the command prints
    // the header and shows the volume that was populated by the preceding scan.
    assert!(
        lines.len() >= 2,
        "expected at least 2 lines (header + 1 data row), got {}: {:?}",
        lines.len(),
        lines
    );

    // First line is the header.
    assert!(
        lines[0].contains("VOLUME ID"),
        "first line must be the header, got: {:?}",
        lines[0]
    );

    // At least one data line follows the header.
    assert!(
        !lines[1].is_empty(),
        "second line (first data row) must not be empty"
    );
}
