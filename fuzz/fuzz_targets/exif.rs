#![no_main]
use libfuzzer_sys::fuzz_target;
// WHY MetadataExtractor from perima_core (NOT perima_media): the trait is
// defined at crates/core/src/metadata.rs and re-exported from
// perima_core's lib.rs root; perima_media only re-exports the concrete
// extractor structs (ImageExtractor / VideoExtractor / CompositeExtractor),
// NOT the trait. Trait method resolution requires the trait to be in scope.
use perima_core::{BlakeHash, MetadataExtractor};
use perima_media::ImageExtractor;
use std::hint::black_box;
use std::io::Write;

fuzz_target!(|data: &[u8]| {
    // WHY tempfile: nom-exif's MediaSource::file_path takes a Path, not bytes.
    // We pay the file-create cost per fuzz iteration; alternative is a
    // Read+Seek wrapper, but the production path goes through file_path so
    // we mirror that. tempfile cleanup happens via TempDir Drop at end of
    // each iteration.
    let Ok(td) = tempfile::tempdir() else { return; };
    let path = td.path().join("fuzz.jpg");
    if std::fs::File::create(&path).and_then(|mut f| f.write_all(data)).is_err() {
        return;
    }

    // WHY this fuzzes the FULL extract pipeline (not just nom-exif):
    // ImageExtractor::extract calls `image::image_dimensions` BEFORE
    // read_exif (per crates/media/src/extractor.rs lines 44-54). With
    // JPEG-mime'd random bytes, the `image` crate's JPEG header parser
    // runs first; nom-exif only runs after image_dimensions has touched
    // the input. Both surfaces must be panic-free on any input — Result-Err
    // is fine; panic is the bug. Widening `read_exif` to `pub` purely to
    // fuzz it in isolation is undesirable production-API growth.
    let extractor = ImageExtractor::new();
    let hash = BlakeHash::from_bytes([0u8; 32]);
    // black_box prevents the optimizer from discarding the call when the
    // result is unused.
    let _ = black_box(extractor.extract(hash, &path, "image/jpeg"));
});
