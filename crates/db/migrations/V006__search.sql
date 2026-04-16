-- FTS5 full-text search index over file metadata + tags.
--
-- WHY contentless FTS5 (content=""): avoids storing a duplicate copy of the
-- indexed text. Downside: UPDATE requires a delete+insert pair; handled in
-- the sync triggers below. The rebuild procedure wipes and repopulates the
-- entire index from the current DB state.
--
-- WHY filename = relative_path: SQLite has no built-in REVERSE() function, so
-- there is no portable single-expression basename extraction. relative_path is
-- indexed as-is; the FTS5 unicode61 tokenizer splits on non-word characters
-- (including '/' and '.') which makes basenames discoverable via token match.

CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(
    filename,
    relative_path,
    mime_type,
    camera_model,
    captured_at,
    tags,
    content=""
);

-- Maps FTS5 rowid → (hash, volume, path) for result lookup after a search hit.
-- WHY separate table: contentless FTS5 stores only the inverted index, not the
-- original row values. After a MATCH we need to join back to file_locations +
-- file_metadata; this side table provides the key.
-- UNIQUE on blake3_hash: one FTS doc per content identity (not per location).
CREATE TABLE IF NOT EXISTS search_rowid_map (
    rowid         INTEGER PRIMARY KEY,
    blake3_hash   TEXT    NOT NULL UNIQUE,
    volume_id     TEXT    NOT NULL,
    relative_path TEXT    NOT NULL
);

-- ── Sync triggers ────────────────────────────────────────────────────────────
-- Keep search_index current on file_metadata and file_tags changes.

-- On new file_metadata row: add a search doc for this hash.
CREATE TRIGGER IF NOT EXISTS search_after_metadata_insert
AFTER INSERT ON file_metadata
BEGIN
    INSERT OR IGNORE INTO search_rowid_map (blake3_hash, volume_id, relative_path)
    SELECT NEW.blake3_hash, fl.volume_id, fl.relative_path
    FROM file_locations fl
    WHERE fl.blake3_hash = NEW.blake3_hash AND fl.deleted_at IS NULL
    LIMIT 1;

    INSERT INTO search_index (rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
    SELECT srm.rowid,
           srm.relative_path,
           srm.relative_path,
           COALESCE(NEW.mime_type, ''),
           COALESCE(NEW.camera_model, ''),
           COALESCE(NEW.captured_at, ''),
           COALESCE((
               SELECT GROUP_CONCAT(t.name, ' ')
               FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
               WHERE ft.blake3_hash = NEW.blake3_hash AND ft.deleted_at IS NULL
           ), '')
    FROM search_rowid_map srm
    WHERE srm.blake3_hash = NEW.blake3_hash;
END;

-- On file_metadata UPDATE: delete stale doc and re-insert.
CREATE TRIGGER IF NOT EXISTS search_after_metadata_update
AFTER UPDATE ON file_metadata
BEGIN
    INSERT INTO search_index (search_index, rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
    SELECT 'delete', srm.rowid, '', '', '', '', '', ''
    FROM search_rowid_map srm WHERE srm.blake3_hash = NEW.blake3_hash;

    INSERT INTO search_index (rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
    SELECT srm.rowid,
           srm.relative_path,
           srm.relative_path,
           COALESCE(NEW.mime_type, ''),
           COALESCE(NEW.camera_model, ''),
           COALESCE(NEW.captured_at, ''),
           COALESCE((
               SELECT GROUP_CONCAT(t.name, ' ')
               FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
               WHERE ft.blake3_hash = NEW.blake3_hash AND ft.deleted_at IS NULL
           ), '')
    FROM search_rowid_map srm WHERE srm.blake3_hash = NEW.blake3_hash;
END;

-- On file_tags INSERT (tag attached): rebuild the tags column.
CREATE TRIGGER IF NOT EXISTS search_after_file_tags_insert
AFTER INSERT ON file_tags
BEGIN
    INSERT INTO search_index (search_index, rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
    SELECT 'delete', srm.rowid, '', '', '', '', '', ''
    FROM search_rowid_map srm WHERE srm.blake3_hash = NEW.blake3_hash;

    INSERT INTO search_index (rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
    SELECT srm.rowid,
           srm.relative_path,
           srm.relative_path,
           COALESCE(m.mime_type, ''),
           COALESCE(m.camera_model, ''),
           COALESCE(m.captured_at, ''),
           COALESCE((
               SELECT GROUP_CONCAT(t.name, ' ')
               FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
               WHERE ft.blake3_hash = NEW.blake3_hash AND ft.deleted_at IS NULL
           ), '')
    FROM search_rowid_map srm
    LEFT JOIN file_metadata m ON m.blake3_hash = srm.blake3_hash
    WHERE srm.blake3_hash = NEW.blake3_hash;
END;

-- On file_tags UPDATE (tag soft-deleted via detach): rebuild tags column.
CREATE TRIGGER IF NOT EXISTS search_after_file_tags_update
AFTER UPDATE ON file_tags
BEGIN
    INSERT INTO search_index (search_index, rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
    SELECT 'delete', srm.rowid, '', '', '', '', '', ''
    FROM search_rowid_map srm WHERE srm.blake3_hash = NEW.blake3_hash;

    INSERT INTO search_index (rowid, filename, relative_path, mime_type, camera_model, captured_at, tags)
    SELECT srm.rowid,
           srm.relative_path,
           srm.relative_path,
           COALESCE(m.mime_type, ''),
           COALESCE(m.camera_model, ''),
           COALESCE(m.captured_at, ''),
           COALESCE((
               SELECT GROUP_CONCAT(t.name, ' ')
               FROM file_tags ft JOIN tags t ON t.id = ft.tag_id
               WHERE ft.blake3_hash = NEW.blake3_hash AND ft.deleted_at IS NULL
           ), '')
    FROM search_rowid_map srm
    LEFT JOIN file_metadata m ON m.blake3_hash = srm.blake3_hash
    WHERE srm.blake3_hash = NEW.blake3_hash;
END;
