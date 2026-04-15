//! Top-level error type crossing the core boundary.
//!
//! Adapters define their own internal errors and implement
//! `From<AdapterError> for CoreError` **inside the adapter crate**
//! so that `core` depends on no adapter (preserves hexagonal
//! direction).

use thiserror::Error;

/// Error returned by every `core` trait method.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Queried item was absent.
    #[error("not found: {0}")]
    NotFound(String),

    /// App-level uniqueness check rejected an insert.
    #[error("duplicate: {0}")]
    Duplicate(String),

    /// Path string could not be normalized or is outside the expected root.
    #[error("invalid path: {0}")]
    InvalidPath(String),

    /// Hex input was not a valid 64-char lowercase BLAKE3 hash.
    #[error("invalid hash hex: {0}")]
    InvalidHash(String),

    /// Underlying I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Feature is declared but not yet implemented at this phase.
    /// Dedicated variant so `main.rs` can map to a stable exit code
    /// without substring-matching prose.
    #[error("unsupported in this phase: {0}")]
    Unsupported(String),

    /// Any adapter-level failure that didn't map to a typed variant.
    #[error("internal: {0}")]
    Internal(String),
}
