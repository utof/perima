//! Integration: `perima scan` enqueues media files into the metadata
//! queue and the bounded drain persists rows before the process exits.
//!
//! WHY this test lives in `crates/cli` (not `crates/media`): we are
//! verifying end-to-end wiring through the scanner + queue + drain —
//! i.e. the thing an end user invokes. The media-crate tests already
//! cover extractors + queue in isolation.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use image::{ImageBuffer, Rgb};

const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

/// Write a solid-red PNG and a solid-red JPEG under `dir`.
///
/// WHY two formats: the scanner should enqueue regardless of the
/// concrete MIME the extractor handles. PNG goes through
/// `ImageExtractor::extract` with no EXIF; JPEG exercises the same
/// path with a (possibly) EXIF-less file. Both should yield a
/// `file_metadata` row.
fn make_fixture(dir: &Path) {
    let png_path = dir.join("red.png");
    let jpg_path = dir.join("red.jpg");

    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(8, 6, |_, _| Rgb([255, 0, 0]));
    img.save_with_format(&png_path, image::ImageFormat::Png)
        .expect("save png");
    img.save_with_format(&jpg_path, image::ImageFormat::Jpeg)
        .expect("save jpg");

    // WHY a non-image sibling: makes the assertion more honest — if
    // our query accidentally matched "all rows in file_metadata across
    // the DB", a non-image would not be there anyway; but if our
    // scanner mistakenly enqueued everything (including .txt), the
    // extractor would still write an empty row and inflate the count.
    // The test asserts "at least 2 rows" to tolerate that case, but
    // having a non-image file present is what makes the scan realistic.
    let txt_path = dir.join("notes.txt");
    std::fs::File::create(&txt_path)
        .expect("create txt")
        .write_all(b"hello")
        .expect("write txt");
}

#[test]
fn scan_persists_metadata_rows_for_images() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    make_fixture(td.path());

    // Default scan performs the bounded 30-s drain; we expect the
    // two PNGs/JPEGs to complete well under that budget.
    let output = Command::new(bin())
        .arg("scan")
        .arg(td.path())
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("run");

    assert!(
        output.status.success(),
        "scan failed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Inspect the DB directly — bypass the CLI to keep the assertion
    // tight. WHY: `perima ls --with-metadata` would work too but it
    // layers another parser over the same rows.
    let db_path = env_dir.path().join("perima.db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");

    // The queue is drained synchronously at scan exit, so by the time
    // `Command::output()` returns the rows should already be present.
    let meta_count: i64 = conn
        .query_row("SELECT count(*) FROM file_metadata", [], |r| r.get(0))
        .expect("count file_metadata");

    // At least two (for the PNG + JPEG). The scanner actually enqueues
    // every successfully hashed file — including .txt — so the txt
    // file will also produce a row with the composite's "no extractor
    // accepts" empty-but-valid fallback. We assert >=2 to keep the
    // test robust against that behaviour either way.
    assert!(
        meta_count >= 2,
        "expected at least 2 file_metadata rows after scan; got {meta_count}",
    );

    // Verify the PNG row has captured dimensions (ImageExtractor ran).
    let png_hash: String = conn
        .query_row(
            "SELECT fl.blake3_hash FROM file_locations fl
             WHERE fl.relative_path = 'red.png' AND fl.deleted_at IS NULL",
            [],
            |r| r.get(0),
        )
        .expect("find png location");
    let png_dims: (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT width, height FROM file_metadata WHERE blake3_hash = ?1",
            [&png_hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("find png metadata");
    assert_eq!(
        png_dims,
        (Some(8), Some(6)),
        "PNG extractor should have captured the 8x6 dimensions",
    );

    // Verify thumbnail was generated + status flipped to 'ready'.
    //
    // WHY on-disk check: the worker calls `update_thumbnail(...,
    // "ready", ...)` only after `ThumbnailGenerator::generate` returns
    // `Ok(Some(path))`, which itself only returns Some after the
    // atomic rename completes. Seeing the file on disk + status =
    // 'ready' proves the full round-trip.
    let (thumb_path_opt, thumb_status): (Option<String>, String) = conn
        .query_row(
            "SELECT thumbnail_path, thumbnail_status FROM file_metadata
             WHERE blake3_hash = ?1",
            [&png_hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("find png thumbnail columns");
    assert_eq!(
        thumb_status, "ready",
        "PNG thumbnail status must flip to 'ready' after scan drain",
    );
    let thumb_path = thumb_path_opt.expect("thumbnail_path must be Some when status = 'ready'");
    assert!(
        std::path::Path::new(&thumb_path).exists(),
        "thumbnail file must exist on disk at {thumb_path}",
    );

    // Sanity: thumbnail lives under <data_dir>/thumbnails/<aa>/.
    let expected_dir_prefix = env_dir.path().join("thumbnails").join(&png_hash[..2]);
    assert!(
        thumb_path.starts_with(expected_dir_prefix.to_str().expect("data_dir utf-8")),
        "thumbnail_path {thumb_path} must live under {}",
        expected_dir_prefix.display(),
    );
}

#[test]
fn scan_no_wait_metadata_skips_drain() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    make_fixture(td.path());

    // With --no-wait-metadata, the CLI should still succeed; it just
    // does not guarantee rows are present by exit. We only assert the
    // exit code here — the row count is indeterminate.
    let output = Command::new(bin())
        .arg("scan")
        .arg(td.path())
        .arg("--no-wait-metadata")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("run");

    assert!(
        output.status.success(),
        "scan --no-wait-metadata failed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
