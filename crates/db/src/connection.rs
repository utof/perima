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
/// scan write transaction. Without it, `SQLite`'s default rollback
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
        {
            let _conn = open_and_migrate(&db_path).expect("first open");
            // WHY: explicit scope drop ensures the first connection is closed
            // before the second open, so the idempotency check is meaningful.
        }
        open_and_migrate(&db_path).expect("second open");
    }
}
