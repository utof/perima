//! Task 11 (spec §4.8): `TagCommand::ListFilesWithTags` must surface
//! `file_uuid` on every returned `FileWithTags.location`.
//!
//! WHY this test matters: post-Task-11, the IPC payload pivots to
//! `file_uuid` as the stable surrogate key. Pending files (no `full_hash`
//! computed yet) have `hash: None` but `file_uuid` is always present so
//! UI / FK lookups still work. Re-introducing a non-nullable `hash` or
//! removing `file_uuid` is a regression caught here.

#![allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.

use std::io::Write as _;
use std::sync::Arc;

use perima_app::{
    FullScan, ScanCommand, ScanUseCase, TagCommand, TagFilter, TagOutput, TagUseCase,
};
use perima_core::{
    AppEvent, CoreError, EventBus, FileRepository, HashService, IdentityCacheRepository,
    MetadataRepository, Scanner, TagRepository, VolumeRepository,
};
use perima_db::{
    ReadPool, SqliteFileRepository, SqliteIdentityCacheRepository, SqliteMetadataRepository,
    SqliteTagRepository, SqliteVolumeRepository, SqliteWriter,
};
use perima_fs::WalkdirScanner;
use perima_hash::Blake3Service;
use perima_media::ThumbnailGenerator;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// No-op event bus — keeps the writer happy without intercepting events.
struct NullBus;
impl EventBus for NullBus {
    fn emit(&self, _: &AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

fn mk_fixture(dir: &std::path::Path) {
    let path = dir.join("only.txt");
    std::fs::File::create(&path)
        .unwrap()
        .write_all(b"only")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_files_with_tags_payload_carries_file_uuid_and_nullable_hash() {
    // ---- wire-up -----------------------------------------------------------
    let db_tmp = TempDir::new().unwrap();
    let fixture = TempDir::new().unwrap();
    mk_fixture(fixture.path());

    let db_path = db_tmp.path().join("perima.db");
    let writer = SqliteWriter::start(&db_path, Arc::new(NullBus)).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();

    let files: Arc<dyn FileRepository> =
        Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
    let volumes: Arc<dyn VolumeRepository> =
        Arc::new(SqliteVolumeRepository::new(writer.sender(), reads.clone()));
    let metadata: Arc<dyn MetadataRepository> = Arc::new(SqliteMetadataRepository::new(
        writer.sender(),
        reads.clone(),
    ));
    let cache: Arc<dyn IdentityCacheRepository> = Arc::new(SqliteIdentityCacheRepository::new(
        writer.sender(),
        reads.clone(),
    ));
    let tags: Arc<dyn TagRepository> = Arc::new(SqliteTagRepository::new(writer.sender(), reads));

    let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());
    let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
    let thumbnailer = Arc::new(ThumbnailGenerator::disabled());
    let events: Arc<dyn EventBus> = Arc::new(NullBus);

    // Run a scan to seed the DB.
    let uc = ScanUseCase::new(
        Arc::clone(&files),
        Arc::clone(&volumes),
        Arc::clone(&metadata),
        Arc::clone(&cache),
        scanner,
        hasher,
        thumbnailer,
        Arc::clone(&events),
    );
    let device_id = perima_core::DeviceId::new();
    let cmd = ScanCommand::Full(FullScan {
        path: fixture.path().to_path_buf(),
        device_id,
        with_metadata: false,
        dry_run: false,
        no_wait_metadata: true,
        no_thumbnails: true,
        cancel: CancellationToken::new(),
        on_persist: None,
    });
    let report = uc.execute(cmd).await.expect("scan should succeed");
    assert_eq!(report.files_seen, 1, "fixture seeded with 1 file");

    // ---- act --------------------------------------------------------------
    let tag_uc = TagUseCase::new(Arc::clone(&tags), Arc::clone(&metadata), events);
    let out = tag_uc
        .execute(TagCommand::ListFilesWithTags {
            filter: Some(TagFilter {
                limit: 100,
                volume: None,
            }),
        })
        .await
        .expect("list_files_with_tags");
    let TagOutput::FilesWithTags(rows) = out else {
        panic!("unexpected TagOutput variant");
    };

    // ---- assert -----------------------------------------------------------
    assert!(!rows.is_empty(), "scan must have produced at least one row");
    let row = &rows[0];
    // file_uuid is always present (stable surrogate, populated in V011).
    assert_ne!(
        row.location.file_uuid.0,
        uuid::Uuid::nil(),
        "file_uuid must be populated post-Task-11 (got nil UUID)",
    );
    // Hash is Some(...) for files that completed full hashing during scan.
    // For pending files (post-Task-9 dedup batches that haven't computed
    // full_hash yet) the hash is None. The scan path always populates it.
    assert!(
        row.location.hash.is_some(),
        "scanner-inserted row must carry full_hash (hash type pivoted to Option in Task 11)",
    );

    drop(tags);
    drop(files);
    drop(volumes);
    drop(metadata);
    drop(cache);
    writer.join();
}
