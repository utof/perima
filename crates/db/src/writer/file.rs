//! Writer-side handler for [`crate::cmd::FileWriteCmd`].
//!
//! Lifts the SQL bodies that previously lived inside
//! `impl FileRepository for SqliteFileRepository::{upsert_file,
//! upsert_location}` and the inherent methods
//! `SqliteFileRepository::{update_location_status, update_location_path,
//! migrate_sentinel_row}` (pre-Batch-C) into writer-owned functions. The
//! writer thread holds the sole writable [`rusqlite::Connection`]
//! (spec §3.1); the adapter on the caller-side is now a thin send →
//! recv shim (see `crates/db/src/file_repo.rs`).
//!
//! # HLC semantics
//!
//! Each command computes `let hlc = Hlc::now().pack();` ONCE at the top
//! of [`handle`] and binds the same packed value to every `files` /
//! `file_locations` row written by the command — one HLC value per
//! user-visible logical event (spec §3.7). Per V009:
//!
//! - `files.hlc` bumps on INSERT and on UPDATE in
//!   [`crate::cmd::FileWriteCmd::UpsertFile`].
//! - `file_locations.hlc` bumps on INSERT and on UPDATE in
//!   [`crate::cmd::FileWriteCmd::UpsertLocation`],
//!   [`crate::cmd::FileWriteCmd::UpdateLocationStatus`],
//!   [`crate::cmd::FileWriteCmd::UpdateLocationPath`], and
//!   [`crate::cmd::FileWriteCmd::MigrateSentinelRow`].
//! - The `Unchanged` arm in `UpsertFile` and `UpsertLocation` skips
//!   every write; no `hlc` is consumed and the prior value is preserved
//!   (same logical event did not fire).
//! - `UpsertLocation` collision path (soft-delete source row when
//!   destination already exists): binds `file_locations.hlc` on the
//!   soft-delete UPDATE.
//! - `volume_mounts` has no `hlc` column per V009 (device-local) — NOT
//!   touched by this module.
//!
//! # Events
//!
//! [`perima_core::FileEvent`] has `Created / Modified / Deleted / Renamed`
//! only. Batch C does NOT change who emits `FileEvent` — filesystem-watch
//! emission stays shell-local (`DbEventHandler` in `crates/cli` +
//! `crates/desktop`). All six writer handlers pass the bus through unused.
//!
//! WHY defer: `update_location_status` / `update_location_path` /
//! `migrate_sentinel_row` are called FROM `DbEventHandler` which already
//! sits INSIDE the `FileEvent` fan-out. Emitting `FileEvent` from inside
//! the writer would cause a double-fire and break the single-source-of-
//! truth invariant. Watch-as-UseCase (#120) resolves this in a future
//! batch; until then the writer handlers return without emitting.

use std::sync::Arc;

use perima_core::{
    BlakeHash, CoreError, DeviceId, EventBus, HashedFile, Hlc, LocationStatus, MediaPath,
    UpsertOutcome, VolumeId,
};
use rusqlite::{Connection, OptionalExtension};

use crate::cmd::FileWriteCmd;
use crate::errors::Error;

