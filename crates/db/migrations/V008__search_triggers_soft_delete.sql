-- V008: FTS5 soft-delete correctness round-2.
-- Closes the four codex-surfaced correctness bugs in V007:
--
--   #1 deleted-tag leak: V007 aggregations filtered `ft.deleted_at IS NULL`
--      but never `t.deleted_at IS NULL`, and no trigger fired on tag soft-delete.
--   #2 rep-path overwrite: V007 trigger 2a unconditionally set
--      search_content.relative_path = NEW.relative_path, clobbering the
--      first-seen representative's path whenever a non-rep location's hash
--      changed to a hash that already had a representative.
--   #3a combined hash-change+soft-delete: V007 trigger 2a fired even when
--      NEW.deleted_at IS NOT NULL, leaking a tombstoned NEW hash into FTS.
--   #3b location restore: V007 had no trigger on
--      WHEN OLD.deleted_at IS NOT NULL AND NEW.deleted_at IS NULL.
--   #4 metadata soft-delete: V007 trigger 3b blindly copied NEW.mime_type
--      etc. with no deleted_at guard, and had no dedicated soft-delete handler.
--
-- Design invariant (strengthened from V007): EVERY aggregation / reinsert
-- reads from joined LIVE state, filtered on `deleted_at IS NULL` across
-- ALL joined tables. NEW.* is used for column values ONLY when NEW is
-- the column's authoritative source (trigger 2b, which is already WHEN-gated
-- to the representative).

-- ---------------------------------------------------------------------------
-- Drop V007 triggers we replace (new bodies below carry the fixes).
-- Expanded per reviewer: search_after_metadata_insert +
-- search_after_file_locations_insert were missed in the initial v0.6.4 cut —
-- they inherit the same bug class this migration closes elsewhere (missing
-- `t.deleted_at`/`m.deleted_at`/NEW tombstone guards).
-- ---------------------------------------------------------------------------
DROP TRIGGER IF EXISTS search_after_location_hash_change;
DROP TRIGGER IF EXISTS search_after_metadata_update;
DROP TRIGGER IF EXISTS search_after_file_tags_insert;
DROP TRIGGER IF EXISTS search_after_file_tags_update;
DROP TRIGGER IF EXISTS search_after_tags_name_update;
DROP TRIGGER IF EXISTS search_after_tags_delete;
DROP TRIGGER IF EXISTS search_after_metadata_insert;
DROP TRIGGER IF EXISTS search_after_file_locations_insert;

-- ---------------------------------------------------------------------------
-- #2 + #3a: split V007 trigger 2a into two triggers.
--
-- 2a.1 (retire OLD) fires on any hash change; OLD's retirement is safe
-- regardless of NEW's tombstone state.
--
-- 2a.2 (seed/refresh NEW) fires only when NEW is live. The refresh UPDATE
-- now reads relative_path + filename from joined live state (first-seen
-- active location for NEW.blake3_hash), never from NEW.*, so a non-rep
-- hash change cannot clobber the representative's indexed path.
-- ---------------------------------------------------------------------------

CREATE TRIGGER search_after_location_hash_change_retire
AFTER UPDATE OF blake3_hash ON file_locations
WHEN OLD.blake3_hash != NEW.blake3_hash
BEGIN
    DELETE FROM search_content
    WHERE blake3_hash = OLD.blake3_hash
      AND NOT EXISTS (
          SELECT 1 FROM file_locations fl
          WHERE fl.blake3_hash = OLD.blake3_hash
            AND fl.deleted_at IS NULL
            AND fl.id != NEW.id
      );
END;

CREATE TRIGGER search_after_location_hash_change_seed
AFTER UPDATE OF blake3_hash ON file_locations
WHEN OLD.blake3_hash != NEW.blake3_hash
 AND NEW.deleted_at IS NULL
