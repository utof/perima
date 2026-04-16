# Phase 1b — DB Adapter + `perima ls` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire `crates/db` as a rusqlite adapter implementing `FileRepository`. `perima scan <path>` persists files + locations to SQLite. `perima ls` reads and prints them. The phase-1a `--dry-run` guard is removed; `--dry-run` becomes optional (default off).

**Architecture:** `crates/db` gets: `errors.rs` (rich `From<Error> for CoreError`), `connection.rs` (WAL + synchronous=NORMAL pragmas), `migrations/V001__initial.sql` (CRDT-ready schema), `file_repo.rs` (`SqliteFileRepository` impls `FileRepository`). `crates/cli` gets: `cmd/ls.rs` (new), `cmd/scan.rs` (drop phase-1a guard, add DB persist path), `main.rs` (add `Ls` command variant). Sentinel `VolumeId(Uuid::nil())` used until 1c wires real volume detection.

**Tech Stack:** rusqlite 0.38 (bundled), refinery 0.9, chrono 0.4, uuid v7, thiserror, tracing. Existing: blake3, walkdir, clap 4, rayon, ctrlc, tempfile, insta.

**Spec:** `docs/superpowers/specs/2026-04-16-phase-1b-db-ls-design.md`

**Execution rule:** All work on `main`. Per-commit: execute → `just ci` green → reviewer approves → commit. No branches, no `--force`.

---

## File Structure

**New/modified in this phase:**

```
Cargo.toml                              # modify — add chrono workspace dep
crates/db/
├── Cargo.toml                          # modify — add deps
├── migrations/
│   └── V001__initial.sql               # new — CRDT-ready schema
└── src/
    ├── lib.rs                          # modify — re-exports
    ├── errors.rs                       # new
    ├── connection.rs                   # new
    └── file_repo.rs                    # new

crates/cli/
├── Cargo.toml                          # modify — add perima-db + chrono
├── src/
│   ├── main.rs                         # modify — add Ls command
│   └── cmd/
│       ├── mod.rs                      # modify — add ls module
│       ├── scan.rs                     # modify — remove guard, add DB path
│       └── ls.rs                       # new
└── tests/
    ├── scan_dry_run.rs                 # modify — update tests
    ├── scan_persists.rs                # new
    └── ls_output.rs                    # new
```

Three reviewer-gated commits: (1) db crate, (2) CLI changes, (3) integration tests.

---

## Task 1: Workspace + crate dependency setup

**Files:**
- Modify: `Cargo.toml` (workspace deps)
- Modify: `crates/db/Cargo.toml`
- Modify: `crates/cli/Cargo.toml`

- [ ] **Step 1: Add `chrono` to workspace dependencies**

In root `Cargo.toml`, add to `[workspace.dependencies]` before the `[profile.release]` section:

```toml
chrono = { version = "0.4", features = ["serde"] }
```

- [ ] **Step 2: Update `crates/db/Cargo.toml`**

Replace contents with:

```toml
[package]
name = "perima-db"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
perima-core = { path = "../core" }
rusqlite.workspace  = true
refinery.workspace  = true
thiserror.workspace = true
tracing.workspace   = true
uuid.workspace      = true
chrono.workspace    = true

[dev-dependencies]
tempfile.workspace = true

[lints]
workspace = true
```

- [ ] **Step 3: Add `perima-db` + `chrono` to `crates/cli/Cargo.toml`**

Add after the `perima-fs` line in `[dependencies]`:

```toml
perima-db = { path = "../db" }
chrono.workspace = true
```

- [ ] **Step 4: Verify resolution**

Run: `cargo metadata --format-version 1 >/dev/null && echo ok`
Expected: `ok`.

---

## Task 2: DB errors (`crates/db/src/errors.rs`)

**Files:**
- Create: `crates/db/src/errors.rs`

- [ ] **Step 1: Write the module**

```rust
//! Internal errors for the database adapter.

use thiserror::Error;

/// Errors raised inside `perima-db`.
#[derive(Debug, Error)]
pub enum Error {
    /// Low-level `rusqlite` failure.
    #[error("rusqlite: {0}")]
    Rusqlite(#[from] rusqlite::Error),

    /// Migration failure via `refinery`.
    #[error("migration: {0}")]
    Refinery(String),

    /// Application-level uniqueness violation (no DB UNIQUE constraint;
    /// enforced in code per CLAUDE.md CRDT rules).
    #[error("app-level duplicate: {0}")]
    AppLevelDuplicate(String),
}

impl From<Error> for perima_core::CoreError {
    fn from(e: Error) -> Self {
        match &e {
            Error::Rusqlite(inner) => match inner {
                rusqlite::Error::QueryReturnedNoRows => Self::NotFound(e.to_string()),
                rusqlite::Error::SqliteFailure(f, _)
                    if f.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    Self::Duplicate(e.to_string())
                }
                _ => Self::Internal(e.to_string()),
            },
            Error::AppLevelDuplicate(_) => Self::Duplicate(e.to_string()),
            Error::Refinery(_) => Self::Internal(e.to_string()),
        }
    }
}
```

---

## Task 3: DB connection + migrations

**Files:**
- Create: `crates/db/src/connection.rs`
- Create: `crates/db/migrations/V001__initial.sql`

- [ ] **Step 1: Create `connection.rs`**

