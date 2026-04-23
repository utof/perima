//! Internal errors for the hash adapter.

use thiserror::Error;

/// Errors raised inside `perima-hash`.
#[derive(Debug, Error)]
pub enum Error {
    /// I/O failure while reading a file.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<Error> for perima_core::CoreError {
    fn from(e: Error) -> Self {
        match e {
            // WHY: delegate through CoreError's From<io::Error> impl so the
            // kind+message lowering stays in one place (errors.rs in core).
            Error::Io(io) => Self::from(io),
        }
    }
}
