//! Verify the writer accepts new writes after a backup completes.
//!
//! Spec slice-1 §4.6 regression class: an earlier design considered
//! pausing the writer for the duration of the VACUUM INTO. That would
//! deadlock further upserts. This test asserts the writer keeps running.

#![allow(clippy::unwrap_used)] // WHY: test code; unwrap panics signal bugs.

use std::sync::Arc;

use perima_app::{BackupCommand, BackupDatabaseUseCase};
use perima_core::{
    BlakeHash, DeviceId, DiscoveredFile, FileRepository, FileSize, HashedFile, MediaPath,
    ports::DatabaseAdmin,
};
use perima_db::{SqliteDatabaseAdmin, SqliteFileRepository};

mod common;
use common::{count_files, fresh_env, insert_n_file_rows};

#[tokio::test]
async fn writer_accepts_writes_after_backup_completes() {
    let env = fresh_env();
    insert_n_file_rows(&env, 1);

    let admin: Arc<dyn DatabaseAdmin> = Arc::new(SqliteDatabaseAdmin::new(env.writer.sender()));
    let use_case = BackupDatabaseUseCase::new(admin, env.tmp.path().to_path_buf());

    let target = env.tmp.path().join("backup.sqlite");
    use_case
        .execute(BackupCommand {
            target: Some(target),
            force: false,
        })
        .await
        .expect("first backup should succeed");

    // Insert ANOTHER row after the backup, with a distinct hash so it
    // doesn't collide with the seeded i=0 row. If the writer were
    // stuck post-backup, this upsert would hang.
    let post_repo = SqliteFileRepository::new(env.writer.sender(), env.reads.clone());
    let mut bytes = [0u8; 32];
    bytes[0..4].copy_from_slice(&7u32.to_le_bytes());
    let hash = BlakeHash::from_bytes(bytes);
    post_repo
        .upsert_file_with_quick_hash(
            &HashedFile {
                discovered: DiscoveredFile {
                    absolute_path: env.tmp.path().join("post.bin"),
                    relative_path: MediaPath::new("post.bin"),
                    size: FileSize(64),
                },
                hash,
            },
            DeviceId::default(),
            None,
        )
        .expect("post-backup upsert");

    let live_count = count_files(&env.db_path());
    assert_eq!(
        live_count, 2,
        "live db should contain both seeded rows after backup"
    );
}