/// Writer-side dispatch for [`FileWriteCmd`]. Consumes the command
/// (the reply channel lives inside each variant) and sends the result
/// back on the caller's reply channel.
///
/// WHY `_bus` unused: all file-related events originate from the
/// filesystem watcher (`crates/fs`), not from the writer — calling
/// `bus.emit` here would double-fire events already emitted by
/// `DbEventHandler`. Keeping the parameter in the signature makes the
/// Batch-E addition of fine-grained `AppEvent::File*` variants a
/// single-file change in this module. See WHY-defer comment in the
/// module doc above.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn handle(conn: &mut Connection, cmd: FileWriteCmd, _bus: &Arc<dyn EventBus>) {
    // WHY one HLC per command (not per row): the "one HLC per
    // user-visible logical event" invariant from spec §3.7. A single
    // upsert_file may INSERT a new row OR UPDATE an existing one; both
    // paths bind the same `hlc` value. The `Unchanged` arm skips
    // every write — no `hlc` is consumed.
    let hlc = Hlc::now().pack();

    match cmd {
        FileWriteCmd::UpsertFile {
            file,
            device,
            reply,
        } => {
            let out = upsert_file_impl(conn, &file, device, hlc);
            if reply.send(out).is_err() {
                // WHY debug (not warn): caller dropped its reply
                // handle — e.g. CLI aborted mid-command. The write
                // already committed; nothing actionable.
                tracing::debug!("file upsert_file reply channel closed before send");
            }
        }
        FileWriteCmd::UpsertLocation {
            hash,
            volume,
            path,
            device,
            reply,
        } => {
            let out = upsert_location_impl(conn, &hash, volume, &path, device, hlc);
            if reply.send(out).is_err() {
                tracing::debug!("file upsert_location reply channel closed before send");
            }
        }
        FileWriteCmd::UpdateLocationStatus {
            volume,
            path,
            status,
            device,
            reply,
        } => {
            let out = update_location_status_impl(conn, volume, &path, status, device, hlc);
            if reply.send(out).is_err() {
                tracing::debug!("file update_location_status reply channel closed before send");
            }
        }
        FileWriteCmd::UpdateLocationPath {
            volume,
            old_path,
            new_path,
            device,
            reply,
        } => {
            let out = update_location_path_impl(conn, volume, &old_path, &new_path, device, hlc);
            if reply.send(out).is_err() {
                tracing::debug!("file update_location_path reply channel closed before send");
            }
        }
        FileWriteCmd::MigrateSentinelRow {
            path,
            real_volume,
            device,
            reply,
        } => {
            let out = migrate_sentinel_row_impl(conn, &path, real_volume, device, hlc);
            if reply.send(out).is_err() {
                tracing::debug!("file migrate_sentinel_row reply channel closed before send");
            }
        }
    }
}

/// ISO-8601 UTC timestamp used for `updated_at` / `first_seen` /
/// `deleted_at`. Matches the pre-Batch-C adapter's `now_iso` helper.
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Convert `FileSize` (`u64`) to the `i64` that `SQLite` stores.
///
/// WHY: `SQLite` integers are signed 64-bit. A file larger than `i64::MAX`
/// (~8 EiB) cannot exist on current hardware; we propagate as `Internal`
/// rather than silently wrapping.
fn size_to_i64(size: perima_core::FileSize) -> Result<i64, CoreError> {
    i64::try_from(size.0)
        .map_err(|_| CoreError::Internal(format!("file size {} overflows i64", size.0)))
}

/// Convert a `LocationStatus` to its DB string representation.
///
/// WHY: status values are stored as lowercase strings so they are
/// human-readable in `SQLite` tooling and stable across future Rust
/// refactors. The deserializer in `list_file_locations` (read path) mirrors it.
const fn status_to_str(status: LocationStatus) -> &'static str {
    match status {
        LocationStatus::Active => "active",
        LocationStatus::Missing => "missing",
        LocationStatus::Moved => "moved",
        LocationStatus::Stale => "stale",
    }
}

