-- V012: transcripts schema (transcription v1 slice).
--
-- Per CLAUDE.md "Schema ownership (dual, FTS5-scoped — Batch F)":
-- this migration owns the table + index DDL + the FTS5 virtual table +
-- non-FTS5 cascade trigger ONLY. The FTS5 maintenance triggers that
-- populate `transcript_search` from `transcript_segment` live in the
-- Rust+minijinja codegen at `crates/db/src/schema/`.
--
-- See `docs/superpowers/specs/2026-05-02-transcription-v1-design.md`
-- § "Storage — dual-ownership per Batch F" for full rationale.

-- Per-(file_uuid, backend) transcript header. Multiple rows per file
-- allowed so users can keep both fast-cloud and careful-local re-runs.
CREATE TABLE transcript (
    id              TEXT PRIMARY KEY,                  -- UUIDv7 (lowercase hex)
    file_uuid       TEXT NOT NULL,                     -- FK to files.file_uuid (immutable
                                                       -- surrogate per V011); no CASCADE
    backend         TEXT NOT NULL,                     -- "groq:whisper-large-v3-turbo"
    language        TEXT,                              -- BCP-47 short, NULL = unknown
    duration_ms     INTEGER NOT NULL,
    completed_at    TEXT,                              -- ISO 8601; nullable so V013
                                                       -- (resumability) can use NULL
                                                       -- for in-progress transcripts.
                                                       -- v1 always sets it (no transcript
                                                       -- row exists until success).
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    device_id       TEXT NOT NULL,
    hlc             INTEGER NOT NULL,
    deleted_at      TEXT
);
CREATE INDEX ix_transcript_file_uuid   ON transcript(file_uuid) WHERE deleted_at IS NULL;
CREATE INDEX ix_transcript_backend     ON transcript(backend)   WHERE deleted_at IS NULL;
-- Note: NO UNIQUE on (file_uuid, backend) — re-transcribing creates a new row;
-- "no UNIQUE on mutable columns" rule + "multiple transcripts per (media, backend)"
-- design intent.

-- One row per segment.
CREATE TABLE transcript_segment (
    id              TEXT PRIMARY KEY,                  -- UUIDv7
    transcript_id   TEXT NOT NULL,                     -- FK to transcript.id; no CASCADE
    start_ms        INTEGER NOT NULL,
    end_ms          INTEGER NOT NULL,
    text            TEXT NOT NULL,
    confidence      REAL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    device_id       TEXT NOT NULL,
    hlc             INTEGER NOT NULL,
    deleted_at      TEXT
);
CREATE INDEX ix_segment_transcript     ON transcript_segment(transcript_id, start_ms)
    WHERE deleted_at IS NULL;

-- FTS5 virtual table is structural (DDL); lives in refinery, NOT codegen.
-- (Codegen owns the maintenance TRIGGERS that fan changes into this table.)
-- Tokenizer: unicode61 + remove_diacritics 2 — multilingual; explicitly NOT
-- `porter` (would silently produce wrong stems for non-English transcripts).
-- Tokenizer note: existing `search_index` (V006) uses default `unicode61`
-- (no remove_diacritics arg, which defaults to "1" — strips combining
-- diacritical marks but NOT all Unicode-normalized accent forms).
-- transcript_search uses `remove_diacritics 2` deliberately — transcripts
-- are pure spoken-content text where diacritic-insensitive matching helps
-- the most (a user typing "cafe" should find "café"); the existing
-- search_index covers filenames + paths where remove_diacritics 2 could
-- produce surprising matches across user-named files. Divergence is
-- intentional, not accidental. If consistency becomes desired, migrate
-- search_index to remove_diacritics 2 in its own slice with the
-- `INSERT INTO search_index(search_index) VALUES('rebuild')` ritual.
CREATE VIRTUAL TABLE transcript_search USING fts5(
    text,
    content='transcript_segment',
    content_rowid='rowid',
    tokenize = 'unicode61 remove_diacritics 2'
);

-- Non-FTS5 trigger: cascade soft-delete + restore from transcript to its
-- segments. Lives in refinery (NOT a search-maintenance trigger; this
-- is row-version propagation across an FK relationship).
--
-- WHY rename from spec-draft `trg_transcript_au_cascade` to
-- `transcript_segment_after_transcript_update_cascade`: matches the
-- codebase trigger-naming convention `<index>_after_<table>_<event>`
-- (e.g. `search_after_metadata_insert`).
CREATE TRIGGER transcript_segment_after_transcript_update_cascade
AFTER UPDATE ON transcript
BEGIN
    -- DELETE arm
    UPDATE transcript_segment
       SET deleted_at = NEW.deleted_at,
           updated_at = NEW.updated_at,
           hlc        = NEW.hlc
     WHERE transcript_id = NEW.id
       AND OLD.deleted_at IS NULL
       AND NEW.deleted_at IS NOT NULL;

    -- RESTORE arm (V007→V008 bug class explicitly avoided)
    UPDATE transcript_segment
       SET deleted_at = NULL,
           updated_at = NEW.updated_at,
           hlc        = NEW.hlc
     WHERE transcript_id = NEW.id
       AND OLD.deleted_at IS NOT NULL
       AND NEW.deleted_at IS NULL;
END;
