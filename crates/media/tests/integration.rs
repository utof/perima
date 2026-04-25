//! Integration tests for `perima-media` extractors.
//!
//! Synthesis helpers live in `tests/common/mod.rs` per the Batch F/G
//! test-architecture convention.

#![allow(clippy::missing_errors_doc)]

mod common;

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use perima_core::{BlakeHash, MediaMetadata, MetadataExtractor};
use perima_media::{CompositeExtractor, ImageExtractor, VideoExtractor};
use tempfile::tempdir;

use common::{
    dummy_hash, make_jpeg_with_exif, make_jpeg_with_exif_offset, make_test_mp4, make_test_png,
};

// ---------------------------------------------------------------------
// ImageExtractor tests
// ---------------------------------------------------------------------

#[test]
fn image_extractor_png_dimensions() {
    let td = tempdir().expect("tempdir");
    let path = td.path().join("solid.png");
    make_test_png(100, 50, &path);

    let extractor = ImageExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), &path, "image/png")
        .expect("extract png");
    assert_eq!(meta.width, Some(100));
    assert_eq!(meta.height, Some(50));
    assert_eq!(meta.mime_type.as_deref(), Some("image/png"));
    assert_eq!(meta.captured_at, None, "PNG has no EXIF");
}

#[test]
fn image_extractor_jpeg_exif() {
    let td = tempdir().expect("tempdir");
    let path = td.path().join("exif.jpg");
    make_jpeg_with_exif("2024:06:01 12:34:56", "Canon", "EOS R5", &path);

    // Sanity: file exists and starts with SOI.
    let bytes = fs::read(&path).expect("read jpeg");
    assert_eq!(&bytes[0..2], &[0xFF, 0xD8], "must start with JPEG SOI");

    let extractor = ImageExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), &path, "image/jpeg")
        .expect("extract jpeg");
    assert_eq!(
        meta.captured_at.as_deref(),
        Some("2024-06-01T12:34:56"),
        "EXIF DateTimeOriginal must convert to ISO 8601",
    );
    assert_eq!(meta.camera_make.as_deref(), Some("Canon"));
    assert_eq!(meta.camera_model.as_deref(), Some("EOS R5"));
    assert_eq!(meta.mime_type.as_deref(), Some("image/jpeg"));
}

#[test]
fn image_extractor_jpeg_exif_with_offset() {
    // WHY: exercises the FixedOffset branch of as_time_components() in
    // read_exif. The OffsetTimeOriginal tag (0x9011, "+08:00") causes
    // nom-exif to populate the Option<FixedOffset> field, which should
    // produce an RFC 3339 string with the offset suffix.
    let td = tempdir().expect("tempdir");
    let path = td.path().join("exif_offset.jpg");
    make_jpeg_with_exif_offset("2024:06:01 12:34:56", "+08:00", "Canon", "EOS R5", &path);

    let bytes = std::fs::read(&path).expect("read jpeg");
    assert_eq!(&bytes[0..2], &[0xFF, 0xD8], "must start with JPEG SOI");

    let extractor = ImageExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), &path, "image/jpeg")
        .expect("extract jpeg with offset");
    assert_eq!(
        meta.captured_at.as_deref(),
        Some("2024-06-01T12:34:56+08:00"),
        "tz-aware EXIF must produce RFC 3339 with offset suffix",
    );
    assert_eq!(meta.camera_make.as_deref(), Some("Canon"));
    assert_eq!(meta.camera_model.as_deref(), Some("EOS R5"));
}

#[test]
fn image_extractor_jpeg_without_exif_returns_default() {
    // WHY: exercises the has_exif() short-circuit path. A JPEG with only
    // SOI + APP0(JFIF) + EOI has no APP1 segment, so nom-exif should
    // report has_exif() == false and read_exif returns (None, None, None).
    let td = tempdir().expect("tempdir");
    let path = td.path().join("no_exif.jpg");

    // Build a minimal JPEG: SOI + APP0(JFIF) + EOI — no APP1.
    {
        let file = File::create(&path).expect("create jpeg");
        let mut w = BufWriter::new(file);
        w.write_all(&[0xFF, 0xD8]).expect("SOI");
        w.write_all(&[0xFF, 0xE0, 0x00, 0x10])
            .expect("APP0 marker+len");
        w.write_all(b"JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00")
            .expect("JFIF payload");
        w.write_all(&[0xFF, 0xD9]).expect("EOI");
        w.flush().expect("flush");
    }

    let extractor = ImageExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), &path, "image/jpeg")
        .expect("extract jpeg without exif");
    assert_eq!(
        meta.captured_at, None,
        "no-EXIF JPEG must have captured_at = None",
    );
    assert_eq!(meta.camera_make, None, "no-EXIF JPEG must have make = None");
    assert_eq!(
        meta.camera_model, None,
        "no-EXIF JPEG must have model = None",
    );
}

