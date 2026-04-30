//! Tier-0 cache lookup + upsert round-trip.
//!
//! WHY this is integration not unit: round-trip tests the writer-actor
//! channel (Batch C) — sends `UpsertCacheRow`, awaits reply, then opens RO
//! and reads back via `SqliteIdentityCacheRepository::lookup` to confirm.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

mod common;

use std::sync::Arc;

use perima_core::{BlakeHash, CacheEntry, CacheKey, DeviceId, IdentityCacheRepository, VolumeId};
use perima_db::{SqliteIdentityCacheRepository, SqliteWriter, pool::ReadPool, test_utils::NoopBus};

#[test]
fn cache_upsert_then_lookup_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("perima.db");
    let writer = SqliteWriter::start(&db_path, Arc::new(NoopBus)).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteIdentityCacheRepository::new(writer.sender(), reads);

    let key = CacheKey {
        device_id: DeviceId(uuid::Uuid::nil()),
        volume_id: VolumeId(uuid::Uuid::nil()),
        fs_file_id: 12345,
        size_bytes: 1024,
        mtime_ns: 1_700_000_000_000_000_000,
    };
    let entry = CacheEntry {
        quick_hash: BlakeHash::from_bytes([0xabu8; 32]),
        full_hash: None,
    };

    repo.upsert(&key, &entry).unwrap();

    let got = repo.lookup(&key).unwrap();
    assert!(got.is_some(), "lookup must hit after upsert");
    let got = got.unwrap();
    assert_eq!(got.quick_hash.as_bytes(), entry.quick_hash.as_bytes());
    assert!(got.full_hash.is_none());
}

#[test]
fn cache_soft_delete_then_lookup_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("perima.db");
    let writer = SqliteWriter::start(&db_path, Arc::new(NoopBus)).unwrap();
    let reads = ReadPool::open(&db_path).unwrap();
    let repo = SqliteIdentityCacheRepository::new(writer.sender(), reads);

    let key = CacheKey {
        device_id: DeviceId(uuid::Uuid::nil()),
        volume_id: VolumeId(uuid::Uuid::nil()),
        fs_file_id: 99,
        size_bytes: 0,
        mtime_ns: 0,
    };
    let entry = CacheEntry {
        quick_hash: BlakeHash::from_bytes([0u8; 32]),
        full_hash: None,
    };

    repo.upsert(&key, &entry).unwrap();
    assert!(repo.lookup(&key).unwrap().is_some());

    repo.soft_delete(&key).unwrap();
    assert!(
        repo.lookup(&key).unwrap().is_none(),
        "lookup must NOT return soft-deleted rows (deleted_at IS NOT NULL)"
    );
}
