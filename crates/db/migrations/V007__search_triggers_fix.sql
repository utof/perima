-- V007: external-content FTS5 + content-table-driven sync (closes #22 #40 #41 #42).
--
-- WHY switch from contentless to external-content: contentless FTS5's
-- 'delete' command requires supplying OLD column values for every
-- indexed column; V006 supplied '' blanks and left stale tokens. See
-- spec doc 2026-04-17-v0.6.3-fts5-triggers-hotfix-design.md.
--
-- WHY content-table-driven sync (search_content INSERT/UPDATE/DELETE
-- triggers drive search_index, NOT direct delete-by-rowid inside
-- business triggers): the 'delete' FTS5 command raises "database disk
-- image is malformed" if the target rowid is not currently indexed.
-- Business triggers (file_locations INSERT etc.) can legitimately fire
-- before any FTS doc exists for that rowid, so unconditional 'delete'
-- is unsafe. Driving FTS sync from search_content AFTER-triggers is
-- the canonical SQLite FTS5 pattern (see SQLite docs 18.2). OLD.* is
-- always available in the AFTER UPDATE/DELETE trigger body, so the
-- 'delete' command always receives the correct old-value payload.
--
-- Business-table triggers therefore become pure search_content
-- maintenance: upsert/update/delete rows in search_content; the
-- search_content triggers handle all FTS5 index updates.

-- WHY drop V006 triggers first: V006 created triggers with the SAME names
-- (search_after_metadata_insert, search_after_metadata_update,
-- search_after_file_tags_insert, search_after_file_tags_update). Recreating
-- them below would fail; drop explicitly so V007 bodies replace V006's.
DROP TRIGGER IF EXISTS search_after_metadata_insert;
DROP TRIGGER IF EXISTS search_after_metadata_update;
DROP TRIGGER IF EXISTS search_after_file_tags_insert;
DROP TRIGGER IF EXISTS search_after_file_tags_update;

DROP TABLE IF EXISTS search_index;
DROP TABLE IF EXISTS search_rowid_map;

CREATE TABLE search_content (
    rowid         INTEGER PRIMARY KEY,
    blake3_hash   TEXT    NOT NULL UNIQUE,
    filename      TEXT    NOT NULL DEFAULT '',
    relative_path TEXT    NOT NULL DEFAULT '',
    mime_type     TEXT    NOT NULL DEFAULT '',
    camera_model  TEXT    NOT NULL DEFAULT '',
    captured_at   TEXT    NOT NULL DEFAULT '',
    tags          TEXT    NOT NULL DEFAULT ''
);

-- WHY default unicode61 tokenizer: splits on '.', '/', '_' — so a path
-- like "photos/sunset.jpg" indexes tokens ["photos", "sunset", "jpg"]
-- and search for "sunset" matches. Explicit `tokenize='unicode61'` is
-- redundant (it's the default) but should we ever want `porter` or a
-- custom tokenizer, this is the knob.
CREATE VIRTUAL TABLE search_index USING fts5(
    filename, relative_path, mime_type, camera_model, captured_at, tags,
    content='search_content', content_rowid='rowid'
);

-- ---------------------------------------------------------------------------
-- search_content <-> search_index sync (INTERNAL — not part of the public
-- trigger API; these just keep FTS5 aligned with the external-content table).
-- ---------------------------------------------------------------------------

CREATE TRIGGER sc_after_insert AFTER INSERT ON search_content BEGIN
    INSERT INTO search_index
        (rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
    VALUES
        (NEW.rowid, NEW.filename, NEW.relative_path, NEW.mime_type,
         NEW.camera_model, NEW.captured_at, NEW.tags);
END;

CREATE TRIGGER sc_after_update AFTER UPDATE ON search_content BEGIN
    INSERT INTO search_index
        (search_index, rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
    VALUES
        ('delete', OLD.rowid, OLD.filename, OLD.relative_path, OLD.mime_type,
         OLD.camera_model, OLD.captured_at, OLD.tags);
    INSERT INTO search_index
        (rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
    VALUES
        (NEW.rowid, NEW.filename, NEW.relative_path, NEW.mime_type,
         NEW.camera_model, NEW.captured_at, NEW.tags);
END;

CREATE TRIGGER sc_after_delete AFTER DELETE ON search_content BEGIN
    INSERT INTO search_index
        (search_index, rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
    VALUES
        ('delete', OLD.rowid, OLD.filename, OLD.relative_path, OLD.mime_type,
         OLD.camera_model, OLD.captured_at, OLD.tags);
END;

-- ---------------------------------------------------------------------------
-- Bulk populate search_content from live state.
-- Source: first-seen active file_locations per hash, joined with
-- file_metadata + file_tags GROUP_CONCAT of tag names.
-- WHY the id=(SELECT ... ORDER BY first_seen ASC, id ASC LIMIT 1) subquery:
-- picks a deterministic representative location per hash. The sc_after_insert
-- trigger fires row-by-row and syncs FTS5 automatically.
-- ---------------------------------------------------------------------------
INSERT INTO search_content (blake3_hash, filename, relative_path, mime_type, camera_model, captured_at, tags)
SELECT
    fl.blake3_hash,
    fl.relative_path,
    fl.relative_path,
    COALESCE(m.mime_type, ''),
    COALESCE(m.camera_model, ''),
    COALESCE(m.captured_at, ''),
    COALESCE((
        SELECT GROUP_CONCAT(t.name, ' ')
        FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
        WHERE ft.blake3_hash = fl.blake3_hash AND ft.deleted_at IS NULL
    ), '')
FROM file_locations fl
LEFT JOIN file_metadata m ON m.blake3_hash = fl.blake3_hash
WHERE fl.deleted_at IS NULL
  AND fl.id = (
      SELECT id FROM file_locations
      WHERE blake3_hash = fl.blake3_hash AND deleted_at IS NULL
      ORDER BY first_seen ASC, id ASC LIMIT 1
  );

-- ---------------------------------------------------------------------------
-- Business-table triggers.
-- Every trigger updates/deletes search_content from JOINED LIVE STATE
-- (never NEW.* for column values), so fire-order across simultaneous
-- multi-table updates is irrelevant. The sc_after_{insert,update,delete}
-- triggers above carry search_index in lock-step.
-- ---------------------------------------------------------------------------

-- Trigger 1: file_locations INSERT -> ensure search_content row exists.
-- WHY INSERT OR IGNORE (no OR REPLACE): if the representative row already
-- exists (same hash), we leave it alone; only a dedicated UPDATE event on
-- the authoritative location should mutate the indexed path.
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
               WHERE ft.blake3_hash = NEW.blake3_hash AND ft.deleted_at IS NULL
           ), '')
    FROM (SELECT NULL) placeholder
    LEFT JOIN file_metadata m ON m.blake3_hash = NEW.blake3_hash;
END;

-- ----------------------------------------------------------------------
-- Trigger 2 is split into THREE WHEN-gated triggers for clarity
-- (SQLite supports multiple triggers on the same table+event).
-- All bodies mutate search_content; sc_after_update propagates to FTS.
--
-- Combined-transaction semantics: a single UPDATE statement that mutates
-- blake3_hash + relative_path + deleted_at simultaneously will fire all
-- three triggers (order by creation: 2a, 2b, 2c). Each maintains
-- search_content correctly; the final state matches joined live state.
-- ----------------------------------------------------------------------

-- Trigger 2a: file_locations UPDATE when blake3_hash changed (#42).
-- Retire OLD hash's search_content row (if no sibling active location
-- still references OLD hash); upsert NEW hash's search_content row with
-- joined live state.
CREATE TRIGGER search_after_location_hash_change
AFTER UPDATE OF blake3_hash ON file_locations
WHEN OLD.blake3_hash != NEW.blake3_hash
BEGIN
    -- Retire OLD hash's search_content row if no sibling survives.
    -- sc_after_delete will emit the FTS 'delete'.
    DELETE FROM search_content
    WHERE blake3_hash = OLD.blake3_hash
      AND NOT EXISTS (
          SELECT 1 FROM file_locations fl
          WHERE fl.blake3_hash = OLD.blake3_hash
            AND fl.deleted_at IS NULL
            AND fl.id != NEW.id
      );

    -- Seed NEW hash's search_content row if not present, then refresh
    -- it from joined live state. The refresh is an UPDATE that fires
    -- sc_after_update (delete-by-rowid with OLD columns + reinsert).
    INSERT OR IGNORE INTO search_content (blake3_hash, filename, relative_path)
    VALUES (NEW.blake3_hash, NEW.relative_path, NEW.relative_path);

    UPDATE search_content
    SET filename      = NEW.relative_path,
        relative_path = NEW.relative_path,
        mime_type     = COALESCE((SELECT mime_type FROM file_metadata WHERE blake3_hash = NEW.blake3_hash), ''),
        camera_model  = COALESCE((SELECT camera_model FROM file_metadata WHERE blake3_hash = NEW.blake3_hash), ''),
        captured_at   = COALESCE((SELECT captured_at FROM file_metadata WHERE blake3_hash = NEW.blake3_hash), ''),
        tags          = COALESCE((
            SELECT GROUP_CONCAT(t.name, ' ')
            FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
            WHERE ft.blake3_hash = NEW.blake3_hash AND ft.deleted_at IS NULL
        ), '')
    WHERE blake3_hash = NEW.blake3_hash;
END;

-- Trigger 2b: file_locations UPDATE when relative_path changed (#22).
-- Only fires when NEW is the representative (first-seen active) location
-- for its hash -- otherwise the representative's path is still authoritative.
CREATE TRIGGER search_after_location_rename
AFTER UPDATE OF relative_path ON file_locations
WHEN OLD.relative_path != NEW.relative_path
 AND NEW.deleted_at IS NULL
 AND NEW.id = (
     SELECT id FROM file_locations
     WHERE blake3_hash = NEW.blake3_hash AND deleted_at IS NULL
     ORDER BY first_seen ASC, id ASC LIMIT 1
 )
BEGIN
    -- WHY NEW.* is safe here: the WHEN-guard above already asserts
    -- NEW is the representative row for this hash, so NEW.* IS the
    -- live state by definition.
    UPDATE search_content
    SET relative_path = NEW.relative_path,
        filename      = NEW.relative_path
    WHERE blake3_hash = NEW.blake3_hash;
END;

-- Trigger 2c: file_locations UPDATE when deleted_at was set (C1 fix).
-- If a sibling active location exists, re-point search_content.relative_path
-- to the new representative. Otherwise retire the search_content row.
CREATE TRIGGER search_after_location_soft_delete
AFTER UPDATE OF deleted_at ON file_locations
WHEN OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL
BEGIN
    -- Re-point to surviving sibling (first-seen active location).
    UPDATE search_content SET
        relative_path = COALESCE((SELECT fl.relative_path FROM file_locations fl
                                  WHERE fl.blake3_hash = OLD.blake3_hash AND fl.deleted_at IS NULL
                                  ORDER BY fl.first_seen ASC, fl.id ASC LIMIT 1), relative_path),
        filename      = COALESCE((SELECT fl.relative_path FROM file_locations fl
                                  WHERE fl.blake3_hash = OLD.blake3_hash AND fl.deleted_at IS NULL
                                  ORDER BY fl.first_seen ASC, fl.id ASC LIMIT 1), filename)
    WHERE blake3_hash = OLD.blake3_hash
      AND EXISTS (SELECT 1 FROM file_locations
                  WHERE blake3_hash = OLD.blake3_hash AND deleted_at IS NULL);

    -- If no sibling survives, retire the search_content row.
    DELETE FROM search_content
    WHERE blake3_hash = OLD.blake3_hash
      AND NOT EXISTS (SELECT 1 FROM file_locations
                      WHERE blake3_hash = OLD.blake3_hash AND deleted_at IS NULL);
END;

-- Trigger 3a: file_metadata INSERT -> ensure search_content + refresh columns.
CREATE TRIGGER search_after_metadata_insert
AFTER INSERT ON file_metadata
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

-- Trigger 3b: file_metadata UPDATE -> refresh columns.
CREATE TRIGGER search_after_metadata_update
AFTER UPDATE ON file_metadata
BEGIN
    UPDATE search_content
    SET mime_type    = COALESCE(NEW.mime_type, ''),
        camera_model = COALESCE(NEW.camera_model, ''),
        captured_at  = COALESCE(NEW.captured_at, '')
    WHERE blake3_hash = NEW.blake3_hash;
END;

-- Trigger 4a: file_tags INSERT -> ensure search_content row + refresh tags.
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
        WHERE ft.blake3_hash = NEW.blake3_hash AND ft.deleted_at IS NULL
    ), '')
    WHERE blake3_hash = NEW.blake3_hash;
END;

-- Trigger 4b: file_tags UPDATE (typically deleted_at set for detach) -> rebuild tags column.
CREATE TRIGGER search_after_file_tags_update
AFTER UPDATE ON file_tags
BEGIN
    UPDATE search_content
    SET tags = COALESCE((
        SELECT GROUP_CONCAT(t.name, ' ')
        FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
        WHERE ft.blake3_hash = NEW.blake3_hash AND ft.deleted_at IS NULL
    ), '')
    WHERE blake3_hash = NEW.blake3_hash;
END;

-- Trigger 5: tags.name UPDATE -> refresh every search_content row that
-- references this tag via file_tags.
-- WHY AFTER UPDATE OF name: SQLite supports column-specific triggers; this
-- keeps fire-scope narrow (other tags columns are not indexed).
CREATE TRIGGER search_after_tags_name_update
AFTER UPDATE OF name ON tags
WHEN OLD.name != NEW.name
BEGIN
    UPDATE search_content
    SET tags = COALESCE((
        SELECT GROUP_CONCAT(t.name, ' ')
        FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
        WHERE ft.blake3_hash = search_content.blake3_hash AND ft.deleted_at IS NULL
    ), '')
    WHERE blake3_hash IN (
        SELECT ft.blake3_hash FROM file_tags ft
        WHERE ft.tag_id = NEW.id AND ft.deleted_at IS NULL
    );
END;

-- Trigger 6: tags hard-DELETE (off-path / operator event) -> rebuild holders.
-- AFTER DELETE triggers see OLD.* -- SQLite guarantee.
CREATE TRIGGER search_after_tags_delete
AFTER DELETE ON tags
BEGIN
    UPDATE search_content
    SET tags = COALESCE((
        SELECT GROUP_CONCAT(t.name, ' ')
        FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
        WHERE ft.blake3_hash = search_content.blake3_hash AND ft.deleted_at IS NULL
    ), '')
    WHERE blake3_hash IN (
        SELECT ft.blake3_hash FROM file_tags ft
        WHERE ft.tag_id = OLD.id AND ft.deleted_at IS NULL
    );
END;
