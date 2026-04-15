//! Internal errors for the filesystem adapter.

use std::path::PathBuf;

use thiserror::Error;

/// Errors raised inside `perima-fs`.
#[derive(Debug, Error)]
pub enum Error {
    /// I/O failure walking the filesystem.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// A discovered path was not under the declared volume root.
    #[error("path not under volume root: {0}")]
    NotUnderVolume(PathBuf),
}

impl From<Error> for perima_core::CoreError {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(io) => Self::Io(io),
            Error::NotUnderVolume(p) => Self::InvalidPath(p.display().to_string()),
        }
    }
}