/// Writer-side body for [`FileWriteCmd::UpsertFile`]. Lifted verbatim
/// from the pre-Batch-C `SqliteFileRepository::upsert_file` impl with
/// `hlc = ?` bound on the INSERT and UPDATE paths.
///
/// Returns `UpsertOutcome::Inserted` / `Updated` / `Unchanged`.
/// `Unchanged` skips all writes — `hlc` is not rebound (prior value
/// preserved, per spec §3.7).
fn upsert_file_impl(
    conn: &mut Connection,
    file: &HashedFile,
    device: DeviceId,
    hlc: i64,
) -> Result<UpsertOutcome, CoreError> {
    // WHY BEGIN IMMEDIATE: historical rationale (pre-Batch-C) guarded
    // against two adapter handles racing on SELECT-then-INSERT. The
    // single writer actor now guarantees serialization, BUT retaining
    // IMMEDIATE is cheap and documents the "only one write tx at a
    // time" expectation at the SQL boundary.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let hash_hex = file.hash.to_hex();
    let now = now_iso();
    let dev_str = device.0.to_string();
    let size_i64 = size_to_i64(file.discovered.size)?;

    // WHY two-statement SELECT-then-INSERT/UPDATE: `SQLite`'s `changes()`
    // cannot distinguish a fresh INSERT from a conflict-triggered UPDATE
    // — both report 1. The SELECT lets us classify Inserted / Updated /
    // Unchanged precisely.
    let existing: Option<(i64, String)> = tx
        .query_row(
            "SELECT file_size, device_id FROM files WHERE blake3_hash = ?1",
            [&hash_hex],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Error::from)?;

    let outcome = match existing {
        None => {
            tx.execute(
                "INSERT INTO files
                 (blake3_hash, file_size, first_seen, updated_at, device_id, hlc)
                 VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
                rusqlite::params![hash_hex, size_i64, now, dev_str, hlc],
            )
            .map_err(Error::from)?;
            UpsertOutcome::Inserted
        }
        Some((existing_size, ref existing_dev))
            if existing_size == size_i64 && *existing_dev == dev_str =>
        {
            // WHY no hlc write on Unchanged: same logical event did
            // not fire — preserving the prior hlc matches the tag /
            // metadata upsert semantics (spec §3.7).
            UpsertOutcome::Unchanged
        }
        Some(_) => {
            tx.execute(
                "UPDATE files
                 SET file_size = ?1, updated_at = ?2, device_id = ?3, hlc = ?4
                 WHERE blake3_hash = ?5",
                rusqlite::params![size_i64, now, dev_str, hlc, hash_hex],
            )
            .map_err(Error::from)?;
            UpsertOutcome::Updated
        }
    };

    tx.commit().map_err(Error::from)?;
    Ok(outcome)
}

/// Writer-side body for [`FileWriteCmd::UpsertLocation`]. Lifted verbatim
/// from the pre-Batch-C `SqliteFileRepository::upsert_location` impl with
/// `hlc = ?` bound on the INSERT and UPDATE paths.
///
/// The collision path (active row already exists at `new_path` when
/// trying to INSERT) soft-deletes the existing row — `hlc` IS bound on
/// that soft-delete UPDATE, since the soft-delete is a distinct logical
/// write event.
fn upsert_location_impl(
    conn: &mut Connection,
    hash: &BlakeHash,
    volume: VolumeId,
    path: &MediaPath,
    device: DeviceId,
    hlc: i64,
) -> Result<UpsertOutcome, CoreError> {
    let hash_hex = hash.to_hex();
    let vol_str = volume.0.to_string();
    let path_str = path.as_str();
    let dev_str = device.0.to_string();
    let now = now_iso();

    // WHY BEGIN IMMEDIATE: the SELECT-then-INSERT/UPDATE sequence must
    // serialize across callers. The single writer actor already serializes
    // commands, but IMMEDIATE is cheap and documents the intent.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    // WHY app-level uniqueness on (volume_id, relative_path,
    // deleted_at IS NULL) replaces a UNIQUE constraint that
    // CLAUDE.md forbids on mutable columns. Safe under IMMEDIATE.
    let existing: Option<(String, String, String)> = tx
        .query_row(
            "SELECT id, blake3_hash, device_id FROM file_locations
             WHERE volume_id = ?1 AND relative_path = ?2 AND deleted_at IS NULL",
            rusqlite::params![vol_str, path_str],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(Error::from)?;

    let outcome = match existing {
        None => {
            let id = perima_core::ids::new_id().to_string();
            tx.execute(
                "INSERT INTO file_locations
                 (id, blake3_hash, volume_id, relative_path, status,
                  first_seen, updated_at, device_id, hlc)
                 VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, ?6, ?7)",
                rusqlite::params![id, hash_hex, vol_str, path_str, now, dev_str, hlc],
            )
            .map_err(Error::from)?;
            UpsertOutcome::Inserted
        }
        Some((_, ref existing_hash, ref existing_dev))
            if *existing_hash == hash_hex && *existing_dev == dev_str =>
        {
            // WHY no hlc write on Unchanged: same logical event did
            // not fire — preserving the prior hlc matches all other
            // upsert Unchanged semantics (spec §3.7).
            UpsertOutcome::Unchanged
        }
        Some((ref row_id, _, _)) => {
            tx.execute(
                "UPDATE file_locations
                 SET blake3_hash = ?1, updated_at = ?2, device_id = ?3, hlc = ?4
                 WHERE id = ?5",
                rusqlite::params![hash_hex, now, dev_str, hlc, row_id],
            )
            .map_err(Error::from)?;
            UpsertOutcome::Updated
        }
    };

    tx.commit().map_err(Error::from)?;
    Ok(outcome)
}

