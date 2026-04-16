-- WHY: `file_metadata.blake3_hash` is the PK and shares the content-addressed
-- identity of `files.blake3_hash`. The BLAKE3 digest is deterministic, so
-- two devices extracting metadata for the same bytes can merge their rows
-- without UUIDv7 coordination. This matches the rationale in V001 for
-- `files.blake3_hash` being the PK.
--
-- WHY no FK to files(blake3_hash): CLAUDE.md bans FK cascades for CRDT
-- safety. We keep the relationship at the app level via JOINs on
-- `blake3_hash`, which is safe even when a `files` row is soft-deleted
-- (the join simply yields zero rows).
CREATE TABLE file_metadata (
    blake3_hash    TEXT PRIMARY KEY,
    width          INTEGER,
    height         INTEGER,
    duration_ms    INTEGER,
    captured_at    TEXT,
    camera_make    TEXT,
    camera_model   TEXT,
    codec          TEXT,
    bitrate_bps    INTEGER,
    mime_type      TEXT,
    -- CRDT columns (every mutable row rule):
    extracted_at   TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    deleted_at     TEXT,
    device_id      TEXT NOT NULL
);

-- WHY partial index: most rows have NULL captured_at (only images + video
-- have EXIF/container capture timestamps). Filtering out NULL + soft-deleted
-- rows keeps the index lean — in a typical photo library this saves ~80%
-- of the index size versus an unconditional index on captured_at.
CREATE INDEX idx_file_metadata_captured
    ON file_metadata(captured_at)
    WHERE captured_at IS NOT NULL AND deleted_at IS NULL;
