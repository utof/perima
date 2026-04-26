//! Online backfill of `files.quick_hash` for files inserted before V011.
//!
//! Spec §4.1.5. Runs at 50 files/sec for installs >10k null-quick-hash
//! files; unlimited for fresh installs (<10k null rows). Override via
//! `PERIMA_BACKFILL_RATE` env var (files-per-second, integer; 0 = unlimited).
//!
//! # Design
//!
//! [`QuickHashBackfillWorker::spawn`] accepts a pre-built iterator of
//! [`BackfillRow`] descriptors. The caller (CLI or desktop setup) is
//! responsible for the `SELECT … WHERE quick_hash IS NULL` query —
//! keeping the worker free of repository-trait dependencies that are not
//! strictly necessary. In practice the shell calls
//! [`perima_core::FileRepository::list_files_needing_backfill`] and
//! passes the result as an iterator.
//!
//! # Rate limiting
//!
//! `BackfillRate::PerSec(n)` enforces an inter-file delay of
//! `1000 / n` milliseconds using a `tokio::time::interval`. The timer
//! fires once per file, not once per batch. [`BackfillRate::Unlimited`]
//! skips the timer entirely (no `tokio::time::sleep` overhead).
//!
//! # Event emission
//!
//! Every 100 processed rows the worker emits
//! `AppEvent::IndexInvalidated { reason: InvalidationReason::FilesChanged }`
//! so the frontend can refresh its file grid without polling.
//! Emission failures are logged at WARN and do not abort the worker.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, DiscoveredFile, EventBus, FileRepository, FileSize,
    HashService, HashedFile, InvalidationReason, MediaPath, UpsertOutcome,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// One row from the backfill SELECT — enough to compute and store `quick_hash`.
///
/// The caller constructs these (typically from
/// [`perima_core::FileRepository::list_files_needing_backfill`]) and passes
/// them as an iterator to [`QuickHashBackfillWorker::spawn`].
#[derive(Debug, Clone)]
pub struct BackfillRow {
    /// BLAKE3 full-content hash — the `files` table PK for the write path.
    pub hash: BlakeHash,
    /// File size in bytes — drives prefix-‖-suffix vs whole-file strategy.
    pub size_bytes: u64,
    /// Absolute on-disk path for the `quick_hash_prefix_suffix` read.
    pub path: PathBuf,
}

/// Rate-limit policy for the backfill worker.
///
/// Callers select the policy based on the total NULL-row count (spec §4.1.5):
/// more than 10k rows → `BackfillRate::PerSec(50)`; otherwise →
/// [`BackfillRate::Unlimited`]. `PERIMA_BACKFILL_RATE` env var always overrides.
#[derive(Debug, Clone, Copy)]
pub enum BackfillRate {
    /// Process files as fast as possible (no sleep between files).
    Unlimited,
    /// Process at most `n` files per second. `0` is treated as [`BackfillRate::Unlimited`].
    PerSec(u32),
}

impl BackfillRate {
    /// Parse the `PERIMA_BACKFILL_RATE` env var if set; return `None` if unset.
    ///
    /// `"0"` → `Unlimited`. Any integer → `PerSec(n)`.
    /// Parse failure is logged at WARN; returns `None` (caller uses default).
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("PERIMA_BACKFILL_RATE").ok()?;
        match raw.trim().parse::<u32>() {
            Ok(0) => Some(Self::Unlimited),
            Ok(n) => Some(Self::PerSec(n)),
            Err(e) => {
                warn!(
                    raw_value = %raw,
                    error = %e,
                    "PERIMA_BACKFILL_RATE is not a valid u32; using default rate"
                );
                None
            }
        }
    }
}

/// Summary report returned by the `JoinHandle` when the worker exits.
#[derive(Debug, Default)]
pub struct BackfillReport {
    /// Files whose `quick_hash` was successfully written.
    pub processed: u64,
    /// Files skipped because the hasher returned an I/O error.
    pub skipped_io_error: u64,
    /// Files skipped because no active `file_locations` path was available.
    ///
    /// These rows had `active_path = None` — the volume is unmounted or all
    /// locations have been soft-deleted. The row's `quick_hash` remains NULL
    /// and will be retried on the next startup.
    pub skipped_no_active_location: u64,
}