/// Writer-side body for [`FileWriteCmd::UpdateLocationStatus`]. Lifted
/// verbatim from the pre-Batch-C `SqliteFileRepository::update_location_status`
/// impl with `hlc = ?` bound on the UPDATE.
///
/// Returns the number of rows updated (0 if no matching row exists, 1 on success).
fn update_location_status_impl(
    conn: &mut Connection,
    volume: VolumeId,
    path: &MediaPath,
    status: LocationStatus,
    device: DeviceId,
    hlc: i64,
) -> Result<u64, CoreError> {
    // WHY BEGIN IMMEDIATE: a pure UPDATE (no preceding SELECT), but
    // IMMEDIATE avoids a write-lock upgrade race under WAL. Consistent
    // with all other write paths in this module.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let vol_str = volume.0.to_string();
    let path_str = path.as_str();
    let status_str = status_to_str(status);
    let dev_str = device.0.to_string();
    let now = now_iso();
    let n = tx
        .execute(
            "UPDATE file_locations
             SET status = ?1, updated_at = ?2, device_id = ?3, hlc = ?4
             WHERE volume_id = ?5 AND relative_path = ?6 AND deleted_at IS NULL",
            rusqlite::params![status_str, now, dev_str, hlc, vol_str, path_str],
        )
        .map_err(Error::from)?;

    tx.commit().map_err(Error::from)?;

    // WHY: at most 1 active row per (volume, path) by app-level invariant.
    u64::try_from(n).map_err(|_| CoreError::Internal(format!("rows_changed {n} is negative")))
}

