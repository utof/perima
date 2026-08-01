//! `VerifyUseCase` — reconcile catalogued locations against the filesystem.
//!
//! Scanning records where files were. Nothing afterwards notices when
//! one goes away: the filesystem watcher only sees deletions that happen
//! while it is running, so a file removed with the app closed stays
//! `Active` in the catalogue forever. This use case closes that gap by
//! walking every location on a currently-mounted volume, stat-ing it,
//! and writing back the statuses that actually changed.
//!
//! It is the precondition for pruning: a prune that deletes `Missing`
//! rows is only as trustworthy as whatever marked them `Missing`.

use std::sync::Arc;

use perima_core::{
    CoreError, DeviceId, FileRepository, LocationStatus, LocationStatusUpdate, LocationToVerify,
};
use tokio_util::sync::CancellationToken;

/// Inputs to [`VerifyUseCase::execute`].
#[derive(Clone, Debug)]
pub struct VerifyCommand {
    /// Device performing the sweep. Scopes the `volume_mounts` join so
    /// paths are resolved against *this* machine's mounts.
    pub device_id: DeviceId,
    /// Report what would change without writing anything.
    pub dry_run: bool,
    /// Cancellation token. Polled between rows; a cancelled sweep writes
    /// nothing.
    pub cancel: CancellationToken,
}

/// Outcome of a verify sweep.
///
/// WHY `Serialize + specta::Type`: the desktop `verify_locations`
/// handler returns this struct directly across the Tauri IPC boundary,
/// same pattern as `ScanReport`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct VerifyReport {
    /// Locations stat-ed (i.e. on a volume mounted on this device).
    pub checked: usize,
    /// Rows that were `Active` (or otherwise present) and whose file is
    /// gone — transitioned to `Missing`.
    pub newly_missing: usize,
    /// Rows that were `Missing` and whose file is back — transitioned to
    /// `Active`.
    pub recovered: usize,
    /// Locations excluded because their volume is not mounted here.
    ///
    /// These were NOT checked and their status was NOT touched. A
    /// non-zero value means the sweep's view of the catalogue is
    /// partial — see [`VerifyReport::is_complete`].
    pub skipped_unmounted: usize,
    /// Rows actually written. `0` in a dry run.
    pub rows_written: u64,
    /// Whether the sweep ran to completion (`false` if cancelled).
    pub completed: bool,
}

impl VerifyReport {
    /// True when the sweep saw every non-deleted location in the catalogue.
    ///
    /// WHY callers should check this before acting destructively: a
    /// prune driven by an incomplete sweep deletes rows for files that
    /// are perfectly intact on a drive that simply was not plugged in.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.completed && self.skipped_unmounted == 0
    }
}

/// Reconciles `file_locations` against the filesystem.
pub struct VerifyUseCase {
    files: Arc<dyn FileRepository>,
}

impl std::fmt::Debug for VerifyUseCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifyUseCase").finish_non_exhaustive()
    }
}

/// Decide the status a location should have, given what is on disk.
///
/// Returns `None` when the recorded status already matches reality, so
/// the caller can skip the write.
///
/// WHY `symlink_metadata` rather than `metadata`: `metadata` follows
/// symlinks, so a symlink whose target has been deleted reports
/// `NotFound` and the location would be marked `Missing` even though the
/// catalogued entry is still sitting there. `symlink_metadata` asks the
/// question the catalogue actually cares about — "is there still an
/// entry at this path" — and leaves "is the target healthy" to the
/// hashing path, which reads the bytes anyway.
///
/// WHY `Moved` and `Stale` are treated as present-and-fine: neither
/// means "gone". `Stale` means the content changed and a re-scan will
/// re-hash it; `Moved` is set by the rename path. Downgrading either to
/// `Active` here would discard information this sweep did not earn — it
/// only looked at existence, not at hashes or paths. The sweep therefore
/// only ever writes the `present <-> absent` axis.
fn decide(loc: &LocationToVerify) -> Option<LocationStatus> {
    let exists = std::fs::symlink_metadata(&loc.absolute_path).is_ok();
    match (exists, loc.status) {
        // Gone, and not already recorded as gone.
        (false, LocationStatus::Missing) => None,
        (false, _) => Some(LocationStatus::Missing),
        // Back from the dead.
        (true, LocationStatus::Missing) => Some(LocationStatus::Active),
        // Present and already recorded as present in some form.
        (true, _) => None,
    }
}