BEGIN
    INSERT OR IGNORE INTO search_content (blake3_hash, filename, relative_path)
    SELECT NEW.blake3_hash, fl.relative_path, fl.relative_path
    FROM file_locations fl
    WHERE fl.blake3_hash = NEW.blake3_hash AND fl.deleted_at IS NULL
    ORDER BY fl.first_seen ASC, fl.id ASC LIMIT 1;

    UPDATE search_content
    SET filename      = (SELECT fl.relative_path FROM file_locations fl
                         WHERE fl.blake3_hash = NEW.blake3_hash AND fl.deleted_at IS NULL
                         ORDER BY fl.first_seen ASC, fl.id ASC LIMIT 1),
        relative_path = (SELECT fl.relative_path FROM file_locations fl
                         WHERE fl.blake3_hash = NEW.blake3_hash AND fl.deleted_at IS NULL
                         ORDER BY fl.first_seen ASC, fl.id ASC LIMIT 1),
        mime_type     = COALESCE((SELECT mime_type FROM file_metadata
                                  WHERE blake3_hash = NEW.blake3_hash
                                    AND deleted_at IS NULL), ''),
        camera_model  = COALESCE((SELECT camera_model FROM file_metadata
                                  WHERE blake3_hash = NEW.blake3_hash
                                    AND deleted_at IS NULL), ''),
        captured_at   = COALESCE((SELECT captured_at FROM file_metadata
                                  WHERE blake3_hash = NEW.blake3_hash
                                    AND deleted_at IS NULL), ''),
        tags          = COALESCE((
            SELECT GROUP_CONCAT(t.name, ' ')
            FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
            WHERE ft.blake3_hash = NEW.blake3_hash
              AND ft.deleted_at IS NULL
              AND t.deleted_at IS NULL
        ), '')
    WHERE blake3_hash = NEW.blake3_hash;
END;

-- ---------------------------------------------------------------------------
-- #3b: location restore trigger — the inverse of V007 trigger 2c.
-- Fires when deleted_at transitions from NON-NULL to NULL. Recreates the
-- FTS doc from joined live state for this hash.
-- ---------------------------------------------------------------------------

CREATE TRIGGER search_after_location_restore
AFTER UPDATE OF deleted_at ON file_locations
WHEN OLD.deleted_at IS NOT NULL AND NEW.deleted_at IS NULL
BEGIN
    INSERT OR IGNORE INTO search_content (blake3_hash, filename, relative_path)
    SELECT NEW.blake3_hash, fl.relative_path, fl.relative_path
    FROM file_locations fl
    WHERE fl.blake3_hash = NEW.blake3_hash AND fl.deleted_at IS NULL
    ORDER BY fl.first_seen ASC, fl.id ASC LIMIT 1;

    UPDATE search_content
    SET filename      = (SELECT fl.relative_path FROM file_locations fl
                         WHERE fl.blake3_hash = NEW.blake3_hash AND fl.deleted_at IS NULL
                         ORDER BY fl.first_seen ASC, fl.id ASC LIMIT 1),
        relative_path = (SELECT fl.relative_path FROM file_locations fl
                         WHERE fl.blake3_hash = NEW.blake3_hash AND fl.deleted_at IS NULL
                         ORDER BY fl.first_seen ASC, fl.id ASC LIMIT 1),
        mime_type     = COALESCE((SELECT mime_type FROM file_metadata
                                  WHERE blake3_hash = NEW.blake3_hash
                                    AND deleted_at IS NULL), ''),
        camera_model  = COALESCE((SELECT camera_model FROM file_metadata
                                  WHERE blake3_hash = NEW.blake3_hash
                                    AND deleted_at IS NULL), ''),
        captured_at   = COALESCE((SELECT captured_at FROM file_metadata
                                  WHERE blake3_hash = NEW.blake3_hash
                                    AND deleted_at IS NULL), ''),
        tags          = COALESCE((
            SELECT GROUP_CONCAT(t.name, ' ')
            FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
            WHERE ft.blake3_hash = NEW.blake3_hash
              AND ft.deleted_at IS NULL
              AND t.deleted_at IS NULL
        ), '')
    WHERE blake3_hash = NEW.blake3_hash;
END;

-- ---------------------------------------------------------------------------
-- #4: metadata update now respects deleted_at. Soft-deleted metadata
-- clears its tokens (mime/camera/capture); live metadata copies NEW.*.
-- The single trigger body handles both branches via CASE, so an
-- UPDATE OF deleted_at either direction converges search_content correctly.
-- ---------------------------------------------------------------------------