// ---------------------------------------------------------------------
// VideoExtractor tests
// ---------------------------------------------------------------------

#[test]
fn video_extractor_mp4_duration() {
    let td = tempdir().expect("tempdir");
    let path = td.path().join("clip.mp4");
    make_test_mp4(&path);

    let extractor = VideoExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), &path, "video/mp4")
        .expect("extract mp4");
    assert!(
        meta.duration_ms.unwrap_or(0) > 0,
        "mp4 duration_ms should be > 0, got {:?}",
        meta.duration_ms,
    );
    assert_eq!(meta.mime_type.as_deref(), Some("video/mp4"));
    assert!(meta.codec.is_some(), "codec string should be extracted");
}

// ---------------------------------------------------------------------
// CompositeExtractor dispatch
// ---------------------------------------------------------------------

/// Always-accepting extractor stamped with an id so we can tell which
/// one was invoked.
struct TaggedExtractor {
    id: &'static str,
    accepted: Vec<&'static str>,
}

impl MetadataExtractor for TaggedExtractor {
    fn accepts(&self, mime: &str) -> bool {
        self.accepted.contains(&mime)
    }
    fn extract(
        &self,
        hash: BlakeHash,
        _absolute_path: &Path,
        mime: &str,
    ) -> Result<MediaMetadata, perima_core::CoreError> {
        Ok(MediaMetadata {
            hash,
            width: None,
            height: None,
            duration_ms: None,
            captured_at: None,
            camera_make: None,
            camera_model: None,
            codec: Some(self.id.to_owned()),
            bitrate_bps: None,
            mime_type: Some(mime.to_owned()),
            thumbnail_path: None,
            thumbnail_status: None,
        })
    }
}

#[test]
fn composite_dispatches_by_mime() {
    let a: Arc<dyn MetadataExtractor> = Arc::new(TaggedExtractor {
        id: "image-handler",
        accepted: vec!["image/png", "image/jpeg"],
    });
    let b: Arc<dyn MetadataExtractor> = Arc::new(TaggedExtractor {
        id: "video-handler",
        accepted: vec!["video/mp4"],
    });
    let c: Arc<dyn MetadataExtractor> = Arc::new(TaggedExtractor {
        id: "audio-handler",
        accepted: vec!["audio/mp3"],
    });

    let composite = CompositeExtractor::new(vec![a, b, c]);

    let png = composite
        .extract(dummy_hash(), Path::new("/tmp/ignored"), "image/png")
        .expect("dispatch png");
    assert_eq!(png.codec.as_deref(), Some("image-handler"));

    let mp4 = composite
        .extract(dummy_hash(), Path::new("/tmp/ignored"), "video/mp4")
        .expect("dispatch mp4");
    assert_eq!(mp4.codec.as_deref(), Some("video-handler"));

    let mp3 = composite
        .extract(dummy_hash(), Path::new("/tmp/ignored"), "audio/mp3")
        .expect("dispatch mp3");
    assert_eq!(mp3.codec.as_deref(), Some("audio-handler"));

    // Unsupported MIME still produces a row (mime_type stamped, other
    // fields empty), matching the CompositeExtractor contract.
    let unknown = composite
        .extract(
            dummy_hash(),
            Path::new("/tmp/ignored"),
            "application/octet-stream",
        )
        .expect("dispatch unknown mime returns empty metadata");
    assert_eq!(
        unknown.mime_type.as_deref(),
        Some("application/octet-stream")
    );
    assert!(unknown.codec.is_none());
}

