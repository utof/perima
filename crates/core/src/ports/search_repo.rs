//! `SearchRepository` port — full-text search over indexed metadata.

use crate::{CoreError, SearchHit};

/// Query and rebuild the `FTS5` search index.
///
/// WHY trait (not a concrete type in core): keeps core free of rusqlite.
/// The desktop, CLI, and future FFI adapters each wire the concrete
/// `SqliteSearchRepository` from `perima-db`.
pub trait SearchRepository: Send + Sync {
    /// Run a `FTS5` MATCH query and return ranked hits (best match first).
    ///
    /// `query` is the raw `FTS5` match expression and is passed directly to
    /// `SQLite`. Callers must validate that `query` is non-empty; the impl
    /// may return `CoreError::Internal` on malformed `FTS5` syntax.
    ///
    /// # Errors
    /// [`CoreError::Internal`] on `SQLite` errors or malformed `FTS5` syntax.
    fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, CoreError>;

    /// Wipe and rebuild the entire `FTS5` index from the current DB state.
    ///
    /// WHY exposed as a port method: needed after migrations that add new
    /// indexed fields, and exposed in CLI as `perima search --rebuild`.
    ///
    /// # Errors
    /// [`CoreError::Internal`] on `SQLite` errors.
    fn rebuild(&self) -> Result<(), CoreError>;
}