impl VerifyUseCase {
    /// Construct the use case from a file repository.
    #[must_use]
    pub const fn new(files: Arc<dyn FileRepository>) -> Self {
        Self { files }
    }

    /// Walk every location on a mounted volume and reconcile its status.
    ///
    /// # Errors
    /// Returns `CoreError` if the catalogue cannot be read or the status
    /// batch cannot be written.
    pub fn execute(&self, cmd: &VerifyCommand) -> Result<VerifyReport, CoreError> {
        let candidates = self.files.list_locations_for_verify(cmd.device_id)?;

        let mut report = VerifyReport {
            skipped_unmounted: candidates.skipped_unmounted,
            ..Default::default()
        };

        let mut updates: Vec<LocationStatusUpdate> = Vec::new();
        for loc in &candidates.locations {
            if cmd.cancel.is_cancelled() {
                // WHY return the partial report rather than an error: a
                // cancelled sweep is a user action, not a failure. The
                // `completed: false` flag is what stops a caller from
                // treating the result as authoritative.
                report.completed = false;
                return Ok(report);
            }
            report.checked += 1;
            if let Some(next) = decide(loc) {
                match next {
                    LocationStatus::Missing => report.newly_missing += 1,
                    LocationStatus::Active => report.recovered += 1,
                    LocationStatus::Moved | LocationStatus::Stale => {}
                }
                updates.push(LocationStatusUpdate {
                    volume: loc.volume,
                    path: loc.path.clone(),
                    status: next,
                });
            }
        }

        report.completed = true;
        if cmd.dry_run {
            return Ok(report);
        }
        report.rows_written = self
            .files
            .update_location_statuses(&updates, cmd.device_id)?;
        Ok(report)
    }
}

/// Inputs to [`PruneUseCase::execute`].
#[derive(Clone, Debug)]
pub struct PruneCommand {
    /// Device performing the prune (recorded as the writing device).
    pub device_id: DeviceId,
    /// Count what would be removed without writing anything.
    pub dry_run: bool,
}

/// Outcome of a prune.
///
/// WHY `Serialize + specta::Type`: returned directly by the desktop
/// `prune_missing_locations` handler across the Tauri IPC boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct PruneReport {
    /// Locations recorded as `Missing` at the time of the call.
    pub missing_found: u64,
    /// Rows actually retired. `0` in a dry run.
    pub rows_pruned: u64,
}

/// Removes catalogue entries for files that verification found missing.
///
/// # Relationship to [`VerifyUseCase`]
///
/// Prune deletes on the strength of `status = Missing` and performs no
/// filesystem check of its own. That split is deliberate — deciding what
/// is missing requires stat-ing paths, which must not happen inside a
/// write transaction — but it means **a prune is only as trustworthy as
/// the sweep that preceded it**. Callers should run
/// [`VerifyUseCase::execute`] first and check
/// [`VerifyReport::is_complete`] before offering a prune, so a user is
/// never invited to delete rows on the strength of a sweep that skipped
/// an unplugged drive.
pub struct PruneUseCase {
    files: Arc<dyn FileRepository>,
}

impl std::fmt::Debug for PruneUseCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PruneUseCase").finish_non_exhaustive()
    }
}

impl PruneUseCase {
    /// Construct the use case from a file repository.
    #[must_use]
    pub const fn new(files: Arc<dyn FileRepository>) -> Self {
        Self { files }
    }

