//! Writer-side handler for `WriteCmd::Backup`.
//!
//! WHY runs in the writer thread (not in a read pool): VACUUM INTO
//! takes a brief reserved-write lock on the source DB. The writer is
//! the only producer of writes in the perima architecture (Batch C);
//! routing backup through it preserves the single-writer invariant
//! and serializes naturally with other writes.
//!
//! WHY no `bus` parameter (asymmetric vs other writer handlers):
//! backup emits no domain events — slice 1 has no `AppEvent::BackupProgress`
//! or `BackupCompleted`. Adding an unused `bus` parameter would be dead.

use std::path::Path;

use perima_core::{CoreError, errors::BackupFailureReason};
use rusqlite::Connection;

use crate::cmd::BackupWriteCmd;

/// Run `VACUUM INTO ?1` against the writable connection and reply with
/// the produced file's size in bytes.
///
/// WHY size-after-VACUUM (not pre-allocate then size): VACUUM INTO is
/// atomic from `SQLite`'s perspective — a successful return guarantees
/// the target file exists and is complete. We then `fs::metadata` to
/// report the size to the user (slice 1 spec §IPC surface).
// WHY allow needless_pass_by_value: `cmd` is passed by value because
// `handle` moves `cmd.reply` (a `flume::Sender`) to send the result.
// Taking `&BackupWriteCmd` would prevent moving the sender out.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn handle(conn: &Connection, cmd: BackupWriteCmd) {
    let result = run(conn, &cmd.target);
    if cmd.reply.send(result).is_err() {
        tracing::debug!("backup reply channel closed before send");
    }
}

fn run(conn: &Connection, target: &Path) -> Result<u64, CoreError> {
    // WHY to_str (not to_string_lossy): a non-UTF8 target would otherwise
    // be silently mangled to U+FFFD, sent to SQLite, which would fail with
    // CannotOpen against a phantom path — the user sees an error citing a
    // path that doesn't exist on disk. Fail fast with TargetUnwritable so
    // the user sees the actual path and can fix it. Mirrors the precedent
    // at crates/db/src/writer/volume.rs:242-251.
    let target_str = target.to_str().ok_or_else(|| CoreError::BackupFailed {
        reason: BackupFailureReason::TargetUnwritable {
            path: target.display().to_string(),
            message: "target path is not valid UTF-8".to_string(),
        },
    })?;
    conn.execute("VACUUM INTO ?1", rusqlite::params![target_str])
        .map_err(|e| {
            // SQLite reports SQLITE_FULL as ErrorCode::DiskFull;
            // SQLITE_CANTOPEN often means the target dir is unwritable.
            if let rusqlite::Error::SqliteFailure(ffi_err, _) = &e {
                match ffi_err.code {
                    rusqlite::ErrorCode::DiskFull => {
                        return CoreError::BackupFailed {
                            reason: BackupFailureReason::DiskFull {
                                path: target.display().to_string(),
                            },
                        };
                    }
                    rusqlite::ErrorCode::CannotOpen => {
                        return CoreError::BackupFailed {
                            reason: BackupFailureReason::TargetUnwritable {
                                path: target.display().to_string(),
                                message: e.to_string(),
                            },
                        };
                    }
                    _ => {}
                }
            }
            CoreError::BackupFailed {
                reason: BackupFailureReason::Internal(e.to_string()),
            }
        })?;

    let size = std::fs::metadata(target)
        .map_err(|e| CoreError::BackupFailed {
            reason: BackupFailureReason::Internal(format!("metadata after VACUUM INTO: {e}")),
        })?
        .len();
    Ok(size)
}
