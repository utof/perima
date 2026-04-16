# Phase 1b — DB adapter + `perima ls`

**Status:** draft awaiting reviewer
**Date:** 2026-04-16
**Parent:** meta-plan, phase 1 entry (split into 1a/1b/1c).
**Prior:** `phase-1a-complete` tag.
**Sibling:** 1c (volumes + manifest + `perima volumes`).

---

## Goal

Wire `crates/db` as a real rusqlite adapter implementing
`FileRepository`. `perima scan <path>` (without `--dry-run`) walks,
hashes, and persists `files` + `file_locations` rows. `perima ls`
reads and prints them. The phase-1a `--dry-run` guard is removed;
`--dry-run` becomes optional (default off).

## Non-goals for 1b

- Volume detection, `VolumeRepository` impl, `volume_mounts`,
  `.perima/manifest.db` (1c).
- `perima volumes` command (1c).
- File watching, EventBus (phase 3).
- Thumbnails, tags, FTS5 (phases 4-5).

---

## Architecture

`crates/db` gains:
- `errors.rs` — `db::Error` with rich `From<Error> for CoreError`
  mapping (`QueryReturnedNoRows` → `NotFound`, constraint →
  `Duplicate`, else `Internal`).
- `migrations/V001__initial.sql` — tables `files`, `file_locations`
  (CRDT-ready per CLAUDE.md). `volumes` + `volume_mounts` also
  created here so migration V001 is the single initial schema
  (1c only populates them, not creates them).
- `connection.rs` — connection factory with WAL + `synchronous =
  NORMAL` pragmas.
- `file_repo.rs` — `SqliteFileRepository` impls `FileRepository`.

`crates/cli` changes:
- `cmd/scan.rs` — remove phase-1a `Unsupported` guard; `--dry-run`
  becomes optional (default false); when false, construct
  `SqliteFileRepository` and persist.
- `cmd/ls.rs` — new `perima ls` command.
- `main.rs` — add `Ls` variant to `Command` enum.

### Connection + pragmas

```rust
// crates/db/connection.rs
use rusqlite::Connection;
use crate::errors::Error;

/// Open (or create) the main database at `path` with production
/// pragmas.
///
/// WHY WAL: Write-Ahead Logging allows concurrent reads during a
/// scan write transaction. Without it, SQLite's default rollback
/// journal serializes all access, making `perima ls` block while
/// `perima scan` is running.
///
/// WHY synchronous=NORMAL: under WAL mode, NORMAL is safe against
/// data loss on process crash (only OS crash can lose the last
/// transaction). FULL would fsync every commit — measurably slower
/// on the 100k-file scan target and unnecessary for a local
/// index that can be rebuilt from source files.
pub fn open(path: &std::path::Path) -> Result<Connection, Error> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = OFF;"
    )?;
    Ok(conn)
}
```

`foreign_keys = OFF` because CLAUDE.md says "no FK cascades" and
the CRDT-ready schema enforces referential integrity in app code.

### Migration strategy

`refinery` with SQL files under `crates/db/migrations/`. The runner
lives in a public `db::migrate(conn: &mut Connection) -> Result<(),
Error>` function. The CLI calls it from a shared
`db::open_and_migrate(path)` helper that combines `connection::open`
+ `migrate`. **Both `cmd/scan.rs` (when `dry_run` is false) and
`cmd/ls.rs` call `open_and_migrate` before any query** — so
`perima ls` on a fresh install creates the DB and runs migrations
rather than crashing with "no such table."

---

## Schema (`crates/db/migrations/V001__initial.sql`)

Exact SQL per CLAUDE.md rules (UUIDv7 PKs except content-addressed
`blake3_hash`, `updated_at` + `device_id` on every mutable row,
soft deletes, no UNIQUE on mutable columns, no FK cascades):

```sql
-- WHY: blake3_hash is the PK on files because a BLAKE3-256 hash is
-- deterministic and content-derived — two devices hashing identical
-- bytes MUST compute the same value, making it CRDT-merge-safe
-- (effectively a deterministic UUID). The UUIDv7 rule applies only
-- to rows whose identity is NOT content-derived.
CREATE TABLE files (
    blake3_hash   TEXT PRIMARY KEY,
    file_size     INTEGER NOT NULL,
    first_seen    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    deleted_at    TEXT,
    device_id     TEXT NOT NULL
);

CREATE TABLE file_locations (
    id             TEXT PRIMARY KEY,
    blake3_hash    TEXT NOT NULL,
    volume_id      TEXT NOT NULL,
    relative_path  TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'active',
    last_verified  TEXT,
    first_seen     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    deleted_at     TEXT,
    device_id      TEXT NOT NULL
);

-- Non-unique indexes for read performance.
CREATE INDEX idx_file_locations_blake3
    ON file_locations(blake3_hash);
CREATE INDEX idx_file_locations_volume_path
    ON file_locations(volume_id, relative_path);

-- Created now so migration is one-shot; populated by 1c.
CREATE TABLE volumes (
    volume_id          TEXT PRIMARY KEY,
    gpt_partition_guid TEXT,
    fs_uuid            TEXT,
    volume_label       TEXT,
    capacity_bytes     INTEGER NOT NULL,
    is_removable       INTEGER NOT NULL,
    last_seen          TEXT NOT NULL,
    updated_at         TEXT NOT NULL,
    deleted_at         TEXT,
    device_id          TEXT NOT NULL
);

CREATE TABLE volume_mounts (
    id          TEXT PRIMARY KEY,
    volume_id   TEXT NOT NULL,
    machine_id  TEXT NOT NULL,
    mount_path  TEXT NOT NULL,
    first_seen  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    deleted_at  TEXT,
    device_id   TEXT NOT NULL
);

CREATE INDEX idx_volume_mounts_volume_machine
    ON volume_mounts(volume_id, machine_id);
```

