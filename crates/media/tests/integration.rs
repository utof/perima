//! Integration tests for `perima-media` extractors.
//!
//! All fixtures are synthesised at test runtime to avoid checking binary
//! blobs into git — see the per-fixture WHY comments below for the
//! rationale on each generator.

#![allow(clippy::missing_errors_doc)]

use std::fs::{self, File};
use std::io::{BufWriter, Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::{ImageBuffer, Rgb};
use perima_core::{BlakeHash, MediaMetadata, MetadataExtractor};
use perima_media::{CompositeExtractor, ImageExtractor, VideoExtractor};
use tempfile::tempdir;

// ---------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------

fn dummy_hash() -> BlakeHash {
    BlakeHash::from_bytes(*blake3::hash(b"test-fixture").as_bytes())
}

/// Write a solid-red PNG at `path`.
fn make_test_png(width: u32, height: u32, path: &Path) -> PathBuf {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(width, height, |_, _| Rgb([255, 0, 0]));
    img.save_with_format(path, image::ImageFormat::Png)
        .expect("save png");
    path.to_path_buf()
}

/// Build a minimal JPEG containing only SOI + APP1(EXIF) + EOI.
///
/// WHY this shape (not a real encoded image):
/// - `kamadak-exif`'s `read_from_container` only needs SOI + APP1;
///   it does not parse the scan data.
/// - `image::image_dimensions` is NOT called against this fixture —
///   the EXIF test focuses on the text fields, and the PNG-dimensions
///   test uses a real PNG.
/// - Avoiding a fully-encoded JPEG keeps the helper small and removes
///   any dependency on the `image` JPEG encoder.
///
/// WHY generated at runtime (not a byte-vendored const):
/// the plan's preferred option is a const `&[u8]` produced externally
/// via `exiftool`. This environment does not have `exiftool`; building
/// the TIFF/EXIF segment at runtime via `exif::experimental::Writer` is
/// documented as the acceptable fallback (plan §Task 3, Option 2) and
/// has the same correctness property (deterministic bytes, no binary
/// blob in git) without the external tool dependency.
fn make_jpeg_with_exif(datetime_original: &str, make: &str, model: &str, path: &Path) -> PathBuf {
    use exif::experimental::Writer;
    use exif::{Field, In, Tag, Value};

    // Build the TIFF/EXIF payload.
    let dt = Field {
        tag: Tag::DateTimeOriginal,
        ifd_num: In::PRIMARY,
        value: Value::Ascii(vec![datetime_original.as_bytes().to_vec()]),
    };
    let mk = Field {
        tag: Tag::Make,
        ifd_num: In::PRIMARY,
        value: Value::Ascii(vec![make.as_bytes().to_vec()]),
    };
    let mdl = Field {
        tag: Tag::Model,
        ifd_num: In::PRIMARY,
        value: Value::Ascii(vec![model.as_bytes().to_vec()]),
    };

    let mut writer = Writer::new();
    writer.push_field(&dt);
    writer.push_field(&mk);
    writer.push_field(&mdl);

    let mut tiff_buf = Cursor::new(Vec::<u8>::new());
    writer.write(&mut tiff_buf, false).expect("write tiff exif");
    let tiff_bytes = tiff_buf.into_inner();

    // Assemble JPEG: SOI + APP1 + (Exif\0\0 + TIFF) + EOI.
    // APP1 length bytes cover [len_hi, len_lo, Exif\0\0, TIFF] — i.e.
    // 2 (len itself) + 6 (identifier) + tiff.len().
    let payload_len = 2 + 6 + tiff_bytes.len();
    let len_u16 = u16::try_from(payload_len).expect("APP1 payload too large for a single segment");

    let file = File::create(path).expect("create jpeg");
    let mut w = BufWriter::new(file);
    w.write_all(&[0xFF, 0xD8]).expect("SOI"); // SOI
    w.write_all(&[0xFF, 0xE1]).expect("APP1 marker"); // APP1
    w.write_all(&len_u16.to_be_bytes()).expect("APP1 length");
    w.write_all(b"Exif\0\0").expect("Exif identifier");
    w.write_all(&tiff_bytes).expect("TIFF body");
    w.write_all(&[0xFF, 0xD9]).expect("EOI"); // EOI
    w.flush().expect("flush jpeg");
    path.to_path_buf()
}

/// Synthesise a minimal valid MP4 with one ~1 second AVC video track.
///
/// WHY programmatic: checking binary fixtures into git bloats the repo
/// and pins a toolchain. The `mp4` crate (dev-dep) builds a valid
/// container at runtime. The output is designed to satisfy `mp4parse`'s
/// demuxer — one video track, one AVC sample, non-zero duration.
fn make_test_mp4(path: &Path) -> PathBuf {
    use mp4::{AvcConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType};

    let config = Mp4Config {
        major_brand: "isom".parse().expect("major_brand"),
        minor_version: 512,
        compatible_brands: vec![
            "isom".parse().expect("brand"),
            "iso2".parse().expect("brand"),
            "avc1".parse().expect("brand"),
            "mp41".parse().expect("brand"),
        ],
        timescale: 1000,
    };

    // WHY File::options: `Mp4Writer` needs both `Write` and `Seek`; a
    // raw `File` opened read/write/create satisfies both without the
    // BufWriter wrapper (BufWriter<File> does not implement Seek).
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .expect("open mp4 rw");

    let mut writer = Mp4Writer::write_start(&mut file, &config).expect("write_start");

    // Minimal AVC SPS/PPS — the smallest byte sequences that keep the
    // `mp4` crate writer satisfied. mp4parse does not re-validate these
    // bytes, it just surfaces the codec type and stored dimensions.
    let track_config = TrackConfig {
        track_type: TrackType::Video,
        timescale: 1000,
        language: "und".into(),
        media_conf: mp4::MediaConfig::AvcConfig(AvcConfig {
            width: 16,
            height: 16,
            seq_param_set: vec![0x67, 0x42, 0xC0, 0x0A, 0xDB, 0x02, 0x80, 0xBF, 0xE5, 0x01],
            pic_param_set: vec![0x68, 0xCE, 0x38, 0x80],
        }),
    };
    writer.add_track(&track_config).expect("add_track");

    // One sync sample of duration = timescale (1 second).
    let sample = Mp4Sample {
        start_time: 0,
        duration: 1000,
        rendering_offset: 0,
        is_sync: true,
        bytes: mp4::Bytes::from(vec![0u8; 64]),
    };
    writer.write_sample(1, &sample).expect("write_sample");
    writer.write_end().expect("write_end");

    // Flush + return path.
    file.sync_all().expect("sync mp4");
    path.to_path_buf()
}

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