#[test]
fn image_extractor_directory_path_returns_default() {
    // WHY: exercises the read_exif `MediaSource::file_path` Err arm
    // (warn log path). Passing a directory path causes nom-exif to fail
    // at File::open (EISDIR on Linux; kind may differ on macOS/Windows).
    // The contract: extract() must NOT propagate the I/O error — it
    // returns Ok(MediaMetadata) with EXIF fields = None and mime_type
    // populated. Regression target: GH #110-style "real I/O bugs in
    // logs" surfacing.
    let td = tempdir().expect("tempdir");
    // Pass the directory itself as the path — File::open on a dir errs.
    let dir_path = td.path();

    let extractor = ImageExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), dir_path, "image/jpeg")
        .expect("extract must not propagate the I/O error");

    assert_eq!(meta.captured_at, None);
    assert_eq!(meta.camera_make, None);
    assert_eq!(meta.camera_model, None);
    assert_eq!(meta.mime_type.as_deref(), Some("image/jpeg"));
    // image::image_dimensions also fails on a dir → dims also None.
    assert_eq!(meta.width, None);
    assert_eq!(meta.height, None);
}

#[test]
fn image_extractor_garbage_png_returns_no_dims() {
    // WHY: exercises the `image::image_dimensions` Err arm in
    // ImageExtractor::extract (debug log path). 16 zero bytes are
    // not a valid PNG header (PNG signature is "\x89PNG\r\n\x1a\n").
    // Contract: extract returns Ok with width=None, height=None,
    // mime_type populated. read_exif also fails (not a parseable
    // image) but does not propagate.
    use std::fs;
    let td = tempdir().expect("tempdir");
    let path = td.path().join("garbage.png");
    fs::write(&path, [0u8; 16]).expect("write garbage");

    let extractor = ImageExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), &path, "image/png")
        .expect("extract must not propagate decode errors");

    assert_eq!(meta.width, None, "garbage PNG must have width = None");
    assert_eq!(meta.height, None, "garbage PNG must have height = None");
    assert_eq!(meta.mime_type.as_deref(), Some("image/png"));
}

#[test]
fn video_extractor_garbage_mp4_returns_default() {
    // WHY: exercises VideoExtractor::extract `mp4parse::read_mp4` Err
    // arm (debug log) per slice 2 spec §4 D-2 B5. 16 zero bytes lack
    // any valid ISO box structure. Contract: extract returns Ok with
    // an empty MediaMetadata (mime_type populated, every other field
    // None).
    use std::fs;
    let td = tempdir().expect("tempdir");
    let path = td.path().join("garbage.mp4");
    fs::write(&path, [0u8; 16]).expect("write garbage");

    let extractor = VideoExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), &path, "video/mp4")
        .expect("extract must not propagate mp4parse errors");

    assert_eq!(meta.duration_ms, None);
    assert_eq!(meta.width, None);
    assert_eq!(meta.height, None);
    assert_eq!(meta.codec, None);
    assert_eq!(meta.mime_type.as_deref(), Some("video/mp4"));
}

#[test]
fn video_extractor_mp4_audio_first_then_video_picks_video() {
    // WHY: exercises video_tracks_summary's loop-then-break branch
    // (spec §4 D-2 B8). With audio at track index 0 and video at
    // track index 1, video_tracks_summary must skip the audio track
    // (it isn't TrackType::Video) and pick up width/height/codec
    // from the video track. Existing test (mp4_duration) only has
    // a video track at index 0 so the loop's "skip non-video" arm
    // is never exercised.
    let td = tempdir().expect("tempdir");
    let path = td.path().join("audio_first.mp4");
    common::make_mp4_audio_first_then_video(&path);

    let extractor = VideoExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), &path, "video/mp4")
        .expect("extract mp4");

    // The video track is at index 1; width/height come from it (32x16).
    assert_eq!(
        meta.width,
        Some(32),
        "width must come from video track, not audio"
    );
    assert_eq!(
        meta.height,
        Some(16),
        "height must come from video track, not audio"
    );
    assert!(
        matches!(meta.codec.as_deref(), Some("h264")),
        "codec must be h264 (video track), got {:?}",
        meta.codec,
    );
    assert_eq!(meta.mime_type.as_deref(), Some("video/mp4"));
    // Duration should be from the video track (1000 ms = 1s @ 1000 timescale).
    assert!(
        meta.duration_ms.unwrap_or(0) > 0,
        "duration should be > 0, got {:?}",
        meta.duration_ms,
    );
}