```rust
//! Database connection factory with production pragmas.

use std::path::Path;

use rusqlite::Connection;

use crate::errors::Error;

mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("migrations");
}

/// Open (or create) the main database at `path`, apply pragmas,
/// and run pending migrations.
///
/// WHY WAL: Write-Ahead Logging allows concurrent reads during a
/// scan write transaction. Without it, SQLite's default rollback
/// journal serializes all access, making `perima ls` block while
/// `perima scan` is running.
///
/// WHY synchronous=NORMAL: under WAL, NORMAL is safe against data
/// loss on process crash (only OS crash can lose the last txn).
/// FULL would fsync every commit — measurably slower on 100k-file
/// scans and unnecessary for a local index rebuildable from source.
///
/// # Errors
/// Returns `Error::Rusqlite` on connection/pragma failure, or
/// `Error::Refinery` on migration failure.
pub fn open_and_migrate(path: &Path) -> Result<Connection, Error> {
    let mut conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = OFF;",
    )?;
    embedded::migrations::runner()
        .run(&mut conn)
        .map_err(|e| Error::Refinery(e.to_string()))?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_sets_wal_mode() {
        let td = tempfile::tempdir().expect("tempdir");
        let conn = open_and_migrate(&td.path().join("test.db")).expect("open");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .expect("pragma");
        assert_eq!(mode, "wal");
    }

    #[test]
    fn migrations_are_idempotent() {
        let td = tempfile::tempdir().expect("tempdir");
        let db_path = td.path().join("test.db");
        let _conn1 = open_and_migrate(&db_path).expect("first open");
        drop(_conn1);
        let _conn2 = open_and_migrate(&db_path).expect("second open");
    }
}
```

- [ ] **Step 2: Create migration SQL**

Create directory: `mkdir -p crates/db/migrations`

Create `crates/db/migrations/V001__initial.sql`:

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

CREATE INDEX idx_file_locations_blake3
    ON file_locations(blake3_hash);
CREATE INDEX idx_file_locations_volume_path
    ON file_locations(volume_id, relative_path);

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

- [ ] **Step 3: Run the gates**

Run: `cargo build -p perima-db`
Expected: exit 0. (refinery embeds the migration at compile time.)

Run: `cargo test -p perima-db`
Expected: 2 tests pass (open_sets_wal_mode, migrations_are_idempotent).

---

## Task 4: `SqliteFileRepository` (`crates/db/src/file_repo.rs`) — TDD

**Files:**
- Create: `crates/db/src/file_repo.rs`
- Modify: `crates/db/src/lib.rs`

- [ ] **Step 1: Write tests first**

Create `crates/db/src/file_repo.rs` with the test module:

```rust
//! `FileRepository` implementation backed by rusqlite.

use std::path::PathBuf;

use perima_core::{
    BlakeHash, CoreError, DeviceId, FileLocationRecord, FileRepository, FileSize, HashedFile,
    LocationStatus, MediaPath, UpsertOutcome, VolumeId,
};
use rusqlite::Connection;

use crate::errors::Error;

/// Rusqlite-backed file + location repository.
pub struct SqliteFileRepository<'conn> {
    conn: &'conn Connection,
}

impl<'conn> SqliteFileRepository<'conn> {
    /// Wrap an existing connection. The caller must have run
    /// migrations before constructing this.
    pub fn new(conn: &'conn Connection) -> Self {
        Self { conn }
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_and_migrate;

    fn test_db() -> (tempfile::TempDir, Connection) {
        let td = tempfile::tempdir().expect("tempdir");
        let conn = open_and_migrate(&td.path().join("test.db")).expect("open");
        (td, conn)
    }

    fn sample_hashed_file(content: &[u8], rel_path: &str) -> HashedFile {
        let hash = BlakeHash::from_bytes(*blake3::hash(content).as_bytes());
        HashedFile {
            discovered: perima_core::DiscoveredFile {
                absolute_path: PathBuf::from("/tmp/fake"),
                relative_path: MediaPath::new(rel_path),
                size: FileSize(content.len() as u64),
            },
            hash,
        }
    }

    fn device() -> DeviceId {
        DeviceId::new()
    }

    fn sentinel_volume() -> VolumeId {
        VolumeId(uuid::Uuid::nil())
    }

    #[test]
    fn upsert_file_inserts_new() {
        let (_td, conn) = test_db();
        let mut repo = SqliteFileRepository::new(&conn);
        let f = sample_hashed_file(b"hello", "a.txt");
        let out = repo.upsert_file(&f, device()).expect("upsert");
        assert_eq!(out, UpsertOutcome::Inserted);
    }

    #[test]
    fn upsert_file_unchanged_on_repeat() {
        let (_td, conn) = test_db();
        let mut repo = SqliteFileRepository::new(&conn);
        let dev = device();
        let f = sample_hashed_file(b"hello", "a.txt");
        repo.upsert_file(&f, dev).expect("first");
        let out = repo.upsert_file(&f, dev).expect("second");
        assert_eq!(out, UpsertOutcome::Unchanged);
    }

    #[test]
    fn upsert_file_updated_on_size_change() {
        let (_td, conn) = test_db();
        let mut repo = SqliteFileRepository::new(&conn);
        let dev = device();
        let f1 = sample_hashed_file(b"hello", "a.txt");
        repo.upsert_file(&f1, dev).expect("first");
        // Same hash, different size (contrived but tests the branch).
        let mut f2 = f1.clone();
        f2.discovered.size = FileSize(999);
        let out = repo.upsert_file(&f2, dev).expect("second");
        assert_eq!(out, UpsertOutcome::Updated);
    }

    #[test]
    fn upsert_location_inserts_new() {
        let (_td, conn) = test_db();
        let mut repo = SqliteFileRepository::new(&conn);
        let dev = device();
        let f = sample_hashed_file(b"hello", "a.txt");
        repo.upsert_file(&f, dev).expect("file");
        let out = repo
            .upsert_location(&f.hash, sentinel_volume(), &f.discovered.relative_path, dev)
            .expect("loc");
        assert_eq!(out, UpsertOutcome::Inserted);
    }

    #[test]
    fn upsert_location_unchanged_on_repeat() {
        let (_td, conn) = test_db();
        let mut repo = SqliteFileRepository::new(&conn);
        let dev = device();
        let f = sample_hashed_file(b"hello", "a.txt");
        repo.upsert_file(&f, dev).expect("file");
        let vol = sentinel_volume();
        let path = &f.discovered.relative_path;
        repo.upsert_location(&f.hash, vol, path, dev).expect("first");
        let out = repo
            .upsert_location(&f.hash, vol, path, dev)
            .expect("second");
        assert_eq!(out, UpsertOutcome::Unchanged);
    }

    #[test]
    fn upsert_location_updated_on_hash_change() {
        let (_td, conn) = test_db();
        let mut repo = SqliteFileRepository::new(&conn);
        let dev = device();
        let f1 = sample_hashed_file(b"hello", "a.txt");
        let f2 = sample_hashed_file(b"world", "a.txt");
        repo.upsert_file(&f1, dev).expect("file1");
        repo.upsert_file(&f2, dev).expect("file2");
        let vol = sentinel_volume();
        let path = MediaPath::new("a.txt");
        repo.upsert_location(&f1.hash, vol, &path, dev)
            .expect("first");
        let out = repo
            .upsert_location(&f2.hash, vol, &path, dev)
            .expect("second");
        assert_eq!(out, UpsertOutcome::Updated);
    }

    #[test]
    fn list_file_locations_returns_all() {
        let (_td, conn) = test_db();
        let mut repo = SqliteFileRepository::new(&conn);
        let dev = device();
        let vol = sentinel_volume();
        for (i, name) in ["a.txt", "b.txt", "c.txt"].iter().enumerate() {
            let f = sample_hashed_file(format!("content{i}").as_bytes(), name);
            repo.upsert_file(&f, dev).expect("file");
            repo.upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
                .expect("loc");
        }
        let results = repo.list_file_locations(100, None).expect("list");
        assert_eq!(results.len(), 3);
        // Ordered by relative_path.
        assert_eq!(results[0].relative_path.as_str(), "a.txt");
        assert_eq!(results[2].relative_path.as_str(), "c.txt");
    }

    #[test]
    fn list_file_locations_respects_limit() {
        let (_td, conn) = test_db();
        let mut repo = SqliteFileRepository::new(&conn);
        let dev = device();
        let vol = sentinel_volume();
        for i in 0..5 {
            let f = sample_hashed_file(format!("c{i}").as_bytes(), &format!("f{i}.txt"));
            repo.upsert_file(&f, dev).expect("file");
            repo.upsert_location(&f.hash, vol, &f.discovered.relative_path, dev)
                .expect("loc");
        }
        let results = repo.list_file_locations(2, None).expect("list");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn list_file_locations_filters_by_volume() {
        let (_td, conn) = test_db();
        let mut repo = SqliteFileRepository::new(&conn);
        let dev = device();
        let vol_a = VolumeId::new();
        let vol_b = VolumeId::new();
        let f1 = sample_hashed_file(b"alpha", "a.txt");
        let f2 = sample_hashed_file(b"beta", "b.txt");
        repo.upsert_file(&f1, dev).expect("f1");
        repo.upsert_file(&f2, dev).expect("f2");
        repo.upsert_location(&f1.hash, vol_a, &f1.discovered.relative_path, dev)
            .expect("loc1");
        repo.upsert_location(&f2.hash, vol_b, &f2.discovered.relative_path, dev)
            .expect("loc2");
        let a_only = repo
            .list_file_locations(100, Some(vol_a))
            .expect("list");
        assert_eq!(a_only.len(), 1);
        assert_eq!(a_only[0].relative_path.as_str(), "a.txt");
    }
}
```