---

## Error taxonomy (`crates/db/errors.rs`)

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("rusqlite: {0}")]
    Rusqlite(#[from] rusqlite::Error),
    #[error("refinery: {0}")]
    Refinery(Box<refinery::Error>),
    #[error("app-level duplicate: {0}")]
    AppLevelDuplicate(String),
}

impl From<Error> for perima_core::CoreError {
    fn from(e: Error) -> Self {
        match e {
            Error::Rusqlite(ref inner) => {
                use rusqlite::Error as RE;
                match inner {
                    RE::QueryReturnedNoRows =>
                        perima_core::CoreError::NotFound(e.to_string()),
                    RE::SqliteFailure(f, _)
                        if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                        perima_core::CoreError::Duplicate(e.to_string()),
                    _ => perima_core::CoreError::Internal(e.to_string()),
                }
            }
            Error::AppLevelDuplicate(s) =>
                perima_core::CoreError::Duplicate(s),
            Error::Refinery(_) =>
                perima_core::CoreError::Internal(e.to_string()),
        }
    }
}
```

---

## `SqliteFileRepository` (`crates/db/file_repo.rs`)

Implements `FileRepository` trait from `perima-core`.

### `upsert_file`

Two-statement approach (SELECT-then-INSERT/UPDATE) because SQLite's
`changes()` cannot distinguish a fresh INSERT from a conflict-
triggered UPDATE — both report 1.

```
BEGIN;
SELECT file_size, device_id FROM files WHERE blake3_hash = ?1;
-- If no row:
  INSERT INTO files (blake3_hash, file_size, first_seen, updated_at, device_id)
  VALUES (?1, ?2, ?3, ?3, ?4);
  → Inserted
-- If row exists and (file_size, device_id) match:
  → Unchanged (skip the UPDATE entirely)
-- If row exists and anything differs:
  UPDATE files SET file_size = ?2, updated_at = ?3, device_id = ?4
  WHERE blake3_hash = ?1;
  → Updated
COMMIT;
```

This is explicit, correct, and pairs naturally with the
`upsert_location` pattern below.

### `upsert_location`

App-level uniqueness on `(volume_id, relative_path, deleted_at IS
NULL)`. Before insert:

1. `SELECT id FROM file_locations WHERE volume_id = ?1 AND
   relative_path = ?2 AND deleted_at IS NULL`
2. If found, `blake3_hash` matches, AND `device_id` matches →
   `Unchanged` (skip the UPDATE entirely; nothing changed).
3. If found and `blake3_hash` OR `device_id` differs → UPDATE
   `blake3_hash`, `updated_at`, `device_id`; return `Updated`.
4. If not found → `INSERT`, return `Inserted`.

This is 2 statements (SELECT + INSERT/UPDATE) wrapped in a
transaction. The app-level uniqueness check replaces the `UNIQUE`
constraint that CLAUDE.md forbids on mutable columns.

### `list_file_locations`

```sql
SELECT f.blake3_hash, f.file_size, fl.volume_id, fl.relative_path,
       fl.status, fl.first_seen
FROM file_locations fl
JOIN files f ON f.blake3_hash = fl.blake3_hash
WHERE fl.deleted_at IS NULL
  AND (?1 IS NULL OR fl.volume_id = ?1)
ORDER BY fl.relative_path
LIMIT ?2
```

Maps rows into `Vec<FileLocationRecord>`.

---

## CLI changes

### `perima scan <path>` (no longer requires `--dry-run`)

- Remove the `CoreError::Unsupported` guard from `cmd/scan.rs`.
- `--dry-run` becomes `#[arg(long)]` (optional, default false).
- When `dry_run` is false:
  1. Open DB via `db::connection::open(config.data_dir.join("perima.db"))`.
  2. Run migrations.
  3. For the scan, use a **well-known sentinel `VolumeId`** —
     `VolumeId(Uuid::nil())` (all-zeros UUID). Every 1b scan writes
     the same sentinel, so `file_locations` rows are dedup-safe
     across runs. 1c replaces the sentinel by running
     `UPDATE file_locations SET volume_id = <real> WHERE volume_id
     = '00000000-0000-0000-0000-000000000000'` after real volume
     detection resolves the actual ID. Using a single well-known
     sentinel (not a random UUID per scan) makes the 1c migration
     trivial and prevents duplicate `file_locations` rows for the
     same `(volume, path)` across multiple scans.
  4. After hashing, call `upsert_file` + `upsert_location` per file.
  5. Summary changes from `(dry-run; DB not yet wired)` to
     `scanned <N> files on volume <short-id> (<K> new, <M> existing,
     <E> errors)`.