CREATE TRIGGER search_after_metadata_update
AFTER UPDATE ON file_metadata
BEGIN
    UPDATE search_content
    SET mime_type    = CASE WHEN NEW.deleted_at IS NULL
                            THEN COALESCE(NEW.mime_type, '')
                            ELSE '' END,
        camera_model = CASE WHEN NEW.deleted_at IS NULL
                            THEN COALESCE(NEW.camera_model, '')
                            ELSE '' END,
        captured_at  = CASE WHEN NEW.deleted_at IS NULL
                            THEN COALESCE(NEW.captured_at, '')
                            ELSE '' END
    WHERE blake3_hash = NEW.blake3_hash;
END;

-- ---------------------------------------------------------------------------
-- #1: every tag aggregation now filters t.deleted_at IS NULL.
-- Triggers 4a, 4b, 5, 6 re-created with the added filter.
-- ---------------------------------------------------------------------------

CREATE TRIGGER search_after_file_tags_insert
AFTER INSERT ON file_tags
WHEN NEW.deleted_at IS NULL
BEGIN
    INSERT OR IGNORE INTO search_content (blake3_hash, filename, relative_path)
    SELECT NEW.blake3_hash, fl.relative_path, fl.relative_path
    FROM file_locations fl
    WHERE fl.blake3_hash = NEW.blake3_hash AND fl.deleted_at IS NULL
    ORDER BY fl.first_seen ASC, fl.id ASC LIMIT 1;

    UPDATE search_content
    SET tags = COALESCE((
        SELECT GROUP_CONCAT(t.name, ' ')
        FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
        WHERE ft.blake3_hash = NEW.blake3_hash
          AND ft.deleted_at IS NULL
          AND t.deleted_at IS NULL
    ), '')
    WHERE blake3_hash = NEW.blake3_hash;
END;

CREATE TRIGGER search_after_file_tags_update
AFTER UPDATE ON file_tags
BEGIN
    UPDATE search_content
    SET tags = COALESCE((
        SELECT GROUP_CONCAT(t.name, ' ')
        FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
        WHERE ft.blake3_hash = NEW.blake3_hash
          AND ft.deleted_at IS NULL
          AND t.deleted_at IS NULL
    ), '')
    WHERE blake3_hash = NEW.blake3_hash;
END;

CREATE TRIGGER search_after_tags_name_update
AFTER UPDATE OF name ON tags
WHEN OLD.name != NEW.name
BEGIN
    UPDATE search_content
    SET tags = COALESCE((
        SELECT GROUP_CONCAT(t.name, ' ')
        FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
        WHERE ft.blake3_hash = search_content.blake3_hash
          AND ft.deleted_at IS NULL
          AND t.deleted_at IS NULL
    ), '')
    WHERE blake3_hash IN (
        SELECT ft.blake3_hash FROM file_tags ft
        WHERE ft.tag_id = NEW.id AND ft.deleted_at IS NULL
    );
END;

CREATE TRIGGER search_after_tags_delete
AFTER DELETE ON tags
BEGIN
    UPDATE search_content
    SET tags = COALESCE((
        SELECT GROUP_CONCAT(t.name, ' ')
        FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
        WHERE ft.blake3_hash = search_content.blake3_hash
          AND ft.deleted_at IS NULL
          AND t.deleted_at IS NULL
    ), '')
    WHERE blake3_hash IN (
        SELECT ft.blake3_hash FROM file_tags ft
        WHERE ft.tag_id = OLD.id AND ft.deleted_at IS NULL
    );
END;

-- ---------------------------------------------------------------------------
-- #1: new trigger on tags soft-delete / restore.
-- WHEN-gate fires on either direction (the two `IS NULL` checks produce 0/1
-- that differ only when the transition happened). Single body refreshes
-- search_content.tags for every hash referencing this tag — the JOIN's
-- `t.deleted_at IS NULL` filter excludes the now-deleted tag (soft-delete
-- case) or includes the now-restored tag (restore case).
-- WHY no `IS DISTINCT FROM`: portability — older SQLite builds don't have it.
-- ---------------------------------------------------------------------------

