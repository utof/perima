//! Writer-side handler for [`crate::cmd::TagWriteCmd`].
//!
//! Lifts the SQL bodies that previously lived inside
//! `impl TagRepository for SqliteTagRepository::{upsert_tag, delete_tag,
//! attach, detach}` (pre-Batch-C) into writer-owned functions. The
//! writer thread holds the sole writable [`rusqlite::Connection`]
//! (spec §3.1); the adapter on the caller-side is now a thin send →
//! recv shim (see `crates/db/src/tag_repo.rs`).
//!
//! # HLC semantics
//!
//! Each command computes `let hlc = Hlc::now().pack();` ONCE at the top
//! of [`handle`] and binds the same packed value to every `tags` /
//! `file_tags` row written by the command — one HLC value per
//! user-visible logical event (spec §3.7). Per V009:
//!
//! - `tags.hlc` bumps on INSERT (new tag) and on UPDATE
//!   (soft-delete via [`crate::cmd::TagWriteCmd::DeleteTag`]).
//! - `file_tags.hlc` bumps on INSERT (new attach) and on UPDATE
//!   (soft-delete via [`crate::cmd::TagWriteCmd::Detach`]).
//! - The idempotent `Attach` arm skips the INSERT entirely when an
//!   active row already exists; no `hlc` write happens and the prior
//!   value is preserved (same logical event did not fire).
//!
//! # Events
//!
//! After a successful COMMIT on `Attach` / `Detach`, the writer emits
//! [`perima_core::AppEvent::IndexInvalidated`] with
//! [`perima_core::InvalidationReason::TagsChanged`] — the coarse v1
//! signal that tag-shaped query indexes (frontend tag list, file→tags
//! join) are stale. `UpsertTag` / `DeleteTag` also emit `TagsChanged`:
//! the tag list itself is the invalidated index.
//!
//! WHY skip emit on no-op `Attach`: re-attaching an already-active
//! `(hash, tag_id)` pair writes zero rows and does not bump `hlc` (see
//! `attach_impl` below). No row written → no logical event happened →
//! no `AppEvent` per spec §3.3 ("publish events AFTER COMMIT" — but
//! only when a COMMIT actually changed state).

use std::sync::Arc;

use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, EventBus, Hlc, InvalidationReason, Tag,
};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

use crate::cmd::TagWriteCmd;
use crate::errors::Error;

/// Writer-side dispatch for [`TagWriteCmd`]. Consumes the command
/// (the reply channel lives inside each variant) and sends the result
/// back on the caller's reply channel.
///
/// After successful writes that actually change state (i.e. did not
/// land on an idempotent no-op path), this fn emits
/// [`AppEvent::IndexInvalidated`] with
/// [`InvalidationReason::TagsChanged`] AFTER the COMMIT — see spec §3.3.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn handle(conn: &mut Connection, cmd: TagWriteCmd, bus: &Arc<dyn EventBus>) {
    // WHY one HLC per command (not per row): the "one HLC per
    // user-visible logical event" invariant from spec §3.7. A single
    // upsert_tag may INSERT a new row OR UPDATE an existing one; both
    // paths bind the same `hlc` value.
    let hlc = Hlc::now().pack();

    match cmd {
        TagWriteCmd::UpsertTag {
            name,
            device,
            reply,
        } => {
            let out = upsert_tag_impl(conn, &name, device, hlc);
            if reply.send(out).is_err() {
                // WHY debug (not warn): caller dropped its reply
                // handle — e.g. CLI aborted mid-command. The write
                // already committed; nothing actionable.
                tracing::debug!("tag upsert_tag reply channel closed before send");
            }
        }
        TagWriteCmd::DeleteTag {
            tag_id,
            device,
            reply,
        } => {
            let out = delete_tag_impl(conn, tag_id, device, hlc);
            if reply.send(out).is_err() {
                tracing::debug!("tag delete_tag reply channel closed before send");
            }
        }
        TagWriteCmd::Attach {
            hash,
            tag_id,
            device,
            reply,
        } => {
            let out = attach_impl(conn, &hash, tag_id, device, hlc);
            // WHY emit gated on rows_changed > 0: the idempotent no-op
            // path (already-attached pair) writes zero rows, does not
            // bump hlc, and is NOT a logical event per spec §3.3.
            if let Ok(rows) = &out
                && *rows > 0
                && let Err(e) = bus.emit(&AppEvent::IndexInvalidated {
                    reason: InvalidationReason::TagsChanged,
                })
            {
                tracing::warn!(?e, "post-commit emit failed: TagsChanged (attach)");
            }
            if reply.send(out).is_err() {
                tracing::debug!("tag attach reply channel closed before send");
            }
        }
        TagWriteCmd::Detach {
            hash,
            tag_id,
            device,
            reply,
        } => {
            let out = detach_impl(conn, &hash, tag_id, device, hlc);
            // WHY emit gated on rows_changed > 0: detaching a pair
            // that's already inactive writes zero rows and is not a
            // logical event (spec §3.3).
            if let Ok(rows) = &out
                && *rows > 0
                && let Err(e) = bus.emit(&AppEvent::IndexInvalidated {
                    reason: InvalidationReason::TagsChanged,
                })
            {
                tracing::warn!(?e, "post-commit emit failed: TagsChanged (detach)");
            }
            if reply.send(out).is_err() {
                tracing::debug!("tag detach reply channel closed before send");
            }
        }
    }
}

