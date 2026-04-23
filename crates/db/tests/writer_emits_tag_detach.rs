//! Verify `WriteCmd::Tag(TagWriteCmd::Detach)` emits
//! `AppEvent::IndexInvalidated { reason: TagsChanged }` on the bus
//! AFTER a successful COMMIT (Batch E Task 8).

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::sync::{Arc, Mutex};

use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, EventBus, InvalidationReason, TagRepository,
};
use perima_db::{ReadPool, SqliteTagRepository, SqliteWriter};

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

#[test]
fn tag_detach_emits_index_invalidated_tags_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus = Arc::new(RecordingBus::default());
    let bus_for_writer: Arc<dyn EventBus> = bus.clone();
    let writer = SqliteWriter::start(&db_path, bus_for_writer).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteTagRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();

    // Seed: create tag + attach. We expect a TagsChanged emit from
    // the seed-attach; we drain it before the assertion.
    let tag = repo.upsert_tag("video", dev).unwrap();
    let hash = BlakeHash::from_bytes(*blake3::hash(b"detach_emit_test").as_bytes());
    repo.attach(&hash, tag.id, dev).unwrap();

    // Drain the seed emits.
    bus.events.lock().unwrap().clear();

    repo.detach(&hash, tag.id, dev).unwrap();

    let events = bus.events.lock().unwrap();
    assert_eq!(events.len(), 1, "expected 1 event, got {events:?}");
    assert!(
        matches!(
            events[0],
            AppEvent::IndexInvalidated {
                reason: InvalidationReason::TagsChanged
            }
        ),
        "expected IndexInvalidated::TagsChanged, got {:?}",
        events[0]
    );
    drop(events);

    // No-op idempotent detach (already detached pair) MUST NOT emit.
    bus.events.lock().unwrap().clear();
    repo.detach(&hash, tag.id, dev).unwrap();
    let events_after_noop = bus.events.lock().unwrap();
    assert!(
        events_after_noop.is_empty(),
        "idempotent re-detach should NOT emit (no row written), got {events_after_noop:?}"
    );
    drop(events_after_noop);

    drop(repo);
    writer.join();
}
