//! Integration tests for `perima-media` extractors.
//!
//! All fixtures are synthesised at test runtime to avoid checking binary
//! blobs into git — see the per-fixture WHY comments below for the
//! rationale on each generator.

#![allow(clippy::missing_errors_doc)]

use std::fs::{self, File};
use std::io::{BufWriter, Write};
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

/// Build a minimal JPEG containing SOI + APP0(JFIF) + APP1(EXIF) + EOI.
///
/// WHY this shape (not a real encoded image):
/// - `nom-exif`'s `MediaParser` requires a preceding APP0 (JFIF) marker
///   before the APP1 (EXIF) segment for its streaming JPEG parser to
///   recognise the file as a valid JPEG container.
/// - `image::image_dimensions` is NOT called against this fixture —
///   the EXIF test focuses on the text fields, and the PNG-dimensions
///   test uses a real PNG.
/// - Avoiding a fully-encoded JPEG keeps the helper small and removes
///   any dependency on the `image` JPEG encoder.
///
/// WHY hand-crafted TIFF bytes (not a third-party writer):
/// We no longer depend on `kamadak-exif`, which previously provided
/// `exif::experimental::Writer`. Instead we build a minimal little-endian
/// TIFF block with a proper `ExifIFDPointer` chain (IFD0 → `ExifSubIFD`)
/// that nom-exif can follow. The byte layout follows EXIF 2.3 §4.5.
fn make_jpeg_with_exif(datetime_original: &str, make: &str, model: &str, path: &Path) -> PathBuf {
    let tiff_bytes = build_tiff_exif(datetime_original, make, model);

    // Assemble JPEG: SOI + APP0(JFIF) + APP1(Exif) + EOI.
    // APP1 length covers [len_hi, len_lo, "Exif\0\0", TIFF] — i.e.
    // 2 (len itself) + 6 (identifier) + tiff.len().
    let app1_payload_len = 2 + 6 + tiff_bytes.len();
    let app1_len =
        u16::try_from(app1_payload_len).expect("APP1 payload too large for a single segment");

    let file = File::create(path).expect("create jpeg");
    let mut w = BufWriter::new(file);
    w.write_all(&[0xFF, 0xD8]).expect("SOI");
    // APP0 (JFIF) — 16-byte minimal header required by nom-exif's JPEG parser.
    w.write_all(&[0xFF, 0xE0, 0x00, 0x10])
        .expect("APP0 marker+len");
    w.write_all(b"JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00")
        .expect("JFIF payload");
    // APP1 (EXIF)
    w.write_all(&[0xFF, 0xE1]).expect("APP1 marker");
    w.write_all(&app1_len.to_be_bytes()).expect("APP1 length");
    w.write_all(b"Exif\0\0").expect("Exif identifier");
    w.write_all(&tiff_bytes).expect("TIFF body");
    w.write_all(&[0xFF, 0xD9]).expect("EOI");
    w.flush().expect("flush jpeg");
    path.to_path_buf()
}