- When `dry_run` is true: same behavior as 1a (walk + hash + print,
  no DB).

### `perima ls`

Options:
- `--volume <id>` — filter to one volume (UUIDv7 hex).
- `--limit <n>` — default 100.
- `--json` — machine-readable output.

Human-readable table output:

```
HASH      SIZE       VOLUME    PATH
a1b2c3…   1.2 MB     f0e9…     photos/2024/IMG_001.jpg
```

JSON: `Vec<FileLocationRecord>` serde-serialized.

---

## Test strategy

### Unit tests (`crates/db/src/`)

- `connection::open` on a tempfile → pragmas applied (query
  `PRAGMA journal_mode` returns `wal`).
- `upsert_file` inserts a new row → `Inserted`; same row again
  → `Unchanged`; same hash different size → `Updated`.
- `upsert_location` insert → `Inserted`; same path same hash
  → `Unchanged`; same path different hash → `Updated`.
- `list_file_locations` with 3 rows → returns 3 in path order;
  with volume filter → returns subset; with limit → truncates.
- Migration runs idempotently (open + migrate twice = no error).

### Integration tests (`crates/cli/tests/`)

- `scan_persists.rs`: create 3 fixture files, run `perima scan
  <tmpdir>` (no `--dry-run`), then open the same DB with rusqlite
  and assert 3 rows in `files` + 3 in `file_locations`. Run scan
  again → 0 new rows (assert via summary line "0 new").
- `ls_output.rs`: after a scan, run `perima ls --limit 10` and
  assert 3 lines of output with correct format. Run `perima ls
  --json` and deserialize the output back into
  `Vec<FileLocationRecord>`.
- Update `scan_dry_run.rs`: remove the
  `real_scan_refused_in_phase_1a` test (guard is gone); replace
  with a test that `perima scan <tmpdir>` (no `--dry-run`) exits 0
  and produces the "scanned ... files" summary.

### Existing tests

All 27 phase-1a tests must remain green. The `--dry-run` path is
unchanged.

---

## Dependencies

Add to `crates/db/Cargo.toml`:

```toml
[dependencies]
perima-core = { path = "../core" }
rusqlite.workspace    = true
refinery.workspace    = true
thiserror.workspace   = true
tracing.workspace     = true
uuid.workspace        = true
chrono = { version = "0.4", features = ["serde"] }

[dev-dependencies]
tempfile.workspace = true
```

`chrono` for ISO 8601 UTC timestamps (`Utc::now().to_rfc3339()`).
Simpler than hand-formatting; widely used.

Add `chrono` to workspace deps:

```toml
chrono = { version = "0.4", features = ["serde"] }
```

Add `perima-db` to `crates/cli/Cargo.toml`:

```toml
perima-db = { path = "../db" }
```

---

## Exit criteria (phase 1b, autonomously verifiable)

B1. `perima scan <fixture>` (no `--dry-run`) populates 3 rows in
    `files` and 3 in `file_locations`. Verified by opening the DB
    from the integration test with rusqlite and counting rows.

B2. Re-running scan → 0 new rows (`Unchanged` for all). Verified
    by the summary line containing "0 new".

B3. `perima ls` output has 3 lines matching
    `^[0-9a-f]{8}…  <size>  <volume-short>  <path>$`. Verified via
    the integration test.

B4. `perima ls --json` output deserializes back into
    `Vec<FileLocationRecord>`.

B5. `perima scan --dry-run <fixture>` still works (unchanged path).

B6. `PRAGMA journal_mode` returns `wal` on an opened DB.

B7. Migration is idempotent (double-run, no error).

B8. All 27 phase-1a tests still pass.

B9. `just ci` green.

B10. `cargo doc --workspace --no-deps` exit 0 — all new pub items
     doc-commented.

B11. WHY-comments: blake3_hash PK rationale in migration SQL, WAL
     pragma rationale in `connection.rs`, app-level uniqueness
     rationale in `file_repo.rs`.

---

## Risks

- **Sentinel VolumeId (`Uuid::nil`).** All 1b scans share the same
  all-zeros volume ID. Multiple scans of the same directory are
  dedup-safe (same sentinel + same relative_path = `Unchanged`).
  Multiple scans of *different* directories merge into the same
  volume — 1c's `UPDATE file_locations SET volume_id = <real>
  WHERE volume_id = '00000000-...'` must resolve by re-detecting
  each file's actual volume. Acceptable: 1c is the next phase.
- **`refinery` + `rusqlite` version pairing.** Pinned in phase 0
  (`rusqlite 0.38` + `refinery 0.9`). No new risk.
- **`chrono` dependency size.** Adds ~50 KB. Acceptable.
- **Two-statement upsert concurrency.** The SELECT-then-INSERT/UPDATE
  pattern is safe under SQLite's single-writer model (WAL allows
  concurrent readers but only one writer holds the lock). No race.
