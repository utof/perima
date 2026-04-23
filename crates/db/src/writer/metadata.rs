//! Writer-side handler for [`crate::cmd::MetadataWriteCmd`].
//!
//! Lifts the SQL bodies that previously lived inside
//! `impl MetadataRepository for SqliteMetadataRepository::{upsert_metadata,
//! update_thumbnail}` (pre-Batch-C) into writer-owned functions. The
//! writer thread holds the sole writable [`rusqlite::Connection`]
//! (spec §3.1); the adapter on the caller-side is now a thin send →
//! recv shim (see `crates/db/src/metadata_repo.rs`).
//!
//! # HLC semantics
//!
//! Each command computes `let hlc = Hlc::now().pack();` ONCE at the top
//! of [`handle`] and binds the same packed value to every `file_metadata`
//! row written by the command — one HLC value per user-visible logical
//! event (spec §3.7). Per V009:
//!
//! - `file_metadata.hlc` bumps on INSERT (new metadata row) and on the
//!   extractor-driven UPDATE branch of [`crate::cmd::MetadataWriteCmd::UpsertMetadata`].
//! - The `Unchanged` arm skips every write entirely (SELECT-only);
//!   no `hlc` write happens and the prior value is preserved (same
//!   logical event did not fire).
//! - [`crate::cmd::MetadataWriteCmd::UpdateThumbnail`] bumps `hlc` on
//!   its UPDATE — the thumbnail flip is its own logical event, distinct
//!   from extractor upserts.
//!
//! # Events
//!
//! After a successful COMMIT on `UpsertMetadata` (Inserted / Updated)
//! or `UpdateThumbnail` (rows > 0), the writer emits
//! [`perima_core::AppEvent::IndexInvalidated`] with
//! [`perima_core::InvalidationReason::MetadataChanged`] — the coarse
//! v1 signal that metadata-shaped query indexes (file detail panel,
//! thumbnail grid, capture-time sort) are stale.
//!
//! WHY skip emit on `Unchanged`: the Unchanged arm writes zero rows
//! and does not bump hlc — not a logical event per spec §3.3.

use std::sync::Arc;

use perima_core::{
    AppEvent, BlakeHash, CoreError, DeviceId, EventBus, Hlc, InvalidationReason, MediaMetadata,
    UpsertOutcome,
};
use rusqlite::{Connection, OptionalExtension};

use crate::cmd::MetadataWriteCmd;
use crate::errors::Error;

/// Writer-side dispatch for [`MetadataWriteCmd`]. Consumes the command
/// (the reply channel lives inside each variant) and sends the result
/// back on the caller's reply channel.
///
/// After successful writes that actually change state, this fn emits
/// [`AppEvent::IndexInvalidated`] with
/// [`InvalidationReason::MetadataChanged`] AFTER the COMMIT — see spec
/// §3.3.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn handle(conn: &mut Connection, cmd: MetadataWriteCmd, bus: &Arc<dyn EventBus>) {
    // WHY one HLC per command (not per row): the "one HLC per
    // user-visible logical event" invariant from spec §3.7. A single
    // upsert_metadata may INSERT a new row OR UPDATE an existing one;
    // both paths bind the same `hlc` value. The `Unchanged` arm skips
    // every write — no `hlc` is consumed.
    let hlc = Hlc::now().pack();

    match cmd {
        MetadataWriteCmd::UpsertMetadata {
            record,
            device,
            reply,
        } => {
            let out = upsert_metadata_impl(conn, &record, device, hlc);
            // WHY emit gated on Inserted | Updated: the `Unchanged`
            // arm writes zero rows and does not bump hlc.
            if matches!(&out, Ok(o) if !matches!(o, UpsertOutcome::Unchanged)) {
                emit_metadata_changed(bus, "upsert_metadata");
            }
            if reply.send(out).is_err() {
                // WHY debug (not warn): caller dropped its reply
                // handle — e.g. CLI aborted mid-command. The write
                // already committed; nothing actionable.
                tracing::debug!("metadata upsert_metadata reply channel closed before send");
            }
        }
        MetadataWriteCmd::UpdateThumbnail {
            hash,
            path,
            status,
            device,
            reply,
        } => {
            let out = update_thumbnail_impl(conn, &hash, path.as_deref(), &status, device, hlc);
            // WHY emit gated on rows > 0: a thumbnail flip against a
            // missing metadata row writes zero rows.
            if matches!(&out, Ok(rows) if *rows > 0) {
                emit_metadata_changed(bus, "update_thumbnail");
            }
            if reply.send(out).is_err() {
                tracing::debug!("metadata update_thumbnail reply channel closed before send");
            }
        }
    }
}

