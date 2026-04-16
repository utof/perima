//! Wire-types for commands that join multiple domain records.
//!
//! WHY separate module: `commands.rs` already hosts `FileEntry` and
//! `VolumeEntry`, which map 1:1 to a single `perima-core` record.
//! Payloads that flatten a `(record, optional_record)` pair — like
//! `FileWithMetadataPayload` — carry their own `From` impls and grow
//! fast as v0.4.x lands thumbnails and derived attributes. Keeping them
//! in their own module prevents `commands.rs` from sprawling.

use perima_core::{FileLocationRecord, MediaMetadata};
use serde::Serialize;

/// Flattened `(FileLocationRecord, Option<MediaMetadata>)` pair for the
/// frontend.
///
/// WHY flat (not nested `{ location: …, metadata: … }`): the UI grid
/// binds one row per location and needs every column addressable with a
/// single key. Nesting would force every cell to traverse an optional
/// subobject just to discover it is absent. Flat fields with `None`
/// columns match SQL's native shape and the existing `FileEntry`
/// encoding.
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct FileWithMetadataPayload {
    // File-location fields (mirror `FileEntry`).
    /// BLAKE3-256 content hash as lowercase hex.
    pub hash: String,
    /// File size in bytes.
    pub size: u64,
    /// Volume UUID.
    pub volume_id: String,
    /// Relative path within the volume.
    pub relative_path: String,
    /// Location status string (`"active"`, `"missing"`, `"moved"`, `"stale"`).
    pub status: String,
    /// ISO 8601 UTC timestamp of first sighting.
    pub first_seen: String,

    // Media-metadata fields — all optional; absent when no
    // `file_metadata` row exists or a field was not extractable.
    /// Pixel width (images / video).
    pub width: Option<u32>,
    /// Pixel height (images / video).
    pub height: Option<u32>,
    /// Duration in milliseconds (video / audio).
    pub duration_ms: Option<u64>,
    /// ISO 8601 UTC capture timestamp.
    pub captured_at: Option<String>,
    /// Camera manufacturer (EXIF `Make`).
    pub camera_make: Option<String>,
    /// Camera model (EXIF `Model`).
    pub camera_model: Option<String>,
    /// Codec identifier (e.g. `"avc1"`, `"hevc"`).
    pub codec: Option<String>,
    /// Overall bitrate in bits per second.
    pub bitrate_bps: Option<u32>,
    /// MIME type as detected at extraction time.
    pub mime_type: Option<String>,
}

impl From<(FileLocationRecord, Option<MediaMetadata>)> for FileWithMetadataPayload {
    fn from((loc, meta): (FileLocationRecord, Option<MediaMetadata>)) -> Self {
        // WHY unzip via `meta.map(...)`: the nine optional metadata
        // fields each need independent `None` defaults when the
        // metadata row is absent. Chained `.map(|m| m.field.clone())`
        // through `Option::and_then` would still require one call per
        // field; an explicit destructure is equally verbose but
        // strictly more readable.
        let (
            width,
            height,
            duration_ms,
            captured_at,
            camera_make,
            camera_model,
            codec,
            bitrate_bps,
            mime_type,
        ) = match meta {
            Some(m) => (
                m.width,
                m.height,
                m.duration_ms,
                m.captured_at,
                m.camera_make,
                m.camera_model,
                m.codec,
                m.bitrate_bps,
                m.mime_type,
            ),
            None => (None, None, None, None, None, None, None, None, None),
        };
        Self {
            hash: loc.hash.to_hex(),
            size: loc.size.0,
            volume_id: loc.volume_id.0.to_string(),
            relative_path: loc.relative_path.as_str().to_owned(),
            status: format!("{:?}", loc.status),
            first_seen: loc.first_seen,
            width,
            height,
            duration_ms,
            captured_at,
            camera_make,
            camera_model,
            codec,
            bitrate_bps,
            mime_type,
        }
    }
}
