//! `ComputeFullHashUseCase` + `DedupUseCase` — on-demand full-hash compute
//! and quick-hash collision dedup orchestration (spec §4.6, §4.7).
//!
//! # Why two use cases in one module
//!
//! Both are part of the v0.6.x dedup story: `ComputeFullHashUseCase` is the
//! mutator (reads bytes, writes `full_hash`); `DedupUseCase` is the reader
//! (lists candidate groups, marks false positives). Sharing a module keeps
//! the imports + WHY-blocks co-located while leaving each `UseCase` struct
//! independently constructable from the container.
//!
//! # Batch handle semantics
//!
//! [`ComputeFullHashUseCase::execute_batch`] returns a [`BatchHandle`]
//! immediately and spawns a tokio task that processes files sequentially
//! (avoids parallel disk thrash on HDDs — spec §4.5). Cancellation is via
//! a `CancellationToken` stored in a `Mutex<HashMap<BatchId, _>>` keyed on
//! the `BatchId` returned to the caller. Per-file `AppEvent::VerifyProgress`
//! emission is wired in Task 10 — Task 9 only emits
//! `AppEvent::VerifyComplete` at the very end of the batch.
//!
//! # `DeviceKind` defaulting
//!
//! `Blake3Service::full_hash_dispatched` (Task 5) takes a `DeviceKind` to
//! pick between mmap / rayon / sequential reads. We don't yet cache the
//! per-volume device kind — Task 14 (`/dedup` route + sidebar Compute) will
//! introduce that lookup. For now we default to `DeviceKind::Unknown` (which
//! the dispatch matrix maps to the SSD path per spec §4.5.3).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use perima_core::{
    AppEvent, BatchHandle, BatchId, BlakeHash, CollisionGroup, CoreError, DeviceId, DeviceKind,
    EventBus, FileRepository, FileUuid, FullHashOutcome, FullHashUnavailableReason, HashService,
};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// ComputeFullHashUseCase
// ---------------------------------------------------------------------------

/// Orchestrator: on-demand compute + promote of `full_hash` for individual
/// files or batches.
///
/// Dependencies are carried as `Arc<dyn Port>` fields; zero generic parameters
/// on the struct.
pub struct ComputeFullHashUseCase {
    hasher: Arc<dyn HashService>,
    files: Arc<dyn FileRepository>,
    events: Arc<dyn EventBus>,
    /// Per-batch cancellation tokens keyed on `BatchId`. Held for the lifetime
    /// of the spawned task so [`Self::cancel_batch`] can find the right token.
    ///
    /// WHY `std::sync::Mutex` (not `tokio::sync::Mutex`): the lock is held only
    /// across a `HashMap::insert` / `remove`, never across an `.await`. Sync
    /// mutex is the right call for fast, sync-only critical sections per the
    /// `clippy::await_holding_lock` lint guidance.
    batches: Arc<Mutex<HashMap<BatchId, CancellationToken>>>,
}

impl std::fmt::Debug for ComputeFullHashUseCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComputeFullHashUseCase")
            .finish_non_exhaustive()
    }
}

impl ComputeFullHashUseCase {
    /// Construct a `ComputeFullHashUseCase` with the given dependency ports.
    ///
    /// The container ([`crate::AppContainer::new`]) calls this once and shares
    /// the resulting `Arc<ComputeFullHashUseCase>` across surfaces.
    #[must_use]
    pub fn new(
        hasher: Arc<dyn HashService>,
        files: Arc<dyn FileRepository>,
        events: Arc<dyn EventBus>,
    ) -> Self {
        Self {
            hasher,
            files,
            events,
            batches: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Compute and persist the full BLAKE3 hash for a single `file_uuid`.
    ///
    /// Looks up the active mounted path, reads the bytes via
    /// [`HashService::full_hash_dispatched`], promotes the hash via
    /// [`FileRepository::update_full_hash`], and returns the freshly-computed
    /// value.
    ///
    /// # Errors
    /// - [`CoreError::FullHashUnavailable`] with reason
    ///   [`FullHashUnavailableReason::NotMounted`] when no active mounted
    ///   location exists for `file_uuid`.
    /// - [`CoreError::FullHashUnavailable`] with reason
    ///   [`FullHashUnavailableReason::IoError`] when reading the bytes fails.
    /// - Propagates other `CoreError` variants from the underlying ports.
    #[tracing::instrument(
        name = "compute_full_hash_single",
        skip(self),
        err(level = "warn", Display)
    )]
    pub async fn execute_single(&self, file_uuid: FileUuid) -> Result<BlakeHash, CoreError> {
        compute_one(&*self.hasher, &*self.files, file_uuid).await
    }

