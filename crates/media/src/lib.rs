//! Media metadata extractors and the async processing queue.
//!
//! This crate implements the `MetadataExtractor` port declared in
//! `perima-core` for image and video files, and wraps extraction in a
//! bounded `tokio::mpsc` queue (`MetadataQueue`) driven from the
//! scanner.
//!
//! The extractors are confined here so the rest of the workspace stays
//! free of `image`, `kamadak-exif`, `mp4parse`, and `mime_guess`
//! dependencies.

pub mod extractor;
pub mod queue;
pub mod thumbnail;

pub use extractor::{CompositeExtractor, ImageExtractor, VideoExtractor};
pub use queue::{MetadataQueue, Work};
pub use thumbnail::{DEFAULT_MAX_SIZE, ThumbnailGenerator};

/// Marker placeholder retained for phase-0 compatibility.
pub const CRATE_NAME: &str = "perima-media";
