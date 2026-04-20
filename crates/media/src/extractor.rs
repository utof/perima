//! Metadata extractors for images and MP4/MOV video.
//!
//! `ImageExtractor` reads PNG/JPEG dimensions via the `image` crate and
//! EXIF tags via `nom-exif`. `VideoExtractor` parses `moov`-box data
//! via Mozilla's `mp4parse` reader. `CompositeExtractor` dispatches to
//! the first registered extractor whose [`MetadataExtractor::accepts`]
//! returns `true` for the requested MIME.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use perima_core::{BlakeHash, CoreError, MediaMetadata, MetadataExtractor};

/// Image metadata extractor backed by `image` (dimensions) and
/// `nom-exif` (capture timestamp + camera).
///
/// Handles every `image/*` MIME the `image` crate can decode.
#[derive(Clone, Copy, Debug, Default)]
pub struct ImageExtractor;

impl ImageExtractor {
    /// Construct a zero-sized extractor.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl MetadataExtractor for ImageExtractor {
    fn accepts(&self, mime: &str) -> bool {
        mime.starts_with("image/")
    }

    fn extract(
        &self,
        hash: BlakeHash,
        absolute_path: &Path,
        mime: &str,
    ) -> Result<MediaMetadata, CoreError> {
        // WHY `image::image_dimensions` over `image::open`: the former
        // parses only the header and is dramatically cheaper on large
        // files; we do not need pixel data here.
        let (width, height) = match image::image_dimensions(absolute_path) {
            Ok((w, h)) => (Some(w), Some(h)),
            Err(err) => {
                tracing::debug!(
                    path = %absolute_path.display(),
                    error = %err,
                    "image_dimensions failed; continuing with None dims",
                );
                (None, None)
            }
        };

        let (captured_at, camera_make, camera_model) = read_exif(absolute_path);

        Ok(MediaMetadata {
            hash,
            width,
            height,
            duration_ms: None,
            captured_at,
            camera_make,
            camera_model,
            codec: None,
            bitrate_bps: None,
            mime_type: Some(mime.to_owned()),
            thumbnail_path: None,
            thumbnail_status: None,
        })
    }
}

/// Read EXIF `DateTimeOriginal`, `Make`, and `Model` from an image.
///
/// Uses `nom-exif`'s unified `MediaParser` / `MediaSource` API which
/// returns strings bare (no quote-wrapping) and datetime values as typed
/// `EntryValue::Time` (timezone-aware, RFC 3339) or
/// `EntryValue::NaiveDateTime` (no timezone, formatted as
/// `"YYYY-MM-DD HH:MM:SS"`). We normalise both to ISO 8601 with a `T`
/// separator before returning.
///
/// Returns `(None, None, None)` if the file has no EXIF segment, the
/// segment is malformed, or the individual fields are absent. A missing
/// EXIF block is expected for PNGs and many camera-exported JPEGs —
/// treating it as an error would be noisy. Any I/O or parser error is
/// traced at `debug` level.
fn read_exif(path: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let ms = match nom_exif::MediaSource::file_path(path) {
        Ok(ms) => ms,
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "nom-exif: could not open file as MediaSource",
            );
            return (None, None, None);
        }
    };

    // Early-return when the container carries no EXIF block (e.g. raw
    // PNG, MP4 without embedded EXIF). This avoids the parse attempt
    // and the noisy "no Exif data" error it would produce.
    if !ms.has_exif() {
        return (None, None, None);
    }

    let mut parser = nom_exif::MediaParser::new();
    let iter: nom_exif::ExifIter = match parser.parse(ms) {
        Ok(iter) => iter,
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "nom-exif: EXIF parse failed",
            );
            return (None, None, None);
        }
    };
    let exif: nom_exif::Exif = iter.into();

    let camera_make = exif
        .get(nom_exif::ExifTag::Make)
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let camera_model = exif
        .get(nom_exif::ExifTag::Model)
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    // WHY reshape: callers consume ISO 8601 throughout the DB
    // (`first_seen`, `last_seen`). Converting at extraction time keeps
    // storage canonical. nom-exif emits either:
    //   - `EntryValue::Time`          → RFC 3339  ("2024-06-01T12:34:56+08:00")
    //   - `EntryValue::NaiveDateTime` → space-sep ("2024-06-01 12:34:56")
    // Both `to_string()` forms already use `YYYY-MM-DD`; we only need
    // to replace the space separator with `T` in the naive case.
    let captured_at = exif.get(nom_exif::ExifTag::DateTimeOriginal).map(|v| {
        let s = v.to_string();
        // Naive datetimes have a space at index 10; RFC 3339 already has `T`.
        // WHY `replacen` not `replace`: only the date/time separator should
        // be substituted; time components use `:` not ` `, so only one
        // substitution ever fires, but `replacen(1)` is explicit.
        if s.as_bytes().get(10).copied() == Some(b' ') {
            s.replacen(' ', "T", 1)
        } else {
            s
        }
    });

    (captured_at, camera_make, camera_model)
}