/// Emit `IndexInvalidated::MetadataChanged` and log on emit failure.
///
/// `who` identifies the calling sub-command for log scoping. Failed
/// emits log-only — the COMMIT already landed and the caller already
/// got (or is about to get) its reply.
fn emit_metadata_changed(bus: &Arc<dyn EventBus>, who: &'static str) {
    if let Err(e) = bus.emit(&AppEvent::IndexInvalidated {
        reason: InvalidationReason::MetadataChanged,
    }) {
        tracing::warn!(?e, who, "post-commit emit failed: MetadataChanged");
    }
}

/// ISO-8601 UTC timestamp used for `updated_at` / `extracted_at`.
/// Matches the pre-Batch-C adapter's `now_iso` helper.
fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Convert `Option<u64>` to `Option<i64>` for binding as `INTEGER`.
///
/// WHY: `rusqlite`'s `ToSql` impl does not cover `u64` (`SQLite`
/// integers are signed 64-bit). Values originating from media
/// containers (duration in ms) fit comfortably in `i64` on any
/// real-world asset; we propagate overflow as `Internal` rather than
/// truncating.
fn u64_opt_to_i64(v: Option<u64>) -> Result<Option<i64>, CoreError> {
    v.map(|raw| {
        i64::try_from(raw).map_err(|_| {
            CoreError::Internal(format!("duration_ms {raw} overflows SQLite INTEGER (i64)"))
        })
    })
    .transpose()
}