/// Build a minimal little-endian TIFF block with the proper EXIF IFD
/// chain for three tags: `Make` and `Model` in IFD0, and
/// `DateTimeOriginal` in the EXIF `SubIFD` (pointed to from IFD0 via tag
/// `ExifIFDPointer` 0x8769).
///
/// Layout (offsets relative to start of TIFF block):
/// ```text
///  0x00  TIFF header (8 bytes): "II" + 0x002A + IFD0_offset=8
///  0x08  IFD0 (42 bytes): count=3, [Make, Model, ExifIFDPointer], next=0
///  0x32  ExifSubIFD (18 bytes): count=1, [DateTimeOriginal], next=0
///  0x44  String area: Make\0, Model\0, DateTimeOriginal\0
/// ```
///
/// Each IFD entry is 12 bytes: tag(2) + type(2) + count(4) + offset(4).
fn build_tiff_exif(datetime_original: &str, make: &str, model: &str) -> Vec<u8> {
    // Tag / type constants (EXIF 2.3).
    const TAG_MAKE: u16 = 0x010F;
    const TAG_MODEL: u16 = 0x0110;
    const TAG_EXIF_IFD_POINTER: u16 = 0x8769;
    const TAG_DATETIME_ORIGINAL: u16 = 0x9003;
    const TYPE_ASCII: u16 = 2;
    const TYPE_LONG: u16 = 4;

    // Offset layout (all LE, relative to TIFF block start).
    // TIFF header: 8. IFD0: 2 + 3*12 + 4 = 42 → ends at 50.
    // ExifSubIFD: 2 + 1*12 + 4 = 18 → ends at 68. String area from 68.
    const IFD0_OFFSET: u32 = 8;
    const IFD0_ENTRY_COUNT: u16 = 3;
    const EXIF_SUBIFD_OFFSET: u32 = IFD0_OFFSET + 2 + (IFD0_ENTRY_COUNT as u32 * 12) + 4; // 50
    const EXIF_SUBIFD_ENTRY_COUNT: u16 = 1;
    const STRING_AREA: u32 = EXIF_SUBIFD_OFFSET + 2 + (EXIF_SUBIFD_ENTRY_COUNT as u32 * 12) + 4; // 68

    // ASCII values are NUL-terminated per TIFF/EXIF spec.
    let make_bytes: Vec<u8> = {
        let mut v = make.as_bytes().to_vec();
        v.push(0);
        v
    };
    let model_bytes: Vec<u8> = {
        let mut v = model.as_bytes().to_vec();
        v.push(0);
        v
    };
    let dt_bytes: Vec<u8> = {
        let mut v = datetime_original.as_bytes().to_vec();
        v.push(0);
        v
    };

    let make_len = u32::try_from(make_bytes.len()).expect("make too long for TIFF");
    let model_len = u32::try_from(model_bytes.len()).expect("model too long for TIFF");
    let dt_len = u32::try_from(dt_bytes.len()).expect("datetime too long for TIFF");

    let make_offset = STRING_AREA;
    let model_offset = make_offset + make_len;
    let dt_offset = model_offset + model_len;

    let mut buf = Vec::new();

    // TIFF header.
    buf.extend_from_slice(b"II"); // little-endian byte order
    buf.extend_from_slice(&42_u16.to_le_bytes()); // TIFF magic
    buf.extend_from_slice(&IFD0_OFFSET.to_le_bytes());

    // IFD0 (entries must be sorted ascending by tag).
    buf.extend_from_slice(&IFD0_ENTRY_COUNT.to_le_bytes());
    // Make (0x010F)
    buf.extend_from_slice(&TAG_MAKE.to_le_bytes());
    buf.extend_from_slice(&TYPE_ASCII.to_le_bytes());
    buf.extend_from_slice(&make_len.to_le_bytes());
    buf.extend_from_slice(&make_offset.to_le_bytes());
    // Model (0x0110)
    buf.extend_from_slice(&TAG_MODEL.to_le_bytes());
    buf.extend_from_slice(&TYPE_ASCII.to_le_bytes());
    buf.extend_from_slice(&model_len.to_le_bytes());
    buf.extend_from_slice(&model_offset.to_le_bytes());
    // ExifIFDPointer (0x8769) — inline LONG offset to ExifSubIFD.
    buf.extend_from_slice(&TAG_EXIF_IFD_POINTER.to_le_bytes());
    buf.extend_from_slice(&TYPE_LONG.to_le_bytes());
    buf.extend_from_slice(&1_u32.to_le_bytes()); // count = 1
    buf.extend_from_slice(&EXIF_SUBIFD_OFFSET.to_le_bytes());
    // IFD0 next-IFD pointer.
    buf.extend_from_slice(&0_u32.to_le_bytes());

    // ExifSubIFD.
    buf.extend_from_slice(&EXIF_SUBIFD_ENTRY_COUNT.to_le_bytes());
    // DateTimeOriginal (0x9003)
    buf.extend_from_slice(&TAG_DATETIME_ORIGINAL.to_le_bytes());
    buf.extend_from_slice(&TYPE_ASCII.to_le_bytes());
    buf.extend_from_slice(&dt_len.to_le_bytes());
    buf.extend_from_slice(&dt_offset.to_le_bytes());
    // ExifSubIFD next-IFD pointer.
    buf.extend_from_slice(&0_u32.to_le_bytes());

    // String area.
    buf.extend_from_slice(&make_bytes);
    buf.extend_from_slice(&model_bytes);
    buf.extend_from_slice(&dt_bytes);

    buf
}