/// Video metadata extractor backed by `mp4parse`.
///
/// Handles `video/mp4` and `video/quicktime` (the two MIMEs the
/// underlying `mp4parse` demuxer is proven against in this project).
#[derive(Clone, Copy, Debug, Default)]
pub struct VideoExtractor;

impl VideoExtractor {
    /// Construct a zero-sized extractor.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl MetadataExtractor for VideoExtractor {
    fn accepts(&self, mime: &str) -> bool {
        matches!(mime, "video/mp4" | "video/quicktime")
    }

    fn extract(
        &self,
        hash: BlakeHash,
        absolute_path: &Path,
        mime: &str,
    ) -> Result<MediaMetadata, CoreError> {
        let mut file = File::open(absolute_path)?;
        let ctx = match mp4parse::read_mp4(&mut file) {
            Ok(ctx) => ctx,
            Err(err) => {
                tracing::debug!(
                    path = %absolute_path.display(),
                    error = ?err,
                    "mp4parse read_mp4 failed",
                );
                return Ok(MediaMetadata {
                    hash,
                    width: None,
                    height: None,
                    duration_ms: None,
                    captured_at: None,
                    camera_make: None,
                    camera_model: None,
                    codec: None,
                    bitrate_bps: None,
                    mime_type: Some(mime.to_owned()),
                    thumbnail_path: None,
                    thumbnail_status: None,
                });
            }
        };

        let (duration_ms, width, height, codec) = video_tracks_summary(&ctx);

        Ok(MediaMetadata {
            hash,
            width,
            height,
            duration_ms,
            captured_at: None,
            camera_make: None,
            camera_model: None,
            codec,
            bitrate_bps: None,
            mime_type: Some(mime.to_owned()),
            thumbnail_path: None,
            thumbnail_status: None,
        })
    }
}

/// Pull the highest-signal fields out of the first video track in an
/// `mp4parse::MediaContext`.
///
/// Returns `(duration_ms, width, height, codec)` — every field
/// independently optional because some tracks lack pixel dimensions
/// (audio-only `.m4a`) or recognised codec strings.
///
/// Duration is computed from the track's own `(duration, timescale)`
/// pair: `duration_units * 1000 / timescale_units_per_second`. The
/// `MediaContext` top-level `timescale` is the movie timescale used as
/// a fallback if the track does not expose its own.
fn video_tracks_summary(
    ctx: &mp4parse::MediaContext,
) -> (Option<u64>, Option<u32>, Option<u32>, Option<String>) {
    let mut duration_ms: Option<u64> = None;
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;
    let mut codec: Option<String> = None;

    for track in ctx.tracks.iter() {
        if matches!(track.track_type, mp4parse::TrackType::Video) {
            duration_ms = track_duration_ms(track, ctx.timescale);

            if let Some(stsd) = track.stsd.as_ref()
                && let Some(mp4parse::SampleEntry::Video(v)) = stsd.descriptions.first()
            {
                width = Some(u32::from(v.width));
                height = Some(u32::from(v.height));
                codec = Some(format_codec(v.codec_type));
            }
            break;
        }
    }

    (duration_ms, width, height, codec)
}

