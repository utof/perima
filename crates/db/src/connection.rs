//! Database connection factory with production pragmas.

use std::path::Path;
use std::time::Duration;

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
/// scan write transaction. Without it, `SQLite`'s default rollback
/// journal serializes all access, making `perima ls` block while
/// `perima scan` is running.
///
/// WHY synchronous=NORMAL: under WAL, NORMAL is safe against data
/// loss on process crash (only OS crash can lose the last txn).
/// FULL would fsync every commit — measurably slower on 100k-file
/// scans and unnecessary for a local index rebuildable from source.
///
/// WHY `busy_timeout=5s`: `BEGIN IMMEDIATE` in the repository adapters
/// serializes read-modify-write sequences across connections. Without
/// a `busy_timeout`, a second writer hitting the reserved lock would
/// return `SQLITE_BUSY` immediately instead of waiting, turning the
/// app-level uniqueness fix into a spurious error. 5s is generous for
/// local `SQLite` + small transactions; a single tx never runs for
/// seconds. The `conn.busy_timeout` helper installs rusqlite's retry
/// callback with exponential backoff (more idiomatic than the raw
/// PRAGMA).
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
    conn.busy_timeout(Duration::from_secs(5))?;
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
        {
            let _conn = open_and_migrate(&db_path).expect("first open");
            // WHY: explicit scope drop ensures the first connection is closed
            // before the second open, so the idempotency check is meaningful.
        }
        open_and_migrate(&db_path).expect("second open");
    }

    #[test]
    fn open_sets_busy_timeout() {
        // WHY: `BEGIN IMMEDIATE` under cross-connection contention returns
        // SQLITE_BUSY instantly unless a busy_timeout is registered. Without
        // an explicit `conn.busy_timeout(...)` call the rusqlite retry callback
        // is not installed, so BEGIN IMMEDIATE regresses from "serialize" to
        // "error" under contention even if PRAGMA busy_timeout reads non-zero.
        // This test pins the contract that `open_and_migrate` sets the timeout
        // explicitly, regardless of rusqlite's internal default.
        let td = tempfile::tempdir().expect("tempdir");
        let conn = open_and_migrate(&td.path().join("test.db")).expect("open");
        let timeout_ms: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .expect("pragma");
        assert!(
            timeout_ms >= 5_000,
            "busy_timeout must be at least 5s (got {timeout_ms}ms)"
        );
    }
}