/// Background worker that populates `files.quick_hash` for pre-V011 rows.
///
/// Construction: call [`QuickHashBackfillWorker::spawn`]; the struct itself
/// is zero-sized — all state lives inside the spawned task.
#[derive(Debug)]
pub struct QuickHashBackfillWorker;

/// How many rows to process before emitting one `IndexInvalidated` event.
const EMIT_EVERY: u64 = 100;

impl QuickHashBackfillWorker {
    /// Spawn the backfill task on the current tokio runtime.
    ///
    /// The returned `JoinHandle<BackfillReport>` resolves when:
    /// - the iterator is drained (normal exit), or
    /// - `cancel` is triggered (early exit — returns whatever was processed).
    ///
    /// The task is `detached` by design — callers can optionally `.await` the
    /// handle for the final report but are not required to. Dropping the
    /// handle does NOT cancel the task.
    ///
    /// # Panics
    ///
    /// Must be called from within a tokio runtime context.
    pub fn spawn(
        iter: Box<dyn Iterator<Item = BackfillRow> + Send>,
        hasher: Arc<dyn HashService>,
        repo: Arc<dyn FileRepository>,
        device_id: DeviceId,
        rate: BackfillRate,
        bus: Arc<dyn EventBus>,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<BackfillReport> {
        tokio::spawn(run_backfill(
            iter, hasher, repo, device_id, rate, bus, cancel,
        ))
    }
}

// ---------------------------------------------------------------------------
// Internal task
// ---------------------------------------------------------------------------

/// Inner async fn so we can use `?` cleanly. Returned by `spawn`.
async fn run_backfill(
    iter: Box<dyn Iterator<Item = BackfillRow> + Send>,
    hasher: Arc<dyn HashService>,
    repo: Arc<dyn FileRepository>,
    device_id: DeviceId,
    rate: BackfillRate,
    bus: Arc<dyn EventBus>,
    cancel: CancellationToken,
) -> BackfillReport {
    let mut report = BackfillReport::default();

    // Build the optional interval timer.
    let mut interval = match rate {
        BackfillRate::Unlimited | BackfillRate::PerSec(0) => None,
        BackfillRate::PerSec(n) => {
            // WHY MissedTickBehavior::Skip: if processing one file takes longer
            // than 1/n seconds (e.g. slow disk), we do NOT want bursting to
            // catch up — just continue at the configured rate from "now".
            let period = Duration::from_millis(1000 / u64::from(n));
            let mut iv = tokio::time::interval(period);
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            Some(iv)
        }
    };

    for row in iter {
        // Check cancellation before each file so the task exits promptly.
        if cancel.is_cancelled() {
            break;
        }

        // Rate limiting: wait for the next tick before processing.
        if let Some(iv) = interval.as_mut() {
            // WHY select! + cancel check: tokio::time::interval::tick() is a
            // plain `Future` that doesn't check the token; wrapping in select!
            // lets us exit cleanly during the inter-file delay.
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                _ = iv.tick() => {}
            }
        }

        match process_row(&row, &*hasher, &*repo, device_id) {
            Ok(()) => {
                report.processed += 1;
            }
            Err(BackfillSkip::IoError(e)) => {
                warn!(
                    path = %row.path.display(),
                    error = %e,
                    "backfill: skipping file — I/O error computing quick_hash"
                );
                report.skipped_io_error += 1;
            }
        }

        // Emit `IndexInvalidated::FilesChanged` every EMIT_EVERY processed rows.
        if report.processed > 0 && report.processed % EMIT_EVERY == 0 {
            emit_invalidated(&*bus, report.processed);
        }
    }

    // Final emission if the last batch didn't land on the boundary.
    if report.processed % EMIT_EVERY != 0 && report.processed > 0 {
        emit_invalidated(&*bus, report.processed);
    }

    info!(
        processed = report.processed,
        skipped_io = report.skipped_io_error,
        skipped_no_loc = report.skipped_no_active_location,
        "quick_hash backfill complete"
    );

    report
}

/// Reasons a row might be skipped without being an error in the worker itself.
enum BackfillSkip {
    /// The hasher could not read the file (deleted, permission denied, etc.).
    IoError(CoreError),
}