/// ISO-8601 UTC timestamp used for `updated_at` / `first_seen` /
/// `deleted_at`. Matches the pre-Batch-C adapter's `now_iso` helper.
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Writer-side body for [`TagWriteCmd::UpsertTag`]. Lifted verbatim
/// from the pre-Batch-C `SqliteTagRepository::upsert_tag` with `hlc = ?`
/// bound on the INSERT path.
///
/// Name normalization happens adapter-side; this function receives the
/// already-normalized `name` and does not re-validate.
fn upsert_tag_impl(
    conn: &mut Connection,
    name: &str,
    device: DeviceId,
    hlc: i64,
) -> Result<Tag, CoreError> {
    // WHY BEGIN IMMEDIATE: historical rationale (pre-Batch-C) guarded
    // against two adapter handles racing on SELECT-then-INSERT. The
    // single writer actor now guarantees serialization, BUT retaining
    // IMMEDIATE is cheap and documents the "only one write tx at a
    // time" expectation at the SQL boundary.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT id, first_seen FROM tags WHERE name = ?1 AND deleted_at IS NULL",
            [name],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Error::from)?;

    let tag = if let Some((id_str, first_seen)) = existing {
        let id = Uuid::parse_str(&id_str)
            .map_err(|e| CoreError::Internal(format!("invalid uuid in db: {e}")))?;
        Tag {
            id,
            name: name.to_owned(),
            first_seen,
        }
    } else {
        let id = Uuid::now_v7();
        let now = now_iso();
        let dev_str = device.0.to_string();
        tx.execute(
            "INSERT INTO tags (id, name, first_seen, updated_at, device_id, hlc)
             VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
            rusqlite::params![id.to_string(), name, now, dev_str, hlc],
        )
        .map_err(Error::from)?;
        Tag {
            id,
            name: name.to_owned(),
            first_seen: now,
        }
    };

    tx.commit().map_err(Error::from)?;
    Ok(tag)
}

/// Writer-side body for [`TagWriteCmd::DeleteTag`]. Lifted verbatim
/// from the pre-Batch-C `SqliteTagRepository::delete_tag` with `hlc = ?`
/// bound on the soft-delete UPDATE.
fn delete_tag_impl(
    conn: &mut Connection,
    tag_id: Uuid,
    device: DeviceId,
    hlc: i64,
) -> Result<u64, CoreError> {
    // WHY BEGIN IMMEDIATE: delete_tag is a pure UPDATE, but
    // IMMEDIATE avoids a write-lock upgrade race that DEFERRED can
    // trigger under WAL. Consistent with all other write paths.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let now = now_iso();
    let dev_str = device.0.to_string();
    let rows_changed = tx
        .execute(
            "UPDATE tags SET deleted_at = ?1, updated_at = ?1, device_id = ?2, hlc = ?3
             WHERE id = ?4 AND deleted_at IS NULL",
            rusqlite::params![now, dev_str, hlc, tag_id.to_string()],
        )
        .map_err(Error::from)?;

    tx.commit().map_err(Error::from)?;

    u64::try_from(rows_changed)
        .map_err(|_| CoreError::Internal(format!("rows_changed {rows_changed} is negative")))
}