- [ ] **Step 2: Run — expect compile error (no impl yet)**

Run: `cargo test -p perima-db --lib`
Expected: compile error — `SqliteFileRepository` doesn't implement `FileRepository`.

- [ ] **Step 3: Implement `FileRepository` for `SqliteFileRepository`**

Add to `crates/db/src/file_repo.rs` (above the `#[cfg(test)]` block):

```rust
impl FileRepository for SqliteFileRepository<'_> {
    fn upsert_file(
        &mut self,
        file: &HashedFile,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError> {
        let hash_hex = file.hash.to_hex();
        let now = now_iso();
        let dev_str = device.0.to_string();

        // WHY: two-statement SELECT-then-INSERT/UPDATE because
        // SQLite's changes() cannot distinguish a fresh INSERT from
        // a conflict-triggered UPDATE — both report 1.
        let existing: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT file_size, device_id FROM files WHERE blake3_hash = ?1",
                [&hash_hex],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Error::from)?;

        match existing {
            None => {
                self.conn
                    .execute(
                        "INSERT INTO files (blake3_hash, file_size, first_seen, updated_at, device_id)
                         VALUES (?1, ?2, ?3, ?3, ?4)",
                        rusqlite::params![hash_hex, file.discovered.size.0 as i64, now, dev_str],
                    )
                    .map_err(Error::from)?;
                Ok(UpsertOutcome::Inserted)
            }
            Some((existing_size, existing_dev))
                if existing_size == file.discovered.size.0 as i64
                    && existing_dev == dev_str =>
            {
                Ok(UpsertOutcome::Unchanged)
            }
            Some(_) => {
                self.conn
                    .execute(
                        "UPDATE files SET file_size = ?1, updated_at = ?2, device_id = ?3
                         WHERE blake3_hash = ?4",
                        rusqlite::params![
                            file.discovered.size.0 as i64,
                            now,
                            dev_str,
                            hash_hex
                        ],
                    )
                    .map_err(Error::from)?;
                Ok(UpsertOutcome::Updated)
            }
        }
    }

    fn upsert_location(
        &mut self,
        hash: &BlakeHash,
        volume: VolumeId,
        path: &MediaPath,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError> {
        let hash_hex = hash.to_hex();
        let vol_str = volume.0.to_string();
        let path_str = path.as_str();
        let dev_str = device.0.to_string();
        let now = now_iso();

        // WHY: app-level uniqueness on (volume_id, relative_path,
        // deleted_at IS NULL) replaces a UNIQUE constraint that
        // CLAUDE.md forbids on mutable columns. The two-statement
        // pattern is safe under SQLite's single-writer model.
        let existing: Option<(String, String, String)> = self
            .conn
            .query_row(
                "SELECT id, blake3_hash, device_id FROM file_locations
                 WHERE volume_id = ?1 AND relative_path = ?2 AND deleted_at IS NULL",
                rusqlite::params![vol_str, path_str],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(Error::from)?;

        match existing {
            None => {
                let id = perima_core::ids::new_id().to_string();
                self.conn
                    .execute(
                        "INSERT INTO file_locations
                         (id, blake3_hash, volume_id, relative_path, status,
                          first_seen, updated_at, device_id)
                         VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, ?6)",
                        rusqlite::params![id, hash_hex, vol_str, path_str, now, dev_str],
                    )
                    .map_err(Error::from)?;
                Ok(UpsertOutcome::Inserted)
            }
            Some((_, ref existing_hash, ref existing_dev))
                if *existing_hash == hash_hex && *existing_dev == dev_str =>
            {
                Ok(UpsertOutcome::Unchanged)
            }
            Some((ref row_id, _, _)) => {
                self.conn
                    .execute(
                        "UPDATE file_locations
                         SET blake3_hash = ?1, updated_at = ?2, device_id = ?3
                         WHERE id = ?4",
                        rusqlite::params![hash_hex, now, dev_str, row_id],
                    )
                    .map_err(Error::from)?;
                Ok(UpsertOutcome::Updated)
            }
        }
    }

    fn list_file_locations(
        &self,
        limit: usize,
        volume: Option<VolumeId>,
    ) -> Result<Vec<FileLocationRecord>, CoreError> {
        let vol_filter = volume.map(|v| v.0.to_string());
        let mut stmt = self
            .conn
            .prepare(
                "SELECT f.blake3_hash, f.file_size, fl.volume_id, fl.relative_path,
                        fl.status, fl.first_seen
                 FROM file_locations fl
                 JOIN files f ON f.blake3_hash = fl.blake3_hash
                 WHERE fl.deleted_at IS NULL
                   AND (?1 IS NULL OR fl.volume_id = ?1)
                 ORDER BY fl.relative_path
                 LIMIT ?2",
            )
            .map_err(Error::from)?;

        let rows = stmt
            .query_map(
                rusqlite::params![vol_filter, limit as i64],
                |row| {
                    let hash_hex: String = row.get(0)?;
                    let size: i64 = row.get(1)?;
                    let vol_str: String = row.get(2)?;
                    let rel_path: String = row.get(3)?;
                    let status_str: String = row.get(4)?;
                    let first_seen: String = row.get(5)?;
                    Ok((hash_hex, size, vol_str, rel_path, status_str, first_seen))
                },
            )
            .map_err(Error::from)?;

        let mut out = Vec::new();
        for row in rows {
            let (hash_hex, size, vol_str, rel_path, status_str, first_seen) =
                row.map_err(Error::from)?;
            let hash = BlakeHash::parse_hex(&hash_hex)?;
            let volume_id = VolumeId(
                uuid::Uuid::parse_str(&vol_str)
                    .map_err(|e| CoreError::Internal(format!("bad volume uuid: {e}")))?,
            );
            let status = match status_str.as_str() {
                "active" => LocationStatus::Active,
                "missing" => LocationStatus::Missing,
                "moved" => LocationStatus::Moved,
                other => {
                    return Err(CoreError::Internal(format!(
                        "unknown location status: {other}"
                    )))
                }
            };
            out.push(FileLocationRecord {
                hash,
                size: FileSize(size as u64),
                volume_id,
                relative_path: MediaPath::new(&rel_path),
                status,
                first_seen,
            });
        }
        Ok(out)
    }
}

use rusqlite::OptionalExtension;
```