/// Writer-side body for [`MetadataWriteCmd::UpsertMetadata`]. Lifted
/// verbatim from the pre-Batch-C `SqliteMetadataRepository::upsert_metadata`
/// with `hlc = ?` bound on both the INSERT path and the extractor-driven
/// UPDATE path.
///
/// # Thumbnail-column preservation
///
/// Neither branch touches `thumbnail_path` or `thumbnail_status` — the
/// worker's [`MetadataWriteCmd::UpdateThumbnail`] is the sole writer of
/// those columns. Preserving whatever state the worker has already
/// written across an Updated upsert is the invariant pinned by the
/// `upsert_metadata_preserves_thumbnail_state` regression
/// (utof/perima#15 HIGH #4). On INSERT, `thumbnail_status` is seeded
/// with the literal `'pending'` so the partial index
/// `idx_file_metadata_thumbnail_pending` stays populated for every new
/// row (utof/perima#15 HIGH #3 + V004 backfill).
fn upsert_metadata_impl(
    conn: &mut Connection,
    meta: &MediaMetadata,
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

    let hash_hex = meta.hash.to_hex();
    let now = now_iso();
    let dev_str = device.0.to_string();
    let duration_ms_i64 = u64_opt_to_i64(meta.duration_ms)?;

    // Mirror `SqliteFileRepository::upsert_file`'s SELECT-then-
    // INSERT/UPDATE on the content-addressed PK (blake3_hash). We
    // fetch the existing row's device_id + mime_type for a cheap
    // equivalence proxy to classify Unchanged vs Updated.
    let existing: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT device_id, mime_type FROM file_metadata
             WHERE blake3_hash = ?1 AND deleted_at IS NULL",
            [&hash_hex],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Error::from)?;

    let outcome = match existing {
        None => {
            // WHY thumbnail_path / thumbnail_status NOT bound from
            // `meta`: extractors always produce `None` for these
            // fields. The queue worker writes them via the dedicated
            // `UpdateThumbnail` variant after thumbnail generation
            // completes. A subsequent Updated upsert (triggered by
            // a mime_type flip on the same hash) would otherwise
            // clobber the worker's state back to NULL, silently
            // losing the thumbnail association. See utof/perima#15
            // HIGH #4 for the regression this prevents.
            //
            // WHY `thumbnail_status` literal-default 'pending' on
            // INSERT: V004 backfills the NULL rows left by v0.4.0–
            // v0.4.1, and future INSERTs need to produce 'pending'
            // on the same path so the
            // `idx_file_metadata_thumbnail_pending` partial index
            // stays populated. The literal lives in the SQL, not
            // in `MediaMetadata`, because the UPDATE branch of
            // this upsert deliberately never touches thumbnail
            // columns (Task 2 decoupling). See utof/perima#15
            // HIGH #3.
            tx.execute(
                "INSERT INTO file_metadata
                 (blake3_hash, width, height, duration_ms, captured_at,
                  camera_make, camera_model, codec, bitrate_bps, mime_type,
                  thumbnail_status,
                  extracted_at, updated_at, device_id, hlc)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                         'pending',
                         ?11, ?11, ?12, ?13)",
                rusqlite::params![
                    hash_hex,
                    meta.width,
                    meta.height,
                    duration_ms_i64,
                    meta.captured_at,
                    meta.camera_make,
                    meta.camera_model,
                    meta.codec,
                    meta.bitrate_bps,
                    meta.mime_type,
                    now,
                    dev_str,
                    hlc,
                ],
            )
            .map_err(Error::from)?;
            UpsertOutcome::Inserted
        }
        Some((existing_dev, existing_mime))
            if existing_dev == dev_str && existing_mime == meta.mime_type =>
        {
            // WHY cheap equality proxy: comparing every Option field
            // would bloat this method and still miss changes hidden
            // in (say) camera_model alone. mime_type + device_id is
            // the coarsest check that classifies "new extraction
            // run" vs "repeat call with identical inputs". v0.4.0
            // accepts occasional false-Updated over false-Unchanged
            // as the safe default.
            //
            // WHY no hlc write on Unchanged: same logical event did
            // not fire — preserving the prior hlc matches the
            // tag/volume upsert semantics (spec §3.7).
            UpsertOutcome::Unchanged
        }
        Some(_) => {
            // WHY UPDATE omits thumbnail_path / thumbnail_status:
            // same rationale as the INSERT branch above. The
            // worker's `UpdateThumbnail` command is the sole writer
            // of those columns.
            tx.execute(
                "UPDATE file_metadata
                 SET width = ?2, height = ?3, duration_ms = ?4,
                     captured_at = ?5, camera_make = ?6, camera_model = ?7,
                     codec = ?8, bitrate_bps = ?9, mime_type = ?10,
                     updated_at = ?11, device_id = ?12, hlc = ?13
                 WHERE blake3_hash = ?1",
                rusqlite::params![
                    hash_hex,
                    meta.width,
                    meta.height,
                    duration_ms_i64,
                    meta.captured_at,
                    meta.camera_make,
                    meta.camera_model,
                    meta.codec,
                    meta.bitrate_bps,
                    meta.mime_type,
                    now,
                    dev_str,
                    hlc,
                ],
            )
            .map_err(Error::from)?;
            UpsertOutcome::Updated
        }
    };

    tx.commit().map_err(Error::from)?;
    Ok(outcome)
}

/// Writer-side body for [`MetadataWriteCmd::UpdateThumbnail`]. Lifted
/// verbatim from the pre-Batch-C `SqliteMetadataRepository::update_thumbnail`
/// with `hlc = ?` bound on the UPDATE.
///
/// Returns the number of rows updated (0 if no metadata row exists
/// for `hash`; 1 otherwise).
fn update_thumbnail_impl(
    conn: &mut Connection,
    hash: &BlakeHash,
    path: Option<&str>,
    status: &str,
    device: DeviceId,
    hlc: i64,
) -> Result<u64, CoreError> {
    // WHY BEGIN IMMEDIATE: the UPDATE is a pure single-statement
    // mutation, but IMMEDIATE avoids a write-lock upgrade race that
    // DEFERRED can trigger under WAL. Consistent with every other
    // write path in this crate.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let hash_hex = hash.to_hex();
    let now = now_iso();
    let dev_str = device.0.to_string();
    let rows_changed = tx
        .execute(
            "UPDATE file_metadata
             SET thumbnail_path = ?1, thumbnail_status = ?2,
                 updated_at = ?3, device_id = ?4, hlc = ?5
             WHERE blake3_hash = ?6 AND deleted_at IS NULL",
            rusqlite::params![path, status, now, dev_str, hlc, hash_hex],
        )
        .map_err(Error::from)?;

    tx.commit().map_err(Error::from)?;

    u64::try_from(rows_changed)
        .map_err(|_| CoreError::Internal(format!("rows_changed {rows_changed} is negative")))
}
