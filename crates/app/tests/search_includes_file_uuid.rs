//! Task 11 (spec §4.8): `SearchHit` must surface `file_uuid` for every result.
//!
//! `blake3_hash` becomes `Option<String>` in the same change (pending files
//! with no `full_hash` still hit the FTS index). This test pins the wire
//! shape with one assertion per field: `file_uuid` is non-empty UUID,
//! `blake3_hash` is `Some(_)` for a scan-seeded row.

#![allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.

use std::io::Write as _;
use std::sync::Arc;

use perima_app::{FullScan, ScanCommand, ScanUseCase, SearchCommand, SearchUseCase};
use perima_core::{
    AppEvent, CoreError, EventBus, FileRepository, HashService, IdentityCacheRepository,
    MetadataRepository, Scanner, SearchRepository, VolumeRepository,
};
use perima_db::{
    ReadPool, SqliteFileRepository, SqliteIdentityCacheRepository, SqliteMetadataRepository,
    SqliteSearchRepository, SqliteVolumeRepository, SqliteWriter,
};
use perima_fs::WalkdirScanner;
use perima_hash::Blake3Service;
use perima_media::ThumbnailGenerator;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

struct NullBus;
impl EventBus for NullBus {
    fn emit(&self, _: &AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

fn mk_fixture(dir: &std::path::Path) {
    // WHY a recognisable filename: gives a deterministic FTS5 token to query.
    let path = dir.join("vacation_paris_2024.txt");
    std::fs::File::create(&path)
        .unwrap()
        .write_all(b"vacation")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_hit_carries_file_uuid_and_nullable_hash() {
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
    let search: Arc<dyn SearchRepository> =
        Arc::new(SqliteSearchRepository::new(writer.sender(), reads));

    let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());
    let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
    let thumbnailer = Arc::new(ThumbnailGenerator::disabled());
    let events: Arc<dyn EventBus> = Arc::new(NullBus);

    // Seed via scan (populates files, file_locations, search_content via triggers).
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
    uc.execute(cmd).await.expect("scan");

    // ---- act --------------------------------------------------------------
    let search_uc = SearchUseCase::new(Arc::clone(&search), events);
    let out = search_uc
        .execute(SearchCommand::Query {
            q: "vacation".into(),
            limit: None,
        })
        .await
        .expect("search query");

    // ---- assert -----------------------------------------------------------
    assert!(!out.hits.is_empty(), "expected at least one search hit");
    let hit = &out.hits[0];
    assert_ne!(
        hit.file_uuid.0,
        uuid::Uuid::nil(),
        "file_uuid must be populated post-Task-11 (got nil UUID)",
    );
    assert!(
        hit.blake3_hash.is_some(),
        "scan-seeded row must carry full_hash (Option<String> shape post-Task-11)",
    );

    drop(search);
    drop(files);
    drop(volumes);
    drop(metadata);
    drop(cache);
    writer.join();
}
