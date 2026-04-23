//! Verify `WriteCmd::File(FileWriteCmd::UpdateLocationStatus)` emits
//! `AppEvent::IndexInvalidated { reason: FilesChanged }` on the bus
//! AFTER a successful COMMIT (Batch E Task 8).

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, DiscoveredFile, EventBus, FileRepository, FileSize,
    HashedFile, InvalidationReason, LocationStatus, MediaPath, VolumeId,
};
use perima_db::{ReadPool, SqliteFileRepository, SqliteWriter};

#[derive(Debug, Default)]
struct RecordingBus {
    events: Mutex<Vec<AppEvent>>,
}

impl EventBus for RecordingBus {
    fn emit(&self, event: &AppEvent) -> Result<(), CoreError> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
}

fn sample_hashed_file(content: &[u8], rel_path: &str) -> HashedFile {
    let hash = BlakeHash::from_bytes(*blake3::hash(content).as_bytes());
    HashedFile {
        discovered: DiscoveredFile {
            absolute_path: PathBuf::from("/tmp/fake"),
            relative_path: MediaPath::new(rel_path),
            size: FileSize(content.len() as u64),
        },
        hash,
    }
}

#[test]
fn file_update_location_status_emits_index_invalidated_files_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus = Arc::new(RecordingBus::default());
    let bus_for_writer: Arc<dyn EventBus> = bus.clone();
    let writer = SqliteWriter::start(&db_path, bus_for_writer).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let vol = VolumeId::new();
    let f = sample_hashed_file(b"status_change_emit_test", "missing.jpg");
    let path = MediaPath::new("missing.jpg");

    // Seed: file + active location. Both seed steps emit; drain.
    repo.upsert_file(&f, dev).unwrap();
    repo.upsert_location(&f.hash, vol, &path, dev).unwrap();

    bus.events.lock().unwrap().clear();

    let n = repo
        .update_location_status(vol, &path, LocationStatus::Missing, dev)
        .unwrap();
    assert_eq!(n, 1, "status update must touch exactly 1 row");

    let events = bus.events.lock().unwrap();
    assert_eq!(events.len(), 1, "expected 1 event, got {events:?}");
    assert!(
        matches!(
            events[0],
            AppEvent::IndexInvalidated {
                reason: InvalidationReason::FilesChanged
            }
        ),
        "expected IndexInvalidated::FilesChanged, got {:?}",
        events[0]
    );
    drop(events);

    // No-op status update against a non-existent (volume, path) MUST
    // NOT emit — the impl writes zero rows.
    bus.events.lock().unwrap().clear();
    let nonexistent = MediaPath::new("does-not-exist.jpg");
    let n2 = repo
        .update_location_status(vol, &nonexistent, LocationStatus::Stale, dev)
        .unwrap();
    assert_eq!(n2, 0);
    let events_after_noop = bus.events.lock().unwrap();
    assert!(
        events_after_noop.is_empty(),
        "no-op status update should NOT emit (no row written), got {events_after_noop:?}"
    );
    drop(events_after_noop);

    drop(repo);
    writer.join();
}
