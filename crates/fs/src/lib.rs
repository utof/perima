//! Filesystem scanning, watching, and path normalization for perima.

#![forbid(unsafe_code)]

pub mod errors;
pub mod paths;
pub mod platform_path;
pub mod volumes;
pub mod walker;
pub mod watcher;

pub use errors::Error;
pub use paths::relativize;
pub use volumes::{DetectedVolume, detect_volume};
pub use walker::WalkdirScanner;
pub use watcher::DebouncedWatcher;

/// Marker placeholder retained for phase-0 compatibility.
pub const CRATE_NAME: &str = "perima-fs";
