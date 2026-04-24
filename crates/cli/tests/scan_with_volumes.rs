//! Integration tests: real volume detection during scan.
//!
//! WHY: these tests verify that `perima scan` populates the `volumes` and
//! `volume_mounts` tables with real entries (not the all-zeros sentinel
//! `VolumeId` used in phase-1b) and that the per-file sentinel migration
//! correctly rewrites any lingering sentinel `volume_id` values.

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

/// The all-zeros UUID used as a sentinel `volume_id` in phase-1b.
const SENTINEL_VOLUME_ID: &str = "00000000-0000-0000-0000-000000000000";

fn run_scan(td: &Path, env_dir: &Path) {
    let output = Command::new(bin())
        .arg("scan")
        .arg(td)
        .env("PERIMA_CONFIG_DIR", env_dir)
        .env("PERIMA_DATA_DIR", env_dir)
        .output()
        .expect("run perima scan");
    assert!(
        output.status.success(),
        "scan failed\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn scan_uses_real_volume() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());

    run_scan(td.path(), env_dir.path());

    let db_path = env_dir.path().join("perima.db");
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open db");

    // The `volumes` table must have at least one row.
    let vol_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM volumes", [], |r| r.get(0))
        .expect("count volumes");
    assert!(
        vol_count >= 1,
        "expected ≥1 row in volumes, got {vol_count}"
    );

    // The `volume_mounts` table must have at least one row.
    let mount_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM volume_mounts", [], |r| r.get(0))
        .expect("count volume_mounts");
    assert!(
        mount_count >= 1,
        "expected ≥1 row in volume_mounts, got {mount_count}"
    );

    // Every file_locations row must have a real (non-sentinel) volume_id.
    // WHY: this is the primary correctness assertion — if any row still holds
    // the all-zeros sentinel it means the volume detection or persist path is
    // broken.
    let sentinel_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_locations WHERE volume_id = ?1",
            [SENTINEL_VOLUME_ID],
            |r| r.get(0),
        )
        .expect("count sentinel rows");
    assert_eq!(
        sentinel_count, 0,
        "found {sentinel_count} file_locations rows still carrying the sentinel volume_id"
    );
}

#[test]
fn sentinel_rows_migrated() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());

    // Step 1: first scan populates the DB with real volume rows.
    run_scan(td.path(), env_dir.path());

    // Step 2: open DB with raw rusqlite and manually corrupt one
    // file_locations row by setting its volume_id back to the all-zeros
    // sentinel, simulating a row written by the phase-1b code.
    // WHY: we inject the sentinel after the first scan rather than before
    // because open_and_migrate is not exposed to integration tests — we let
    // the binary create the schema, then poison one row.
    let db_path = env_dir.path().join("perima.db");
    {
        // WHY #[allow]: this Connection performs an UPDATE so it must be
        // read-write. The `perima scan` subprocess (writer actor) has
        // already exited by the time this test code runs, so there is no
        // concurrent second writable Connection — the GH #131 lock-order-inversion bug
        // class does not apply. The clippy lint cannot prove the
        // subprocess-then-direct-Connection sequencing, hence the allow.
        #[allow(clippy::disallowed_methods)]
        let conn = rusqlite::Connection::open(&db_path).expect("open db for sentinel injection");
        // Pick the first active file_locations row and set its volume_id to
        // the sentinel. LIMIT 1 keeps the test deterministic.
        let rows_updated = conn
            .execute(
                "UPDATE file_locations
                 SET volume_id = ?1
                 WHERE id = (
                     SELECT id FROM file_locations
                     WHERE deleted_at IS NULL
                     LIMIT 1
                 )",
                [SENTINEL_VOLUME_ID],
            )
            .expect("inject sentinel volume_id");
        assert_eq!(rows_updated, 1, "expected exactly 1 row updated");
    }

    // Verify the injection worked.
    {
        let conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .expect("re-open db");
        let sentinel_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_locations WHERE volume_id = ?1",
                [SENTINEL_VOLUME_ID],
                |r| r.get(0),
            )
            .expect("count after injection");
        assert_eq!(sentinel_count, 1, "sentinel injection did not take effect");
    }

    // Step 3: second scan — the sentinel migration in scan.rs should fire
    // and rewrite the poisoned row's volume_id to the real value.
    run_scan(td.path(), env_dir.path());

    // Step 4: assert no sentinel rows remain.
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("open db after second scan");
    let sentinel_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM file_locations WHERE volume_id = ?1",
            [SENTINEL_VOLUME_ID],
            |r| r.get(0),
        )
        .expect("count after second scan");
    assert_eq!(
        sentinel_count, 0,
        "sentinel row was not migrated by the second scan"
    );
}
