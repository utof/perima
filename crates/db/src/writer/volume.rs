//! Writer-side handler for [`crate::cmd::VolumeWriteCmd`].
//!
//! Lifts the SQL bodies that previously lived inside
//! `impl VolumeRepository for SqliteVolumeRepository::{find_or_create,
//! record_mount}` (pre-Batch-C) into writer-owned functions. The
//! writer thread holds the sole writable [`rusqlite::Connection`]
//! (spec §3.1); the adapter on the caller-side is now a thin send →
//! recv shim (see `crates/db/src/volume_repo.rs`).
//!
//! # HLC semantics
//!
//! Each command computes `let hlc = Hlc::now().pack();` ONCE and binds
//! to every `volumes` row written. `volume_mounts` has no `hlc` column
//! per V009 (device-local rows do not participate in CRDT sync; Batch
//! A §5 — see `crates/db/migrations/V009__hlc_columns.sql`). One HLC
//! value per user-visible logical event (spec §3.7).
//!
//! # Events
//!
//! Volume writes do NOT emit any [`perima_core::AppEvent`] in v1.
//!
//! WHY no emit: volume writes don't invalidate any v1 query index.
//! The volumes UI reads volumes directly via
//! [`perima_core::VolumeRepository::list`], not through a cached/
//! invalidated query layer. The four `InvalidationReason` variants
//! today (`TagsChanged`, `FilesChanged`, `MetadataChanged`,
//! `SearchIndexRebuilt`) intentionally exclude volume scope.
//!
//! Add an emit here (`InvalidationReason::VolumesChanged` or similar)
//! when a v2 frontend caches the volume list and needs a hint to
//! refetch — until then a silent write is correct.

use std::sync::Arc;

use perima_core::{CoreError, DeviceId, EventBus, Hlc, VolumeId, VolumeIdentifiers};
use rusqlite::{Connection, OptionalExtension};

use crate::cmd::VolumeWriteCmd;
use crate::errors::Error;

/// Writer-side dispatch for [`VolumeWriteCmd`]. Consumes the command
/// (the reply channel lives inside each variant) and sends the result
/// back on the caller's reply channel.
///
/// WHY `_bus` unused: see module-level WHY block — volume writes
/// invalidate no v1 query index. The parameter stays in the
/// signature so adding a `VolumesChanged` invalidation in v2 is an
/// additive change in this one module, not a churn across
/// `writer/mod.rs`.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn handle(conn: &mut Connection, cmd: VolumeWriteCmd, _bus: &Arc<dyn EventBus>) {
    // WHY one HLC per command (not per row): the "one HLC per
    // user-visible logical event" invariant from spec §3.7. A single
    // find_or_create may UPDATE an existing volume row; both paths
    // bind the same `hlc` value.
    let hlc = Hlc::now().pack();

    match cmd {
        VolumeWriteCmd::FindOrCreate {
            identifiers,
            device,
            reply,
        } => {
            let out = find_or_create_impl(conn, &identifiers, device, hlc);
            if reply.send(out).is_err() {
                // WHY debug (not warn): caller dropped its reply
                // handle — e.g. CLI aborted mid-command. The write
                // already committed; nothing actionable.
                tracing::debug!("volume find_or_create reply channel closed before send");
            }
        }
        VolumeWriteCmd::RecordMount {
            volume,
            device,
            mount,
            reply,
        } => {
            let out = record_mount_impl(conn, volume, device, &mount, hlc);
            if reply.send(out).is_err() {
                tracing::debug!("volume record_mount reply channel closed before send");
            }
        }
    }
}

/// ISO-8601 UTC timestamp used for `updated_at` / `last_seen` /
/// `first_seen`. Matches the pre-Batch-C adapter's `now_iso` helper.
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Cast `u64` capacity to the `i64` column width with an explicit
/// error on overflow. Shared between `find_or_create_impl` and the
/// INSERT path.
fn capacity_to_i64(cap: u64) -> Result<i64, CoreError> {
    i64::try_from(cap).map_err(|_| CoreError::Internal(format!("capacity {cap} overflows i64")))
}

/// Refresh `last_seen` / `updated_at` / `device_id` / `hlc` on an
/// existing volumes row and commit. Shared tail for all three match
/// arms (GUID / `fs_uuid` / label+capacity).
fn touch_and_commit(
    tx: rusqlite::Transaction<'_>,
    vol_id_str: &str,
    now: &str,
    dev_str: &str,
    hlc: i64,
) -> Result<VolumeId, CoreError> {
    tx.execute(
        "UPDATE volumes
         SET last_seen = ?1, updated_at = ?1, device_id = ?2, hlc = ?3
         WHERE volume_id = ?4",
        rusqlite::params![now, dev_str, hlc, vol_id_str],
    )
    .map_err(Error::from)?;
    let vol_id = uuid::Uuid::parse_str(vol_id_str)
        .map_err(|e| CoreError::Internal(format!("bad volume uuid: {e}")))?;
    tx.commit().map_err(Error::from)?;
    Ok(VolumeId(vol_id))
}