/// Build a minimal JPEG containing SOI + APP0(JFIF) + APP1(EXIF) + EOI with
/// both `DateTimeOriginal` (0x9003) and `OffsetTimeOriginal` (0x9011) set in
/// the `ExifSubIFD`.
///
/// WHY separate helper (not a flag on `make_jpeg_with_exif`): keeping the
/// two helpers independent avoids a boolean parameter that would change the
/// byte layout mid-function and make the offset arithmetic harder to follow.
fn make_jpeg_with_exif_offset(
    datetime_original: &str,
    offset: &str,
    make: &str,
    model: &str,
    path: &Path,
) -> PathBuf {
    let tiff_bytes = build_tiff_exif_with_offset(datetime_original, offset, make, model);

    let app1_payload_len = 2 + 6 + tiff_bytes.len();
    let app1_len =
        u16::try_from(app1_payload_len).expect("APP1 payload too large for a single segment");

    let file = File::create(path).expect("create jpeg");
    let mut w = BufWriter::new(file);
    w.write_all(&[0xFF, 0xD8]).expect("SOI");
    w.write_all(&[0xFF, 0xE0, 0x00, 0x10])
        .expect("APP0 marker+len");
    w.write_all(b"JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00")
        .expect("JFIF payload");
    w.write_all(&[0xFF, 0xE1]).expect("APP1 marker");
    w.write_all(&app1_len.to_be_bytes()).expect("APP1 length");
    w.write_all(b"Exif\0\0").expect("Exif identifier");
    w.write_all(&tiff_bytes).expect("TIFF body");
    w.write_all(&[0xFF, 0xD9]).expect("EOI");
    w.flush().expect("flush jpeg");
    path.to_path_buf()
}

