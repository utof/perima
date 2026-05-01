//! Database-engine-level administration port (slice 1: backup).
//!
//! WHY a new port (not a method on `FileRepository`): every method on
//! `FileRepository` operates on file-row shape (`upsert_file`,
//! `lookup_by_file_uuid`, etc.). A backup operates at the database-engine
//! level, not the row level — adding it to `FileRepository` is a category
//! mismatch. The same logic would put `vacuum`, `analyze`, `integrity_check`,
//! `set_pragma` all on `FileRepository`, which is wrong.
//!
//! WHY a port (not direct concrete `SqliteDatabaseAdmin` injection into the
//! use case): slice 2 (`vault.toml` writer) and slice 3 (restore) of
//! issue #168 will both want database-engine-level operations. A trait
//! gives them a clean home and lets tests inject a stub adapter.

use std::path::Path;

use crate::CoreError;

/// Database-engine-level administration: backup (slice 1), restore (slice 3),
/// integrity-check (TBD).
///
/// Adapters MUST be `Send + Sync` for `Arc<dyn DatabaseAdmin>` injection.
pub trait DatabaseAdmin: Send + Sync {
    /// Produce a single-file consistent snapshot of the database at `target`.
    ///
    /// Returns `size_bytes` of the produced file on success.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::BackupFailed { reason }` on any failure;
    /// adapters map their underlying SQLite/IO errors to typed
    /// [`crate::errors::BackupFailureReason`] variants.
    fn backup(&self, target: &Path) -> Result<u64, CoreError>;
}
