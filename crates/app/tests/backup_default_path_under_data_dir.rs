//! Verify default-target resolution lands the file under
//! `<data_dir>/backups/perima-<UTC ISO>.sqlite`.
//!
//! Spec slice-1 §4.6 (default target). Complements the unit-level
//! `resolve_target_default_uses_iso_filename` test by exercising the
//! actual filesystem write through the use case.

#![allow(clippy::unwrap_used)] // WHY: test code; unwrap panics signal bugs.

use std::sync::Arc;

use perima_app::{BackupCommand, BackupDatabaseUseCase};
use perima_core::ports::DatabaseAdmin;
use perima_db::SqliteDatabaseAdmin;

mod common;
use common::{fresh_env, insert_one_file_row};

#[tokio::test]
async fn default_path_lands_under_data_dir_backups_subdir() {
    let env = fresh_env();
    insert_one_file_row(&env);

    let admin: Arc<dyn DatabaseAdmin> = Arc::new(SqliteDatabaseAdmin::new(env.writer.sender()));
    let data_dir = env.tmp.path().to_path_buf();
    let use_case = BackupDatabaseUseCase::new(admin, data_dir.clone());

    let out = use_case
        .execute(BackupCommand {
            target: None,
            force: false,
        })
        .await
        .expect("default-path backup should succeed");

    let backups_dir = data_dir.join("backups");
    assert!(
        out.absolute_path.starts_with(&backups_dir),
        "backup must live under <data_dir>/backups/, got {}",
        out.absolute_path.display()
    );
    let fname = out
        .absolute_path
        .file_name()
        .and_then(|s| s.to_str())
        .expect("utf-8 filename");
    assert!(
        fname.starts_with("perima-") && fname.ends_with(".sqlite"),
        "filename must be perima-<stamp>.sqlite, got {fname}"
    );
    assert!(out.absolute_path.exists(), "backup file should exist");
}