/// Build a minimal little-endian TIFF block with two `ExifSubIFD` entries:
/// `DateTimeOriginal` (0x9003) and `OffsetTimeOriginal` (0x9011).
///
/// Layout (offsets relative to start of TIFF block):
/// ```text
///  0x00  TIFF header (8 bytes): "II" + 0x002A + IFD0_offset=8
///  0x08  IFD0 (42 bytes): count=3, [Make, Model, ExifIFDPointer], next=0
///  0x32  ExifSubIFD (30 bytes): count=2, [DateTimeOriginal, OffsetTimeOriginal], next=0
///  0x50  String area: Make\0, Model\0, DateTimeOriginal\0, OffsetTimeOriginal\0
/// ```
fn build_tiff_exif_with_offset(
    datetime_original: &str,
    offset: &str,
    make: &str,
    model: &str,
) -> Vec<u8> {
    const TAG_MAKE: u16 = 0x010F;
    const TAG_MODEL: u16 = 0x0110;
    const TAG_EXIF_IFD_POINTER: u16 = 0x8769;
    const TAG_DATETIME_ORIGINAL: u16 = 0x9003;
    const TAG_OFFSET_TIME_ORIGINAL: u16 = 0x9011;
    const TYPE_ASCII: u16 = 2;
    const TYPE_LONG: u16 = 4;

    // IFD0: 2 + 3*12 + 4 = 42 → [8, 50)
    // ExifSubIFD: 2 + 2*12 + 4 = 30 → [50, 80)
    // String area from 80.
    const IFD0_OFFSET: u32 = 8;
    const IFD0_ENTRY_COUNT: u16 = 3;
    const EXIF_SUBIFD_OFFSET: u32 = IFD0_OFFSET + 2 + (IFD0_ENTRY_COUNT as u32 * 12) + 4; // 50
    const EXIF_SUBIFD_ENTRY_COUNT: u16 = 2;
    const STRING_AREA: u32 = EXIF_SUBIFD_OFFSET + 2 + (EXIF_SUBIFD_ENTRY_COUNT as u32 * 12) + 4; // 80

    let make_bytes: Vec<u8> = {
        let mut v = make.as_bytes().to_vec();
        v.push(0);
        v
    };
    let model_bytes: Vec<u8> = {
        let mut v = model.as_bytes().to_vec();
        v.push(0);
        v
    };
    let dt_bytes: Vec<u8> = {
        let mut v = datetime_original.as_bytes().to_vec();
        v.push(0);
        v
    };
    // EXIF spec: OffsetTimeOriginal is ASCII, count includes the NUL.
    let offset_bytes: Vec<u8> = {
        let mut v = offset.as_bytes().to_vec();
        v.push(0);
        v
    };

    let make_len = u32::try_from(make_bytes.len()).expect("make too long");
    let model_len = u32::try_from(model_bytes.len()).expect("model too long");
    let dt_len = u32::try_from(dt_bytes.len()).expect("datetime too long");
    let offset_len = u32::try_from(offset_bytes.len()).expect("offset too long");

    let make_offset = STRING_AREA;
    let model_offset = make_offset + make_len;
    let dt_offset = model_offset + model_len;
    let offset_str_offset = dt_offset + dt_len;

    let mut buf = Vec::new();

    // TIFF header.
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42_u16.to_le_bytes());
    buf.extend_from_slice(&IFD0_OFFSET.to_le_bytes());

    // IFD0.
    buf.extend_from_slice(&IFD0_ENTRY_COUNT.to_le_bytes());
    // Make (0x010F)
    buf.extend_from_slice(&TAG_MAKE.to_le_bytes());
    buf.extend_from_slice(&TYPE_ASCII.to_le_bytes());
    buf.extend_from_slice(&make_len.to_le_bytes());
    buf.extend_from_slice(&make_offset.to_le_bytes());
    // Model (0x0110)
    buf.extend_from_slice(&TAG_MODEL.to_le_bytes());
    buf.extend_from_slice(&TYPE_ASCII.to_le_bytes());
    buf.extend_from_slice(&model_len.to_le_bytes());
    buf.extend_from_slice(&model_offset.to_le_bytes());
    // ExifIFDPointer (0x8769)
    buf.extend_from_slice(&TAG_EXIF_IFD_POINTER.to_le_bytes());
    buf.extend_from_slice(&TYPE_LONG.to_le_bytes());
    buf.extend_from_slice(&1_u32.to_le_bytes());
    buf.extend_from_slice(&EXIF_SUBIFD_OFFSET.to_le_bytes());
    // IFD0 next-IFD pointer.
    buf.extend_from_slice(&0_u32.to_le_bytes());

    // ExifSubIFD (entries must be sorted ascending by tag).
    buf.extend_from_slice(&EXIF_SUBIFD_ENTRY_COUNT.to_le_bytes());
    // DateTimeOriginal (0x9003)
    buf.extend_from_slice(&TAG_DATETIME_ORIGINAL.to_le_bytes());
    buf.extend_from_slice(&TYPE_ASCII.to_le_bytes());
    buf.extend_from_slice(&dt_len.to_le_bytes());
    buf.extend_from_slice(&dt_offset.to_le_bytes());
    // OffsetTimeOriginal (0x9011)
    buf.extend_from_slice(&TAG_OFFSET_TIME_ORIGINAL.to_le_bytes());
    buf.extend_from_slice(&TYPE_ASCII.to_le_bytes());
    buf.extend_from_slice(&offset_len.to_le_bytes());
    buf.extend_from_slice(&offset_str_offset.to_le_bytes());
    // ExifSubIFD next-IFD pointer.
    buf.extend_from_slice(&0_u32.to_le_bytes());

    // String area.
    buf.extend_from_slice(&make_bytes);
    buf.extend_from_slice(&model_bytes);
    buf.extend_from_slice(&dt_bytes);
    buf.extend_from_slice(&offset_bytes);

    buf
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