/// Writer-side body for [`FileWriteCmd::UpdateLocationPath`]. Lifted verbatim
/// from the pre-Batch-C `SqliteFileRepository::update_location_path` impl with
/// `hlc = ?` bound on both branches.
///
/// If an active row already exists at `new_path`, the source row is
/// soft-deleted and the destination row is left untouched — `hlc` IS
/// bound on the soft-delete UPDATE. If no collision: the path UPDATE
/// binds `hlc`.
///
/// Returns the number of rows written (0 if no source row exists, 1 otherwise).
fn update_location_path_impl(
    conn: &mut Connection,
    volume: VolumeId,
    old_path: &MediaPath,
    new_path: &MediaPath,
    device: DeviceId,
    hlc: i64,
) -> Result<u64, CoreError> {
    let vol_str = volume.0.to_string();
    let old_str = old_path.as_str();
    let new_str = new_path.as_str();
    let dev_str = device.0.to_string();
    let now = now_iso();

    // WHY BEGIN IMMEDIATE: the collision check + UPDATE/soft-delete
    // sequence must serialize. The single writer actor already serializes
    // commands, but IMMEDIATE is cheap and documents the intent.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    // Check whether an active row already exists at `new_path`.
    let collision: Option<String> = tx
        .query_row(
            "SELECT id FROM file_locations
             WHERE volume_id = ?1 AND relative_path = ?2 AND deleted_at IS NULL",
            rusqlite::params![vol_str, new_str],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)?;

    let n = if collision.is_some() {
        // WHY: destination wins. Soft-delete the source row so the
        // invariant "1 active row per (vol, path)" holds. CRDT-friendly:
        // no hard delete, deleted_at/updated_at/device_id/hlc all stamped.
        tx.execute(
            "UPDATE file_locations
             SET deleted_at = ?1, updated_at = ?1, device_id = ?2, hlc = ?3
             WHERE volume_id = ?4 AND relative_path = ?5 AND deleted_at IS NULL",
            rusqlite::params![now, dev_str, hlc, vol_str, old_str],
        )
        .map_err(Error::from)?
    } else {
        tx.execute(
            "UPDATE file_locations
             SET relative_path = ?1, status = 'active', updated_at = ?2,
                 device_id = ?3, hlc = ?4
             WHERE volume_id = ?5 AND relative_path = ?6 AND deleted_at IS NULL",
            rusqlite::params![new_str, now, dev_str, hlc, vol_str, old_str],
        )
        .map_err(Error::from)?
    };

    tx.commit().map_err(Error::from)?;

    // WHY: at most 1 active row per (volume, path) by app-level invariant,
    // so `n` is always 0 or 1.
    u64::try_from(n).map_err(|_| CoreError::Internal(format!("rows_changed {n} is negative")))
}

/// Writer-side body for [`FileWriteCmd::MigrateSentinelRow`]. Lifted verbatim
/// from the pre-Batch-C `SqliteFileRepository::migrate_sentinel_row` impl with
/// `hlc = ?` bound on the UPDATE.
///
/// WHY: scan in phase 1b wrote every `file_locations` row with
/// `volume_id = '00000000-0000-0000-0000-000000000000'` (the nil UUID).
/// Phase 1c resolves the real volume for each scan root. Rather than
/// a bulk UPDATE, we update one row at a time — scoped by `(relative_path,
/// sentinel volume_id, deleted_at IS NULL)` — immediately after the live
/// upsert confirms the path still exists on disk.
///
/// Returns the number of rows updated (0 if no sentinel row existed).
fn migrate_sentinel_row_impl(
    conn: &mut Connection,
    path: &MediaPath,
    real_volume: VolumeId,
    device: DeviceId,
    hlc: i64,
) -> Result<u64, CoreError> {
    // WHY BEGIN IMMEDIATE: pure UPDATE but IMMEDIATE avoids write-lock
    // upgrade race. Consistent with all other write paths in this module.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let now = now_iso();
    let vol_str = real_volume.0.to_string();
    let dev_str = device.0.to_string();
    let path_str = path.as_str();
    // WHY: nil UUID string literal is hard-coded here because this method
    // is the *only* place we intentionally touch sentinel rows. Using a
    // constant avoids importing VolumeId into a string constant but keeps
    // the magic value visible and auditable.
    let n = tx
        .execute(
            "UPDATE file_locations
             SET volume_id = ?1, updated_at = ?2, device_id = ?3, hlc = ?4
             WHERE volume_id = '00000000-0000-0000-0000-000000000000'
               AND relative_path = ?5 AND deleted_at IS NULL",
            rusqlite::params![vol_str, now, dev_str, hlc, path_str],
        )
        .map_err(Error::from)?;

    tx.commit().map_err(Error::from)?;

    // WHY: schema guarantees at most 1 sentinel row per path, so n is 0 or 1.
    u64::try_from(n).map_err(|_| CoreError::Internal(format!("rows_changed {n} is negative")))
}