#[test]
fn image_extractor_jpeg_corrupt_tiff_returns_default() {
    // WHY: exercises read_exif's `parser.parse(ms)` Err arm (debug log)
    // per slice 2 spec §4 D-2 B2. The fixture has has_exif() == true
    // (APP1 segment present with Exif\0\0 identifier) but TIFF magic is
    // bogus ("XX"), so parse() fails. Must NOT propagate; returns
    // (None, None, None) for EXIF fields.
    let td = tempdir().expect("tempdir");
    let path = td.path().join("corrupt.jpg");
    common::make_jpeg_with_corrupt_tiff(&path);

    let extractor = ImageExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), &path, "image/jpeg")
        .expect("extract must not propagate parse errors");

    assert_eq!(meta.captured_at, None);
    assert_eq!(meta.camera_make, None);
    assert_eq!(meta.camera_model, None);
    // Note: image_dimensions ALSO fails on this synthetic JPEG (no real
    // image data after EOI), so width/height = None too. That's
    // acceptable; the test point is the EXIF parse arm.
    assert_eq!(meta.mime_type.as_deref(), Some("image/jpeg"));
}

#[test]
fn image_extractor_jpeg_padded_ascii_is_trimmed() {
    // WHY: exercises read_exif's `trim_end_matches(['\0', ' '])` branch
    // (spec §4 D-2 B3). Real-world Nikon-style padding pattern: trailing
    // spaces + NUL on the Make field. extractor.rs comment line 134
    // documents "NIKON CORPORATION   \0" as the trigger pattern; the
    // test mirrors that.
    let td = tempdir().expect("tempdir");
    let path = td.path().join("padded.jpg");
    // "NIKON CORPORATION" + 3 spaces (no trailing NUL in the bytes we pass
    // — build_tiff_exif_padded appends ONE NUL terminator, which
    // trim_end_matches will also strip).
    common::make_jpeg_with_padded_ascii(
        b"NIKON CORPORATION   ",
        b"D850 ",
        "2024:06:01 12:34:56",
        &path,
    );

    let extractor = ImageExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), &path, "image/jpeg")
        .expect("extract jpeg");

    assert_eq!(
        meta.camera_make.as_deref(),
        Some("NIKON CORPORATION"),
        "trailing spaces + NUL must be trimmed; got {:?}",
        meta.camera_make,
    );
    assert_eq!(
        meta.camera_model.as_deref(),
        Some("D850"),
        "trailing space + NUL must be trimmed; got {:?}",
        meta.camera_model,
    );
}

#[test]
fn image_extractor_jpeg_make_model_only_no_datetime() {
    // WHY: exercises the outer `.and_then(EntryValue::as_time_components)`
    // collapse-to-None branch in read_exif (spec §4 D-2 B9). The fixture
    // has EXIF Make+Model populated but NO DateTimeOriginal entry —
    // distinct from `image_extractor_jpeg_without_exif_returns_default`
    // (which fails at has_exif() == false) and B2 corrupt-TIFF (which
    // fails at parser.parse()). Real-world example: scanned/edited
    // images that retain camera identification but lose capture timestamps.
    let td = tempdir().expect("tempdir");
    let path = td.path().join("make_model_only.jpg");
    common::make_jpeg_with_make_model_only("Canon", "EOS 5D Mark IV", &path);

    let extractor = ImageExtractor::new();
    let meta = extractor
        .extract(dummy_hash(), &path, "image/jpeg")
        .expect("extract jpeg");

    assert_eq!(meta.camera_make.as_deref(), Some("Canon"));
    assert_eq!(meta.camera_model.as_deref(), Some("EOS 5D Mark IV"));
    assert_eq!(
        meta.captured_at, None,
        "no DateTimeOriginal must collapse captured_at to None even when EXIF parses cleanly",
    );
    assert_eq!(meta.mime_type.as_deref(), Some("image/jpeg"));
}
