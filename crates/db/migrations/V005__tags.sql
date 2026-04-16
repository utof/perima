-- Tags and file-tag associations — content-addressed (blake3_hash keyed).

CREATE TABLE tags (
    id            TEXT PRIMARY KEY,       -- UUIDv7
    name          TEXT NOT NULL,          -- NFC-normalized lowercase label
    first_seen    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    deleted_at    TEXT,
    device_id     TEXT NOT NULL
);

-- App-level uniqueness on (name, deleted_at IS NULL). NOT a UNIQUE
-- constraint (CRDT rule — no UNIQUE on mutable columns). Enforced via
-- SELECT-then-INSERT under BEGIN IMMEDIATE.
CREATE INDEX idx_tags_name_active
  ON tags(name)
  WHERE deleted_at IS NULL;

CREATE TABLE file_tags (
    id            TEXT PRIMARY KEY,       -- UUIDv7
    blake3_hash   TEXT NOT NULL,
    tag_id        TEXT NOT NULL,
    first_seen    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    deleted_at    TEXT,
    device_id     TEXT NOT NULL
);

-- Composite covering index: "does this file have this tag" + "list tags on file".
CREATE INDEX idx_file_tags_hash_tag_active
  ON file_tags(blake3_hash, tag_id)
  WHERE deleted_at IS NULL;

-- Reverse lookup: "which files have this tag" (sidebar filter, CLI ls --tag).
CREATE INDEX idx_file_tags_tag_active
  ON file_tags(tag_id)
  WHERE deleted_at IS NULL;
