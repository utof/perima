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
            // WHY: delegate through CoreError's From<io::Error> impl so the
            // kind+message lowering stays in one place (errors.rs in core).
            Error::Io(io) => Self::from(io),
            Error::NotUnderVolume(p) => Self::InvalidPath(p.display().to_string()),
        }
    }
}
