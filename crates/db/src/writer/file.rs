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
//!   [`crate::cmd::FileWriteCmd::UpdateLocationStatuses`],
//!   [`crate::cmd::FileWriteCmd::SoftDeleteMissingLocations`],
//!   [`crate::cmd::FileWriteCmd::UpdateLocationPath`], and
//!   [`crate::cmd::FileWriteCmd::MigrateSentinelRow`].
//! - `UpdateLocationStatuses` binds ONE `hlc` across the whole batch:
//!   a verify sweep is a single logical event ("the catalogue was
//!   reconciled at time T"), not N independent ones, and every row it
//!   touches observed the same filesystem state.
//! - The `Unchanged` arm in `UpsertFile` and `UpsertLocation` skips
//!   every write; no `hlc` is consumed and the prior value is preserved
//!   (same logical event did not fire).
//! - `UpdateLocationPath` collision path (soft-delete source row when
//!   destination already exists): binds `file_locations.hlc` on the
//!   soft-delete UPDATE.
//! - `volume_mounts` has no `hlc` column per V009 (device-local) — NOT
//!   touched by this module.
//!
//! # Events
//!
//! After a successful COMMIT on any file-shaped write that actually
//! changed state, the writer emits
//! [`perima_core::AppEvent::IndexInvalidated`] with
//! [`perima_core::InvalidationReason::FilesChanged`] — the coarse v1
//! signal that file-shaped query indexes (file grid, location list,
//! status filters) are stale.
//!
//! `FileEvent` (filesystem-watcher events: Created / Modified /
//! Deleted / Renamed) is a SEPARATE concern emitted by the watcher
//! (now wrapped as `AppEvent::File`). The writer does NOT re-emit
//! `FileEvent` — that would double-fire on every watcher-triggered
//! status flip. The `IndexInvalidated::FilesChanged` signal is the
//! cache-invalidation hint for query-state, not a re-broadcast of
//! the underlying filesystem change.
//!
//! WHY emit on every variant: every `FileWriteCmd` variant mutates
//! `files` or `file_locations` — both are read by the frontend file
//! grid + location list. Coarse invalidation is the v1 contract;
//! per-row surgical invalidation is a Batch H decision.

use std::sync::Arc;

use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, EventBus, FileUuid, HashedFile, Hlc,
    InvalidationReason, LocationStatus, LocationStatusUpdate, MediaPath, UpsertOutcome, VolumeId,
};
use rusqlite::{Connection, OptionalExtension};

use crate::cmd::FileWriteCmd;
use crate::errors::Error;