    /// Count missing locations without removing anything.
    ///
    /// Exposed on its own so a UI can label its confirm button with the
    /// real number before the user commits.
    ///
    /// # Errors
    /// Returns `CoreError` if the catalogue cannot be read.
    pub fn count_missing(&self) -> Result<u64, CoreError> {
        self.files.count_missing_locations()
    }

    /// Soft-delete every location currently marked `Missing`.
    ///
    /// # Errors
    /// Returns `CoreError` if the catalogue cannot be read or written.
    pub fn execute(&self, cmd: &PruneCommand) -> Result<PruneReport, CoreError> {
        let missing_found = self.files.count_missing_locations()?;
        if cmd.dry_run || missing_found == 0 {
            return Ok(PruneReport {
                missing_found,
                rows_pruned: 0,
            });
        }
        let rows_pruned = self.files.soft_delete_missing_locations(cmd.device_id)?;
        Ok(PruneReport {
            missing_found,
            rows_pruned,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.
mod tests {
    use std::sync::Mutex;

    use perima_core::{MediaPath, VerifyCandidates, VolumeId};

    use super::*;

    /// Repository stub that serves a fixed candidate set and records the
    /// batch it was asked to write.
    ///
    /// WHY a hand-written stub rather than a real `SqliteFileRepository`:
    /// these tests pin the sweep's *decision* logic — what it concludes
    /// from `(status on record, exists on disk)` and what it refuses to
    /// touch. The adapter's SQL is exercised separately; mixing the two
    /// would mean a query bug and a policy bug produce the same red test.
    struct StubRepo {
        candidates: Mutex<VerifyCandidates>,
        written: Mutex<Vec<LocationStatusUpdate>>,
        missing_count: Mutex<u64>,
        pruned: Mutex<u64>,
    }

    impl FileRepository for StubRepo {
        fn upsert_file(
            &self,
            _file: &perima_core::HashedFile,
            _device: DeviceId,
        ) -> Result<perima_core::UpsertOutcome, CoreError> {
            unreachable!("verify sweep does not upsert files")
        }
        fn upsert_location(
            &self,
            _hash: &perima_core::BlakeHash,
            _volume: VolumeId,
            _path: &MediaPath,
            _device: DeviceId,
        ) -> Result<perima_core::UpsertOutcome, CoreError> {
            unreachable!("verify sweep does not upsert locations")
        }
        fn list_file_locations(
            &self,
            _limit: usize,
            _volume: Option<VolumeId>,
        ) -> Result<Vec<perima_core::FileLocationRecord>, CoreError> {
            Ok(vec![])
        }
        fn list_locations_for_verify(
            &self,
            _device: DeviceId,
        ) -> Result<VerifyCandidates, CoreError> {
            Ok(self.candidates.lock().unwrap().clone())
        }
        fn update_location_statuses(
            &self,
            updates: &[LocationStatusUpdate],
            _device: DeviceId,
        ) -> Result<u64, CoreError> {
            self.written.lock().unwrap().extend_from_slice(updates);
            Ok(u64::try_from(updates.len()).unwrap())
        }
        fn count_missing_locations(&self) -> Result<u64, CoreError> {
            Ok(*self.missing_count.lock().unwrap())
        }
        fn soft_delete_missing_locations(&self, _device: DeviceId) -> Result<u64, CoreError> {
            let n = *self.missing_count.lock().unwrap();
            *self.pruned.lock().unwrap() = n;
            *self.missing_count.lock().unwrap() = 0;
            Ok(n)
        }
    }

    fn loc(path: &std::path::Path, status: LocationStatus) -> LocationToVerify {
        LocationToVerify {
            volume: VolumeId(uuid::Uuid::nil()),
            path: MediaPath::new("rel/path"),
            absolute_path: path.to_path_buf(),
            status,
        }
    }

    fn harness(locations: Vec<LocationToVerify>, skipped: usize) -> Arc<StubRepo> {
        Arc::new(StubRepo {
            candidates: Mutex::new(VerifyCandidates {
                locations,
                skipped_unmounted: skipped,
            }),
            written: Mutex::new(Vec::new()),
            missing_count: Mutex::new(0),
            pruned: Mutex::new(0),
        })
    }

    fn cmd(dry_run: bool) -> VerifyCommand {
        VerifyCommand {
            device_id: DeviceId::new(),
            dry_run,
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn marks_absent_active_location_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone.mp4");
        let repo = harness(vec![loc(&gone, LocationStatus::Active)], 0);
        let uc = VerifyUseCase::new(repo.clone());

        let report = uc.execute(&cmd(false)).unwrap();

        assert_eq!(report.checked, 1);
        assert_eq!(report.newly_missing, 1);
        assert_eq!(report.recovered, 0);
        assert_eq!(report.rows_written, 1);
        // WHY clone out of the guard: holding a MutexGuard across the
        // asserts trips `significant_drop_tightening`. Cloning releases
        // the lock at the end of this statement.
        let written = repo.written.lock().unwrap().clone();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].status, LocationStatus::Missing);
    }

    #[test]
    fn recovers_missing_location_that_came_back() {
        let tmp = tempfile::tempdir().unwrap();
        let present = tmp.path().join("back.mp4");
        std::fs::write(&present, b"x").unwrap();
        let repo = harness(vec![loc(&present, LocationStatus::Missing)], 0);
        let uc = VerifyUseCase::new(repo.clone());

        let report = uc.execute(&cmd(false)).unwrap();

        assert_eq!(report.recovered, 1, "a file that returns must go Active");
        assert_eq!(report.newly_missing, 0);
        assert_eq!(
            repo.written.lock().unwrap()[0].status,
            LocationStatus::Active,
        );
    }

    #[test]
    fn writes_nothing_when_nothing_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let present = tmp.path().join("here.mp4");
        std::fs::write(&present, b"x").unwrap();
        let repo = harness(vec![loc(&present, LocationStatus::Active)], 0);
        let uc = VerifyUseCase::new(repo.clone());

        let report = uc.execute(&cmd(false)).unwrap();

        assert_eq!(report.checked, 1);
        assert_eq!(report.rows_written, 0, "steady state must not write");
        assert!(repo.written.lock().unwrap().is_empty());
    }

    /// The sweep must not silently rewrite statuses it did not evaluate.
    #[test]
    fn present_moved_and_stale_rows_are_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.mp4");
        let b = tmp.path().join("b.mp4");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();
        let repo = harness(
            vec![
                loc(&a, LocationStatus::Moved),
                loc(&b, LocationStatus::Stale),
            ],
            0,
        );
        let uc = VerifyUseCase::new(repo.clone());

        let report = uc.execute(&cmd(false)).unwrap();

        assert_eq!(report.checked, 2);
        assert_eq!(
            report.rows_written, 0,
            "existence-only sweep must not downgrade Moved/Stale to Active",
        );
        assert!(
            repo.written.lock().unwrap().is_empty(),
            "no status transition may be proposed for present Moved/Stale rows",
        );
    }

    #[test]
    fn dry_run_reports_but_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone.mp4");
        let repo = harness(vec![loc(&gone, LocationStatus::Active)], 0);
        let uc = VerifyUseCase::new(repo.clone());

        let report = uc.execute(&cmd(true)).unwrap();

        assert_eq!(report.newly_missing, 1, "dry run still reports the finding");
        assert_eq!(report.rows_written, 0);
        assert!(repo.written.lock().unwrap().is_empty());
    }

