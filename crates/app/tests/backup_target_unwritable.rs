//! Verify a target whose parent we can neither create nor write to
//! returns `BackupFailureReason::TargetUnwritable`.
//!
//! Spec slice-1 §4.6 (typed error UX). On Linux we abuse `/proc/1/foo/`
//! (procfs is read-only); on macOS we abuse `/System/Volumes/Data/.fs.write/`
//! (SIP-protected). Windows runners have no equivalent portable
//! always-unwritable path, so we ignore the test there.

#![allow(clippy::unwrap_used)] // WHY: test code; unwrap panics signal bugs.

use std::sync::Arc;

use perima_app::{BackupCommand, BackupDatabaseUseCase};
use perima_core::{CoreError, errors::BackupFailureReason, ports::DatabaseAdmin};
use perima_db::SqliteDatabaseAdmin;

mod common;
use common::{fresh_env, insert_one_file_row};

#[cfg_attr(
    target_os = "windows",
    ignore = "no portable unwritable path on Windows runners"
)]
#[tokio::test]
async fn target_with_unwritable_parent_returns_target_unwritable() {
    let env = fresh_env();
    insert_one_file_row(&env);

    let admin: Arc<dyn DatabaseAdmin> = Arc::new(SqliteDatabaseAdmin::new(env.writer.sender()));
    let use_case = BackupDatabaseUseCase::new(admin, env.tmp.path().to_path_buf());

    let unwritable = if cfg!(target_os = "linux") {
        std::path::PathBuf::from("/proc/1/foo/backup.sqlite")
    } else {
        std::path::PathBuf::from("/System/Volumes/Data/.fs.write/test/backup.sqlite")
    };

    let res = use_case
        .execute(BackupCommand {
            target: Some(unwritable),
            force: false,
        })
        .await;

    assert!(
        matches!(
            res,
            Err(CoreError::BackupFailed {
                reason: BackupFailureReason::TargetUnwritable { .. }
            })
        ),
        "expected TargetUnwritable, got {res:?}"
    );
}
