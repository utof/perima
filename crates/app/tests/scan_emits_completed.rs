//! Verify `ScanUseCase::execute` emits `AppEvent::ScanCompleted` after a
//! successful (non-dry-run, non-interrupted) scan.

#![allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.

use std::io::Write as _;
use std::sync::{Arc, Mutex};

use perima_app::{FullScan, ScanCommand, ScanUseCase};
use perima_core::{
    AppEvent, CoreError, EventBus, FileRepository, HashService, MetadataRepository, Scanner,
    VolumeRepository,
};
use perima_db::{
    ReadPool, SqliteFileRepository, SqliteMetadataRepository, SqliteVolumeRepository, SqliteWriter,
};
use perima_fs::WalkdirScanner;
use perima_hash::Blake3Service;
use perima_media::ThumbnailGenerator;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// An `EventBus` that records every emitted `AppEvent`.
#[derive(Default)]
struct RecordingBus {
    events: Mutex<Vec<AppEvent>>,
}

impl EventBus for RecordingBus {
    fn emit(&self, e: &AppEvent) -> Result<(), CoreError> {
        self.events
            .lock()
            .expect("RecordingBus mutex poisoned")
            .push(e.clone());
        Ok(())
    }
}

/// Minimal fixture: three files that the scanner will walk + hash.
fn mk_fixture(dir: &std::path::Path) {
    for (name, content) in [
        ("alpha.txt", b"alpha" as &[u8]),
        ("sub/beta.txt", b"beta"),
        ("sub/gamma.bin", b"\x00\x01\x02"),
    ] {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::File::create(&path)
            .unwrap()
            .write_all(content)
            .unwrap();
    }
}

/// No-op event bus — used for the `SqliteWriter` so its `IndexInvalidated`
/// events don't land in the `RecordingBus` (keeping assertion simple).
struct NullBus;
impl EventBus for NullBus {
    fn emit(&self, _e: &AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

/// A successful non-dry-run scan must emit exactly one `ScanCompleted` event
/// on the bus passed to `ScanUseCase::new`.
///
/// Other events (e.g. `IndexInvalidated::FilesChanged` from the writer actor)
/// may also be present — the assertion only checks that `ScanCompleted` exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scan_use_case_emits_scan_completed_on_success() {
    // ---- wire-up -----------------------------------------------------------
    let db_tmp = TempDir::new().unwrap();
    let fixture = TempDir::new().unwrap();
    mk_fixture(fixture.path());

    let db_path = db_tmp.path().join("perima.db");
    // WHY NullBus for writer: the writer emits IndexInvalidated events; we
    // pass NullBus so those don't appear in `recording_bus` — makes the
    // ScanCompleted assertion unambiguous.
    let writer = SqliteWriter::start(&db_path, Arc::new(NullBus)).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();

    let files: Arc<dyn FileRepository> =
        Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
    let volumes: Arc<dyn VolumeRepository> =
        Arc::new(SqliteVolumeRepository::new(writer.sender(), reads.clone()));
    let metadata: Arc<dyn MetadataRepository> =
        Arc::new(SqliteMetadataRepository::new(writer.sender(), reads));

    let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());
    let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
    let thumbnailer = Arc::new(ThumbnailGenerator::disabled());

    let recording_bus = Arc::new(RecordingBus::default());
    // The recording bus is the `events` arg — ScanUseCase will emit
    // ScanCompleted on it after execute_full succeeds.
    let events_arc: Arc<dyn EventBus> = recording_bus.clone();

    let uc = ScanUseCase::new(
        files,
        volumes,
        metadata,
        scanner,
        hasher,
        thumbnailer,
        events_arc,
    );

    // ---- execute -----------------------------------------------------------
    let device_id = perima_core::DeviceId::new();
    let cmd = ScanCommand::Full(FullScan {
        path: fixture.path().to_path_buf(),
        device_id,
        with_metadata: false,
        dry_run: false,
        no_wait_metadata: true,
        no_thumbnails: true,
        cancel: CancellationToken::new(),
        on_persist: None,
    });

    let report = uc.execute(cmd).await.expect("scan should succeed");
    assert_eq!(report.files_seen, 3, "sanity: fixture has 3 files");

    // ---- assert ------------------------------------------------------------
    // WHY no sleep: emit() is synchronous and happens before Ok(report)
    // returns. By the time execute() returns the event is already in the vec.
    // WHY clone + drop guard: release the mutex before the assertions so the
    // guard doesn't live across the drop(writer) at the end of the test,
    // avoiding the `significant_drop_tightening` lint.
    let captured: Vec<AppEvent> = recording_bus.events.lock().unwrap().clone();

    let scan_completed = captured
        .iter()
        .find(|e| matches!(e, AppEvent::ScanCompleted { .. }));

    assert!(
        scan_completed.is_some(),
        "expected AppEvent::ScanCompleted in bus events, got: {captured:?}",
    );

    // Verify the payload fields match the scan report.
    if let Some(AppEvent::ScanCompleted {
        files_seen,
        files_new,
        duration_ms,
        ..
    }) = scan_completed
    {
        assert_eq!(*files_seen, 3, "ScanCompleted.files_seen matches report");
        assert_eq!(
            *files_new, 3,
            "ScanCompleted.files_new: first scan inserts all"
        );
        assert!(
            *duration_ms > 0,
            "ScanCompleted.duration_ms must be non-zero"
        );
    }

    // WHY explicit drop order: writer must outlive all repo handles so the
    // actor thread sees a clean shutdown rather than a broken channel.
    // TempDir is dropped last to avoid the DB file disappearing while the
    // writer is still flushing.
    drop(writer);
    drop(db_tmp);
    drop(fixture);
}