/// Writer-side body for [`TagWriteCmd::Attach`]. Lifted verbatim from
/// the pre-Batch-C `SqliteTagRepository::attach` with `hlc = ?` bound
/// on the INSERT path.
fn attach_impl(
    conn: &mut Connection,
    hash: &BlakeHash,
    tag_id: Uuid,
    device: DeviceId,
    hlc: i64,
) -> Result<u64, CoreError> {
    let hash_hex = hash.to_hex();
    let tag_id_str = tag_id.to_string();

    // WHY BEGIN IMMEDIATE: SELECT-then-INSERT must be atomic to
    // prevent duplicate active (hash, tag_id) rows under concurrency.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM file_tags
             WHERE blake3_hash = ?1 AND tag_id = ?2 AND deleted_at IS NULL",
            [&hash_hex, &tag_id_str],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)?;

    let rows_changed = if existing.is_none() {
        let id = Uuid::now_v7();
        let now = now_iso();
        let dev_str = device.0.to_string();
        // WHY file_uuid via subquery: post-V011 + Task 3 trigger pivot
        // (spec §4.1.4) — `search_after_file_tags_insert` joins on
        // file_uuid. The owning `files` row exists by precondition
        // (callers attach tags to known hashes), so the lookup is
        // deterministic. Without this, file_uuid stays NULL on the
        // file_tags row and the trigger's `WHERE file_uuid = NEW.file_uuid`
        // never matches, leaving tags absent from search_content.
        tx.execute(
            "INSERT INTO file_tags
                (id, blake3_hash, file_uuid, tag_id, first_seen, updated_at, device_id, hlc)
             VALUES (?1, ?2,
                     (SELECT f.file_uuid FROM files f WHERE f.blake3_hash = ?2),
                     ?3, ?4, ?4, ?5, ?6)",
            rusqlite::params![id.to_string(), hash_hex, tag_id_str, now, dev_str, hlc],
        )
        .map_err(Error::from)?
    } else {
        0
    };

    tx.commit().map_err(Error::from)?;

    u64::try_from(rows_changed)
        .map_err(|_| CoreError::Internal(format!("rows_changed {rows_changed} is negative")))
}

/// Writer-side body for [`TagWriteCmd::Detach`]. Lifted verbatim from
/// the pre-Batch-C `SqliteTagRepository::detach` with `hlc = ?` bound
/// on the soft-delete UPDATE.
fn detach_impl(
    conn: &mut Connection,
    hash: &BlakeHash,
    tag_id: Uuid,
    device: DeviceId,
    hlc: i64,
) -> Result<u64, CoreError> {
    let hash_hex = hash.to_hex();
    let tag_id_str = tag_id.to_string();

    // WHY BEGIN IMMEDIATE: detach is a pure UPDATE (no preceding
    // SELECT), but IMMEDIATE avoids a write-lock upgrade race that
    // DEFERRED can trigger under WAL. Consistent with all other
    // write paths in this repo.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let now = now_iso();
    let dev_str = device.0.to_string();
    let rows_changed = tx
        .execute(
            "UPDATE file_tags SET deleted_at = ?1, updated_at = ?1, device_id = ?2, hlc = ?3
             WHERE blake3_hash = ?4 AND tag_id = ?5 AND deleted_at IS NULL",
            rusqlite::params![now, dev_str, hlc, hash_hex, tag_id_str],
        )
        .map_err(Error::from)?;

    tx.commit().map_err(Error::from)?;

    u64::try_from(rows_changed)
        .map_err(|_| CoreError::Internal(format!("rows_changed {rows_changed} is negative")))
}