/// Convert a track's scaled duration to milliseconds using either the
/// track-local timescale or, failing that, the movie-level timescale.
fn track_duration_ms(
    track: &mp4parse::Track,
    movie_timescale: Option<mp4parse::MediaTimeScale>,
) -> Option<u64> {
    let raw = track.duration.as_ref()?.0;
    let ts = track
        .timescale
        .as_ref()
        .map(|t| t.0)
        .or_else(|| movie_timescale.map(|t| t.0))?;
    if ts == 0 {
        return None;
    }
    Some(raw.saturating_mul(1000) / ts)
}

/// Render `mp4parse::CodecType` as a short string tag (e.g. `"h264"`,
/// `"av1"`). Unknown codecs become the enum's `Debug` form.
fn format_codec(codec: mp4parse::CodecType) -> String {
    match codec {
        mp4parse::CodecType::H264 => "h264".into(),
        mp4parse::CodecType::H263 => "h263".into(),
        mp4parse::CodecType::AV1 => "av1".into(),
        mp4parse::CodecType::VP8 => "vp8".into(),
        mp4parse::CodecType::VP9 => "vp9".into(),
        mp4parse::CodecType::AAC => "aac".into(),
        mp4parse::CodecType::FLAC => "flac".into(),
        mp4parse::CodecType::MP3 => "mp3".into(),
        mp4parse::CodecType::Opus => "opus".into(),
        mp4parse::CodecType::Unknown => "unknown".into(),
        // WHY `{:?}` fallback: new CodecType variants appear between
        // mp4parse versions (MP4V, LPCM, ALAC, EncryptedVideo/Audio
        // today). Rather than re-compile on every bump, surface the
        // enum name verbatim.
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

/// MIME-dispatching composite of extractors.
///
/// Walks the registered extractors in insertion order and delegates to
/// the first whose [`MetadataExtractor::accepts`] returns `true`. WHY
/// first-match not first-non-empty: a JPEG extractor that returns
/// `{width: None, mime_type: Some(..)}` would falsely "win" against a
/// video extractor that can actually extract duration.
pub struct CompositeExtractor {
    extractors: Vec<Arc<dyn MetadataExtractor>>,
}

impl std::fmt::Debug for CompositeExtractor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositeExtractor").finish_non_exhaustive()
    }
}

impl CompositeExtractor {
    /// Construct a composite from an explicit ordered list.
    ///
    /// Earlier entries win ties; put the most-specific extractors first.
    #[must_use]
    pub const fn new(extractors: Vec<Arc<dyn MetadataExtractor>>) -> Self {
        Self { extractors }
    }

    /// Construct the default composite for v0.4.0: image first, video
    /// second.
    #[must_use]
    pub fn default_extractors() -> Self {
        Self::new(vec![
            Arc::new(ImageExtractor::new()) as Arc<dyn MetadataExtractor>,
            Arc::new(VideoExtractor::new()) as Arc<dyn MetadataExtractor>,
        ])
    }
}

impl Default for CompositeExtractor {
    fn default() -> Self {
        Self::default_extractors()
    }
}

impl MetadataExtractor for CompositeExtractor {
    fn accepts(&self, mime: &str) -> bool {
        self.extractors.iter().any(|e| e.accepts(mime))
    }

    fn extract(
        &self,
        hash: BlakeHash,
        absolute_path: &Path,
        mime: &str,
    ) -> Result<MediaMetadata, CoreError> {
        if let Some(e) = self.extractors.iter().find(|e| e.accepts(mime)) {
            return e.extract(hash, absolute_path, mime);
        }
        // No extractor accepts — return an empty-but-valid MediaMetadata
        // so the queue worker still writes a row (callers distinguish
        // "unsupported MIME" from "pending" via the row's existence).
        Ok(MediaMetadata {
            hash,
            width: None,
            height: None,
            duration_ms: None,
            captured_at: None,
            camera_make: None,
            camera_model: None,
            codec: None,
            bitrate_bps: None,
            mime_type: Some(mime.to_owned()),
            thumbnail_path: None,
            thumbnail_status: None,
        })
    }
}