    /// Spawn a batch task that computes `full_hash` for every uuid in
    /// `file_uuids`, returning a [`BatchHandle`] immediately.
    ///
    /// The batch is processed sequentially (one file at a time) to avoid
    /// parallel disk thrash on HDDs — spec §4.5. After each file completes
    /// (successfully or not) the worker emits [`AppEvent::VerifyProgress`]
    /// with the per-file outcome so the frontend can drive a progress bar.
    /// One [`AppEvent::VerifyComplete`] is emitted at the very end.
    ///
    /// # Errors
    /// Currently infallible — all per-file failures are routed through
    /// `AppEvent::VerifyProgress` rather than aborting the batch.
    #[allow(clippy::unused_async)]
    pub async fn execute_batch(&self, file_uuids: Vec<FileUuid>) -> Result<BatchHandle, CoreError> {
        let batch_id = BatchId::new();
        let files_total = u32::try_from(file_uuids.len()).unwrap_or(u32::MAX);

        let cancel = CancellationToken::new();
        // Insert into the map BEFORE spawning so a racy `cancel_batch` call
        // is observable even before the task starts polling.
        self.batches
            .lock()
            .map_err(|e| CoreError::Internal(format!("batches lock poisoned: {e}")))?
            .insert(batch_id, cancel.clone());

        let hasher = Arc::clone(&self.hasher);
        let files = Arc::clone(&self.files);
        let events = Arc::clone(&self.events);
        let batches = Arc::clone(&self.batches);

        tokio::spawn(async move {
            let mut files_done: u32 = 0;

            for uuid in file_uuids {
                if cancel.is_cancelled() {
                    break;
                }

                let result = compute_one(&*hasher, &*files, uuid).await;

                // Build the per-file outcome regardless of success/failure so
                // the frontend can distinguish `Computed` from `Failed` and
                // update its progress bar with the right icon per file.
                let outcome = match result {
                    Ok(hash) => FullHashOutcome::Computed {
                        file_uuid: uuid,
                        hash,
                    },
                    Err(ref e) => {
                        tracing::warn!(
                            file_uuid = %uuid.0,
                            error = %e,
                            "batch full_hash compute failed",
                        );
                        FullHashOutcome::Failed {
                            file_uuid: uuid,
                            error: e.clone(),
                        }
                    }
                };

                files_done = files_done.saturating_add(1);

                // WHY best-effort emit (ignore error): a slow/overflowed bus
                // must not abort the batch. The bus logs its own warning on
                // capacity-full. The `let _ =` silences the `must_use` lint.
                let _ = events.emit(&AppEvent::VerifyProgress {
                    batch_id,
                    files_done,
                    files_total,
                    latest_outcome: outcome,
                });
            }

            // Final `VerifyComplete` event so the frontend knows the batch is done.
            if let Err(e) = events.emit(&AppEvent::VerifyComplete { batch_id }) {
                tracing::warn!(?e, batch = %batch_id.0, "VerifyComplete emit failed");
            }

            // Clean up the cancellation token entry so the map doesn't leak
            // across long-running processes.
            if let Ok(mut map) = batches.lock() {
                map.remove(&batch_id);
            }
        });

        Ok(BatchHandle {
            batch_id,
            total: files_total,
        })
    }

    /// Cancel an in-flight batch by id.
    ///
    /// # Errors
    /// Returns `CoreError::NotFound` if no batch with `batch_id` is currently
    /// running (already finished, never started, or already cancelled).
    #[allow(clippy::unused_async)]
    pub async fn cancel_batch(&self, batch_id: BatchId) -> Result<(), CoreError> {
        let token = self
            .batches
            .lock()
            .map_err(|e| CoreError::Internal(format!("batches lock poisoned: {e}")))?
            .remove(&batch_id);
        token.map_or_else(
            || {
                Err(CoreError::NotFound(format!(
                    "no in-flight batch with id={}",
                    batch_id.0
                )))
            },
            |t| {
                t.cancel();
                Ok(())
            },
        )
    }
}

