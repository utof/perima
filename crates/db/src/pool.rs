//! Read-only `SQLite` connection pool built on `r2d2` + `r2d2_sqlite`.
//!
//! WHY `r2d2_sqlite` over `deadpool-sqlite`: port traits in `crates/core`
//! are synchronous; `deadpool-sqlite::Object::interact` is `async` and
//! would force async through every adapter method and every `UseCase`
//! caller. `r2d2_sqlite` is the sync-fit for sync ports. Library-audit
//! §Q1 deferred the pool pick; Batch C resolves to `r2d2_sqlite 0.32`.
//!
//! The pool is built with `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`
//! and a `with_init` hook that applies per-connection pragmas
//! (`busy_timeout`, `temp_store`, `mmap_size`, `query_only`). Migrations
//! are expected to have already run via [`crate::SqliteWriter::start`]
//! before the pool opens — spec §3.6 invariant.

use std::path::Path;

use perima_core::CoreError;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::OpenFlags;

/// Read-only `SQLite` connection pool, `max_size = 4`.
///
/// Cheap to [`Clone`]; the inner [`r2d2::Pool`] is `Arc`-backed internally.
#[derive(Clone)]
pub struct ReadPool {
    inner: Pool<SqliteConnectionManager>,
}

impl std::fmt::Debug for ReadPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadPool")
            .field("max_size", &self.inner.max_size())
            .finish()
    }
}

impl ReadPool {
    /// Open a read-only pool against `db_path`.
    ///
    /// Migrations MUST have already run via [`crate::SqliteWriter::start`]
    /// on the writer thread (spec §3.6 invariant) — this pool opens its
    /// connections `SQLITE_OPEN_READ_ONLY` and cannot apply migrations
    /// itself.
    ///
    /// # Errors
    /// Returns [`CoreError::Internal`] on pool build failure.
    pub fn open(db_path: &Path) -> Result<Self, CoreError> {
        let manager = SqliteConnectionManager::file(db_path)
            .with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_init(|conn| {
                conn.execute_batch(
                    "PRAGMA busy_timeout = 5000;\n\
                     PRAGMA temp_store = MEMORY;\n\
                     PRAGMA mmap_size = 268435456;\n\
                     PRAGMA query_only = 1;",
                )
            });
        let inner = Pool::builder()
            .max_size(4)
            .build(manager)
            .map_err(|e| CoreError::Internal(format!("r2d2 build: {e}")))?;
        Ok(Self { inner })
    }

    /// Acquire a pooled read connection.
    ///
    /// # Errors
    /// Returns [`CoreError::Internal`] on pool checkout timeout (default
    /// `connection_timeout = 30s`; see spec §9 Q5).
    pub fn get(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>, CoreError> {
        self.inner
            .get()
            .map_err(|e| CoreError::Internal(format!("r2d2 get: {e}")))
    }

    /// Test-only helper: build an in-memory shared-cache pool for unit tests.
    ///
    /// `unique_name` must differ per-test so parallel runs don't collide
    /// on a single shared-cache namespace.
    ///
    /// # Errors
    /// Returns [`CoreError::Internal`] on pool build failure.
    #[cfg(test)]
    #[allow(dead_code)] // WHY: consumed by Task 2+ test fixtures.
    pub(crate) fn in_memory_shared_cache(unique_name: &str) -> Result<Self, CoreError> {
        let uri = format!("file:{unique_name}?mode=memory&cache=shared");
        let manager = SqliteConnectionManager::file(&uri)
            .with_flags(
                OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .with_init(|conn| {
                conn.execute_batch(
                    "PRAGMA busy_timeout = 5000;\n\
                     PRAGMA temp_store = MEMORY;\n\
                     PRAGMA query_only = 1;",
                )
            });
        let inner = Pool::builder()
            .max_size(4)
            .build(manager)
            .map_err(|e| CoreError::Internal(format!("r2d2 build: {e}")))?;
        Ok(Self { inner })
    }
}
