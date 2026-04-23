//! Verify `WriteCmd::Search(SearchWriteCmd::Rebuild)` emits
//! `AppEvent::IndexInvalidated { reason: SearchIndexRebuilt }` on
//! the bus AFTER a successful COMMIT (Batch E Task 8).

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

use std::sync::{Arc, Mutex};

use perima_core::{AppEvent, CoreError, EventBus, InvalidationReason, SearchRepository};
use perima_db::{ReadPool, SqliteSearchRepository, SqliteWriter};

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
fn search_rebuild_emits_index_invalidated_search_index_rebuilt() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus = Arc::new(RecordingBus::default());
    let bus_for_writer: Arc<dyn EventBus> = bus.clone();
    let writer = SqliteWriter::start(&db_path, bus_for_writer).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteSearchRepository::new(writer.sender(), reads);

    // Rebuild against an empty source set is still a logical event:
    // the FTS index state changed (cleared + reseeded with zero rows).
    repo.rebuild().unwrap();

    let events = bus.events.lock().unwrap();
    assert_eq!(events.len(), 1, "expected 1 event, got {events:?}");
    assert!(
        matches!(
            events[0],
            AppEvent::IndexInvalidated {
                reason: InvalidationReason::SearchIndexRebuilt
            }
        ),
        "expected IndexInvalidated::SearchIndexRebuilt, got {:?}",
        events[0]
    );
    drop(events);

    // Second rebuild MUST emit again — every successful rebuild is
    // its own logical event regardless of source-state churn.
    bus.events.lock().unwrap().clear();
    repo.rebuild().unwrap();
    let events_after_second = bus.events.lock().unwrap();
    assert_eq!(
        events_after_second.len(),
        1,
        "second rebuild should emit again, got {events_after_second:?}"
    );
    drop(events_after_second);

    drop(repo);
    writer.join();
}
