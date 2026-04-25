//! Filesystem event types and the `EventBus` trait.

use serde::Serialize;

use crate::{
    CoreError, MediaPath, VolumeId,
    dedup::{BatchId, FullHashOutcome},
};

/// A filesystem event detected by the watcher.
///
/// WHY `tag = "type"` (inline, no content key): matches the pre-Batch-D
/// `FileEventPayload` mirror in `crates/desktop/src/events.rs`, keeping the
/// frontend's `'file-event'` channel listener byte-compatible. `CoreError` uses
/// `tag = "kind", content = "data"` — a different shape intentionally, because
/// `CoreError` is a Result error type while `FileEvent` is a v1-frozen channel payload.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "type")]
pub enum FileEvent {
    /// A new file appeared at this path.
    Created {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
    /// An existing file's content was modified.
    Modified {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
    /// A file was deleted from this path.
    Deleted {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
    /// A file was renamed/moved within the same volume.
    Renamed {
        /// Previous relative path.
        from: MediaPath,
        /// New relative path.
        to: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
}

/// Application-level event broadcast through the bus.
///
/// Wraps the existing [`FileEvent`] for filesystem-watcher events and
/// adds two domain events emitted by the writer / use-case layer.
///
/// Wire shape: `#[serde(tag = "kind", content = "data")]` produces a
/// TypeScript discriminated union the frontend pattern-matches on.
/// Inner `FileEvent` keeps its own `#[serde(tag = "type")]` inline
/// shape (Batch D D-4 invariant) — wraps cleanly inside `AppEvent::File`.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", content = "data")]
pub enum AppEvent {
    /// Filesystem-watcher event (Created/Modified/Deleted/Renamed).
    File(FileEvent),

    /// Emitted by `ScanUseCase::execute` after a successful scan.
    /// Frontend triggers an immediate refetch (no debounce).
    ScanCompleted {
        /// Volume the scan ran against.
        volume: VolumeId,
        /// Total files seen by the walker (incl. existing).
        files_seen: u64,
        /// New files inserted since last scan.
        files_new: u64,
        /// Wall-clock duration of the scan.
        duration_ms: u64,
    },

    /// Emitted by the writer actor after a successful `WriteCmd` that
    /// changes a query-relevant index.
    IndexInvalidated {
        /// Which index category was invalidated.
        reason: InvalidationReason,
    },

    /// Emitted by the full-hash worker after each file completes.
    /// Frontend uses `batch_id` to correlate with the `BatchHandle`
    /// returned by `compute_full_hash_batch`.
    VerifyProgress {
        /// The batch this event belongs to.
        batch_id: BatchId,
        /// Number of files completed so far (including this one).
        files_done: u32,
        /// Total files in the batch.
        files_total: u32,
        /// Outcome for the file just processed.
        latest_outcome: FullHashOutcome,
    },

    /// Emitted by the full-hash worker when all files in a batch have
    /// been processed (successfully or not).
    VerifyComplete {
        /// The batch that has finished.
        batch_id: BatchId,
    },
}

/// Categorical reason an index was invalidated.
///
/// Kept coarse in v1 — Batch H may split into per-row variants once
/// `TanStack` Query lands and profiling shows surgical invalidation pays.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum InvalidationReason {
    /// Tag attach/detach.
    TagsChanged,
    /// File location upsert / status change.
    FilesChanged,
    /// Media metadata extraction or attach.
    MetadataChanged,
    /// FTS5 rebuild.
    SearchIndexRebuilt,
    /// Collision groups changed (quick-hash or full-hash dedup result updated).
    CollisionsChanged,
}

/// Consumer of application events.
///
/// Multiple implementations can be composed via a fan-out adapter
/// (e.g., `CompositeEventBus`). The composite logs errors from
/// individual handlers but does not abort — remaining handlers
/// still fire.
pub trait EventBus: Send + Sync {
    /// Publish an event onto the bus. Implementations are expected to
    /// be cheap (clone + push to channel); slow work happens in the
    /// async `EventHandler` tasks spawned by `AppContainer::new`.
    ///
    /// # Errors
    /// Returns `CoreError` if the publish fails. The production `Bus`
    /// (in `crates/app::bus`) returns `Ok(())` on capacity-Full (logs
    /// a warning instead) and on Closed (shutdown path).
    fn emit(&self, event: &AppEvent) -> Result<(), CoreError>;
}