    /// The load-bearing safety property: an unmounted volume must never
    /// produce a `Missing` transition, and the report must admit that its
    /// view was partial.
    #[test]
    fn unmounted_volumes_are_reported_not_marked_missing() {
        let repo = harness(vec![], 417);
        let uc = VerifyUseCase::new(repo.clone());

        let report = uc.execute(&cmd(false)).unwrap();

        assert_eq!(report.checked, 0);
        assert_eq!(
            report.newly_missing, 0,
            "rows on an unmounted volume must never be marked Missing",
        );
        assert_eq!(report.skipped_unmounted, 417);
        assert!(repo.written.lock().unwrap().is_empty());
        assert!(
            !report.is_complete(),
            "a sweep that skipped rows must not report itself complete",
        );
    }

    #[test]
    fn cancellation_writes_nothing_and_reports_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone.mp4");
        let repo = harness(vec![loc(&gone, LocationStatus::Active)], 0);
        let uc = VerifyUseCase::new(repo.clone());
        let c = cmd(false);
        c.cancel.cancel();

        let report = uc.execute(&c).unwrap();

        assert!(!report.completed);
        assert!(!report.is_complete());
        assert!(
            repo.written.lock().unwrap().is_empty(),
            "a cancelled sweep must not write a partial reconciliation",
        );
    }

    #[test]
    fn complete_sweep_over_mounted_volumes_reports_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let present = tmp.path().join("here.mp4");
        std::fs::write(&present, b"x").unwrap();
        let repo = harness(vec![loc(&present, LocationStatus::Active)], 0);
        let uc = VerifyUseCase::new(repo);

        let report = uc.execute(&cmd(false)).unwrap();

        assert!(report.is_complete());
    }

    // -----------------------------------------------------------------
    // Prune
    // -----------------------------------------------------------------

    fn prune_harness(missing: u64) -> Arc<StubRepo> {
        let repo = harness(vec![], 0);
        *repo.missing_count.lock().unwrap() = missing;
        repo
    }

    #[test]
    fn prune_removes_missing_rows_and_reports_the_count() {
        let repo = prune_harness(5);
        let uc = PruneUseCase::new(repo.clone());

        let report = uc
            .execute(&PruneCommand {
                device_id: DeviceId::new(),
                dry_run: false,
            })
            .unwrap();

        assert_eq!(report.missing_found, 5);
        assert_eq!(report.rows_pruned, 5);
        assert_eq!(*repo.pruned.lock().unwrap(), 5);
    }

    #[test]
    fn prune_dry_run_counts_without_deleting() {
        let repo = prune_harness(5);
        let uc = PruneUseCase::new(repo.clone());

        let report = uc
            .execute(&PruneCommand {
                device_id: DeviceId::new(),
                dry_run: true,
            })
            .unwrap();

        assert_eq!(report.missing_found, 5, "dry run still reports the count");
        assert_eq!(report.rows_pruned, 0);
        assert_eq!(
            *repo.pruned.lock().unwrap(),
            0,
            "dry run must not reach the delete",
        );
    }

    /// A prune with nothing to do must not reach the writer at all —
    /// an empty destructive write is still a write lock and an
    /// invalidation event the frontend would act on.
    #[test]
    fn prune_with_nothing_missing_is_a_no_op() {
        let repo = prune_harness(0);
        let uc = PruneUseCase::new(repo.clone());

        let report = uc
            .execute(&PruneCommand {
                device_id: DeviceId::new(),
                dry_run: false,
            })
            .unwrap();

        assert_eq!(report.missing_found, 0);
        assert_eq!(report.rows_pruned, 0);
        assert_eq!(*repo.pruned.lock().unwrap(), 0);
    }

    #[test]
    fn count_missing_does_not_mutate() {
        let repo = prune_harness(3);
        let uc = PruneUseCase::new(repo.clone());

        assert_eq!(uc.count_missing().unwrap(), 3);
        assert_eq!(uc.count_missing().unwrap(), 3, "counting is idempotent");
        assert_eq!(*repo.pruned.lock().unwrap(), 0);
    }
}
