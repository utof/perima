-- V011: file_uuid + quick_hash + file_identity_cache.
-- See spec §4.1.1, §4.1.2, §4.1.3.
--
-- WHY surrogate file_uuid: blake3_hash is a content address — it changes
-- whenever a file is modified. Joins across tables need a stable identity
-- key that persists through edits. UUIDv7 PKs are the repo-wide convention
-- (CLAUDE.md "Schema rules"); file_uuid is a per-file surrogate that fills
-- that role without breaking existing hash-based dedup logic.
--
-- WHY quick_hash starts NULL: backfill happens in Task 8 (the background
-- worker). Making it NOT NULL here would require a full scan at migration
-- time — too slow for large libraries. The column is nullable in v0.6.x
-- and tightened to NOT NULL (with DEFAULT) once the backfill worker lands.
--
-- WHY cache lookup index is NON-UNIQUE: the (device_id, volume_id,
-- fs_file_id, size_bytes, mtime_ns) tuple is the lookup key, but mtime_ns
-- is mutable (any touch updates it). A UNIQUE constraint on a mutable
-- column violates the schema rule "no UNIQUE on mutable columns" (CLAUDE.md
-- "Schema rules"). The index is a performance index only; uniqueness is
-- enforced by the application layer.

-- ===== files: add columns =====
ALTER TABLE files ADD COLUMN file_uuid TEXT;
ALTER TABLE files ADD COLUMN quick_hash TEXT;

-- Backfill file_uuid for existing rows.
-- WHY this encoding: SQLite has no native UUIDv7 generator; the backfill
-- uses current UTC milliseconds in the high 16 hex chars (48 bits) followed
-- by 20 hex chars from randomblob(10) for the low bits.
-- printf '%016x' gives exactly 16 hex chars for the ms timestamp;
-- hex(randomblob(10)) gives exactly 20 hex chars.  lower() matches the
-- UUIDv7-hex convention used elsewhere (uuid crate's to_string is lower-case).
-- Acceptable for legacy rows: no semantic ordering is required for
-- pre-migration data.
UPDATE files
SET file_uuid = lower(
    printf('%016x', CAST((julianday('now') - 2440587.5) * 86400.0 * 1000 AS INTEGER))
    || hex(randomblob(10))
)
WHERE file_uuid IS NULL;

CREATE UNIQUE INDEX idx_files_file_uuid ON files(file_uuid);

-- ===== FK columns on the 5 dependent tables =====
-- WHY: `search_rowid_map` was replaced by `search_content` in V007 (external-
-- content FTS5 rewrite). Only 4 tables get a non-unique FK column here; the
-- 5th column (`search_content.file_uuid`) is added below with UNIQUE semantics
-- because `file_uuid` is itself immutable (unlike `blake3_hash` which is a
-- content address and can change on file modification).
ALTER TABLE file_locations ADD COLUMN file_uuid TEXT;
ALTER TABLE file_metadata  ADD COLUMN file_uuid TEXT;
ALTER TABLE file_tags      ADD COLUMN file_uuid TEXT;
ALTER TABLE search_content ADD COLUMN file_uuid TEXT;

-- Backfill via JOIN on the existing blake3_hash.
-- Rows with no matching files row (orphaned) remain NULL — accepted for
-- the migration; the application layer handles them gracefully.
UPDATE file_locations
    SET file_uuid = (SELECT f.file_uuid FROM files f WHERE f.blake3_hash = file_locations.blake3_hash);
UPDATE file_metadata
    SET file_uuid = (SELECT f.file_uuid FROM files f WHERE f.blake3_hash = file_metadata.blake3_hash);
UPDATE file_tags
    SET file_uuid = (SELECT f.file_uuid FROM files f WHERE f.blake3_hash = file_tags.blake3_hash);
UPDATE search_content
    SET file_uuid = (SELECT f.file_uuid FROM files f WHERE f.blake3_hash = search_content.blake3_hash);

-- Non-unique FK indexes on mutable-join-column tables (per schema rules:
-- no UNIQUE on mutable columns; blake3_hash is a content address that changes
-- when files are modified, so the join column itself is mutable in principle).
CREATE INDEX idx_file_locations_file_uuid ON file_locations(file_uuid);
CREATE INDEX idx_file_metadata_file_uuid  ON file_metadata(file_uuid);
CREATE INDEX idx_file_tags_file_uuid      ON file_tags(file_uuid);

-- UNIQUE index for search_content: one FTS doc per logical file.
-- `file_uuid` is the surrogate identity key and is immutable once assigned,
-- so UNIQUE is safe here — unlike `blake3_hash` which changes with content.
CREATE UNIQUE INDEX idx_search_content_file_uuid ON search_content(file_uuid);

-- ===== file_identity_cache (device-local) =====
-- WHY device-local: this table caches per-device filesystem metadata
-- (inode, mtime, size) to avoid rehashing unchanged files. It is never
-- synced across devices, so it carries no hlc column (CLAUDE.md
-- "Schema rules expansion": device-local rows omit hlc).
CREATE TABLE file_identity_cache (
    id            TEXT PRIMARY KEY,
    device_id     TEXT NOT NULL,
    volume_id     TEXT NOT NULL,
    fs_file_id    INTEGER NOT NULL,
    size_bytes    INTEGER NOT NULL,
    mtime_ns      INTEGER NOT NULL,
    quick_hash    TEXT NOT NULL,
    full_hash     TEXT NULL,
    last_verified TEXT NOT NULL,
    first_seen    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    deleted_at    TEXT NULL
);

-- Lookup index: used for the "has this file changed?" check.
-- WHY NON-UNIQUE: mtime_ns is mutable — a UNIQUE constraint on a mutable
-- column violates the schema rule. Multiple entries for the same inode
-- (same fs_file_id) with different mtimes are legal during the transition
-- period before the old entry is soft-deleted.
CREATE INDEX idx_fic_lookup ON file_identity_cache
    (device_id, volume_id, fs_file_id, size_bytes, mtime_ns);

-- Partial index on full_hash for efficient duplicate detection queries.
CREATE INDEX idx_fic_full_hash ON file_identity_cache(full_hash)
    WHERE full_hash IS NOT NULL;

-- ===== verified_distinct flag =====
-- WHY: for groups of files that share quick_hash but whose full_hash
-- proves them distinct, this flag suppresses repeated dedup re-flagging.
-- INTEGER NOT NULL DEFAULT 0 matches the boolean-as-int convention used
-- throughout this schema (e.g. thumbnail_queue.in_progress).
ALTER TABLE files ADD COLUMN verified_distinct INTEGER NOT NULL DEFAULT 0;