CREATE TRIGGER search_after_tag_soft_delete_or_restore
AFTER UPDATE OF deleted_at ON tags
WHEN (OLD.deleted_at IS NULL) != (NEW.deleted_at IS NULL)
BEGIN
    UPDATE search_content
    SET tags = COALESCE((
        SELECT GROUP_CONCAT(t.name, ' ')
        FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
        WHERE ft.blake3_hash = search_content.blake3_hash
          AND ft.deleted_at IS NULL
          AND t.deleted_at IS NULL
    ), '')
    WHERE blake3_hash IN (
        SELECT ft.blake3_hash FROM file_tags ft
        WHERE ft.tag_id = NEW.id AND ft.deleted_at IS NULL
    );
END;

-- ---------------------------------------------------------------------------
-- Reviewer #2: `search_after_metadata_insert` now guarded on NEW.deleted_at.
-- Prior body at V007:252 blindly copied NEW.mime/camera/capture into
-- search_content — a tombstoned metadata row arriving via CRDT merge
-- seeded live tokens for a deleted peer-state. WHEN-guard on the whole
-- trigger skips those inserts entirely; the UPDATE path (above) still
-- clears tokens if an already-seeded row later tombstones.
-- ---------------------------------------------------------------------------

CREATE TRIGGER search_after_metadata_insert
AFTER INSERT ON file_metadata
WHEN NEW.deleted_at IS NULL
BEGIN
    INSERT OR IGNORE INTO search_content (blake3_hash, filename, relative_path)
    SELECT NEW.blake3_hash, fl.relative_path, fl.relative_path
    FROM file_locations fl
    WHERE fl.blake3_hash = NEW.blake3_hash AND fl.deleted_at IS NULL
    ORDER BY fl.first_seen ASC, fl.id ASC LIMIT 1;

    UPDATE search_content
    SET mime_type    = COALESCE(NEW.mime_type, ''),
        camera_model = COALESCE(NEW.camera_model, ''),
        captured_at  = COALESCE(NEW.captured_at, '')
    WHERE blake3_hash = NEW.blake3_hash;
END;

-- ---------------------------------------------------------------------------
-- Reviewer #3: `search_after_file_locations_insert` now filters
-- `t.deleted_at IS NULL` on the tags JOIN and `m.deleted_at IS NULL` on the
-- metadata LEFT JOIN. Prior body at V007:132 was missed when V008 enumerated
-- the trigger set — same bug class as elsewhere in this migration. Without
-- these filters, a fresh location insert re-seeds search_content with
-- tombstoned tag/metadata tokens when the prior doc was retired.
-- ---------------------------------------------------------------------------

CREATE TRIGGER search_after_file_locations_insert
AFTER INSERT ON file_locations
WHEN NEW.deleted_at IS NULL
BEGIN
    INSERT OR IGNORE INTO search_content
        (blake3_hash, filename, relative_path, mime_type, camera_model, captured_at, tags)
    SELECT NEW.blake3_hash,
           NEW.relative_path,
           NEW.relative_path,
           COALESCE(m.mime_type, ''),
           COALESCE(m.camera_model, ''),
           COALESCE(m.captured_at, ''),
           COALESCE((
               SELECT GROUP_CONCAT(t.name, ' ')
               FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
               WHERE ft.blake3_hash = NEW.blake3_hash
                 AND ft.deleted_at IS NULL
                 AND t.deleted_at IS NULL
           ), '')
    FROM (SELECT NULL) placeholder
    LEFT JOIN file_metadata m ON m.blake3_hash = NEW.blake3_hash
                              AND m.deleted_at IS NULL;
END;

-- ---------------------------------------------------------------------------
-- #5 / #7 perf: covering partial index on the representative-selection
-- subquery. Every aggregation trigger + search() + rebuild() does
-- `SELECT ... WHERE blake3_hash = ? AND deleted_at IS NULL
--  ORDER BY first_seen ASC, id ASC LIMIT 1`. Without this index SQLite
-- re-enters file_locations for every hit and sorts siblings in a temp
-- B-tree (EXPLAIN QUERY PLAN confirms USE TEMP B-TREE FOR ORDER BY).
-- Partial predicate keeps the index slim; only active rows matter for FTS.
-- ---------------------------------------------------------------------------

CREATE INDEX idx_file_locations_rep_active
    ON file_locations(blake3_hash, first_seen, id)
    WHERE deleted_at IS NULL;