Note: `use rusqlite::OptionalExtension;` must be at the module level (not inside `impl`) for the `.optional()` call to work. Place it with the other imports at the top of the file.

- [ ] **Step 4: Update `crates/db/src/lib.rs`**

```rust
//! SQLite adapter for perima.

pub mod connection;
pub mod errors;
pub mod file_repo;

pub use connection::open_and_migrate;
pub use errors::Error;
pub use file_repo::SqliteFileRepository;
```

- [ ] **Step 5: Add `blake3` dev-dep to `crates/db/Cargo.toml`**

The tests use `blake3::hash` to generate sample hashes. Add:

```toml
blake3.workspace = true
```

under `[dev-dependencies]`.

- [ ] **Step 6: Run the gates**

Run: `cargo test -p perima-db` → expect 11 tests pass (2 connection + 9 file_repo).
Run: `just ci` → exit 0.

- [ ] **Step 7: Dispatch reviewer (checkpoint #1 — db crate)**

Reviewer checklist:
- [ ] Schema matches spec (CRDT rules, WHY comment on blake3_hash PK).
- [ ] WAL + synchronous=NORMAL pragmas applied, WHY comments present.
- [ ] `open_and_migrate` runs both pragmas and migrations.
- [ ] `upsert_file`: SELECT-then-INSERT/UPDATE, returns correct outcomes.
- [ ] `upsert_location`: app-level uniqueness, WHY comment, correct outcomes.
- [ ] `list_file_locations`: JOIN query, volume filter, limit, path ordering.
- [ ] `From<Error> for CoreError`: `QueryReturnedNoRows`→`NotFound`, constraint→`Duplicate`.
- [ ] 11 tests pass. `just ci` green.

- [ ] **Step 8: Commit (after APPROVED)**

```bash
git add Cargo.toml Cargo.lock crates/db/
git commit -m "$(cat <<'EOF'
feat(phase-1b): rusqlite DB adapter with FileRepository

crates/db wired with: connection.rs (WAL + synchronous=NORMAL
pragmas), V001__initial.sql (CRDT-ready schema — files,
file_locations, volumes, volume_mounts), SqliteFileRepository
implementing FileRepository with two-statement SELECT-then-
INSERT/UPDATE for upsert_file and upsert_location (app-level
uniqueness replaces UNIQUE constraint per CLAUDE.md CRDT rules).

Rich error mapping: rusqlite QueryReturnedNoRows → CoreError::
NotFound, constraint violations → Duplicate, refinery failures
→ Internal.

11 unit tests: WAL pragma, idempotent migration, insert/unchanged/
updated for both upsert_file and upsert_location, list with
ordering/limit/volume-filter.

Refs: docs/superpowers/specs/2026-04-16-phase-1b-db-ls-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: CLI — update `scan.rs` to persist

**Files:**
- Modify: `crates/cli/src/cmd/scan.rs`

- [ ] **Step 1: Rewrite `scan.rs`**

Replace the entire contents of `crates/cli/src/cmd/scan.rs` with:

```rust
//! `perima scan` implementation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use perima_core::{
    BlakeHash, CoreError, DeviceId, DiscoveredFile, FileRepository, HashService, MediaPath,
    Scanner, UpsertOutcome, VolumeId,
};
use rayon::prelude::*;