/// Shared per-file compute path used by both `execute_single` and
/// `execute_batch`. Fully sync-async-bridged so both entry points share
/// the same I/O + lookup + promote sequence.
///
/// WHY async fn (with no `.await` today): keeps the signature uniform with
/// the public `execute_single` / `execute_batch` `UseCase` contract so a
/// future `spawn_blocking` rewrite (if BLAKE3 hashing on the worker thread
/// becomes a bottleneck) is a single-site change without churning every
/// caller. Mirrors the `clippy::unused_async` pattern used in
/// `volume::execute` and the other `UseCase` methods.
#[allow(clippy::unused_async)]
async fn compute_one(
    hasher: &dyn HashService,
    files: &dyn FileRepository,
    file_uuid: FileUuid,
) -> Result<BlakeHash, CoreError> {
    let (_existing_hash, abs_path, size_bytes) =
        files
            .lookup_by_file_uuid(file_uuid)?
            .ok_or_else(|| CoreError::FullHashUnavailable {
                // WHY NotMounted when path is unavailable: the row exists (or
                // doesn't), but the user-actionable message is "mount the
                // volume" — the frontend distinguishes by the `kind` discriminant
                // in `FullHashUnavailableReason`.
                reason: FullHashUnavailableReason::NotMounted {
                    volume_id: file_uuid.0.to_string(),
                },
            })?;

    // WHY DeviceKind::Unknown default: per-volume device kind caching is
    // tracked in Task 14 (sidebar Compute UX). The dispatch matrix
    // (spec §4.5.3) maps Unknown → SSD path, which is the safer default
    // for 2026 hardware.
    let device_kind = DeviceKind::Unknown;

    // WHY synchronous hash here (no spawn_blocking): the call sites are
    // Tauri commands running on tokio worker threads. BLAKE3 over a single
    // file completes in milliseconds for typical media; spawn_blocking
    // would add scheduler hops without measurable benefit. The scan-path
    // already accepts the same trade-off via rayon's `par_iter`.
    let new_hash = hasher
        .full_hash_dispatched(&abs_path, size_bytes, device_kind)
        .map_err(|e| CoreError::FullHashUnavailable {
            reason: FullHashUnavailableReason::IoError {
                message: e.to_string(),
            },
        })?;

    files.update_full_hash(file_uuid, new_hash)?;

    Ok(new_hash)
}

// ---------------------------------------------------------------------------
// DedupUseCase
// ---------------------------------------------------------------------------

/// Orchestrator: dedup queries (list candidate groups) and dedup mutations
/// (mark verified-distinct).
///
/// Dependencies are carried as `Arc<dyn Port>` fields; zero generic
/// parameters on the struct.
pub struct DedupUseCase {
    files: Arc<dyn FileRepository>,
    events: Arc<dyn EventBus>,
}

impl std::fmt::Debug for DedupUseCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DedupUseCase").finish_non_exhaustive()
    }
}

impl DedupUseCase {
    /// Construct a `DedupUseCase` with the given dependency ports.
    #[must_use]
    pub const fn new(files: Arc<dyn FileRepository>, events: Arc<dyn EventBus>) -> Self {
        Self { files, events }
    }

    /// List every group of files whose `quick_hash` matches one or more
    /// other rows AND that have not been marked `verified_distinct`.
    ///
    /// # Errors
    /// Propagates `CoreError` from the underlying [`FileRepository`].
    pub fn list_collisions(&self) -> Result<Vec<CollisionGroup>, CoreError> {
        // WHY events held but unused here: list is read-only — no event
        // emission. The bus is held for symmetry with the mutating method
        // and to enable per-call instrumentation in a future expansion.
        let _ = &self.events;
        self.files.list_quick_hash_collisions()
    }

    /// Mark the given file uuids as `verified_distinct = 1`.
    ///
    /// Memorises that these files share a `quick_hash` but were verified
    /// to have distinct `full_hash` values — they will be excluded from
    /// subsequent `list_collisions` calls.
    ///
    /// # Errors
    /// Propagates `CoreError` from the underlying [`FileRepository`].
    pub fn mark_verified_distinct(
        &self,
        file_uuids: Vec<FileUuid>,
        device: DeviceId,
    ) -> Result<(), CoreError> {
        self.files.mark_verified_distinct(file_uuids, device)?;
        // The writer emits `IndexInvalidated::CollisionsChanged` after COMMIT,
        // so we don't double-fire here.
        Ok(())
    }
}
