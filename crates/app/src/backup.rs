//! Online single-file `SQLite` backup orchestration.
//!
//! [`BackupDatabaseUseCase`] resolves the target path, enforces
//! single-call concurrency via an `AtomicBool` + RAII guard,
//! pre-removes when `--force`, and dispatches the actual copy via
//! [`perima_core::ports::DatabaseAdmin`].
//!
//! # Why a use case (not direct adapter call from shells)
//!
//! Path resolution + force semantics + concurrency-gating are
//! application-layer concerns; the writer-actor adapter only knows
//! "VACUUM INTO this path". Keeping the policy here lets CLI + Tauri
//! IPC + future scheduler share one implementation.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use perima_core::{CoreError, errors::BackupFailureReason, ports::DatabaseAdmin};
use serde::Serialize;

/// Successful output of [`BackupDatabaseUseCase::execute`].
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct BackupOutput {
    /// Absolute path to the freshly written backup file.
    pub absolute_path: PathBuf,
    /// Size in bytes of the freshly written backup file.
    pub size_bytes: u64,
}

/// Input to [`BackupDatabaseUseCase::execute`].
///
/// WHY no specta derive: this type is internal to the use-case; the IPC
/// handler reconstructs from `(target, force)` IPC args, so we don't
/// need to expose `BackupCommand` to the TS bindings.
#[derive(Debug, Clone)]
pub struct BackupCommand {
    /// Explicit destination path, if user passed `--to <path>`. `None`
    /// triggers default-path resolution under `<data_dir>/backups/`.
    pub target: Option<PathBuf>,
    /// If `true`, an existing target file is removed before the backup
    /// runs. If `false`, an existing target returns
    /// [`BackupFailureReason::TargetExists`].
    pub force: bool,
}

/// Backup orchestration: resolves target, enforces in-flight guard,
/// pre-removes when `--force`, and dispatches via [`DatabaseAdmin`].
pub struct BackupDatabaseUseCase {
    admin: Arc<dyn DatabaseAdmin>,
    data_dir: PathBuf,
    in_flight: Arc<AtomicBool>,
}

// WHY manual Debug (vs. derive): `Arc<dyn DatabaseAdmin>` is not Debug —
// the trait does not require it. Matches the pattern used for every
// other UseCase struct in this crate (see ScanUseCase, MetadataUseCase).
impl std::fmt::Debug for BackupDatabaseUseCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupDatabaseUseCase")
            .finish_non_exhaustive()
    }
}