use crate::signals::Cancellation;

/// Arguments for the scan command.
#[derive(Debug, Clone)]
pub struct ScanArgs {
    /// Root directory to walk.
    pub root: PathBuf,
    /// When true, hashes and prints but skips all DB writes.
    pub dry_run: bool,
    /// Suppress per-file stdout lines; print summary only.
    pub quiet: bool,
}

/// Scan statistics.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanStats {
    /// Files newly indexed.
    pub new: u64,
    /// Files already present (unchanged).
    pub existing: u64,
    /// Files that errored during hash or persist.
    pub errors: u64,
}

/// Exit code returned to `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Completed normally.
    Success,
    /// Ctrl-C received; partial scan summarized.
    Interrupted,
}

/// Execute `scan`.
///
/// # Errors
/// Returns `CoreError::InvalidPath` if `root` is not a directory;
/// propagates `CoreError` from hashing and walking.
pub fn run<S, H, R>(
    scanner: &S,
    hasher: &H,
    repo: Option<&mut R>,
    device: DeviceId,
    volume: VolumeId,
    cancel: &Cancellation,
    args: &ScanArgs,
) -> Result<(ExitCode, ScanStats), CoreError>
where
    S: Scanner + ?Sized,
    H: HashService + ?Sized,
    R: FileRepository + ?Sized,
{
    validate_root(&args.root)?;

    let canonical_root = canonicalize_for_walk(&args.root)?;
    let volume_root = canonical_root.clone();
    let stdout = std::io::stdout();
    let mut stats = ScanStats::default();

    let discovered: Vec<DiscoveredFile> = scanner
        .walk(&canonical_root, &volume_root)?
        .take_while(|_| !cancel.cancelled())
        .collect();

    let cancel_flag = cancel.token();
    let results: Vec<Result<(DiscoveredFile, BlakeHash), CoreError>> = discovered
        .into_par_iter()
        .map(|d| {
            if cancel_flag.load(Ordering::SeqCst) {
                return Err(CoreError::Internal("cancelled".into()));
            }
            let h = hasher.full_hash(&d.absolute_path)?;
            Ok((d, h))
        })
        .collect();

    let mut handle = stdout.lock();
    for res in results {
        match res {
            Ok((d, h)) => {
                if !args.quiet {
                    writeln!(
                        handle,
                        "{}  {}  {}",
                        h.to_hex(),
                        d.size.0,
                        d.relative_path.as_str()
                    )
                    .map_err(CoreError::Io)?;
                }
                if let Some(ref mut r) = repo {
                    match persist_file(*r, &d, &h, device, volume) {
                        Ok(outcome) => match outcome {
                            UpsertOutcome::Inserted => stats.new += 1,
                            UpsertOutcome::Updated | UpsertOutcome::Unchanged => {
                                stats.existing += 1;
                            }
                        },
                        Err(e) => {
                            tracing::warn!(error = %e, "persist failed");
                            stats.errors += 1;
                        }
                    }
                } else {
                    stats.new += 1;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "skipping file: hash failed");
                stats.errors += 1;
            }
        }
    }
    drop(handle);

    let interrupted = cancel.cancelled();
    let suffix = if interrupted { " (interrupted)" } else { "" };
    if args.dry_run {
        let total = stats.new + stats.existing + stats.errors;
        eprintln!("scanned {total} files (dry-run; DB not wired){suffix}");
    } else {
        let vol_short = &volume.0.to_string()[..8];
        eprintln!(
            "scanned {} files on volume {vol_short} ({} new, {} existing, {} errors){suffix}",
            stats.new + stats.existing + stats.errors,
            stats.new,
            stats.existing,
            stats.errors
        );
    }

    Ok((
        if interrupted {
            ExitCode::Interrupted
        } else {
            ExitCode::Success
        },
        stats,
    ))
}

fn persist_file<R: FileRepository + ?Sized>(
    repo: &mut R,
    d: &DiscoveredFile,
    h: &BlakeHash,
    device: DeviceId,
    volume: VolumeId,
) -> Result<UpsertOutcome, CoreError> {
    let hf = perima_core::HashedFile {
        discovered: d.clone(),
        hash: *h,
    };
    repo.upsert_file(&hf, device)?;
    repo.upsert_location(h, volume, &d.relative_path, device)
}

fn validate_root(root: &Path) -> Result<(), CoreError> {
    if !root.exists() {
        return Err(CoreError::InvalidPath(format!(
            "does not exist: {}",
            root.display()
        )));
    }
    if !root.is_dir() {
        return Err(CoreError::InvalidPath(format!(
            "not a directory: {}",
            root.display()
        )));
    }
    Ok(())
}

fn canonicalize_for_walk(root: &Path) -> Result<PathBuf, CoreError> {
    dunce::canonicalize(root).map_err(CoreError::Io)
}
```

Note: `repo` is `Option<&mut R>` — when `dry_run` is true, the caller passes `None`; when false, passes `Some(&mut sqlite_repo)`. This avoids the old `Unsupported` guard entirely.

- [ ] **Step 2: Run gates**

Run: `cargo build -p perima` → exit 0 (will fail until main.rs is updated in Task 7).

Expected: compile errors in main.rs because `scan::run` signature changed. Continue to Task 6+7.

---

## Task 6: CLI — `cmd/ls.rs`

**Files:**
- Create: `crates/cli/src/cmd/ls.rs`
- Modify: `crates/cli/src/cmd/mod.rs`

- [ ] **Step 1: Create `ls.rs`**

```rust
//! `perima ls` implementation.

