//! Verify `ComputeFullHashUseCase` + `DedupUseCase` end-to-end against
//! a real `SQLite`-backed file repository.
//!
//! Spec §4.6 + §4.7. Tests cover:
//!  - Single `full_hash` compute persists onto the `files` row.
//!  - `list_quick_hash_collisions` groups by `quick_hash` for active rows.
//!  - `mark_verified_distinct` excludes those `file_uuids` from later listings.

#![allow(clippy::unwrap_used)] // WHY: test code; unwrap panics signal bugs.
#![allow(clippy::too_many_arguments)] // WHY: test seed helper takes one arg per column on the row.

use std::sync::Arc;

use perima_app::{ComputeFullHashUseCase, DedupUseCase};
use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, EventBus, FileRepository, FileSize, FileUuid,
    HashService, HashedFile, MediaPath, VolumeId, VolumeIdentifiers, VolumeRepository,
};
use perima_db::{ReadPool, SqliteFileRepository, SqliteVolumeRepository, SqliteWriter};
use perima_hash::Blake3Service;
use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Test stubs
// ---------------------------------------------------------------------------

/// No-op event bus — the tests don't observe events; they only verify DB state.
struct NullBus;
impl EventBus for NullBus {
    fn emit(&self, _: &AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Build a real DB harness: tempdir, writer, file repo, volume repo, RO conn.
fn harness() -> (
    TempDir,
    Arc<SqliteFileRepository>,
    Arc<SqliteVolumeRepository>,
    Connection,
) {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("perima.db");

    let bus: Arc<dyn EventBus> = Arc::new(NullBus);
    let writer = SqliteWriter::start(&db_path, bus).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();

    let file_repo = Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
    let vol_repo = Arc::new(SqliteVolumeRepository::new(writer.sender(), reads));

    // RO inspection connection (writer-bypass; reads only).
    #[allow(clippy::disallowed_methods)]
    let ro = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();

    // Drop writer handle: senders inside the repos keep the writer thread alive.
    drop(writer);

    (tmp, file_repo, vol_repo, ro)
}

/// Insert a `volumes` row + `volume_mounts` row at `mount_path` so
/// `lookup_by_file_uuid` can resolve the absolute path.
fn make_volume(
    vol_repo: &SqliteVolumeRepository,
    dev: DeviceId,
    mount: &std::path::Path,
) -> VolumeId {
    let identifiers = VolumeIdentifiers {
        gpt_partition_guid: None,
        fs_uuid: Some(format!("test-fs-{}", uuid::Uuid::now_v7())),
        label: Some("test-volume".into()),
        capacity_bytes: 1024 * 1024,
        is_removable: false,
    };
    let vol = vol_repo.find_or_create(&identifiers, dev).unwrap();
    vol_repo.record_mount(vol, dev, mount).unwrap();
    vol
}

/// Seed a file with bytes on disk + `files` row + active `file_locations` row.
/// Returns the `file_uuid` (read back via the RO connection).
fn seed_file(
    file_repo: &SqliteFileRepository,
    ro: &Connection,
    dev: DeviceId,
    vol: VolumeId,
    mount: &std::path::Path,
    rel_name: &str,
    content: &[u8],
    quick_hash: Option<BlakeHash>,
) -> (FileUuid, BlakeHash) {
    let abs_path = mount.join(rel_name);
    std::fs::write(&abs_path, content).unwrap();

    // WHY use Blake3Service to hash: avoids a direct `blake3` dev-dep in
    // `perima-app`. The on-disk file is freshly written so a `full_hash`
    // call against it yields the same value the test will assert later.
    let temp_for_hash = mount.join(format!(".tmp_{}", uuid::Uuid::now_v7()));
    std::fs::write(&temp_for_hash, content).unwrap();
    let hash = Blake3Service::new().full_hash(&temp_for_hash).unwrap();
    std::fs::remove_file(&temp_for_hash).ok();
    let hf = HashedFile {
        discovered: perima_core::DiscoveredFile {
            absolute_path: abs_path,
            relative_path: MediaPath::new(rel_name),
            size: FileSize(content.len() as u64),
        },
        hash,
    };

    file_repo
        .upsert_file_with_quick_hash(&hf, dev, quick_hash)
        .unwrap();
    file_repo
        .upsert_location(&hash, vol, &hf.discovered.relative_path, dev)
        .unwrap();

    let uuid_str: String = ro
        .query_row(
            "SELECT file_uuid FROM files WHERE blake3_hash = ?1",
            [hash.to_hex()],
            |row| row.get(0),
        )
        .unwrap();
    let uuid = FileUuid(uuid::Uuid::parse_str(&uuid_str).unwrap());
    (uuid, hash)
}

/// Read `files.blake3_hash` for a given `file_uuid`.
fn read_blake3_hash(ro: &Connection, file_uuid: FileUuid) -> String {
    ro.query_row(
        "SELECT blake3_hash FROM files WHERE file_uuid = ?1",
        [file_uuid.0.to_string()],
        |row| row.get(0),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compute_full_hash_single_populates_files_full_hash() {
    let (tmp, file_repo, vol_repo, ro) = harness();
    let dev = DeviceId::new();
    let mount = tmp.path().join("mount-a");
    std::fs::create_dir_all(&mount).unwrap();
    let vol = make_volume(&vol_repo, dev, &mount);

    // Seed a file. Pre-promote: blake3_hash is the actual file hash since
    // `upsert_file` uses the supplied hash. The test verifies the writer
    // path actually overwrites the column post-promote (round-trip identity
    // since the bytes match).
    let content = b"hello, dedup world";
    let (uuid, original_hash) = seed_file(
        &file_repo,
        &ro,
        dev,
        vol,
        &mount,
        "a.bin",
        content,
        Some(BlakeHash::from_bytes([0u8; 32])),
    );

    let bus: Arc<dyn EventBus> = Arc::new(NullBus);
    let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
    let files: Arc<dyn FileRepository> = file_repo.clone();
    let uc = ComputeFullHashUseCase::new(hasher, files, bus);

    let computed = uc.execute_single(uuid).await.expect("compute");
    assert_eq!(
        computed, original_hash,
        "compute_full_hash must return the BLAKE3 of the file bytes"
    );

    // Verify the writer landed the new hash on the row.
    let stored_hex = read_blake3_hash(&ro, uuid);
    assert_eq!(
        stored_hex,
        original_hash.to_hex(),
        "files.blake3_hash must be promoted to the freshly-computed value",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_quick_hash_collisions_groups_files_with_matching_quick_hash() {
    let (tmp, file_repo, vol_repo, ro) = harness();
    let dev = DeviceId::new();
    let mount = tmp.path().join("mount-b");
    std::fs::create_dir_all(&mount).unwrap();
    let vol = make_volume(&vol_repo, dev, &mount);

    // Two files share quick_hash QH1; one file has a unique quick_hash QH2.
    let qh_shared = BlakeHash::from_bytes([0xAA; 32]);
    let qh_unique = BlakeHash::from_bytes([0xBB; 32]);

    let (_uuid_a, _) = seed_file(
        &file_repo,
        &ro,
        dev,
        vol,
        &mount,
        "shared_a.bin",
        b"alpha bytes",
        Some(qh_shared),
    );
    let (_uuid_b, _) = seed_file(
        &file_repo,
        &ro,
        dev,
        vol,
        &mount,
        "shared_b.bin",
        b"beta bytes",
        Some(qh_shared),
    );
    let (_uuid_c, _) = seed_file(
        &file_repo,
        &ro,
        dev,
        vol,
        &mount,
        "lonely.bin",
        b"lonely bytes",
        Some(qh_unique),
    );

    let bus: Arc<dyn EventBus> = Arc::new(NullBus);
    let files: Arc<dyn FileRepository> = file_repo;
    let dedup = DedupUseCase::new(files, bus);

    let groups = dedup.list_collisions().expect("list_collisions");
    assert_eq!(groups.len(), 1, "exactly one collision group expected");
    let group = &groups[0];
    assert_eq!(group.quick_hash, qh_shared);
    assert_eq!(group.files.len(), 2, "shared group must contain both files");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mark_verified_distinct_excludes_files_from_subsequent_collision_lists() {
    let (tmp, file_repo, vol_repo, ro) = harness();
    let dev = DeviceId::new();
    let mount = tmp.path().join("mount-c");
    std::fs::create_dir_all(&mount).unwrap();
    let vol = make_volume(&vol_repo, dev, &mount);

    let qh_shared = BlakeHash::from_bytes([0xCC; 32]);
    let (uuid_a, _) = seed_file(
        &file_repo,
        &ro,
        dev,
        vol,
        &mount,
        "verify_a.bin",
        b"a contents",
        Some(qh_shared),
    );
    let (uuid_b, _) = seed_file(
        &file_repo,
        &ro,
        dev,
        vol,
        &mount,
        "verify_b.bin",
        b"b contents",
        Some(qh_shared),
    );

    let bus: Arc<dyn EventBus> = Arc::new(NullBus);
    let files: Arc<dyn FileRepository> = file_repo;
    let dedup = DedupUseCase::new(files, bus);

    // Sanity: pre-mark, the group surfaces.
    let pre = dedup.list_collisions().unwrap();
    assert_eq!(pre.len(), 1, "pre-mark: one collision group expected");

    // Mark both as verified-distinct.
    dedup
        .mark_verified_distinct(vec![uuid_a, uuid_b], dev)
        .expect("mark_verified_distinct");

    // Post-mark: no group surfaces (both rows have verified_distinct = 1, so
    // the GROUP BY filters them out).
    let post = dedup.list_collisions().unwrap();
    assert_eq!(
        post.len(),
        0,
        "post-mark: verified_distinct rows must not surface as collisions"
    );
}
