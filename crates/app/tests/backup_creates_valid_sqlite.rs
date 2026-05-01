//! Verify `BackupDatabaseUseCase::execute` produces a `SQLite` file that
//! can be opened independently and contains the same row count as the
//! source.
//!
//! Spec slice-1 §4.6. This is the canonical happy-path: real
//! `SqliteDatabaseAdmin` adapter + writer, one seeded row, one backup,
//! one read-only-open count assertion.

#![allow(clippy::unwrap_used)] // WHY: test code; unwrap panics signal bugs.

use std::sync::Arc;

use perima_app::{BackupCommand, BackupDatabaseUseCase};
use perima_core::ports::DatabaseAdmin;
use perima_db::SqliteDatabaseAdmin;

mod common;
use common::{count_files, fresh_env, insert_one_file_row};

#[tokio::test]
async fn backup_produces_valid_sqlite_with_correct_row_count() {
    let env = fresh_env();
    insert_one_file_row(&env);

    let admin: Arc<dyn DatabaseAdmin> = Arc::new(SqliteDatabaseAdmin::new(env.writer.sender()));
    let use_case = BackupDatabaseUseCase::new(admin, env.tmp.path().to_path_buf());

    let target = env.tmp.path().join("backup.sqlite");
    let out = use_case
        .execute(BackupCommand {
            target: Some(target.clone()),
            force: false,
        })
        .await
        .expect("backup should succeed");

    assert_eq!(out.absolute_path, target);
    assert!(out.size_bytes > 0, "backup file should be non-empty");
    assert!(target.exists(), "backup file should exist on disk");

    let backup_count = count_files(&target);
    assert_eq!(
        backup_count, 1,
        "backup snapshot must contain the one seeded row"
    );
}
