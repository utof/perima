//! Verify `WriteCmd::Metadata(MetadataWriteCmd::UpsertMetadata)` emits
//! `AppEvent::IndexInvalidated { reason: MetadataChanged }` on the bus
//! AFTER a successful COMMIT (Batch E Task 8).
//!
//! Spec uses "`MetadataAttach`" as a descriptive name; the actual
//! `MetadataWriteCmd` variant is `UpsertMetadata` — the writer's
//! INSERT branch is the metadata-attach logical event.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::sync::{Arc, Mutex};

use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, EventBus, InvalidationReason, MediaMetadata,
    MetadataRepository,
};
use perima_db::{ReadPool, SqliteMetadataRepository, SqliteWriter};

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

fn sample_metadata() -> MediaMetadata {
    let hash = BlakeHash::parse_hex(&"b".repeat(64)).expect("hash");
    MediaMetadata {
        hash,
        width: Some(3840),
        height: Some(2160),
        duration_ms: None,
        captured_at: Some("2025-01-01T00:00:00Z".into()),
        camera_make: None,
        camera_model: None,
        codec: None,
        bitrate_bps: None,
        mime_type: Some("image/jpeg".into()),
        thumbnail_path: None,
        thumbnail_status: None,
    }
}

#[test]
fn metadata_upsert_emits_index_invalidated_metadata_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus = Arc::new(RecordingBus::default());
    let bus_for_writer: Arc<dyn EventBus> = bus.clone();
    let writer = SqliteWriter::start(&db_path, bus_for_writer).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteMetadataRepository::new(writer.sender(), reads);

    let dev = DeviceId::new();
    let meta = sample_metadata();

    // INSERT path → MetadataChanged emit.
    repo.upsert_metadata(&meta, dev).unwrap();

    let events = bus.events.lock().unwrap();
    assert_eq!(events.len(), 1, "expected 1 event, got {events:?}");
    assert!(
        matches!(
            events[0],
            AppEvent::IndexInvalidated {
                reason: InvalidationReason::MetadataChanged
            }
        ),
        "expected IndexInvalidated::MetadataChanged, got {:?}",
        events[0]
    );
    drop(events);

    // Unchanged arm (re-upsert with identical inputs) MUST NOT emit.
    bus.events.lock().unwrap().clear();
    repo.upsert_metadata(&meta, dev).unwrap();
    let events_after_unchanged = bus.events.lock().unwrap();
    assert!(
        events_after_unchanged.is_empty(),
        "Unchanged metadata upsert should NOT emit, got {events_after_unchanged:?}"
    );
    drop(events_after_unchanged);

    // Updated arm (mime_type flip) MUST emit again.
    bus.events.lock().unwrap().clear();
    let mut meta2 = meta;
    meta2.mime_type = Some("image/png".into());
    repo.upsert_metadata(&meta2, dev).unwrap();
    let events_after_update = bus.events.lock().unwrap();
    assert_eq!(
        events_after_update.len(),
        1,
        "Updated metadata upsert should emit exactly 1 event, got {events_after_update:?}"
    );
    drop(events_after_update);

    drop(repo);
    writer.join();
}