/// Writer-side dispatch for [`FileWriteCmd`]. Consumes the command
/// (the reply channel lives inside each variant) and sends the result
/// back on the caller's reply channel.
///
/// After successful writes that actually change state (i.e. did not
/// land on the `Unchanged` arm of an upsert and did not no-op a path/
/// status update), this fn emits [`AppEvent::IndexInvalidated`] with
/// [`InvalidationReason::FilesChanged`] AFTER the COMMIT — see spec
/// §3.3.
#[allow(clippy::needless_pass_by_value)]
// WHY allow cognitive_complexity: the body is a single five-arm
// `match`; each arm is one impl call + a small emit + a reply send.
// The repetition lives in the data shape (5 sub-variants), not in
// per-arm logic. Splitting into per-arm helpers either pushes every
// helper past `clippy::too_many_arguments` (8 params: conn, payload
// fields, hlc, bus, reply) or buys cleanliness via reference-passing
// `reply` in a way that obscures the consume-on-send semantics.
#[allow(clippy::cognitive_complexity)]
// WHY allow too_many_lines: same "single match over N variants" rationale as
// cognitive_complexity above. Splitting the variants into helpers either
// inflates parameter counts past `too_many_arguments` or sacrifices the
// consume-on-send reply-channel semantics. The arms grow strictly with the
// number of `FileWriteCmd` variants (now 9, after the verify/prune slice
// added `UpdateLocationStatuses` + `SoftDeleteMissingLocations`).
#[allow(clippy::too_many_lines)]
pub(super) fn handle(conn: &mut Connection, cmd: FileWriteCmd, bus: &Arc<dyn EventBus>) {
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
            quick_hash,
            reply,
        } => {
            let out = upsert_file_impl(conn, &file, device, hlc, quick_hash.as_ref());
            // WHY emit gated on Inserted | Updated: the `Unchanged`
            // arm writes zero rows and does not bump hlc — not a
            // logical event per spec §3.3.
            if matches!(&out, Ok(o) if !matches!(o, UpsertOutcome::Unchanged)) {
                emit_files_changed(bus, "upsert_file");
            }
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
            if matches!(&out, Ok(o) if !matches!(o, UpsertOutcome::Unchanged)) {
                emit_files_changed(bus, "upsert_location");
            }
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
            // WHY emit gated on rows > 0: a status update against a
            // non-existent (volume, path) pair writes zero rows.
            if matches!(&out, Ok(rows) if *rows > 0) {
                emit_files_changed(bus, "update_location_status");
            }
            if reply.send(out).is_err() {
                tracing::debug!("file update_location_status reply channel closed before send");
            }
        }
        FileWriteCmd::UpdateLocationStatuses {
            updates,
            device,
            reply,
        } => {
            let out = update_location_statuses_impl(conn, &updates, device, hlc);
            // WHY emit gated on rows > 0: a sweep that finds nothing
            // changed writes zero rows and must not invalidate every
            // file-shaped query index for no reason.
            if matches!(&out, Ok(rows) if *rows > 0) {
                emit_files_changed(bus, "update_location_statuses");
            }
            if reply.send(out).is_err() {
                tracing::debug!("file update_location_statuses reply channel closed before send");
            }
        }
        FileWriteCmd::SoftDeleteMissingLocations { device, reply } => {
            let out = soft_delete_missing_locations_impl(conn, device, hlc);
            if matches!(&out, Ok(rows) if *rows > 0) {
                emit_files_changed(bus, "soft_delete_missing_locations");
            }
            if reply.send(out).is_err() {
                tracing::debug!(
                    "file soft_delete_missing_locations reply channel closed before send"
                );
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
            if matches!(&out, Ok(rows) if *rows > 0) {
                emit_files_changed(bus, "update_location_path");
            }
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
            if matches!(&out, Ok(rows) if *rows > 0) {
                emit_files_changed(bus, "migrate_sentinel_row");
            }
            if reply.send(out).is_err() {
                tracing::debug!("file migrate_sentinel_row reply channel closed before send");
            }
        }
        FileWriteCmd::PromoteFullHash {
            file_uuid,
            full_hash,
            device,
            reply,
        } => {
            let out = promote_full_hash_impl(conn, file_uuid, &full_hash, device, hlc);
            if matches!(&out, Ok(rows) if *rows > 0) {
                emit_collisions_changed(bus, "promote_full_hash");
            }
            if reply.send(out).is_err() {
                tracing::debug!("file promote_full_hash reply channel closed before send");
            }
        }
        FileWriteCmd::MarkVerifiedDistinct {
            file_uuids,
            device,
            reply,
        } => {
            let out = mark_verified_distinct_impl(conn, &file_uuids, device, hlc);
            if matches!(&out, Ok(rows) if *rows > 0) {
                emit_collisions_changed(bus, "mark_verified_distinct");
            }
            if reply.send(out).is_err() {
                tracing::debug!("file mark_verified_distinct reply channel closed before send");
            }
        }
    }
}

/// Emit `IndexInvalidated::FilesChanged` and log on emit failure.
///
/// `who` identifies the calling sub-command for log scoping. Failed
/// emits log-only — the COMMIT already landed, the caller already got
/// (or is about to get) its reply, and other handlers should still fire.
fn emit_files_changed(bus: &Arc<dyn EventBus>, who: &'static str) {
    if let Err(e) = bus.emit(&AppEvent::IndexInvalidated {
        reason: InvalidationReason::FilesChanged,
    }) {
        tracing::warn!(?e, who, "post-commit emit failed: FilesChanged");
    }
}