use std::io::Write;

use perima_core::{CoreError, FileLocationRecord, FileRepository, VolumeId};

/// Arguments for the ls command.
#[derive(Debug, Clone)]
pub struct LsArgs {
    /// Filter to a specific volume.
    pub volume: Option<VolumeId>,
    /// Maximum number of rows to return.
    pub limit: usize,
    /// Output as JSON instead of a human-readable table.
    pub json: bool,
}

/// Execute `ls`.
///
/// # Errors
/// Propagates `CoreError` from the repository.
pub fn run<R: FileRepository + ?Sized>(
    repo: &R,
    args: &LsArgs,
) -> Result<(), CoreError> {
    let records = repo.list_file_locations(args.limit, args.volume)?;

    if args.json {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        serde_json::to_writer_pretty(&mut handle, &records)
            .map_err(|e| CoreError::Internal(format!("json: {e}")))?;
        writeln!(handle).map_err(CoreError::Io)?;
    } else {
        print_table(&records)?;
    }
    Ok(())
}

fn print_table(records: &[FileLocationRecord]) -> Result<(), CoreError> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{:<10} {:<10} {:<10} {}", "HASH", "SIZE", "VOLUME", "PATH")
        .map_err(CoreError::Io)?;
    for r in records {
        let hash_short = &r.hash.to_hex()[..8];
        let vol_short = &r.volume_id.0.to_string()[..8];
        let size = format_size(r.size.0);
        writeln!(handle, "{hash_short}…  {size:<10} {vol_short}…  {}", r.relative_path.as_str())
            .map_err(CoreError::Io)?;
    }
    Ok(())
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}
```

- [ ] **Step 2: Update `cmd/mod.rs`**

```rust
//! CLI subcommand modules.

pub mod ls;
pub mod scan;
```

- [ ] **Step 3: Run gates — expect compile fail until main.rs updated**

---

## Task 7: CLI — update `main.rs`

**Files:**
- Modify: `crates/cli/src/main.rs`

- [ ] **Step 1: Full rewrite**

```rust
//! `perima` command-line entry point.

mod cmd;
mod config;
mod logging;
mod panic;
mod signals;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use perima_core::VolumeId;
use perima_db::{SqliteFileRepository, open_and_migrate};
use perima_fs::WalkdirScanner;
use perima_hash::Blake3Service;

/// Cross-platform media asset manager.
#[derive(Parser, Debug)]
#[command(
    name = "perima",
    version,
    about = "Index your media across drives by content hash"
)]
struct Cli {
    /// Bump tracing verbosity; repeatable (-v, -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Override the main database directory.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Walk a directory, hash every file, and persist to the database.
    Scan {
        /// Directory to walk.
        root: PathBuf,

        /// Dry-run mode: hash and print, skip DB writes.
        #[arg(long)]
        dry_run: bool,

        /// Suppress per-file stdout lines.
        #[arg(long)]
        quiet: bool,
    },