/// Writer-side body for [`VolumeWriteCmd::FindOrCreate`]. Lifted
/// verbatim from the pre-Batch-C `SqliteVolumeRepository::find_or_create`
/// with `hlc = ?` bound on every INSERT / UPDATE to `volumes`.
fn find_or_create_impl(
    conn: &mut Connection,
    ident: &VolumeIdentifiers,
    device: DeviceId,
    hlc: i64,
) -> Result<VolumeId, CoreError> {
    let now = now_iso();
    let dev_str = device.0.to_string();

    // WHY BEGIN IMMEDIATE: historical rationale (pre-Batch-C) guarded
    // against two adapter handles racing on SELECT-then-INSERT. The
    // single writer actor now guarantees serialization, BUT retaining
    // IMMEDIATE is cheap and documents the "only one write tx at a
    // time" expectation at the SQL boundary. Also protects against
    // any stray future reader that happens to hold a read tx while
    // this handler tries to promote.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    // WHY: priority chain — GUID is the most stable identifier (survives
    // reformatting on the same hardware). fs_uuid is next. label+capacity
    // is the v1 fallback. Each arm SELECT-then-UPDATE-last-seen, or falls
    // through to the next.

    // Arm 1: GPT partition GUID
    if let Some(ref guid) = ident.gpt_partition_guid {
        let existing: Option<String> = tx
            .query_row(
                "SELECT volume_id FROM volumes
                 WHERE gpt_partition_guid = ?1 AND deleted_at IS NULL",
                [guid],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)?;

        if let Some(vol_id_str) = existing {
            return touch_and_commit(tx, &vol_id_str, &now, &dev_str, hlc);
        }
    }

    // Arm 2: Filesystem UUID
    if let Some(ref fs_uuid) = ident.fs_uuid {
        let existing: Option<String> = tx
            .query_row(
                "SELECT volume_id FROM volumes
                 WHERE fs_uuid = ?1 AND deleted_at IS NULL",
                [fs_uuid],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)?;

        if let Some(vol_id_str) = existing {
            return touch_and_commit(tx, &vol_id_str, &now, &dev_str, hlc);
        }
    }

    // Arm 3: label + capacity (v1 primary matching path)
    if let Some(ref label) = ident.label {
        let cap_i64 = capacity_to_i64(ident.capacity_bytes)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT volume_id FROM volumes
                 WHERE volume_label = ?1 AND capacity_bytes = ?2
                   AND deleted_at IS NULL",
                rusqlite::params![label, cap_i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)?;

        if let Some(vol_id_str) = existing {
            return touch_and_commit(tx, &vol_id_str, &now, &dev_str, hlc);
        }
    }

    // No match → INSERT new volume row.
    let new_id = VolumeId::new();
    let new_id_str = new_id.0.to_string();
    let cap_i64 = capacity_to_i64(ident.capacity_bytes)?;
    tx.execute(
        "INSERT INTO volumes
         (volume_id, gpt_partition_guid, fs_uuid, volume_label,
          capacity_bytes, is_removable, last_seen, updated_at, device_id, hlc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)",
        rusqlite::params![
            new_id_str,
            ident.gpt_partition_guid,
            ident.fs_uuid,
            ident.label,
            cap_i64,
            i64::from(ident.is_removable),
            now,
            dev_str,
            hlc,
        ],
    )
    .map_err(Error::from)?;
    tx.commit().map_err(Error::from)?;

    Ok(new_id)
}

/// Writer-side body for [`VolumeWriteCmd::RecordMount`]. Lifted
/// verbatim from the pre-Batch-C `SqliteVolumeRepository::record_mount`.
///
/// No `hlc` binding: `volume_mounts` is device-local (V009 excludes it
/// from the sync-eligible table list — see module-doc).
fn record_mount_impl(
    conn: &mut Connection,
    volume: VolumeId,
    machine: DeviceId,
    mount: &std::path::Path,
    _hlc: i64,
) -> Result<(), CoreError> {
    // WHY: validate UTF-8 at the boundary. `to_string_lossy` silently
    // replaces invalid bytes with U+FFFD, corrupting future identity
    // matches. InvalidPath is the taxonomy hit used elsewhere for path
    // problems that fail validation before we touch the DB.
    let mount_str = mount.to_str().ok_or_else(|| {
        CoreError::InvalidPath(format!(
            "mount path is not valid UTF-8: {}",
            mount.display()
        ))
    })?;

    let now = now_iso();
    let vol_str = volume.0.to_string();
    let machine_str = machine.0.to_string();

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    // WHY: soft-delete any active mount row for the same
    // (volume_id, machine_id) whose mount_path differs from the new one.
    // CRDT rules forbid hard-delete on mutable rows; retired mounts
    // remain observable to future sync with deleted_at set.
    tx.execute(
        "UPDATE volume_mounts
         SET deleted_at = ?1, updated_at = ?1, device_id = ?2
         WHERE volume_id = ?3 AND machine_id = ?4
           AND mount_path <> ?5 AND deleted_at IS NULL",
        rusqlite::params![now, machine_str, vol_str, machine_str, mount_str],
    )
    .map_err(Error::from)?;

    // WHY: app-level uniqueness on (volume_id, machine_id, mount_path,
    // deleted_at IS NULL) replaces a UNIQUE constraint that CLAUDE.md
    // forbids on mutable columns. Under the writer actor SELECT-then-
    // INSERT is race-safe because this is the only writer.
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM volume_mounts
             WHERE volume_id = ?1 AND machine_id = ?2
               AND mount_path = ?3 AND deleted_at IS NULL",
            rusqlite::params![vol_str, machine_str, mount_str],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)?;

    if existing.is_none() {
        let new_id = perima_core::ids::new_id().to_string();
        tx.execute(
            "INSERT INTO volume_mounts
             (id, volume_id, machine_id, mount_path, first_seen, updated_at, device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
            rusqlite::params![new_id, vol_str, machine_str, mount_str, now, machine_str,],
        )
        .map_err(Error::from)?;
    }

    tx.commit().map_err(Error::from)?;
    Ok(())
}