/// Emit `IndexInvalidated::CollisionsChanged` and log on emit failure.
///
/// WHY a separate emitter: dedup writes (`PromoteFullHash`,
/// `MarkVerifiedDistinct`) change the *answer* `list_quick_hash_collisions`
/// returns, not the underlying file grid. The frontend's `useDomainEvents`
/// hook routes `CollisionsChanged` to the dedup query keys without
/// invalidating the entire file list.
fn emit_collisions_changed(bus: &Arc<dyn EventBus>, who: &'static str) {
    if let Err(e) = bus.emit(&AppEvent::IndexInvalidated {
        reason: InvalidationReason::CollisionsChanged,
    }) {
        tracing::warn!(?e, who, "post-commit emit failed: CollisionsChanged");
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
    quick_hash: Option<&BlakeHash>,
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
    let quick_hex: Option<String> = quick_hash.map(BlakeHash::to_hex);

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
            // WHY file_uuid here: post-V011 + Task 3 trigger pivot,
            // FTS5 triggers join on file_uuid (spec §4.1.4). Every new
            // `files` row needs a fresh UUIDv7 surrogate identity so
            // dependent tables (file_locations, file_metadata, file_tags,
            // search_content) can join back via subquery lookup. Stable
            // across blake3_hash changes (lazy full_hash workflow).
            //
            // WHY quick_hash column populated here (spec §4.1.1): new rows
            // carry the cheap prefix+suffix fingerprint so
            // `list_quick_hash_collisions` (Task 9) can find candidates
            // immediately without waiting for the Task 8 backfill worker.
            let file_uuid = perima_core::ids::new_id().to_string();
            tx.execute(
                "INSERT INTO files
                 (blake3_hash, file_uuid, file_size, quick_hash, first_seen, updated_at, device_id, hlc)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)",
                rusqlite::params![hash_hex, file_uuid, size_i64, quick_hex, now, dev_str, hlc],
            )
            .map_err(Error::from)?;
            UpsertOutcome::Inserted
        }
        Some((existing_size, ref existing_dev))
            if existing_size == size_i64 && *existing_dev == dev_str =>
        {
            // WHY quick_hash fill-in even on Unchanged: the backfill
            // worker (Task 8) calls `upsert_file_with_quick_hash` on
            // rows whose size and device haven't changed; without this
            // targeted UPDATE the `Unchanged` arm would skip the write
            // and leave `quick_hash` NULL forever. We only write when
            // `quick_hex` is `Some` AND the stored value is already NULL
            // (COALESCE semantics preserved: non-NULL stored value wins).
            if let Some(ref qh_hex) = quick_hex {
                tx.execute(
                    "UPDATE files SET quick_hash = ?1 WHERE blake3_hash = ?2 AND quick_hash IS NULL",
                    rusqlite::params![qh_hex, hash_hex],
                )
                .map_err(Error::from)?;
            }
            // WHY no hlc write on Unchanged: same logical event did
            // not fire — preserving the prior hlc matches the tag /
            // metadata upsert semantics (spec §3.7).
            UpsertOutcome::Unchanged
        }
        Some(_) => {
            // WHY COALESCE on quick_hash in the UPDATE path: the backfill
            // worker (Task 8) may have already promoted this row with a
            // real quick_hash; a re-scan carrying a newly-computed value
            // must NOT overwrite a previously-stored fingerprint.
            // COALESCE(quick_hash, ?) preserves the stored value when
            // non-NULL and fills it when NULL (first re-scan after a
            // schema migration, or a scan that predated Task 7 fix).
            tx.execute(
                "UPDATE files
                 SET file_size = ?1, updated_at = ?2, device_id = ?3, hlc = ?4,
                     quick_hash = COALESCE(quick_hash, ?6)
                 WHERE blake3_hash = ?5",
                rusqlite::params![size_i64, now, dev_str, hlc, hash_hex, quick_hex],
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
/// Three arms: None → INSERT (destination wins at app level), hash+device
/// match → `Unchanged` (skip write, preserve prior `hlc`), else → UPDATE
/// the existing row by id. No collision-soft-delete here — that only
/// fires in [`update_location_path_impl`] when an explicit rename would
/// introduce a duplicate active (volume, path).
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
            // WHY file_uuid via subquery: post-V011 + Task 3 trigger pivot,
            // FTS5 triggers join on file_uuid. The owning `files` row was
            // upserted earlier in the same scan command, so its file_uuid
            // is available via blake3_hash lookup. NULL result here would
            // mean the caller violated the upsert-file-before-upsert-location
            // contract — the bare subquery returns NULL silently, which keeps
            // the INSERT alive (file_uuid TEXT is nullable in V011) and
            // surfaces the bug downstream rather than crashing the writer.
            tx.execute(
                "INSERT INTO file_locations
                 (id, blake3_hash, file_uuid, volume_id, relative_path, status,
                  first_seen, updated_at, device_id, hlc)
                 VALUES (?1, ?2,
                         (SELECT f.file_uuid FROM files f WHERE f.blake3_hash = ?2),
                         ?3, ?4, 'active', ?5, ?5, ?6, ?7)",
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

/// Writer-side body for [`FileWriteCmd::UpdateLocationStatuses`].
///
/// Applies every transition inside ONE transaction. Same UPDATE as
/// [`update_location_status_impl`], including the `hlc` binding — the
/// difference is transaction count, not row semantics.
///
/// WHY a single `hlc` for the whole batch (rather than one per row): the
/// sweep is one logical event ("the catalogue was reconciled against the
/// filesystem at time T"), not N independent ones. Every row it touches
/// observed the same filesystem state, so they share a timestamp. This
/// also keeps the batch atomic under CRDT merge: a peer either sees the
/// whole reconciliation or none of it.
///
/// Returns the total number of rows updated across the batch. Rows whose
/// `(volume, path)` no longer matches an active location contribute 0 and
/// are not an error — the sweep races against the watcher by nature.
fn update_location_statuses_impl(
    conn: &mut Connection,
    updates: &[LocationStatusUpdate],
    device: DeviceId,
    hlc: i64,
) -> Result<u64, CoreError> {
    // WHY early return: `BEGIN IMMEDIATE` takes the write lock, so an
    // empty sweep would still contend with concurrent writers for no
    // reason. A clean library is the common case once steady-state.
    if updates.is_empty() {
        return Ok(0);
    }

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let dev_str = device.0.to_string();
    let now = now_iso();
    let mut total: u64 = 0;
    {
        // WHY a prepared statement hoisted out of the loop: the batch is
        // the same UPDATE N times with different bindings; preparing once
        // avoids re-parsing the SQL per row.
        let mut stmt = tx
            .prepare(
                "UPDATE file_locations
                 SET status = ?1, updated_at = ?2, device_id = ?3, hlc = ?4
                 WHERE volume_id = ?5 AND relative_path = ?6 AND deleted_at IS NULL",
            )
            .map_err(Error::from)?;
        for u in updates {
            let n = stmt
                .execute(rusqlite::params![
                    status_to_str(u.status),
                    now,
                    dev_str,
                    hlc,
                    u.volume.0.to_string(),
                    u.path.as_str(),
                ])
                .map_err(Error::from)?;
            total += u64::try_from(n)
                .map_err(|_| CoreError::Internal(format!("rows_changed {n} is negative")))?;
        }
    }

    tx.commit().map_err(Error::from)?;
    Ok(total)
}

/// Writer-side body for [`FileWriteCmd::SoftDeleteMissingLocations`].
///
/// Retires every active `missing` location in one statement.
///
/// WHY `status = 'missing'` is the sole predicate and no filesystem
/// check happens here: the writer actor owns a database connection, not
/// a view of the disk. Deciding what is missing is the verify sweep's
/// job; conflating the two would put a `stat()` inside a write
/// transaction, holding the write lock across filesystem I/O.
///
/// WHY `deleted_at` rather than `DELETE FROM`: `file_locations` is
/// CRDT-replicated. A hard delete has no merge representation and would
/// resurrect on the next sync from a peer that still holds the row.
///
/// Returns the number of rows retired.
fn soft_delete_missing_locations_impl(
    conn: &mut Connection,
    device: DeviceId,
    hlc: i64,
) -> Result<u64, CoreError> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let now = now_iso();
    let n = tx
        .execute(
            "UPDATE file_locations
             SET deleted_at = ?1, updated_at = ?1, device_id = ?2, hlc = ?3
             WHERE status = 'missing' AND deleted_at IS NULL",
            rusqlite::params![now, device.0.to_string(), hlc],
        )
        .map_err(Error::from)?;

    tx.commit().map_err(Error::from)?;
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

/// Writer-side body for [`FileWriteCmd::PromoteFullHash`].
///
/// Updates `files.blake3_hash` AND a placeholder `full_hash` mirror column,
/// keyed on `file_uuid`. Today the schema has only `blake3_hash` (the
/// content-addressed PK) — V011 added the lazy-full-hash workflow without
/// adding a separate column. The placeholder convention from #161 means we
/// rewrite `blake3_hash` with the freshly computed value (the existing row's
/// PK was the cheap quick fingerprint OR a previously computed full hash;
/// promoting overwrites it). Bumps `files.hlc` per spec §3.7.
///
/// WHY two columns mentioned even though only one exists: the spec calls out
/// the future column split (#161). Until then the writer treats both as the
/// same on-disk byte slot.
///
/// Returns the number of rows updated (0 if the `file_uuid` no longer exists,
/// 1 on success).
fn promote_full_hash_impl(
    conn: &mut Connection,
    file_uuid: FileUuid,
    full_hash: &BlakeHash,
    device: DeviceId,
    hlc: i64,
) -> Result<u64, CoreError> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let uuid_str = file_uuid.0.to_string();
    let hash_hex = full_hash.to_hex();
    let now = now_iso();
    let dev_str = device.0.to_string();

    // WHY UPDATE blake3_hash: in v0.6.x's lazy workflow the `files.blake3_hash`
    // column transiently holds a quick_hash placeholder until the user (or a
    // batch verify) demands the real full content hash. Once computed, this
    // command rewrites the column. A future schema slice (#161) splits the
    // two — at that point the SQL grows a `full_hash = ?` clause; the
    // signature here does not change.
    let n = tx
        .execute(
            "UPDATE files
             SET blake3_hash = ?1, updated_at = ?2, device_id = ?3, hlc = ?4
             WHERE file_uuid = ?5 AND deleted_at IS NULL",
            rusqlite::params![hash_hex, now, dev_str, hlc, uuid_str],
        )
        .map_err(Error::from)?;

    tx.commit().map_err(Error::from)?;

    u64::try_from(n).map_err(|_| CoreError::Internal(format!("rows_changed {n} is negative")))
}

/// Writer-side body for [`FileWriteCmd::MarkVerifiedDistinct`].
///
/// Sets `files.verified_distinct = 1` for every row in `file_uuids`.
/// One transaction so the UI flip is atomic. Bumps `files.hlc` per spec §3.7.
///
/// Returns the total number of rows updated.
fn mark_verified_distinct_impl(
    conn: &mut Connection,
    file_uuids: &[FileUuid],
    device: DeviceId,
    hlc: i64,
) -> Result<u64, CoreError> {
    if file_uuids.is_empty() {
        return Ok(0);
    }

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let now = now_iso();
    let dev_str = device.0.to_string();
    let mut total: i64 = 0;

    {
        // WHY a prepared statement reused per uuid: SQLite plans the UPDATE once
        // and feeds the per-row uuid via bind. Faster than building a single
        // statement with `IN (?, ?, ...)` (which would require reformatting the
        // SQL string per call) and equivalent in transaction semantics.
        let mut stmt = tx
            .prepare(
                "UPDATE files
                 SET verified_distinct = 1, updated_at = ?1, device_id = ?2, hlc = ?3
                 WHERE file_uuid = ?4 AND deleted_at IS NULL",
            )
            .map_err(Error::from)?;

        for uuid in file_uuids {
            let uuid_str = uuid.0.to_string();
            let n = stmt
                .execute(rusqlite::params![now, dev_str, hlc, uuid_str])
                .map_err(Error::from)?;
            // WHY i64 try_from: rusqlite::Statement::execute returns usize; on
            // 32-bit platforms a usize fits in i64, on 64-bit it might not in
            // theory but n is row count (0 or 1 here) so the cast is exact.
            let n_i64 = i64::try_from(n)
                .map_err(|_| CoreError::Internal(format!("rows_changed {n} too large")))?;
            total += n_i64;
        }
    }

    tx.commit().map_err(Error::from)?;

    u64::try_from(total)
        .map_err(|_| CoreError::Internal(format!("rows_changed {total} is negative")))
}
