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
            Error::Io(io) => Self::Io(io),
        }
    }
}
