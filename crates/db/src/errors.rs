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
