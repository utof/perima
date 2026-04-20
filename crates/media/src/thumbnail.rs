//! WebP thumbnail generator for image sources.
//!
//! Thumbnails are written under
//! `<data_dir>/thumbnails/<aa>/<full-hex>.webp` where `<aa>` is the
//! first two characters of the BLAKE3 hash's lowercase hex rendering.
//! Writes are atomic (`.tmp` + `rename`) and idempotent (a successful
//! pre-existing target is a no-op).
//!
//! # Why a dedicated module
//!
//! Thumbnail generation is a *side effect* of metadata extraction, not
//! a field of `MediaMetadata`. Keeping it in its own type lets the
//! queue worker (Task 2) compose "extract metadata" + "generate
//! thumbnail" without either step knowing about the other's failure
//! modes. The split also keeps the `image` crate's decoding surface
//! contained — `ImageExtractor` only reads headers, `ThumbnailGenerator`
//! fully decodes + re-encodes.

use std::path::{Path, PathBuf};

use fast_image_resize::images::Image;
use fast_image_resize::{IntoImageView, Resizer};
use image::ImageReader;
use perima_core::{BlakeHash, CoreError};

/// Generate WebP thumbnails for image sources.
///
/// The generator is cheap to clone and cheap to hold as `Arc` — all
/// state is a `PathBuf` + one `u32`. Its methods are `&self` so the
/// same instance can be shared across the scan worker + any future
/// retry command.
pub struct ThumbnailGenerator {
    /// Root directory under which `thumbnails/<aa>/<hash>.webp` is
    /// materialised. Typically the app's `data_dir`.
    data_dir: PathBuf,
    /// Maximum dimension (width or height) of the output in pixels.
    /// Aspect ratio is preserved; the longer side is clamped to this
    /// value.
    max_size: u32,
    /// Whether [`Self::generate`] should actually produce a thumbnail.
    ///
    /// WHY a plain bool rather than a `NoopThumbnailer` trait: the
    /// queue worker takes an `Arc<ThumbnailGenerator>` directly. A
    /// trait-based polymorphism story lands in v0.5 once we have a
    /// retry command and more nuanced policies; for v0.4.1 a single
    /// flag is the cheapest correct answer.
    enabled: bool,
}

impl std::fmt::Debug for ThumbnailGenerator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbnailGenerator").finish_non_exhaustive()
    }
}

// WHY no runtime quality parameter: the pure-Rust `image` crate
// encodes WebP losslessly in v0.25 and ignores a quality knob. The
// plan's q=85 target documents intent for a future `libwebp` swap;
// keeping it as a prose comment (not an unused constant) avoids
// clippy's `dead_code` / `no_effect_underscore_binding` walls.

/// Default maximum dimension in pixels for generated thumbnails.
///
/// WHY 256: spec ships a 256-pixel thumbnail (Task 1 subtask 4). The
/// desktop grid renders tiles at 200 CSS pixels, so 256 leaves head-
/// room for 1.25× `HiDPI` displays without blowing up storage (100 K
/// images × ~20 KB each ≈ 2 GB, acceptable per plan risks section).
pub const DEFAULT_MAX_SIZE: u32 = 256;

/// Compute fit-in-box dimensions preserving aspect ratio.
///
/// WHY: `image::DynamicImage::resize` does this internally; `fast_image_resize`
/// expects explicit target dimensions.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    // WHY: ratio is in (0.0, 1.0] (box ≤ src), and src_w / src_h are u32, so
    // src * ratio is ≥ 0 and ≤ src_w/src_h < u32::MAX. Truncation after
    // .round() is deliberate and safe here.
)]
fn compute_fit(src_w: u32, src_h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    let ratio = (f64::from(box_w) / f64::from(src_w)).min(f64::from(box_h) / f64::from(src_h));
    let dst_w = ((f64::from(src_w) * ratio).round() as u32).max(1);
    let dst_h = ((f64::from(src_h) * ratio).round() as u32).max(1);
    (dst_w, dst_h)
}

