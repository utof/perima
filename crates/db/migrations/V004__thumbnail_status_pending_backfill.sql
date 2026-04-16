-- V004: backfill NULL thumbnail_status rows to 'pending'.
--
-- WHY: V003 added `thumbnail_status` as nullable without a default, and no
-- writer ever produced 'pending' — the queue worker only writes 'ready',
-- 'failed' (and, as of v0.4.2, 'skipped') while the extractor always supplied
-- None. Rows created under v0.4.0 (pre-thumbnails) and v0.4.1 (queue didn't
-- seed 'pending') therefore stuck at NULL forever, excluded from the
-- `idx_file_metadata_thumbnail_pending` partial index. A future `perima
-- thumbnail` retry command would never see them.
--
-- Future rows are seeded to 'pending' by `upsert_metadata`'s INSERT statement
-- (via a literal default on the `thumbnail_status` column, not via the
-- `MediaMetadata` struct). UPDATE path never touches thumbnail columns per
-- v0.4.2's upsert/thumbnail decoupling (utof/perima#15 HIGH #4).
--
-- WHY no `updated_at` bump: refinery migrations are pure SQL with no Rust-
-- callable parameter binding. Using a literal timestamp would lie; omitting
-- the bump deviates from the CRDT rule "every mutable-row write updates
-- updated_at" but is acceptable for a one-time data-correction migration —
-- no user-supplied state changes, only a label that the column should always
-- have carried.
--
-- Refs utof/perima#15 (HIGH #3).
UPDATE file_metadata
   SET thumbnail_status = 'pending'
 WHERE thumbnail_status IS NULL
   AND deleted_at IS NULL;
