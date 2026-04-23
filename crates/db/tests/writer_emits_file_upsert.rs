//! Verify `WriteCmd::File(FileWriteCmd::UpsertLocation)` emits
//! `AppEvent::IndexInvalidated { reason: FilesChanged }` on the bus
//! AFTER a successful COMMIT (Batch E Task 8).

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, DiscoveredFile, EventBus, FileRepository, FileSize,
    HashedFile, InvalidationReason, MediaPath, VolumeId,
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
fn file_upsert_location_emits_index_invalidated_files_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus = Arc::new(RecordingBus::default());
    let bus_for_writer: Arc<dyn EventBus> = bus.clone();
    let writer = SqliteWriter::start(&db_path, bus_for_writer).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteFileRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let vol = VolumeId::new();
    let f = sample_hashed_file(b"file_upsert_emit_test", "photo.jpg");
    let path = MediaPath::new("photo.jpg");

    // Seed the files row first. UpsertFile is itself a FilesChanged
    // emitter (per writer/file.rs handle); we drain that emit before
    // the targeted UpsertLocation assertion.
    repo.upsert_file(&f, dev).unwrap();

    bus.events.lock().unwrap().clear();

    repo.upsert_location(&f.hash, vol, &path, dev).unwrap();

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

    // Unchanged arm (repeat upsert with same hash + device) MUST NOT
    // emit — the writer wrote zero rows on the Unchanged path.
    bus.events.lock().unwrap().clear();
    repo.upsert_location(&f.hash, vol, &path, dev).unwrap();
    let events_after_unchanged = bus.events.lock().unwrap();
    assert!(
        events_after_unchanged.is_empty(),
        "Unchanged upsert should NOT emit, got {events_after_unchanged:?}"
    );
    drop(events_after_unchanged);

    drop(repo);
    writer.join();
}
