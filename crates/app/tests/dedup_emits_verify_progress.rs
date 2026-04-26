//! Verify that `ComputeFullHashUseCase::execute_batch` emits per-file
//! `AppEvent::VerifyProgress` events followed by one `AppEvent::VerifyComplete`.
//!
//! Spec §4.7.3.

#![allow(clippy::unwrap_used)] // WHY: test code; unwrap panics signal bugs.
#![allow(clippy::too_many_arguments)] // WHY: seed_file helper takes one arg per column.
#![allow(clippy::too_many_lines)] // WHY: the N-file test body is assertion-heavy by design.

use std::sync::{Arc, Mutex};

use perima_app::ComputeFullHashUseCase;
use perima_core::{
    AppEvent, BatchId, BlakeHash, CoreError, DeviceId, EventBus, FileRepository, FileSize,
    FileUuid, FullHashOutcome, HashService, HashedFile, MediaPath, VolumeId, VolumeIdentifiers,
    VolumeRepository,
};
use perima_db::{ReadPool, SqliteFileRepository, SqliteVolumeRepository, SqliteWriter};
use perima_hash::Blake3Service;
use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Stubs
// ---------------------------------------------------------------------------

/// Event bus that records every emitted `AppEvent` for later assertion.
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

/// No-op bus for the `SqliteWriter` so its `IndexInvalidated` events don't
/// pollute the `RecordingBus` we inspect.
struct NullBus;
impl EventBus for NullBus {
    fn emit(&self, _: &AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn harness() -> (
    TempDir,
    Arc<SqliteFileRepository>,
    Arc<SqliteVolumeRepository>,
    Connection,
) {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("perima.db");

    let writer = SqliteWriter::start(&db_path, Arc::new(NullBus)).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();

    let file_repo = Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
    let vol_repo = Arc::new(SqliteVolumeRepository::new(writer.sender(), reads));

    // RO inspection connection (writer-bypass).
    #[allow(clippy::disallowed_methods)]
    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    drop(writer);
    (tmp, file_repo, vol_repo, ro)
}

fn make_volume(
    vol_repo: &SqliteVolumeRepository,
    dev: DeviceId,
    mount: &std::path::Path,
) -> VolumeId {
    let ids = VolumeIdentifiers {
        gpt_partition_guid: None,
        fs_uuid: Some(format!("test-fs-{}", uuid::Uuid::now_v7())),
        label: Some("test".into()),
        capacity_bytes: 1024 * 1024,
        is_removable: false,
    };
    let vol = vol_repo.find_or_create(&ids, dev).unwrap();
    vol_repo.record_mount(vol, dev, mount).unwrap();
    vol
}

fn seed_file(
    file_repo: &SqliteFileRepository,
    ro: &Connection,
    dev: DeviceId,
    vol: VolumeId,
    mount: &std::path::Path,
    rel_name: &str,
    content: &[u8],
    quick_hash: Option<BlakeHash>,
) -> (FileUuid, BlakeHash) {
    let abs_path = mount.join(rel_name);
    std::fs::write(&abs_path, content).unwrap();

    let temp = mount.join(format!(".tmp_{}", uuid::Uuid::now_v7()));
    std::fs::write(&temp, content).unwrap();
    let hash = Blake3Service::new().full_hash(&temp).unwrap();
    std::fs::remove_file(&temp).ok();

    let hf = HashedFile {
        discovered: perima_core::DiscoveredFile {
            absolute_path: abs_path,
            relative_path: MediaPath::new(rel_name),
            size: FileSize(content.len() as u64),
        },
        hash,
    };
    file_repo
        .upsert_file_with_quick_hash(&hf, dev, quick_hash)
        .unwrap();
    file_repo
        .upsert_location(&hash, vol, &hf.discovered.relative_path, dev)
        .unwrap();

    let uuid_str: String = ro
        .query_row(
            "SELECT file_uuid FROM files WHERE blake3_hash = ?1",
            [hash.to_hex()],
            |row| row.get(0),
        )
        .unwrap();
    let uuid = FileUuid(uuid::Uuid::parse_str(&uuid_str).unwrap());
    (uuid, hash)
}

/// Poll until the recording bus contains at least one `VerifyComplete`, then
/// return all captured events.
async fn wait_for_complete(bus: &RecordingBus, timeout: std::time::Duration) -> Vec<AppEvent> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let captured = bus.events.lock().unwrap().clone();
        if captured
            .iter()
            .any(|e| matches!(e, AppEvent::VerifyComplete { .. }))
        {
            return captured;
        }
        assert!(
            std::time::Instant::now() <= deadline,
            "VerifyComplete not received within {timeout:?}",
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A batch of N files must emit N `VerifyProgress` events (one per file) and
/// one final `VerifyComplete` event, all sharing the same `batch_id`.
///
/// Per spec §4.7.3.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_batch_emits_n_verify_progress_and_one_verify_complete() {
    let (tmp, file_repo, vol_repo, ro) = harness();
    let dev = DeviceId::new();
    let mount = tmp.path().join("mount");
    std::fs::create_dir_all(&mount).unwrap();
    let vol = make_volume(&vol_repo, dev, &mount);

    // Seed 3 files with distinct content.
    let (uuid_a, _) = seed_file(
        &file_repo,
        &ro,
        dev,
        vol,
        &mount,
        "a.bin",
        b"aaa content",
        None,
    );
    let (uuid_b, _) = seed_file(
        &file_repo,
        &ro,
        dev,
        vol,
        &mount,
        "b.bin",
        b"bbb content",
        None,
    );
    let (uuid_c, _) = seed_file(
        &file_repo,
        &ro,
        dev,
        vol,
        &mount,
        "c.bin",
        b"ccc content",
        None,
    );

    let recording_bus = Arc::new(RecordingBus::default());
    let events_arc: Arc<dyn EventBus> = recording_bus.clone();
    let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
    let files: Arc<dyn FileRepository> = file_repo;

    let uc = ComputeFullHashUseCase::new(hasher, files, events_arc);

    let handle = uc
        .execute_batch(vec![uuid_a, uuid_b, uuid_c])
        .await
        .expect("execute_batch should not fail");

    let expected_batch_id: BatchId = handle.batch_id;
    assert_eq!(
        handle.total, 3,
        "BatchHandle.total must equal the number of queued files",
    );

    let captured = wait_for_complete(&recording_bus, std::time::Duration::from_secs(5)).await;

    // ---- VerifyProgress assertions ------------------------------------------

    let progress_events: Vec<_> = captured
        .iter()
        .filter(|e| matches!(e, AppEvent::VerifyProgress { .. }))
        .collect();

    assert_eq!(
        progress_events.len(),
        3,
        "must have exactly 3 VerifyProgress events (one per file); got: {captured:?}",
    );

    for (i, ev) in progress_events.iter().enumerate() {
        if let AppEvent::VerifyProgress {
            batch_id,
            files_done,
            files_total,
            latest_outcome,
        } = ev
        {
            assert_eq!(
                *batch_id, expected_batch_id,
                "VerifyProgress[{i}].batch_id must match BatchHandle",
            );
            // WHY `u32::try_from` + expect: `i` is bounded by `files_total`
            // (at most u32::MAX) so the conversion cannot realistically fail
            // in a unit test; `expect` is clearer than `as u32` (silent
            // truncation on 64-bit targets triggers clippy::cast_possible_truncation).
            let expected_done = u32::try_from(i + 1).expect("test index fits in u32");
            assert_eq!(
                *files_done, expected_done,
                "VerifyProgress[{i}].files_done must be monotonically increasing",
            );
            assert_eq!(
                *files_total, 3,
                "VerifyProgress[{i}].files_total must be the batch size",
            );
            assert!(
                matches!(latest_outcome, FullHashOutcome::Computed { .. }),
                "VerifyProgress[{i}].latest_outcome must be Computed (all files are readable)",
            );
        }
    }

    // ---- VerifyComplete assertions ------------------------------------------

    let complete_events: Vec<_> = captured
        .iter()
        .filter(|e| matches!(e, AppEvent::VerifyComplete { .. }))
        .collect();

    assert_eq!(
        complete_events.len(),
        1,
        "must have exactly one VerifyComplete; got: {captured:?}",
    );

    if let AppEvent::VerifyComplete { batch_id } = complete_events[0] {
        assert_eq!(
            *batch_id, expected_batch_id,
            "VerifyComplete.batch_id must match BatchHandle",
        );
    }

    // ---- Ordering assertion --------------------------------------------------
    // All VerifyProgress events must precede the VerifyComplete.

    let complete_idx = captured
        .iter()
        .position(|e| matches!(e, AppEvent::VerifyComplete { .. }))
        .expect("VerifyComplete must exist");

    let last_progress_idx = captured
        .iter()
        .rposition(|e| matches!(e, AppEvent::VerifyProgress { .. }))
        .expect("at least one VerifyProgress must exist");

    assert!(
        last_progress_idx < complete_idx,
        "last VerifyProgress ({last_progress_idx}) must precede VerifyComplete \
         ({complete_idx})",
    );

    drop(tmp);
}

/// A batch with one file that cannot be located (`NotMounted`) must emit a
/// `VerifyProgress` with `FullHashOutcome::Failed` and then `VerifyComplete`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_batch_emits_failed_outcome_for_missing_file() {
    let (tmp, file_repo, _vol_repo, _ro) = harness();

    let recording_bus = Arc::new(RecordingBus::default());
    let events_arc: Arc<dyn EventBus> = recording_bus.clone();
    let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
    let files: Arc<dyn FileRepository> = file_repo;

    let uc = ComputeFullHashUseCase::new(hasher, files, events_arc);

    // A random FileUuid that has no DB row — compute_one will return NotMounted.
    let ghost_uuid = FileUuid(uuid::Uuid::now_v7());

    let handle = uc
        .execute_batch(vec![ghost_uuid])
        .await
        .expect("execute_batch is infallible");

    let expected_batch_id: BatchId = handle.batch_id;

    let captured = wait_for_complete(&recording_bus, std::time::Duration::from_secs(5)).await;

    let progress = captured
        .iter()
        .find(|e| matches!(e, AppEvent::VerifyProgress { .. }))
        .expect("must have a VerifyProgress for the ghost file");

    if let AppEvent::VerifyProgress {
        batch_id,
        files_done,
        files_total,
        latest_outcome,
    } = progress
    {
        assert_eq!(*batch_id, expected_batch_id);
        assert_eq!(*files_done, 1);
        assert_eq!(*files_total, 1);
        assert!(
            matches!(latest_outcome, FullHashOutcome::Failed { .. }),
            "ghost file must produce a Failed outcome; got: {latest_outcome:?}",
        );
    }

    drop(tmp);
}