/// Hash one file and write `quick_hash` via the repository.
///
/// Uses [`FileRepository::upsert_file_with_quick_hash`] which runs a
/// `COALESCE`-guarded UPDATE — safe to call even if another writer
/// concurrently set the value first.
fn process_row(
    row: &BackfillRow,
    hasher: &dyn HashService,
    repo: &dyn FileRepository,
    device_id: DeviceId,
) -> Result<(), BackfillSkip> {
    // Compute quick_hash by reading prefix ‖ suffix of the file.
    let quick_hash = hasher
        .quick_hash_prefix_suffix(&row.path, row.size_bytes)
        .map_err(BackfillSkip::IoError)?;

    // Build the minimal HashedFile the repo's upsert path expects.
    // WHY we use a dummy relative_path here: `upsert_file_with_quick_hash`
    // only uses `file.hash` and `file.discovered.size` for the
    // `WHERE blake3_hash = ?` + size-change detection logic; `relative_path`
    // and `absolute_path` are not persisted by the `files` UPDATE path.
    // Using `row.path.file_name()` gives a sensible relative name without
    // rebuilding the full volume-relative path (we don't have volume context
    // here — the caller only gave us the absolute path).
    let rel = row
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let hashed_file = HashedFile {
        discovered: DiscoveredFile {
            absolute_path: row.path.clone(),
            relative_path: MediaPath::new(rel),
            size: FileSize(row.size_bytes),
        },
        hash: row.hash,
    };

    // Write via COALESCE-safe path — idempotent if already populated.
    let _outcome: UpsertOutcome = repo
        .upsert_file_with_quick_hash(&hashed_file, device_id, Some(quick_hash))
        .map_err(BackfillSkip::IoError)?;

    Ok(())
}

/// Emit `AppEvent::IndexInvalidated { reason: FilesChanged }`.
///
/// Failures are logged at WARN; the worker continues regardless.
fn emit_invalidated(bus: &dyn EventBus, processed: u64) {
    let event = AppEvent::IndexInvalidated {
        reason: InvalidationReason::FilesChanged,
    };
    if let Err(e) = bus.emit(&event) {
        warn!(
            processed,
            error = %e,
            "backfill: IndexInvalidated emit failed"
        );
    }
}

// ---------------------------------------------------------------------------
// Public helpers for shell wire-up
// ---------------------------------------------------------------------------

/// Build the rate policy for CLI / desktop startup.
///
/// Decision (spec §4.1.5):
/// - `PERIMA_BACKFILL_RATE` env var → always wins.
/// - `null_count > 10_000` → `PerSec(50)` (throttled for large libraries).
/// - Otherwise → `Unlimited` (fresh installs, small libraries finish instantly).
#[must_use]
pub fn choose_backfill_rate(null_count: u64) -> BackfillRate {
    // Env override wins regardless of null_count.
    if let Some(rate) = BackfillRate::from_env() {
        return rate;
    }
    if null_count > 10_000 {
        BackfillRate::PerSec(50)
    } else {
        BackfillRate::Unlimited
    }
}

/// Maximum rows to fetch per `list_files_needing_backfill` call.
///
/// WHY 50 000: covers any reasonable library in one SELECT. Larger libraries
/// are still bounded by the rate limiter (50/s × 50k rows = 1 000 s max
/// before re-query). Keeps the heap footprint of the iterator manageable
/// (each row is ~60 bytes → ~3 MB for 50k rows).
pub const BACKFILL_QUERY_LIMIT: u32 = 50_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_rate_unlimited_for_small_count() {
        // No env override; small count → Unlimited.
        // WHY std::env is not mutated: just verify the small-count branch.
        // (Env-var branch covered by choose_backfill_rate logic above.)
        let rate = choose_backfill_rate(100);
        assert!(matches!(rate, BackfillRate::Unlimited));
    }

    #[test]
    fn choose_rate_throttled_for_large_count() {
        let rate = choose_backfill_rate(100_001);
        assert!(matches!(rate, BackfillRate::PerSec(50)));
    }

    #[test]
    fn choose_rate_boundary_exactly_10000() {
        // ≤ 10 000 → Unlimited.
        let rate = choose_backfill_rate(10_000);
        assert!(matches!(rate, BackfillRate::Unlimited));
    }
}