    /// List indexed files.
    Ls {
        /// Filter to a specific volume UUID.
        #[arg(long)]
        volume: Option<String>,

        /// Maximum rows to return.
        #[arg(long, default_value = "100")]
        limit: usize,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    panic::install();
    let cli = Cli::parse();

    if let Err(e) = logging::init(cli.verbose) {
        eprintln!("perima: logging init failed: {e}");
        return ExitCode::from(1);
    }

    let cancel = match signals::install() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: signal handler install failed: {e}");
            return ExitCode::from(1);
        }
    };

    let config = match config::Config::resolve(cli.data_dir.clone()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: config resolution failed: {e}");
            return ExitCode::from(1);
        }
    };

    match cli.command {
        Command::Scan {
            root,
            dry_run,
            quiet,
        } => {
            let args = cmd::scan::ScanArgs {
                root,
                dry_run,
                quiet,
            };
            let scanner = WalkdirScanner::new();
            let hasher = Blake3Service::new();

            // WHY: sentinel VolumeId (all-zeros) is used until
            // phase 1c wires real volume detection. 1c will UPDATE
            // file_locations SET volume_id = <real> WHERE volume_id
            // = '00000000-...' after resolving actual volumes.
            let volume = VolumeId(uuid::Uuid::nil());

            if dry_run {
                // WHY 'static: the type parameter R is never
                // instantiated (repo = None), but Rust needs a
                // concrete type for inference. 'static is valid
                // because no SqliteFileRepository is actually created.
                match cmd::scan::run::<_, _, perima_db::SqliteFileRepository<'static>>(
                    &scanner,
                    &hasher,
                    None,
                    config.device_id,
                    volume,
                    &cancel,
                    &args,
                ) {
                    Ok((cmd::scan::ExitCode::Success, _)) => ExitCode::from(0),
                    Ok((cmd::scan::ExitCode::Interrupted, _)) => ExitCode::from(130),
                    Err(perima_core::CoreError::InvalidPath(msg)) => {
                        eprintln!("perima: {msg}");
                        ExitCode::from(2)
                    }
                    Err(e) => {
                        eprintln!("perima: {e}");
                        ExitCode::from(1)
                    }
                }
            } else {
                let db_path = config.data_dir.join("perima.db");
                let conn = match open_and_migrate(&db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("perima: database: {e}");
                        return ExitCode::from(1);
                    }
                };
                let mut repo = SqliteFileRepository::new(&conn);
                match cmd::scan::run(
                    &scanner,
                    &hasher,
                    Some(&mut repo),
                    config.device_id,
                    volume,
                    &cancel,
                    &args,
                ) {
                    Ok((cmd::scan::ExitCode::Success, _)) => ExitCode::from(0),
                    Ok((cmd::scan::ExitCode::Interrupted, _)) => ExitCode::from(130),
                    Err(perima_core::CoreError::InvalidPath(msg)) => {
                        eprintln!("perima: {msg}");
                        ExitCode::from(2)
                    }
                    Err(e) => {
                        eprintln!("perima: {e}");
                        ExitCode::from(1)
                    }
                }
            }
        }

        Command::Ls {
            volume,
            limit,
            json,
        } => {
            let volume_id = volume
                .map(|v| {
                    uuid::Uuid::parse_str(&v)
                        .map(VolumeId)
                        .map_err(|e| format!("bad volume UUID: {e}"))
                })
                .transpose();
            let volume_id = match volume_id {
                Ok(v) => v,
                Err(msg) => {
                    eprintln!("perima: {msg}");
                    return ExitCode::from(2);
                }
            };

            let db_path = config.data_dir.join("perima.db");
            let conn = match open_and_migrate(&db_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("perima: database: {e}");
                    return ExitCode::from(1);
                }
            };
            let repo = SqliteFileRepository::new(&conn);
            let args = cmd::ls::LsArgs {
                volume: volume_id,
                limit,
                json,
            };
            match cmd::ls::run(&repo, &args) {
                Ok(()) => ExitCode::from(0),
                Err(e) => {
                    eprintln!("perima: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}
```

- [ ] **Step 2: Add `serde_json` dep to cli Cargo.toml**

Add to `[dependencies]`:

```toml
serde_json.workspace = true
serde.workspace      = true
```

- [ ] **Step 3: Run the gates**

Run: `just ci` → exit 0 (all previous tests + db tests).
Run: `cargo run -p perima -- --help` → shows `scan` and `ls` subcommands.

- [ ] **Step 4: Dispatch reviewer (checkpoint #2 — CLI changes)**

Reviewer checklist:
- [ ] `scan.rs` no longer has `Unsupported` guard; `--dry-run` is optional.
- [ ] `scan` opens DB + migrates when not `dry_run`; uses sentinel `VolumeId(Uuid::nil())`.
- [ ] `ls.rs` reads via `list_file_locations`, human table + JSON paths.
- [ ] `main.rs` wires both commands; both DB-touching paths call `open_and_migrate`.
- [ ] `just ci` green. All previous 27+ tests pass.
- [ ] WHY comment on sentinel VolumeId in main.rs.

- [ ] **Step 5: Commit (after APPROVED)**

```bash
git add crates/cli/ Cargo.lock
git commit -m "$(cat <<'EOF'
feat(phase-1b): scan persists to DB, perima ls reads

scan.rs: drop phase-1a Unsupported guard; --dry-run now optional
(default off). When dry_run=false, opens DB via open_and_migrate,
constructs SqliteFileRepository, persists upsert_file +
upsert_location per hashed file. Summary prints new/existing/error
counts. Sentinel VolumeId(Uuid::nil()) until 1c volume detection.

ls.rs: new command reading via FileRepository::list_file_locations.
Human table (HASH SIZE VOLUME PATH) and --json (serde_json
pretty-print of Vec<FileLocationRecord>). --volume filter and
--limit supported.

main.rs: both scan (non-dry-run) and ls call open_and_migrate so
perima ls on a fresh install creates the DB and runs migrations.

Refs: docs/superpowers/specs/2026-04-16-phase-1b-db-ls-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Integration tests

**Files:**
- Create: `crates/cli/tests/scan_persists.rs`
- Create: `crates/cli/tests/ls_output.rs`
- Modify: `crates/cli/tests/scan_dry_run.rs`

- [ ] **Step 1: Create `scan_persists.rs`**

```rust
//! Integration test: scan without --dry-run persists to DB.

use std::io::Write;
use std::path::Path;
use std::process::Command;

fn mk_fixture(dir: &Path) {
    for (name, content) in [
        ("alpha.txt", b"alpha" as &[u8]),
        ("sub/beta.txt", b"beta"),
        ("sub/gamma.bin", b"\x00\x01\x02\x03"),
    ] {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::File::create(&path)
            .expect("create")
            .write_all(content)
            .expect("write");
    }
}

const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

#[test]
fn scan_persists_three_files() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());

    let output = Command::new(bin())
        .arg("scan")
        .arg(td.path())
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("run");

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("3 new"), "expected '3 new' in: {stderr}");

    // Open the DB directly and count rows.
    let db_path = env_dir.path().join("perima.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let file_count: i64 = conn
        .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
        .expect("count files");
    assert_eq!(file_count, 3);
    let loc_count: i64 = conn
        .query_row("SELECT count(*) FROM file_locations", [], |r| r.get(0))
        .expect("count locations");
    assert_eq!(loc_count, 3);
}

#[test]
fn rescan_produces_zero_new() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());

    let run = || {
        Command::new(bin())
            .arg("scan")
            .arg(td.path())
            .env("PERIMA_CONFIG_DIR", env_dir.path())
            .env("PERIMA_DATA_DIR", env_dir.path())
            .output()
            .expect("run")
    };

    let first = run();
    assert!(first.status.success());

    let second = run();
    assert!(second.status.success());
    let stderr = String::from_utf8(second.stderr).expect("utf8");
    assert!(stderr.contains("0 new"), "expected '0 new' in second scan: {stderr}");
}
```

- [ ] **Step 2: Create `ls_output.rs`**

```rust
//! Integration test: perima ls after scan.

use std::io::Write;
use std::path::Path;
use std::process::Command;

fn mk_fixture(dir: &Path) {
    for (name, content) in [
        ("alpha.txt", b"alpha" as &[u8]),
        ("sub/beta.txt", b"beta"),
        ("sub/gamma.bin", b"\x00\x01\x02\x03"),
    ] {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::File::create(&path)
            .expect("create")
            .write_all(content)
            .expect("write");
    }
}

const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

fn scan_first(td: &Path, env_dir: &Path) {
    let output = Command::new(bin())
        .arg("scan")
        .arg(td)
        .env("PERIMA_CONFIG_DIR", env_dir)
        .env("PERIMA_DATA_DIR", env_dir)
        .output()
        .expect("scan");
    assert!(output.status.success(), "scan failed: {}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn ls_shows_three_files() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    scan_first(td.path(), env_dir.path());

    let output = Command::new(bin())
        .arg("ls")
        .arg("--limit")
        .arg("10")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("ls");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let lines: Vec<&str> = stdout.lines().collect();
    // Header + 3 data lines.
    assert_eq!(lines.len(), 4, "expected header + 3 lines, got: {lines:?}");
}

#[test]
fn ls_json_deserializes() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    scan_first(td.path(), env_dir.path());

    let output = Command::new(bin())
        .arg("ls")
        .arg("--json")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("ls --json");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let records: Vec<perima_core::FileLocationRecord> =
        serde_json::from_str(&stdout).expect("deserialize");
    assert_eq!(records.len(), 3);
}
```

- [ ] **Step 3: Update `scan_dry_run.rs`**

Remove the `real_scan_refused_in_phase_1a` test (the guard no longer exists). Replace with:

```rust
#[test]
fn scan_without_dry_run_succeeds() {
    let td = tempfile::tempdir().expect("tempdir");
    mk_fixture(td.path());
    let tmp_env = tempfile::tempdir().expect("env dir");

    let output = Command::new(bin())
        .arg("scan")
        .arg(td.path())
        .env("PERIMA_CONFIG_DIR", tmp_env.path())
        .env("PERIMA_DATA_DIR", tmp_env.path())
        .output()
        .expect("run perima");

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stderr = String::from_utf8(output.stderr).expect("utf8");
    assert!(stderr.contains("scanned"), "missing summary in: {stderr}");
}
```

- [ ] **Step 4: Add dev-deps to `crates/cli/Cargo.toml`**

Add to `[dev-dependencies]`:

```toml
rusqlite.workspace   = true
serde_json.workspace = true
```

- [ ] **Step 5: Run the gates**

Run: `cargo test --workspace` → all tests pass (db: 11, cli unit: 2, cli integration: ~8, core: 10, core props: 3, fs: 6, hash: 3 = ~43 total).
Run: `just ci` → exit 0.

- [ ] **Step 6: Dispatch reviewer (checkpoint #3 — integration tests)**

Reviewer checklist:
- [ ] `scan_persists.rs`: 2 tests (persist + rescan-zero-new). Opens DB with raw rusqlite to assert row counts.
- [ ] `ls_output.rs`: 2 tests (human table has header+3, JSON deserializes back to `Vec<FileLocationRecord>`).
- [ ] `scan_dry_run.rs`: old guard test replaced with `scan_without_dry_run_succeeds`.
- [ ] `--dry-run` path still works (existing tests).
- [ ] All phase-1a tests still pass.
- [ ] `just ci` green.

- [ ] **Step 7: Commit (after APPROVED)**

```bash
git add crates/cli/ Cargo.lock
git commit -m "$(cat <<'EOF'
test(phase-1b): integration tests for scan persist + ls output

scan_persists.rs: scan without --dry-run writes 3 files + 3
file_locations to SQLite (verified by opening DB with raw
rusqlite); rescan produces 0 new rows.

ls_output.rs: human table has header + 3 data lines; --json
output deserializes back to Vec<FileLocationRecord>.

scan_dry_run.rs: removed phase-1a guard test (guard is gone);
added scan_without_dry_run_succeeds asserting exit 0 + summary.

Refs: docs/superpowers/specs/2026-04-16-phase-1b-db-ls-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Final sweep + push + tag

- [ ] **Step 1: Clean build sweep**

Run: `cargo clean && just ci`
Expected: exit 0.

- [ ] **Step 2: WHY-comment check**

Run: `grep -rE '^\s*//\s*WHY:' crates/db/ crates/cli/ | wc -l`
Expected: ≥ 3 new (WAL pragma, blake3_hash PK in SQL, app-level uniqueness, sentinel VolumeId).

- [ ] **Step 3: `.md` hygiene**

Run: `git ls-files '*.md'`
Expected: empty.

- [ ] **Step 4: Final reviewer (checkpoint #4)**

Reviewer checklist:
- [ ] All exit criteria B1–B11 from the spec met.
- [ ] `just ci` green.
- [ ] WHY comments present for all spec-mandated items.
- [ ] No `.md` files tracked.

- [ ] **Step 5: Push + wait CI green**

```bash
git push origin main
gh run watch --exit-status "$(gh run list --workflow=ci.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
```

- [ ] **Step 6: Tag (ONLY after CI green)**

```bash
git tag -a phase-1b-complete -m "Phase 1b: DB adapter + perima ls"
git push origin phase-1b-complete
```

---

## Self-review

**Spec coverage:**
- Schema (V001) → Task 3 ✓
- Connection + WAL pragmas → Task 3 ✓
- `db::Error` + rich `From` → Task 2 ✓
- `SqliteFileRepository` (upsert_file, upsert_location, list) → Task 4 ✓
- `open_and_migrate` called from both scan + ls → Task 7 ✓
- Sentinel `VolumeId(Uuid::nil())` → Task 7 ✓
- `scan.rs` remove guard + persist path → Task 5 ✓
- `perima ls` human + JSON → Task 6 ✓
- Integration tests → Task 8 ✓
- Exit criteria B1–B11 → Task 9 ✓

**Placeholder scan:** no TBD/TODO. Every file has complete content.

**Type consistency:** `SqliteFileRepository<'conn>` matches between file_repo.rs and main.rs. `ScanStats` + return type `(ExitCode, ScanStats)` consistent between scan.rs and main.rs. `LsArgs` consistent between ls.rs and main.rs. `FileLocationRecord` used consistently for JSON round-trip.

**Commit discipline:** three reviewer-gated commits (db, CLI, integration tests) + final push/tag. Matches CLAUDE.md "execute → tests → reviewer → commit."
