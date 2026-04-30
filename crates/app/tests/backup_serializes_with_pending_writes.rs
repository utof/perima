//! Verify backup snapshot is consistent with the writer's serialised
//! command order: rows enqueued BEFORE the backup are visible in the
//! snapshot; rows enqueued AFTER are not.
//!
//! Spec slice-1 §4.6: the writer is a single-actor FIFO; backup is just
//! another `WriteCmd`. Sending 100 upserts → backup → 1 more upsert
//! must yield a snapshot with exactly 100 rows and a live db with 101.

#![allow(clippy::unwrap_used)] // WHY: test code; unwrap panics signal bugs.

use std::sync::Arc;

use perima_app::{BackupCommand, BackupDatabaseUseCase};
use perima_core::{FileRepository, ports::DatabaseAdmin};
use perima_db::SqliteDatabaseAdmin;

mod common;
use common::{count_files, fresh_env, insert_n_file_rows};

#[tokio::test]
async fn backup_snapshot_contains_pre_backup_rows_only() {
    let env = fresh_env();
    insert_n_file_rows(&env, 100);

    let admin: Arc<dyn DatabaseAdmin> = Arc::new(SqliteDatabaseAdmin::new(env.writer.sender()));
    let use_case = BackupDatabaseUseCase::new(admin, env.tmp.path().to_path_buf());

    let target = env.tmp.path().join("snapshot.sqlite");
    use_case
        .execute(BackupCommand {
            target: Some(target.clone()),
            force: false,
        })
        .await
        .expect("backup should succeed");

    // One more row enqueued AFTER the backup completes.
    // (`execute` returns once the writer's reply lands → backup's VACUUM
    // INTO has finished → next upsert must follow it in writer FIFO.)
    let post_repo = perima_db::SqliteFileRepository::new(env.writer.sender(), env.reads.clone());
    let lo = 100u32;
    let mut bytes = [0u8; 32];
    bytes[0..4].copy_from_slice(&lo.to_le_bytes());
    let hash = perima_core::BlakeHash::from_bytes(bytes);
    post_repo
        .upsert_file_with_quick_hash(
            &perima_core::HashedFile {
                discovered: perima_core::DiscoveredFile {
                    absolute_path: env.tmp.path().join("post-100.bin"),
                    relative_path: perima_core::MediaPath::new("post-100.bin"),
                    size: perima_core::FileSize(64),
                },
                hash,
            },
            perima_core::DeviceId::default(),
            None,
        )
        .expect("post-backup upsert");

    assert_eq!(
        count_files(&target),
        100,
        "snapshot must contain only pre-backup rows"
    );
    assert_eq!(
        count_files(&env.db_path()),
        101,
        "live db must contain pre-backup + post-backup rows"
    );
}
