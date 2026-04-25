//! Writer-side handler for [`crate::cmd::CacheWriteCmd`].
//!
//! `file_identity_cache` is device-local: it caches per-device filesystem
//! metadata (inode, mtime, size) alongside a `quick_hash` fingerprint so
//! the scan loop can skip rehashing unchanged files.
//!
//! # HLC semantics
//!
//! Cache rows are **device-local** per CLAUDE.md "Schema rules expansion":
//! they carry no `hlc` column and are never synced. The handler does NOT
//! generate or bind any `hlc` value.
//!
//! # Events
//!
//! Cache writes do NOT emit [`perima_core::AppEvent::IndexInvalidated`].
//! The search/files/tags indexes are not stale after a cache write; emitting
//! would cause spurious frontend refreshes. Handler returns empty `Vec`.
//!
//! # Upsert strategy
//!
//! The lookup index `idx_fic_lookup` is non-unique (CLAUDE.md forbids
//! UNIQUE on mutable columns; `mtime_ns` is mutable). `INSERT … ON CONFLICT
//! DO UPDATE` cannot target a non-unique index. Instead, the handler runs a
//! single `BEGIN IMMEDIATE` transaction with a SELECT-then-INSERT-or-UPDATE.

use std::sync::Arc;

use perima_core::{BlakeHash, CacheEntry, CacheKey, CoreError, EventBus};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

use crate::cmd::CacheWriteCmd;
use crate::errors::Error;

/// Writer-side dispatch for [`CacheWriteCmd`]. Consumes the command
/// (reply channel lives inside each variant) and sends the result back.
///
/// Cache writes emit NO events — see module-level doc.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn handle(conn: &mut Connection, cmd: CacheWriteCmd, _bus: &Arc<dyn EventBus>) {
    match cmd {
        CacheWriteCmd::UpsertCacheRow { key, entry, reply } => {
            let out = upsert_cache_impl(conn, &key, &entry);
            if reply.send(out).is_err() {
                tracing::debug!("cache upsert reply channel closed before send");
            }
        }
        CacheWriteCmd::SoftDeleteCacheRow { key, reply } => {
            let out = soft_delete_cache_impl(conn, &key);
            if reply.send(out).is_err() {
                tracing::debug!("cache soft_delete reply channel closed before send");
            }
        }
    }
}

/// ISO-8601 UTC timestamp.
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Convert `u64` to `i64` for `SQLite` storage.
///
/// WHY: `SQLite` integers are signed 64-bit. Values > `i64::MAX` (~9.2 EiB for
/// `size_bytes`, ~292 years for `mtime_ns` in seconds) are implausible on
/// current hardware; we propagate as `Internal` rather than silently wrapping.
fn u64_to_i64(v: u64, field: &'static str) -> Result<i64, CoreError> {
    i64::try_from(v).map_err(|_| CoreError::Internal(format!("{field} {v} overflows i64")))
}

/// Writer-side body for [`CacheWriteCmd::UpsertCacheRow`].
///
/// Runs a single `BEGIN IMMEDIATE` transaction:
/// 1. SELECT existing live row by lookup tuple.
/// 2. If none: INSERT fresh UUIDv7-PKd row.
/// 3. If found: UPDATE `quick_hash` / `full_hash` / `last_verified` /
///    `updated_at`.
///
/// WHY BEGIN IMMEDIATE: the SELECT-then-INSERT/UPDATE must serialize;
/// the single writer actor already serializes commands, but IMMEDIATE is
/// cheap and documents the intent.
fn upsert_cache_impl(
    conn: &mut Connection,
    key: &CacheKey,
    entry: &CacheEntry,
) -> Result<(), CoreError> {
    let dev_str = key.device_id.0.to_string();
    let vol_str = key.volume_id.0.to_string();
    let fs_file_id = u64_to_i64(key.fs_file_id, "fs_file_id")?;
    let size_bytes = u64_to_i64(key.size_bytes, "size_bytes")?;
    // WHY: mtime_ns is already i64 (nanoseconds since epoch, can be negative
    // for pre-epoch timestamps on some platforms).
    let mtime_ns = key.mtime_ns;
    let quick_hash_hex = entry.quick_hash.to_hex();
    let full_hash_hex: Option<String> = entry.full_hash.as_ref().map(BlakeHash::to_hex);
    let now = now_iso();

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    // WHY filter deleted_at IS NULL: a soft-deleted row is no longer live.
    // A new entry must be inserted fresh (separate identity).
    let existing_id: Option<String> = tx
        .query_row(
            "SELECT id FROM file_identity_cache
             WHERE device_id = ?1
               AND volume_id = ?2
               AND fs_file_id = ?3
               AND size_bytes = ?4
               AND mtime_ns = ?5
               AND deleted_at IS NULL",
            rusqlite::params![dev_str, vol_str, fs_file_id, size_bytes, mtime_ns],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)?;

    match existing_id {
        None => {
            // Insert fresh row. UUIDv7 PK per CLAUDE.md "Schema rules".
            // WHY hyphenated: matches Uuid::now_v7().to_string() used by
            // all other UUIDv7 PKs in this schema (V011 comment rationale).
            let id = Uuid::now_v7().to_string();
            tx.execute(
                "INSERT INTO file_identity_cache
                 (id, device_id, volume_id, fs_file_id, size_bytes, mtime_ns,
                  quick_hash, full_hash, last_verified, first_seen, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?9)",
                rusqlite::params![
                    id,
                    dev_str,
                    vol_str,
                    fs_file_id,
                    size_bytes,
                    mtime_ns,
                    quick_hash_hex,
                    full_hash_hex,
                    now
                ],
            )
            .map_err(Error::from)?;
        }
        Some(ref row_id) => {
            // Update existing row.
            tx.execute(
                "UPDATE file_identity_cache
                 SET quick_hash = ?1, full_hash = ?2,
                     last_verified = ?3, updated_at = ?3
                 WHERE id = ?4",
                rusqlite::params![quick_hash_hex, full_hash_hex, now, row_id],
            )
            .map_err(Error::from)?;
        }
    }

    tx.commit().map_err(Error::from)?;
    Ok(())
}

/// Writer-side body for [`CacheWriteCmd::SoftDeleteCacheRow`].
///
/// Sets `deleted_at = NOW`, `updated_at = NOW` on the live row matching the
/// full lookup tuple. If no live row exists, returns `Ok(())` (idempotent).
fn soft_delete_cache_impl(conn: &mut Connection, key: &CacheKey) -> Result<(), CoreError> {
    let dev_str = key.device_id.0.to_string();
    let vol_str = key.volume_id.0.to_string();
    let fs_file_id = u64_to_i64(key.fs_file_id, "fs_file_id")?;
    let size_bytes = u64_to_i64(key.size_bytes, "size_bytes")?;
    let mtime_ns = key.mtime_ns;
    let now = now_iso();

    // WHY BEGIN IMMEDIATE: pure UPDATE but IMMEDIATE avoids write-lock upgrade
    // race under WAL. Consistent with all other write paths in this module.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    tx.execute(
        "UPDATE file_identity_cache
         SET deleted_at = ?1, updated_at = ?1
         WHERE device_id = ?2
           AND volume_id = ?3
           AND fs_file_id = ?4
           AND size_bytes = ?5
           AND mtime_ns = ?6
           AND deleted_at IS NULL",
        rusqlite::params![now, dev_str, vol_str, fs_file_id, size_bytes, mtime_ns],
    )
    .map_err(Error::from)?;

    tx.commit().map_err(Error::from)?;
    Ok(())
}
