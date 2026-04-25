#![no_main]
use libfuzzer_sys::fuzz_target;
use perima_core::HashService;
use perima_hash::Blake3Service;
use std::hint::black_box;
use std::io::Write;

fuzz_target!(|data: &[u8]| {
    // WHY tempfile (NOT Cursor / byte slice): Blake3Service exposes only
    // `quick_hash(&Path)` and `full_hash(&Path)` (verified against
    // crates/hash/src/blake3_service.rs lines 30-37). There is no Read /
    // byte-slice / Cursor entry point. Adding a `hash_bytes(&[u8])`
    // method purely for fuzz testability would be production-API bloat
    // (YAGNI per spec §10 Q2). Mirror the exif.rs tempfile pattern.
    let Ok(td) = tempfile::tempdir() else { return; };
    let path = td.path().join("fuzz.bin");
    if std::fs::File::create(&path).and_then(|mut f| f.write_all(data)).is_err() {
        return;
    }

    let svc = Blake3Service::new();
    // Both hash modes share the inner `hash_file` helper (lines 39-62);
    // calling both per iteration covers the cap-Some (quick) and cap-None
    // (full) branches with near-zero added cost (BLAKE3 on ≤64 KiB is
    // microsecond-range). black_box prevents optimizer discard.
    let _ = black_box(svc.quick_hash(&path));
    let _ = black_box(svc.full_hash(&path));
});
