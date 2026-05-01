//! Verify the in-flight `AtomicBool` guard refuses concurrent backups.
//!
//! Spec slice-1 §4.6 (concurrency). Uses a `SlowAdmin` test-double
//! that sleeps for 200ms before producing a fake backup file — gives
//! us a deterministic 10ms-stagger window to confirm the second
//! `execute` returns `AlreadyInProgress` instead of racing on the
//! VACUUM INTO timing of a real adapter.

#![allow(clippy::unwrap_used)] // WHY: test code; unwrap panics signal bugs.

use std::sync::Arc;

use perima_app::{BackupCommand, BackupDatabaseUseCase};
use perima_core::{CoreError, errors::BackupFailureReason, ports::DatabaseAdmin};

mod common;
use common::SlowAdmin;

#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_backups_one_succeeds_one_returns_already_in_progress() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let admin: Arc<dyn DatabaseAdmin> = Arc::new(SlowAdmin { sleep_ms: 200 });
    let use_case = Arc::new(BackupDatabaseUseCase::new(admin, tmp.path().to_path_buf()));

    let uc1 = Arc::clone(&use_case);
    let uc2 = Arc::clone(&use_case);
    let p1 = tmp.path().join("a.sqlite");
    let p2 = tmp.path().join("b.sqlite");

    let h1 = tokio::spawn(async move {
        uc1.execute(BackupCommand {
            target: Some(p1),
            force: false,
        })
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let h2 = tokio::spawn(async move {
        uc2.execute(BackupCommand {
            target: Some(p2),
            force: false,
        })
        .await
    });

    let r1 = h1.await.expect("join1");
    let r2 = h2.await.expect("join2");

    assert!(r1.is_ok(), "first backup should succeed: {r1:?}");
    assert!(
        matches!(
            r2,
            Err(CoreError::BackupFailed {
                reason: BackupFailureReason::AlreadyInProgress
            })
        ),
        "second backup should return AlreadyInProgress, got {r2:?}"
    );

    let p3 = tmp.path().join("c.sqlite");
    use_case
        .execute(BackupCommand {
            target: Some(p3),
            force: false,
        })
        .await
        .expect("post-contention backup must succeed");
}
