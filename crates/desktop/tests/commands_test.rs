//! Headless integration tests for the Tauri backend commands.
//!
//! WHY: `tauri::State<AppState>` cannot be constructed outside a running Tauri
//! app. These tests call the `_inner` helpers extracted from each command,
//! which accept plain `Path` + `DeviceId` arguments. The underlying logic is
//! identical — only the Tauri IPC wrapping is absent.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use perima_core::{
    CoreError, DeviceId, EventBus, FileEvent, MediaMetadata, MetadataRepository, SearchRepository,
};
use perima_db::{
    ReadPool, SqliteMetadataRepository, SqliteSearchRepository, SqliteTagRepository, SqliteWriter,
};
use perima_desktop::commands::{
    attach_tag_inner, detach_tag_inner, list_files_inner, list_files_with_metadata_inner,
    list_files_with_tags_inner, list_tags_inner, list_volumes_inner, run_scan_inner,
    run_scan_inner_with_metadata, search_inner,
};
use perima_desktop::config::resolve_with_app_data_dir;

/// Create three fixture files that mimic the canonical CLI test fixtures.
fn mk_fixture(dir: &Path) {
    for (name, content) in [
        ("alpha.txt", b"alpha" as &[u8]),
        ("sub/beta.txt", b"beta"),
        ("sub/gamma.bin", b"\x00\x01\x02\x03"),
    ] {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::File::create(&path)
            .expect("create fixture")
            .write_all(content)
            .expect("write fixture");
    }
}

/// Write a tiny valid PNG (8x6 RGB) at `path` so the image extractor
/// can decode + thumbnail it. Kept inline to avoid a binary fixture.
///
/// `fill` controls the fill colour — each test caller passes a
/// distinct value so the two PNGs hash to different `blake3` digests
/// (otherwise content-addressed storage collapses them to one row +
/// one thumbnail, breaking the "2 thumbnails generated" assertion).
fn write_tiny_png(path: &Path, fill: [u8; 3]) {
    use std::io::Write as _;
    // 8x6 RGB PNG, fully procedural so no binary fixture is needed.
    let img = image::RgbImage::from_pixel(8, 6, image::Rgb(fill));
    let mut buf: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .expect("encode png");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::File::create(path)
        .expect("create png")
        .write_all(&buf)
        .expect("write png");
}

/// Scan three fixture files and assert `files_seen=3, files_new=3, files_errored=0`.
#[tokio::test]
async fn scan_indexes_files() {
    let fixture_dir = tempfile::tempdir().expect("tempdir for fixtures");
    let data_dir = tempfile::tempdir().expect("tempdir for data");
    mk_fixture(fixture_dir.path());

    let device_id = DeviceId::new();
    let result = run_scan_inner(
        fixture_dir.path(),
        /* dry_run */ false,
        data_dir.path(),
        device_id,
    )
    .await
    .expect("scan_inner should succeed");

    // WHY ScanReport fields (not ScanResult): Batch D Task 8 deleted the
    // shell-side ScanResult mirror; run_scan_inner now returns ScanReport
    // from crates/app directly. files_seen == files_new + files_updated +
    // files_errored for a clean first-run scan.
    assert_eq!(
        result.files_seen, 3,
        "expected 3 total files, got {}",
        result.files_seen
    );
    assert_eq!(
        result.files_new, 3,
        "expected 3 new files, got {}",
        result.files_new
    );
    assert_eq!(
        result.files_errored, 0,
        "expected 0 errors, got {}",
        result.files_errored
    );
}

/// After a successful scan, `list_files_inner` must return all 3 records.
#[tokio::test]
async fn list_files_after_scan() {
    let fixture_dir = tempfile::tempdir().expect("tempdir for fixtures");
    let data_dir = tempfile::tempdir().expect("tempdir for data");
    mk_fixture(fixture_dir.path());

    let device_id = DeviceId::new();
    run_scan_inner(fixture_dir.path(), false, data_dir.path(), device_id)
        .await
        .expect("scan_inner should succeed");

    let entries =
        list_files_inner(data_dir.path(), 100, None).expect("list_files_inner should succeed");

    assert_eq!(
        entries.len(),
        3,
        "expected 3 file entries, got {}",
        entries.len()
    );
}

