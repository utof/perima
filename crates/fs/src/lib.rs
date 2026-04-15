//! Filesystem scanning, watching, and path normalization for perima.

pub mod errors;
pub mod paths;
pub mod walker;

pub use errors::Error;
pub use paths::relativize;
pub use walker::WalkdirScanner;

/// Marker placeholder retained for phase-0 compatibility.
pub const CRATE_NAME: &str = "perima-fs";
