//! Structured media metadata — domain type and extractor port.
//!
//! Framework-free value types consumed by `perima-media` extractors
//! and persisted by `perima-db`'s `SqliteMetadataRepository`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{BlakeHash, CoreError};

/// Structured metadata extracted from a media file.
///
/// WHY optional everywhere: not every file has every field (a PNG has
/// no `captured_at`; an MP4 may have no camera info). Partial
/// information is the norm with EXIF / container tag extraction, so
/// each field is independently nullable.
///
/// WHY `captured_at: Option<String>` (ISO 8601) not `DateTime<Utc>`:
/// consistency with the existing `first_seen` / `last_seen` String
/// columns, and it keeps `chrono` out of `perima-core`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaMetadata {
    /// Content hash of the file this metadata describes. Matches the
    /// corresponding row in the `files` table.
    pub hash: BlakeHash,
    /// Pixel width (images / video). `None` for formats without a
    /// natural width (audio, future PDF extractors).
    pub width: Option<u32>,
    /// Pixel height (images / video). `None` for formats without a
    /// natural height.
    pub height: Option<u32>,
    /// Duration in milliseconds (video / audio). `None` for still
    /// images.
    pub duration_ms: Option<u64>,
    /// ISO 8601 UTC capture timestamp. Sourced from EXIF
    /// `DateTimeOriginal` for images, container tags for video.
    pub captured_at: Option<String>,
    /// Camera manufacturer (EXIF `Make`).
    pub camera_make: Option<String>,
    /// Camera model (EXIF `Model`).
    pub camera_model: Option<String>,
    /// Codec identifier (e.g. `"avc1"`, `"hevc"`). Video only.
    pub codec: Option<String>,
    /// Overall bitrate in bits per second. Video only.
    pub bitrate_bps: Option<u32>,
    /// MIME type as detected at extraction time.
    pub mime_type: Option<String>,
}

/// MIME-dispatched extractor.
///
/// WHY MIME dispatch not "first-non-empty": a JPEG EXIF extractor that
/// returns `{width: None, mime_type: Some(..)}` would falsely "win"
/// against a video extractor that could actually extract duration.
/// Dispatching by `accepts(mime)` avoids this ambiguity — each
/// extractor declares the MIME families it handles, and the composite
/// picks the first match.
pub trait MetadataExtractor: Send + Sync {
    /// Whether this extractor handles the given MIME type.
    fn accepts(&self, mime: &str) -> bool;

    /// Extract metadata from the file at `absolute_path`.
    ///
    /// # Errors
    /// Returns `CoreError::Io` if the file cannot be read, or
    /// `CoreError::Internal` on decoder-level failures. Missing
    /// optional fields are not errors — they are `None` in the
    /// returned `MediaMetadata`.
    fn extract(
        &self,
        hash: BlakeHash,
        absolute_path: &Path,
        mime: &str,
    ) -> Result<MediaMetadata, CoreError>;
}