/// After inserting metadata for a scanned file, the
/// `list_files_with_metadata_inner` helper must return at least one row
/// with metadata fields populated from the stored record.
#[tokio::test]
async fn list_files_with_metadata_returns_rows() {
    let fixture_dir = tempfile::tempdir().expect("tempdir for fixtures");
    let data_dir = tempfile::tempdir().expect("tempdir for data");
    mk_fixture(fixture_dir.path());

    let device_id = DeviceId::new();
    run_scan_inner(fixture_dir.path(), false, data_dir.path(), device_id)
        .await
        .expect("scan_inner should succeed");

    // Attach a metadata row to one of the scanned files. We pull its
    // hash from `list_files_inner` to guarantee FK-compatibility with
    // the `files` row the scanner just inserted.
    //
    // WHY `entries[0].hash` is now `BlakeHash` (not `String`): Batch D Task 8
    // deleted the `FileEntry` wire mirror; `list_files_inner` now returns
    // `Vec<FileLocationRecord>` where `hash` is a typed `BlakeHash` value.
    let entries = list_files_inner(data_dir.path(), 100, None).expect("list_files_inner");
    assert!(!entries.is_empty(), "scan must have inserted ≥1 file");
    let first_hash = entries[0].hash;

    let db_path = data_dir.path().join("perima.db");
    // WHY writer+pool harness (post-Batch-C Task 4): the metadata
    // adapter now takes `(flume::Sender<WriteCmd>, ReadPool)`.
    // `run_scan_inner` above opened its own writer + dropped it at
    // scope end; we spin up a fresh writer here and keep its handle
    // alive via `_writer` until teardown (WAL lets the two writers
    // coexist).
    struct TestNoopBus;
    impl EventBus for TestNoopBus {
        fn emit(&self, _: &FileEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }
    let bus: Arc<dyn EventBus> = Arc::new(TestNoopBus);
    let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
    let reads = ReadPool::open(&db_path).expect("pool open");
    let repo = SqliteMetadataRepository::new(writer.sender(), reads);
    let meta = MediaMetadata {
        hash: first_hash,
        width: Some(640),
        height: Some(480),
        duration_ms: None,
        captured_at: Some("2026-04-16T00:00:00Z".into()),
        camera_make: Some("Acme".into()),
        camera_model: Some("Cam One".into()),
        codec: None,
        bitrate_bps: None,
        mime_type: Some("image/jpeg".into()),
        thumbnail_path: None,
        thumbnail_status: None,
    };
    repo.upsert_metadata(&meta, device_id)
        .expect("upsert_metadata");

    let rows = list_files_with_metadata_inner(&repo, 100, None)
        .expect("list_files_with_metadata_inner should succeed");

    assert!(
        !rows.is_empty(),
        "expected ≥1 FileWithMetadataPayload row, got 0"
    );
    // WHY `entries[0].hash.to_hex()`: `FileWithMetadataPayload.hash` is a
    // hex String (flat IPC payload); `FileLocationRecord.hash` is `BlakeHash`.
    // Compare using the hex representation.
    let populated = rows
        .iter()
        .find(|r| r.hash == entries[0].hash.to_hex())
        .expect("row for inserted metadata must be present");
    assert_eq!(populated.width, Some(640));
    assert_eq!(populated.height, Some(480));
    assert_eq!(populated.camera_make.as_deref(), Some("Acme"));
    assert_eq!(populated.mime_type.as_deref(), Some("image/jpeg"));

    // Tear down explicitly — drops the repo's sender clone + reaps
    // the writer thread cleanly before the tempdir is removed.
    drop(repo);
    writer.join();
}

/// After a successful scan, `list_volumes_inner` must return at least one volume.
#[tokio::test]
async fn list_volumes_after_scan() {
    let fixture_dir = tempfile::tempdir().expect("tempdir for fixtures");
    let data_dir = tempfile::tempdir().expect("tempdir for data");
    mk_fixture(fixture_dir.path());

    let device_id = DeviceId::new();
    run_scan_inner(fixture_dir.path(), false, data_dir.path(), device_id)
        .await
        .expect("scan_inner should succeed");

    let volumes =
        list_volumes_inner(data_dir.path(), device_id).expect("list_volumes_inner should succeed");

    assert!(!volumes.is_empty(), "expected ≥1 volume after scan, got 0");
}

/// Regression for utof/perima#15 HIGH #11a: the desktop scan must wire
/// the metadata queue + thumbnailer end-to-end. Scanning a tempdir of
/// PNG files must produce `file_metadata` rows AND WebP thumbnails on
/// disk under `<data_dir>/thumbnails/` — the same subtree the Tauri
/// asset-protocol scope exposes.
#[tokio::test]
async fn desktop_scan_populates_metadata_and_thumbnails() {
    let fixture_dir = tempfile::tempdir().expect("tempdir for fixtures");
    let data_dir = tempfile::tempdir().expect("tempdir for data");

    write_tiny_png(&fixture_dir.path().join("a.png"), [200, 150, 100]);
    write_tiny_png(&fixture_dir.path().join("sub/b.png"), [10, 20, 200]);

    let device_id = DeviceId::new();
    let db_path = data_dir.path().join("perima.db");

    // Share the repo handle with the worker (matches production wiring
    // in `crates/desktop/src/lib.rs::run`): a single
    // `SqliteMetadataRepository` Arc cloned into the queue worker and
    // inspected directly after the scan drain completes.
    //
    // WHY writer+pool harness (post-Batch-C Task 4): the adapter now
    // takes `(flume::Sender<WriteCmd>, ReadPool)`. `run_scan_inner_with_metadata`
    // below opens its own writer internally; we keep a separate
    // writer for this test's direct assertions (WAL lets both
    // coexist).
    struct MetaTestNoopBus;
    impl EventBus for MetaTestNoopBus {
        fn emit(&self, _: &FileEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }
    let bus: Arc<dyn EventBus> = Arc::new(MetaTestNoopBus);
    let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
    let reads = ReadPool::open(&db_path).expect("pool open");
    let metadata_repo = Arc::new(SqliteMetadataRepository::new(writer.sender(), reads));

    let result = run_scan_inner_with_metadata(
        fixture_dir.path(),
        false,
        data_dir.path(),
        device_id,
        Some(Arc::clone(&metadata_repo) as Arc<dyn MetadataRepository>),
    )
    .await
    .expect("scan with metadata should succeed");

    // WHY ScanReport fields: see scan_indexes_files test comment.
    assert_eq!(
        result.files_seen, 2,
        "expected 2 files, got {}",
        result.files_seen
    );
    assert_eq!(
        result.files_new, 2,
        "expected 2 new, got {}",
        result.files_new
    );

    // Assert 2 metadata rows exist via the shared handle. The drain
    // path above guarantees the worker has persisted by the time we
    // reach this assertion.
    let rows = list_files_with_metadata_inner(metadata_repo.as_ref(), 100, None)
        .expect("list_files_with_metadata_inner");
    assert_eq!(
        rows.iter().filter(|r| r.mime_type.is_some()).count(),
        2,
        "expected 2 rows with mime_type populated (extractor ran), got {} (rows: {:?})",
        rows.iter().filter(|r| r.mime_type.is_some()).count(),
        rows.iter()
            .map(|r| (&r.relative_path, &r.mime_type, &r.thumbnail_status))
            .collect::<Vec<_>>(),
    );

    // Assert 2 thumbnails exist on disk under <data_dir>/thumbnails.
    let thumb_root = data_dir.path().join("thumbnails");
    let mut thumb_count = 0usize;
    if thumb_root.exists() {
        for bucket in std::fs::read_dir(&thumb_root).expect("read thumbnails root") {
            let bucket = bucket.expect("bucket entry");
            if bucket.file_type().expect("file_type").is_dir() {
                for entry in std::fs::read_dir(bucket.path()).expect("read bucket") {
                    let entry = entry.expect("thumb entry");
                    if entry.path().extension().and_then(|e| e.to_str()) == Some("webp") {
                        thumb_count += 1;
                    }
                }
            }
        }
    }
    assert_eq!(
        thumb_count,
        2,
        "expected 2 .webp thumbnails under {}, found {}",
        thumb_root.display(),
        thumb_count,
    );

    // Tear down explicitly — drops the repo's sender clone + reaps
    // the writer thread cleanly before the tempdir is removed.
    drop(metadata_repo);
    writer.join();
}

/// Exercises the four tag `_inner` helpers end-to-end:
/// attach → list-with-tags → list-tags → detach → verify empty.
#[tokio::test]
async fn list_files_with_tags_returns_tagged_rows() {
    let td = tempfile::tempdir().expect("tempdir");
    let data_dir = td.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("mkdir data");
    let fixture_dir = td.path().join("fixture");
    mk_fixture(&fixture_dir);

    let device = DeviceId::new();
    run_scan_inner(&fixture_dir, false, &data_dir, device)
        .await
        .expect("scan");

    // Open tag + metadata repos against the same DB. Post-Batch-C
    // Tasks 3 + 4, both `SqliteTagRepository` and
    // `SqliteMetadataRepository` require `(writer.sender(), ReadPool)`;
    // spin up a dedicated writer for this test and keep its handle
    // alive via `writer` until teardown. WAL mode lets this writer
    // coexist with the one spawned internally by `run_scan_inner`
    // above.
    let db_path = data_dir.join("perima.db");

    struct TestNoopBus;
    impl EventBus for TestNoopBus {
        fn emit(&self, _: &FileEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }
    let bus: Arc<dyn EventBus> = Arc::new(TestNoopBus);
    let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
    let reads = ReadPool::open(&db_path).expect("pool open");
    let tag_repo = SqliteTagRepository::new(writer.sender(), reads.clone());
    let metadata_repo = SqliteMetadataRepository::new(writer.sender(), reads);

    // Get files list to find a hash. `FileWithMetadataPayload.hash` is a
    // hex String (flat composite payload retained in Batch D Task 8).
    let files = list_files_with_metadata_inner(&metadata_repo, 100, None).expect("list");
    assert!(!files.is_empty(), "scan must have produced ≥1 file");

    let first_hash = files[0].hash.clone();

    // Attach a tag via the inner helper.
    // WHY `attach_tag_inner` now returns `Tag` (not `TagPayload`):
    // Batch D Task 8 deleted TagPayload; Tag is the core type.
    let tag = attach_tag_inner(&tag_repo, &first_hash, "test-tag", device).expect("attach");
    assert_eq!(tag.name, "test-tag");

    // List files with tags — the tagged file must appear with 1 tag.
    // WHY `fwt.tags` is `Vec<Tag>` now: FileWithTagsPayload.tags was
    // updated from Vec<TagPayload> to Vec<Tag> in Batch D Task 8.
    let tagged =
        list_files_with_tags_inner(&metadata_repo, &tag_repo, 100, None).expect("list with tags");
    assert!(!tagged.is_empty());
    let tagged_file = tagged
        .iter()
        .find(|f| f.file.hash == first_hash)
        .expect("find tagged file");
    assert_eq!(tagged_file.tags.len(), 1, "must have exactly 1 tag");
    assert_eq!(tagged_file.tags[0].name, "test-tag");

    // List tags — must return exactly 1. `list_tags_inner` now returns
    // `Vec<Tag>` directly.
    let tags = list_tags_inner(&tag_repo).expect("list tags");
    assert_eq!(tags.len(), 1);

    // Detach. WHY `tag.id.to_string()`: `Tag.id` is `Uuid` (not String);
    // `detach_tag_inner` takes `tag_id_str: &str` and parses it internally.
    detach_tag_inner(&tag_repo, &first_hash, &tag.id.to_string(), device).expect("detach");

    // Verify empty after detach.
    let tagged2 = list_files_with_tags_inner(&metadata_repo, &tag_repo, 100, None)
        .expect("list after detach");
    let tagged_file2 = tagged2
        .iter()
        .find(|f| f.file.hash == first_hash)
        .expect("find file after detach");
    assert!(tagged_file2.tags.is_empty(), "no tags after detach");

    // Tear down explicitly — drops both repos' sender clones + reaps
    // the writer thread cleanly before the tempdir is removed.
    drop(tag_repo);
    drop(metadata_repo);
    writer.join();
}

/// Regression for v0.4.3: thumbnail directory referenced by
/// `AppState.data_dir` must fall under the same subtree the
/// `tauri.conf.json` `assetProtocol.scope` allows. If this diverges,
/// `convertFileSrc` returns 404 silently and every grid tile becomes
/// a broken image.
///
/// WHY no real Tauri runtime: we cannot exercise `convertFileSrc`
/// without a display, but we CAN pin that the `data_dir`-derived
/// thumbnail root starts with the simulated `app_data_dir` (matching
/// what Tauri's `$APPDATA` variable would expand to at runtime) AND
/// contains the `/perima/thumbnails/` segment the scope literal
/// declares. A future change that flips config to point elsewhere
/// trips this test.
#[test]
fn thumbnail_root_matches_asset_protocol_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Simulate `app.path().app_data_dir()` on Linux:
    // `~/.local/share/dev.perima.desktop`.
    let app_data_dir = tmp.path().join("dev.perima.desktop");

    let cfg = resolve_with_app_data_dir(&app_data_dir).expect("resolve_with_app_data_dir");

    // Production thumbnail root is `<data_dir>/thumbnails/` — see
    // `ThumbnailGenerator::new(data_dir)` construction in
    // `crates/desktop/src/commands.rs::run_scan_inner_with_metadata`.
    let thumb_root = cfg.data_dir.join("thumbnails");

    assert!(
        thumb_root.starts_with(&app_data_dir),
        "thumbnail root {} must live under app_data_dir {} (Tauri's $APPDATA)",
        thumb_root.display(),
        app_data_dir.display(),
    );

    // The scope literal is `$APPDATA/perima/thumbnails/**`. With
    // `data_dir = <app_data_dir>/perima`, `<data_dir>/thumbnails` is
    // exactly the scope-root directory. `.ends_with` here is a
    // logical suffix match on path components, not a substring match.
    assert!(
        thumb_root.ends_with("perima/thumbnails"),
        "thumbnail root {} must end in perima/thumbnails (matches $APPDATA/perima/thumbnails/** scope literal)",
        thumb_root.display(),
    );
}

/// Smoke test for the `search` Tauri command path — plan Task 5 Step 5.
///
/// Scans a fixture dir, rebuilds the FTS5 index, runs `search_inner`
/// via the `SearchRepository` trait, and asserts the query returns the
/// seeded filename. Exercises the inner helper end-to-end without
/// constructing `tauri::State`.
#[tokio::test]
async fn search_returns_hit_after_scan_and_rebuild() {
    let td = tempfile::tempdir().expect("tempdir");
    let data_dir = td.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("mkdir data");
    let fixture_dir = td.path().join("fixture");
    mk_fixture(&fixture_dir);

    let device = DeviceId::new();
    run_scan_inner(&fixture_dir, false, &data_dir, device)
        .await
        .expect("scan");

    let db_path = data_dir.join("perima.db");
    // WHY spawn a test-local writer + pool: the repo adapter is constructed
    // from `(Sender<WriteCmd>, ReadPool)`, and this integration test drives
    // search without going through `AppContainer`. The repo clones the
    // sender; dropping `search_writer` at end-of-test closes the channel
    // and lets the writer thread exit.
    struct NoopBus;
    impl EventBus for NoopBus {
        fn emit(&self, _: &FileEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }
    let search_writer = SqliteWriter::start(&db_path, Arc::new(NoopBus) as Arc<dyn EventBus>)
        .expect("search writer");
    let search_reads = ReadPool::open(&db_path).expect("search pool");
    let search_repo = SqliteSearchRepository::new(search_writer.sender(), search_reads);
    drop(search_writer);
    search_repo.rebuild().expect("rebuild index");

    // WHY `h.relative_path` on `SearchHit`: Batch D Task 8 deleted
    // `SearchHitPayload`; `search_inner` now returns `Vec<SearchHit>`
    // from `perima_core`. `SearchHit.relative_path` is a `String`.
    // `alpha.txt` is one of the mk_fixture files; unicode61 splits on
    // `.` so the `alpha` token is indexed.
    let hits = search_inner(&search_repo, "alpha", 10).expect("search");
    assert!(
        hits.iter().any(|h| h.relative_path.ends_with("alpha.txt")),
        "expected a hit ending in alpha.txt, got: {:?}",
        hits.iter().map(|h| &h.relative_path).collect::<Vec<_>>()
    );

    // Empty query must return [] without hitting FTS5.
    let empty = search_inner(&search_repo, "   ", 10).expect("empty search");
    assert!(empty.is_empty(), "whitespace-only query must return []");

    // Limit clamp: passing 0 must not panic or return garbage.
    let zero_limit = search_inner(&search_repo, "alpha", 0).expect("zero limit");
    assert!(
        !zero_limit.is_empty(),
        "limit=0 should fall back to default, not empty"
    );
}
