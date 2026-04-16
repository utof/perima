//! Headless integration tests for the Tauri backend commands.
//!
//! WHY: `tauri::State<AppState>` cannot be constructed outside a running Tauri
//! app. These tests call the `_inner` helpers extracted from each command,
//! which accept plain `Path` + `DeviceId` arguments. The underlying logic is
//! identical — only the Tauri IPC wrapping is absent.

use std::io::Write;
use std::path::Path;

use perima_core::{BlakeHash, DeviceId, MediaMetadata, MetadataRepository};
use perima_db::{SqliteMetadataRepository, open_and_migrate};
use perima_desktop::commands::{
    list_files_inner, list_files_with_metadata_inner, list_volumes_inner, run_scan_inner,
};

/// Create three fixture files that mimic the canonical CLI test fixtures.
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

/// Scan three fixture files and assert `total=3, new=3, errors=0`.
#[test]
fn scan_indexes_files() {
    let fixture_dir = tempfile::tempdir().expect("tempdir for fixtures");
    let data_dir = tempfile::tempdir().expect("tempdir for data");
    mk_fixture(fixture_dir.path());

    let device_id = DeviceId::new();
    let result = run_scan_inner(
        fixture_dir.path(),
        /* dry_run */ false,
        data_dir.path(),
        device_id,
    )
    .expect("scan_inner should succeed");

    assert_eq!(
        result.total, 3,
        "expected 3 total files, got {}",
        result.total
    );
    assert_eq!(result.new, 3, "expected 3 new files, got {}", result.new);
    assert_eq!(result.errors, 0, "expected 0 errors, got {}", result.errors);
}

/// After a successful scan, `list_files_inner` must return all 3 records.
#[test]
fn list_files_after_scan() {
    let fixture_dir = tempfile::tempdir().expect("tempdir for fixtures");
    let data_dir = tempfile::tempdir().expect("tempdir for data");
    mk_fixture(fixture_dir.path());

    let device_id = DeviceId::new();
    run_scan_inner(fixture_dir.path(), false, data_dir.path(), device_id)
        .expect("scan_inner should succeed");

    let entries =
        list_files_inner(data_dir.path(), 100, None).expect("list_files_inner should succeed");

    assert_eq!(
        entries.len(),
        3,
        "expected 3 file entries, got {}",
        entries.len()
    );
}

/// After inserting metadata for a scanned file, the
/// `list_files_with_metadata_inner` helper must return at least one row
/// with metadata fields populated from the stored record.
#[test]
fn list_files_with_metadata_returns_rows() {
    let fixture_dir = tempfile::tempdir().expect("tempdir for fixtures");
    let data_dir = tempfile::tempdir().expect("tempdir for data");
    mk_fixture(fixture_dir.path());

    let device_id = DeviceId::new();
    run_scan_inner(fixture_dir.path(), false, data_dir.path(), device_id)
        .expect("scan_inner should succeed");

    // Attach a metadata row to one of the scanned files. We pull its
    // hash from `list_files_inner` to guarantee FK-compatibility with
    // the `files` row the scanner just inserted.
    let entries = list_files_inner(data_dir.path(), 100, None).expect("list_files_inner");
    assert!(!entries.is_empty(), "scan must have inserted ≥1 file");
    let first_hash = BlakeHash::parse_hex(&entries[0].hash).expect("parse hash");

    let db_path = data_dir.path().join("perima.db");
    let repo = SqliteMetadataRepository::new(open_and_migrate(&db_path).expect("open"));
    let meta = MediaMetadata {
        hash: first_hash,
        width: Some(640),
        height: Some(480),
        duration_ms: None,
        captured_at: Some("2026-04-16T00:00:00Z".into()),
        camera_make: Some("Acme".into()),
        camera_model: Some("Cam One".into()),
        codec: None,
        bitrate_bps: None,
        mime_type: Some("image/jpeg".into()),
    };
    repo.upsert_metadata(&meta, device_id)
        .expect("upsert_metadata");

    let rows = list_files_with_metadata_inner(&repo, 100, None)
        .expect("list_files_with_metadata_inner should succeed");

    assert!(
        !rows.is_empty(),
        "expected ≥1 FileWithMetadataPayload row, got 0"
    );
    let populated = rows
        .iter()
        .find(|r| r.hash == entries[0].hash)
        .expect("row for inserted metadata must be present");
    assert_eq!(populated.width, Some(640));
    assert_eq!(populated.height, Some(480));
    assert_eq!(populated.camera_make.as_deref(), Some("Acme"));
    assert_eq!(populated.mime_type.as_deref(), Some("image/jpeg"));
}

/// After a successful scan, `list_volumes_inner` must return at least one volume.
#[test]
fn list_volumes_after_scan() {
    let fixture_dir = tempfile::tempdir().expect("tempdir for fixtures");
    let data_dir = tempfile::tempdir().expect("tempdir for data");
    mk_fixture(fixture_dir.path());

    let device_id = DeviceId::new();
    run_scan_inner(fixture_dir.path(), false, data_dir.path(), device_id)
        .expect("scan_inner should succeed");

    let volumes =
        list_volumes_inner(data_dir.path(), device_id).expect("list_volumes_inner should succeed");

    assert!(!volumes.is_empty(), "expected ≥1 volume after scan, got 0");
}