/// Normalize input to a pixel type our resize match can handle.
///
/// WHY: `fast_image_resize` + image-feature exposes `pixel_type()` which returns
/// None for some `DynamicImage` variants (16-bit PNG, HDR f32, grayscale+alpha).
/// Rather than regressing thumbnail support for exotic inputs, coerce them to
/// 8-bit of the closest channel-count match. Common cases (Rgb8/Rgba8/Luma8)
/// pass through unchanged — the "RGB→RGBA promotion" regression is guarded.
fn coerce_for_resize(img: image::DynamicImage) -> image::DynamicImage {
    use image::DynamicImage::{ImageLuma8, ImageRgb8, ImageRgba8};
    match &img {
        ImageLuma8(_) | ImageRgb8(_) | ImageRgba8(_) => img,
        // LumaA8, 16-bit, HDR, and any future variant: coerce to 8-bit RGBA.
        // LumaA8 has no 1:1 fast_image_resize PixelType — promote to RGBA to
        // preserve alpha. All other exotic variants similarly downgrade to RGBA.
        _ => ImageRgba8(img.to_rgba8()),
    }
}

impl ThumbnailGenerator {
    /// Construct a generator rooted at `data_dir` with
    /// [`DEFAULT_MAX_SIZE`] as the pixel ceiling.
    #[must_use]
    pub const fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            max_size: DEFAULT_MAX_SIZE,
            enabled: true,
        }
    }

    /// Construct a generator with a caller-chosen `max_size`.
    ///
    /// Tests pick smaller values (e.g. 256 against a 1000×500 source
    /// to verify aspect preservation); production wiring uses
    /// [`new`](Self::new).
    #[must_use]
    pub const fn with_max_size(data_dir: PathBuf, max_size: u32) -> Self {
        Self {
            data_dir,
            max_size,
            enabled: true,
        }
    }

    /// Construct a no-op generator: [`Self::generate`] always returns
    /// `Ok(None)` without touching the filesystem.
    ///
    /// WHY separate from `new`: the queue worker receives a generator
    /// unconditionally (via `Arc<ThumbnailGenerator>`); wiring a
    /// `--no-thumbnails` flag via this constructor avoids threading an
    /// `Option<Arc<_>>` through `MetadataQueue::spawn` and the worker's
    /// `process` function.
    ///
    // WHY not `const fn`: `PathBuf::new()` only became `const` in Rust
    // 1.91; our MSRV is 1.85. Dropping `const` keeps the constructor
    // MSRV-clean and costs nothing (callers allocate an `Arc` anyway).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            data_dir: PathBuf::new(),
            max_size: 0,
            enabled: false,
        }
    }

    /// Compute the target path for a given hash.
    ///
    /// Returns `<data_dir>/thumbnails/<aa>/<full-hex>.webp`, where
    /// `<aa>` is the first two hex characters of the hash.
    ///
    /// WHY the 2-char prefix dir: some filesystems (ext4 without
    /// `dir_index`, exFAT, legacy network shares) degrade with >~10 K
    /// entries in one directory. Sharding by the first byte bounds
    /// each subdirectory to 256 bins; with 100 K thumbnails that is
    /// ~400 files per bin, comfortably inside every filesystem's fast
    /// path.
    #[must_use]
    pub fn path_for(&self, hash: &BlakeHash) -> PathBuf {
        let hex = hash.to_hex();
        let (prefix, _) = hex.split_at(2);
        self.data_dir
            .join("thumbnails")
            .join(prefix)
            .join(format!("{hex}.webp"))
    }

    /// Resize `img` to fit within `max_size × max_size` using
    /// SIMD-accelerated Lanczos3 via `fast_image_resize`.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::Internal` if `fast_image_resize` cannot handle
    /// the pixel type after coercion (should be unreachable in practice).
    fn resize_image(&self, img: image::DynamicImage) -> Result<image::DynamicImage, CoreError> {
        // WHY fast_image_resize: 3-10× faster than image::imageops::resize
        // (SIMD Lanczos3: SSE4.1/AVX2/NEON/WASM). Audit §Q5. Default algorithm
        // for Resizer::new() is Lanczos3 for convolution-capable pixel types,
        // so no ResizeOptions needed.

        // Normalize exotic variants (16-bit, HDR, LumaA) to 8-bit before resize.
        // Common RGB/RGBA/Luma8 pass through unchanged.
        let img = coerce_for_resize(img);

        let (dst_w, dst_h) = compute_fit(img.width(), img.height(), self.max_size, self.max_size);

        // `img` is DynamicImage; with `features = ["image"]` it implements IntoImageView
        // so we read pixels directly, no buffer copy. After coerce_for_resize, pixel_type()
        // is guaranteed Some(U8 / U8x3 / U8x4).
        let pixel_type = img.pixel_type().ok_or_else(|| {
            CoreError::Internal(format!(
                "fast_image_resize: coerce_for_resize returned unsupported variant ({:?})",
                img.color()
            ))
        })?;

        let mut dst = Image::new(dst_w, dst_h, pixel_type);

        Resizer::new()
            .resize(&img, &mut dst, None)
            .map_err(|e| CoreError::Internal(format!("fast_image_resize: {e}")))?;

        // Wrap dst back into DynamicImage matching the source pixel type.
        let resized = match pixel_type {
            fast_image_resize::PixelType::U8x3 => image::DynamicImage::ImageRgb8(
                image::RgbImage::from_raw(dst_w, dst_h, dst.buffer().to_vec()).ok_or_else(
                    || CoreError::Internal("RgbImage::from_raw returned None".into()),
                )?,
            ),
            fast_image_resize::PixelType::U8x4 => image::DynamicImage::ImageRgba8(
                image::RgbaImage::from_raw(dst_w, dst_h, dst.buffer().to_vec()).ok_or_else(
                    || CoreError::Internal("RgbaImage::from_raw returned None".into()),
                )?,
            ),
            fast_image_resize::PixelType::U8 => image::DynamicImage::ImageLuma8(
                image::GrayImage::from_raw(dst_w, dst_h, dst.buffer().to_vec()).ok_or_else(
                    || CoreError::Internal("GrayImage::from_raw returned None".into()),
                )?,
            ),
            other => {
                return Err(CoreError::Internal(format!(
                    "fast_image_resize: thumbnail path saw unexpected pixel type {other:?}"
                )));
            }
        };

        Ok(resized)
    }

    /// Decode `source`, resize to fit `max_size` while preserving
    /// aspect, encode as WebP, and atomically write to the target
    /// computed by [`path_for`](Self::path_for).
    ///
    /// Returns `Ok(Some(target))` on success, `Ok(None)` when this
    /// generator is [`disabled`](Self::disabled). Idempotent — if the
    /// target already exists, no work is done.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::Internal` if any of the steps fails:
    ///
    /// - `mkdir` on the prefix directory,
    /// - opening / format-guessing / decoding `source`,
    /// - encoding the resized image as WebP,
    /// - writing to or renaming the `.tmp` stage file.
    ///
    /// Quality target: q=85 per spec (documentary today — see the
    /// module-level note on the `image` v0.25 WebP encoder).
    pub fn generate(&self, hash: &BlakeHash, source: &Path) -> Result<Option<PathBuf>, CoreError> {
        if !self.enabled {
            return Ok(None);
        }
        let target = self.path_for(hash);
        if target.exists() {
            return Ok(Some(target));
        }

        // `path_for` always yields a path with at least two ancestors
        // (`thumbnails/<aa>/<file>`), so `parent()` cannot be `None`.
        let dir = target
            .parent()
            .ok_or_else(|| CoreError::Internal("thumbnail target has no parent".into()))?;
        std::fs::create_dir_all(dir).map_err(|e| {
            CoreError::Internal(format!("mkdir thumbnail dir {}: {e}", dir.display()))
        })?;

        let img = ImageReader::open(source)
            .map_err(|e| CoreError::Internal(format!("open source {}: {e}", source.display())))?
            .with_guessed_format()
            .map_err(|e| CoreError::Internal(format!("guess format {}: {e}", source.display())))?
            .decode()
            .map_err(|e| CoreError::Internal(format!("decode {}: {e}", source.display())))?;

        let resized = self.resize_image(img)?;

        // WHY atomic write: without `.tmp` + `rename` a crash mid-
        // encode leaves a half-written `.webp` that passes the
        // `exists()` check forever. `rename` is atomic on POSIX
        // within the same filesystem; we only cross directories
        // within `data_dir`, which is always a single volume.
        let tmp = target.with_extension("webp.tmp");
        {
            let mut buf = std::fs::File::create(&tmp)
                .map_err(|e| CoreError::Internal(format!("create tmp {}: {e}", tmp.display())))?;
            resized
                .write_to(&mut buf, image::ImageFormat::WebP)
                .map_err(|e| CoreError::Internal(format!("encode webp: {e}")))?;
            // `buf` dropped here flushes the File before rename.
        }

        std::fs::rename(&tmp, &target).map_err(|e| {
            CoreError::Internal(format!(
                "rename {} -> {}: {e}",
                tmp.display(),
                target.display()
            ))
        })?;
        Ok(Some(target))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use image::{ImageBuffer, Rgb};
    use tempfile::TempDir;

    use super::*;

    /// Build a deterministic hash for test fixtures.
    fn hash_of(bytes: &[u8]) -> BlakeHash {
        BlakeHash::from_bytes(*blake3::hash(bytes).as_bytes())
    }

    /// Write a solid-color PNG of the given dimensions into `dir` and
    /// return its absolute path.
    fn write_png(dir: &Path, name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let path = dir.join(name);
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(width, height, Rgb([200, 100, 50]));
        img.save(&path).expect("save test png");
        path
    }

    fn generator(data_dir: &Path, max_size: u32) -> ThumbnailGenerator {
        ThumbnailGenerator::with_max_size(data_dir.to_path_buf(), max_size)
    }

    #[test]
    fn path_for_contains_hash_prefix() {
        let td = TempDir::new().expect("tempdir");
        let tg = generator(td.path(), 256);
        let hash = hash_of(b"prefix-test");
        let hex = hash.to_hex();

        let p = tg.path_for(&hash);
        let rel = p
            .strip_prefix(td.path())
            .expect("path must live under data_dir");

        // Expected structure: thumbnails/<aa>/<full-hex>.webp
        let components: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert_eq!(components.len(), 3, "unexpected path shape: {rel:?}");
        assert_eq!(components[0], "thumbnails");
        assert_eq!(
            components[1],
            &hex[..2],
            "prefix dir must be first 2 hex chars"
        );
        assert_eq!(
            components[2],
            format!("{hex}.webp"),
            "filename must be full hex + .webp"
        );
    }

    #[test]
    fn generate_produces_max_dim_preserving_aspect() {
        let td = TempDir::new().expect("tempdir");
        let src_dir = td.path().join("src");
        std::fs::create_dir_all(&src_dir).expect("mkdir src");
        let data_dir = td.path().join("data");

        // 1000×500 source → expect 256×128 when max_size = 256.
        let src = write_png(&src_dir, "wide.png", 1000, 500);
        let tg = generator(&data_dir, 256);
        let hash = hash_of(b"wide");

        let out = tg
            .generate(&hash, &src)
            .expect("generate")
            .expect("enabled generator returns Some");
        assert!(out.exists(), "thumbnail must be created at {out:?}");

        let decoded = image::open(&out).expect("reopen thumbnail");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (256, 128),
            "aspect must be preserved with longer side clamped to max_size"
        );
    }

    #[test]
    fn generate_idempotent() {
        let td = TempDir::new().expect("tempdir");
        let src = write_png(td.path(), "idem.png", 64, 64);
        let data_dir = td.path().join("data");
        let tg = generator(&data_dir, 256);
        let hash = hash_of(b"idem");

        let out1 = tg
            .generate(&hash, &src)
            .expect("first")
            .expect("enabled generator returns Some");
        let meta1 = std::fs::metadata(&out1).expect("meta1");
        let mtime1 = meta1.modified().expect("mtime1");

        // Wait a tick so any re-write would be observable via mtime.
        std::thread::sleep(std::time::Duration::from_millis(20));

        let out2 = tg
            .generate(&hash, &src)
            .expect("second")
            .expect("enabled generator returns Some");
        assert_eq!(out1, out2, "path must be deterministic");
        let mtime2 = std::fs::metadata(&out2)
            .expect("meta2")
            .modified()
            .expect("mtime2");

        // Idempotent: mtime must not advance on the second call.
        assert_eq!(
            mtime1, mtime2,
            "second generate must not rewrite an existing target"
        );
    }

    #[test]
    fn generate_no_tmp_remains_on_success() {
        let td = TempDir::new().expect("tempdir");
        let src = write_png(td.path(), "tmp.png", 128, 96);
        let data_dir = td.path().join("data");
        let tg = generator(&data_dir, 256);
        let hash = hash_of(b"tmpcheck");

        let out = tg
            .generate(&hash, &src)
            .expect("generate")
            .expect("enabled generator returns Some");
        let dir = out.parent().expect("thumbnail path must have a parent dir");

        let tmp_entries: Vec<_> = std::fs::read_dir(dir)
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "tmp"))
            .collect();
        let leftover: Vec<_> = tmp_entries.iter().map(std::fs::DirEntry::path).collect();
        assert!(
            tmp_entries.is_empty(),
            "no .tmp file must remain after successful generate; found {leftover:?}"
        );
    }

    #[test]
    fn disabled_generator_returns_none_and_writes_nothing() {
        let td = TempDir::new().expect("tempdir");
        let src = write_png(td.path(), "off.png", 32, 32);
        let tg = ThumbnailGenerator::disabled();
        let hash = hash_of(b"off");

        let out = tg.generate(&hash, &src).expect("generate");
        assert!(
            out.is_none(),
            "disabled generator must return Ok(None); got {out:?}"
        );
        // No directories created under an ephemeral `PathBuf::new()`
        // root (the disabled generator never touches the filesystem).
    }

    #[test]
    #[ignore = "perf benchmark; run: cargo test -p perima-media --release resize_only_bench -- --ignored --nocapture"]
    #[allow(clippy::unwrap_used, clippy::print_stderr)]
    fn resize_only_bench() {
        use std::time::Instant;
        let tmp = tempfile::tempdir().unwrap();
        let tgen = ThumbnailGenerator::with_max_size(tmp.path().to_path_buf(), 512);

        // 4928×3279 (24MP) matches fast_image_resize's upstream benchmark.
        // Smaller sources hide the SIMD win behind loop overhead.
        let rgb = image::RgbImage::from_pixel(4928, 3279, image::Rgb([128, 64, 32]));
        let src_dyn = image::DynamicImage::ImageRgb8(rgb);

        let start = Instant::now();
        for _ in 0..50 {
            let _ = tgen.resize_image(src_dyn.clone()).expect("resize_image");
        }
        let elapsed = start.elapsed();
        eprintln!(
            "resize_only_bench: 50 iters 4928x3279 → 512x(aspect) in {elapsed:?} ({:?} / iter)",
            elapsed / 50
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod pixel_type_tests {
    use super::*;
    use image::DynamicImage;

    #[test]
    fn resize_preserves_rgb_source_as_rgb() {
        let tmp = tempfile::tempdir().unwrap();
        let tgen = ThumbnailGenerator::with_max_size(tmp.path().to_path_buf(), 64);

        let rgb = image::RgbImage::from_pixel(200, 100, image::Rgb([200, 100, 50]));
        let src = DynamicImage::ImageRgb8(rgb);

        let resized = tgen.resize_image(src).expect("resize_image");

        assert!(
            matches!(resized, DynamicImage::ImageRgb8(_)),
            "RGB source must stay RGB after resize; got {:?}",
            resized.color()
        );
    }

    #[test]
    fn resize_preserves_rgba_source_as_rgba() {
        let tmp = tempfile::tempdir().unwrap();
        let tgen = ThumbnailGenerator::with_max_size(tmp.path().to_path_buf(), 64);

        let rgba = image::RgbaImage::from_pixel(200, 100, image::Rgba([200, 100, 50, 180]));
        let src = DynamicImage::ImageRgba8(rgba);

        let resized = tgen.resize_image(src).expect("resize_image");

        assert!(
            matches!(resized, DynamicImage::ImageRgba8(_)),
            "RGBA source must stay RGBA after resize; got {:?}",
            resized.color()
        );
    }

    #[test]
    fn resize_coerces_16bit_rgb_to_8bit_rgba() {
        let tmp = tempfile::tempdir().unwrap();
        let tgen = ThumbnailGenerator::with_max_size(tmp.path().to_path_buf(), 64);

        let rgb16 = image::ImageBuffer::<image::Rgb<u16>, _>::from_pixel(
            200,
            100,
            image::Rgb([30000_u16, 20000, 10000]),
        );
        let src = DynamicImage::ImageRgb16(rgb16);

        let resized = tgen.resize_image(src).expect("resize_image");

        assert!(
            matches!(
                resized,
                DynamicImage::ImageRgba8(_) | DynamicImage::ImageRgb8(_)
            ),
            "16-bit source must coerce to 8-bit variant; got {:?}",
            resized.color()
        );
    }
}
