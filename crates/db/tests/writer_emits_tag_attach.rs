//! Verify `WriteCmd::Tag(TagWriteCmd::Attach)` emits
//! `AppEvent::IndexInvalidated { reason: TagsChanged }` on the bus
//! AFTER a successful COMMIT (Batch E Task 8).

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::sync::{Arc, Mutex};

use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, EventBus, InvalidationReason, TagRepository,
};
use perima_db::{ReadPool, SqliteTagRepository, SqliteWriter};

/// Recording bus that captures every emit for assertion.
///
/// WHY inlined per test file (vs a shared helper): consolidation
/// isn't worth a dedicated `test_utils` module for 6 files — each
/// test owns its bus + its assertion shape, and the impl is 4 lines.
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
fn tag_attach_emits_index_invalidated_tags_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus = Arc::new(RecordingBus::default());
    let bus_for_writer: Arc<dyn EventBus> = bus.clone();
    let writer = SqliteWriter::start(&db_path, bus_for_writer).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteTagRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();

    // Seed: create the tag (UpsertTag does NOT emit by design — only
    // Attach/Detach are wired in Batch E Task 8).
    let tag = repo.upsert_tag("photo", dev).unwrap();

    // Hash to attach against — files row need not exist for the
    // attach to land (file_tags has no FK cascade per CLAUDE.md
    // schema rules).
    let hash = BlakeHash::from_bytes(*blake3::hash(b"attach_emit_test").as_bytes());

    // Snapshot: any seed-time emits drained.
    bus.events.lock().unwrap().clear();

    repo.attach(&hash, tag.id, dev).unwrap();

    // The writer thread emits BEFORE replying (Approach B), and the
    // adapter blocks on reply, so by the time `attach` returns the
    // emit has already happened — no sleep needed.
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

    // No-op idempotent attach (same pair) MUST NOT emit a second event.
    bus.events.lock().unwrap().clear();
    repo.attach(&hash, tag.id, dev).unwrap();
    let events_after_noop = bus.events.lock().unwrap();
    assert!(
        events_after_noop.is_empty(),
        "idempotent re-attach should NOT emit (no row written), got {events_after_noop:?}"
    );
    drop(events_after_noop);

    drop(repo);
    writer.join();
}
