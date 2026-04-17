//! BLAKE3-based content-hashing adapter for perima.

#![forbid(unsafe_code)]

pub mod blake3_service;
pub mod errors;

pub use blake3_service::Blake3Service;
pub use errors::Error;

/// Marker placeholder retained for phase-0 compatibility.
pub const CRATE_NAME: &str = "perima-hash";