impl BackupDatabaseUseCase {
    /// Construct a use case from the database-admin adapter and the
    /// resolved on-disk data directory (used to compute the default
    /// target path under `<data_dir>/backups/`).
    #[must_use]
    pub fn new(admin: Arc<dyn DatabaseAdmin>, data_dir: PathBuf) -> Self {
        Self {
            admin,
            data_dir,
            in_flight: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Run a single backup.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::BackupFailed`] with one of:
    /// - [`BackupFailureReason::AlreadyInProgress`] — another backup is
    ///   running on this instance.
    /// - [`BackupFailureReason::TargetExists`] — target already exists
    ///   and `force` was not passed.
    /// - [`BackupFailureReason::TargetUnwritable`] — parent directory
    ///   could not be created or pre-existing file could not be removed.
    /// - Anything propagated from the underlying [`DatabaseAdmin::backup`].
    // WHY allow(unused_async): the body holds no `.await`. The `async`
    // keyword on `execute` is the canonical UseCase shape (matches
    // ScanUseCase, MetadataUseCase, …); preserving it leaves the door
    // open for future awaits without churn at every callsite.
    #[allow(clippy::unused_async)]
    pub async fn execute(&self, cmd: BackupCommand) -> Result<BackupOutput, CoreError> {
        // Acquire in-flight slot; release in `BackupGuard::drop`.
        let _guard = BackupGuard::acquire(&self.in_flight)?;

        let now = chrono::Utc::now();
        let target = resolve_target(cmd.target.as_deref(), &self.data_dir, now);

        // Create parent directory if missing.
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::BackupFailed {
                reason: BackupFailureReason::TargetUnwritable {
                    path: target.display().to_string(),
                    message: format!("create_dir_all parent: {e}"),
                },
            })?;
        }

        // Existence check + force semantics.
        if target.exists() {
            if cmd.force {
                std::fs::remove_file(&target).map_err(|e| CoreError::BackupFailed {
                    reason: BackupFailureReason::TargetUnwritable {
                        path: target.display().to_string(),
                        message: format!("remove pre-existing: {e}"),
                    },
                })?;
            } else {
                return Err(CoreError::BackupFailed {
                    reason: BackupFailureReason::TargetExists {
                        path: target.display().to_string(),
                    },
                });
            }
        }

        let size_bytes = self.admin.backup(&target)?;

        Ok(BackupOutput {
            absolute_path: target,
            size_bytes,
        })
    }
}

/// RAII guard for the in-flight slot.
///
/// WHY a guard (not bare `compare_exchange` + manual release): a panic
/// inside `execute` would otherwise strand the flag set, locking out
/// all future backups. `Drop` is panic-safe.
///
/// WHY this also handles `tokio::spawn` cancellation: `BackupGuard` is
/// a stack value inside `execute()`. If the future is dropped (cancelled)
/// between `acquire()` and natural completion, Rust's drop ordering runs
/// `BackupGuard::drop` as the future's frame is unwound — the slot is
/// released. A future "optimisation" that hoists the guard out of the
/// stack frame would break this property; do NOT do that.
struct BackupGuard<'a> {
    flag: &'a AtomicBool,
}

impl<'a> BackupGuard<'a> {
    fn acquire(flag: &'a AtomicBool) -> Result<Self, CoreError> {
        flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| CoreError::BackupFailed {
                reason: BackupFailureReason::AlreadyInProgress,
            })?;
        Ok(Self { flag })
    }
}

impl Drop for BackupGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// Resolve the backup target path.
///
/// `Some(p)` → `p` verbatim. `None` →
/// `<data_dir>/backups/perima-<UTC ISO 8601 with hyphens>.sqlite`.
///
/// WHY UTC + hyphens (not local time + colons): backups sort
/// lexicographically across machines and DST transitions; Windows +
/// classic-macOS reject `:` in filenames. The `Z` suffix marks UTC
/// explicitly.
#[must_use]
pub fn resolve_target(
    cmd_target: Option<&Path>,
    data_dir: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> PathBuf {
    if let Some(p) = cmd_target {
        return p.to_path_buf();
    }
    let stamp = now.format("%Y-%m-%dT%H-%M-%SZ").to_string();
    data_dir
        .join("backups")
        .join(format!("perima-{stamp}.sqlite"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_target_uses_explicit_when_provided() {
        let p = PathBuf::from("/explicit/path.sqlite");
        let now = chrono::DateTime::<chrono::Utc>::default();
        assert_eq!(resolve_target(Some(&p), Path::new("/data"), now), p);
    }

    #[test]
    fn resolve_target_default_uses_iso_filename() {
        let now = chrono::DateTime::<chrono::Utc>::from_timestamp(1_714_492_200, 0)
            .expect("valid timestamp");
        let got = resolve_target(None, Path::new("/data"), now);
        let s = got.to_string_lossy();
        assert!(
            s.contains("perima-"),
            "filename should start with perima-: {s}"
        );
        assert!(s.ends_with(".sqlite"), "extension should be .sqlite: {s}");
        assert!(
            s.contains("Z."),
            "UTC marker should appear before extension: {s}"
        );
    }

    #[test]
    fn resolve_target_replaces_colons_for_windows() {
        let now = chrono::Utc::now();
        let got = resolve_target(None, Path::new("/data"), now);
        assert!(
            !got.to_string_lossy().contains(':'),
            "filename must not contain ':' (Windows hostile): {}",
            got.display()
        );
    }
}
