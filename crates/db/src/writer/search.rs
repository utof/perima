//! Writer-side handler for [`crate::cmd::SearchWriteCmd`].
//!
//! # HLC semantics
//!
//! Per spec §3.7 HLC-bearing table list: `files`, `file_locations`,
//! `file_metadata`, `tags`, `file_tags`, `volumes`. The FTS5 virtual
//! table backing `search` is NOT on that list — `rebuild` is a
//! dump-and-reseed of the FTS index from source rows that carry their
//! own `hlc`. The source rows are untouched by `Rebuild`. No `hlc`
//! binding in this module.
//!
//! # Events
//!
//! After a successful COMMIT on `Rebuild`, the writer emits
//! [`perima_core::AppEvent::IndexInvalidated`] with
//! [`perima_core::InvalidationReason::SearchIndexRebuilt`] — the v1
//! signal that the FTS5 search index has been wiped and reseeded;
//! any cached search-result list is stale.
//!
//! WHY no `now_iso` / `Hlc` import: the FTS5 virtual table itself
//! carries no `hlc` column (it is a derived index, not a source-of-truth
//! row). Writing `hlc` here would have no effect on CRDT semantics.

use std::sync::Arc;

use perima_core::{AppEvent, CoreError, EventBus, InvalidationReason};
use rusqlite::Connection;

use crate::cmd::SearchWriteCmd;
use crate::errors::Error;

/// Writer-side dispatch for [`SearchWriteCmd`]. Consumes the command
/// (the reply channel lives inside each variant) and sends the result
/// back on the caller's reply channel.
///
/// After a successful COMMIT on `Rebuild`, this fn emits
/// [`AppEvent::IndexInvalidated`] with
/// [`InvalidationReason::SearchIndexRebuilt`] AFTER the COMMIT — see
/// spec §3.3.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn handle(conn: &mut Connection, cmd: SearchWriteCmd, bus: &Arc<dyn EventBus>) {
    match cmd {
        SearchWriteCmd::Rebuild { reply } => {
            let out = rebuild_impl(conn);
            // WHY unconditional emit on Ok: rebuild ALWAYS wipes +
            // reseeds the FTS index. Even if the source rows were
            // empty, the index state changed (cleared); a cached
            // search result list with stale row metadata is
            // invalidated.
            if out.is_ok()
                && let Err(e) = bus.emit(&AppEvent::IndexInvalidated {
                    reason: InvalidationReason::SearchIndexRebuilt,
                })
            {
                tracing::warn!(?e, "post-commit emit failed: SearchIndexRebuilt");
            }
            if reply.send(out).is_err() {
                // WHY debug (not warn): caller dropped its reply handle —
                // e.g. CLI aborted mid-command. The rebuild already ran;
                // nothing actionable here.
                tracing::debug!("search rebuild reply channel closed before send");
            }
        }
    }
}

/// Writer-side body for [`SearchWriteCmd::Rebuild`]. Lifted verbatim
/// from the pre-Batch-C `SqliteSearchRepository::rebuild` with no `hlc`
/// binding — per spec §3.7, the FTS5 virtual table is not on the
/// HLC-bearing table list.
///
/// Semantics: open an IMMEDIATE transaction, DELETE all rows from
/// `search_content`, then repopulate from the live join of
/// `file_locations` + `file_metadata` + `file_tags`. The FTS5 index
/// (`search_index`) is kept in sync via `SQLite` AFTER-INSERT / AFTER-DELETE
/// triggers on `search_content`; no explicit
/// `INSERT INTO search_index(search_index) VALUES('rebuild')` needed.
fn rebuild_impl(conn: &mut Connection) -> Result<(), CoreError> {
    // WHY BEGIN IMMEDIATE: consistent with all other writer handlers
    // (see tag.rs, metadata.rs, file.rs rationale). Prevents a
    // write-lock upgrade race under WAL that DEFERRED can trigger.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    // V007 rebuild: wipe search_content, then repopulate from joined
    // live state. The search_content AFTER-INSERT/DELETE triggers keep
    // search_index in sync row-by-row — no explicit FTS5 'rebuild'
    // needed here.
    //
    // WHY not 'INSERT INTO search_index(search_index) VALUES('rebuild')':
    // that primitive is an external-content resync from search_content,
    // but the DELETE + INSERT path above already drives FTS via triggers.
    // Calling 'rebuild' would be redundant (and defensive for a case
    // that doesn't exist here: search_content-out-of-sync-with-index).
    tx.execute_batch("DELETE FROM search_content;")
        .map_err(Error::from)?;

    // Populate search_content: one representative location per hash,
    // joined with metadata + tags. Mirrors V007 migration bulk-insert.
    // WHY filename = relative_path: SQLite has no built-in REVERSE() for
    // basename extraction; the unicode61 tokenizer splits on '/' and '.'
    // so basenames are discoverable via token match on relative_path.
    tx.execute_batch(
        "INSERT INTO search_content
             (blake3_hash, filename, relative_path, mime_type, camera_model, captured_at, tags)
         SELECT fl.blake3_hash,
                fl.relative_path,
                fl.relative_path,
                COALESCE(m.mime_type, ''),
                COALESCE(m.camera_model, ''),
                COALESCE(m.captured_at, ''),
                COALESCE((
                    SELECT GROUP_CONCAT(t.name, ' ')
                    FROM file_tags ft
                    JOIN tags t ON t.id = ft.tag_id
                    WHERE ft.blake3_hash = fl.blake3_hash
                      AND ft.deleted_at IS NULL
                      AND t.deleted_at IS NULL
                ), '')
         FROM file_locations fl
         LEFT JOIN file_metadata m ON m.blake3_hash = fl.blake3_hash
                                   AND m.deleted_at IS NULL
         WHERE fl.deleted_at IS NULL
           AND fl.id = (
               SELECT id FROM file_locations
               WHERE blake3_hash = fl.blake3_hash AND deleted_at IS NULL
               ORDER BY first_seen ASC, id ASC LIMIT 1
           );",
    )
    .map_err(Error::from)?;

    tx.commit().map_err(Error::from)?;
    Ok(())
}
