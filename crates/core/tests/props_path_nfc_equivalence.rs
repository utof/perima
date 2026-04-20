//! macOS-only test: `fs::canonicalize` returns the stored dirent name
//! for any NFC/NFD lookup spelling of the same file. This replaces
//! the pre-L5 proptest that asserted
//! `MediaPath::new(NFC) == MediaPath::new(NFD)` — see
//! `crates/core/src/types.rs` `MediaPath` doc for the invariant shift.
//!
//! # Mechanism (reviewer-verified)
//!
//! Both APFS and HFS+ are normalization-insensitive at the dirent-
//! lookup layer:
//! - HFS+ stores NFD; any NFC or NFD lookup resolves to the NFD entry.
//! - APFS stores caller-form-preserving; lookup uses a hash of the
//!   canonically-normalized filename (Apple APFS FAQ). Any NFC or NFD
//!   lookup of a given file resolves to the single stored dirent.
//!
//! `realpath(3)` — which `std::fs::canonicalize` calls — returns the
//! stored dirent name (whichever form the file was written under). So
//! `canonicalize(NFC_lookup)` and `canonicalize(NFD_lookup)` return
//! the SAME string (the stored one) on both filesystems.
//!
//! On Linux ext4: byte-exact, no normalization; `canonicalize(NFD)`
//! of an NFC-stored file FAILS. We don't attempt the equivalence
//! there.
//!
//! On Windows NTFS: the Win32 API layer applies some normalization
//! but the behavior is hand-wavy across NT versions + drivers. We
//! skip Windows rather than assert a weaker claim that might mislead
//! a debugger.

#[cfg(target_os = "macos")]
#[test]
fn fs_canonicalize_resolves_nfc_and_nfd_lookups_to_same_stored_name() {
    use std::fs;
    let dir = tempfile::tempdir().expect("tempdir");
    // Write the fixture under an NFC-spelled filename.
    let nfc_name = "caf\u{00E9}.txt";
    let nfc_path = dir.path().join(nfc_name);
    fs::write(&nfc_path, b"hello").expect("write NFC fixture");

    // Look up the same file via its NFD spelling. Both APFS + HFS+
    // resolve this to the SAME stored dirent.
    let nfd_name = "cafe\u{0301}.txt";
    let nfd_path = dir.path().join(nfd_name);

    let canon_nfc = fs::canonicalize(&nfc_path).expect("canonicalize NFC");
    let canon_nfd = fs::canonicalize(&nfd_path).expect("canonicalize NFD on macOS");
    assert_eq!(
        canon_nfc, canon_nfd,
        "macOS realpath should return the same stored dirent name for NFC + NFD lookups"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn media_path_byte_identity_off_macos() {
    // On Linux + Windows we don't exercise fs::canonicalize NFC/NFD
    // equivalence — Linux has none (byte-exact ext4), Windows is
    // hand-wavy across NT versions. The weaker invariant tested here:
    // MediaPath::new is deterministic byte-identity under identical
    // input. NFC-equivalence is no longer MediaPath's job.
    use perima_core::MediaPath;
    let s = "caf\u{00E9}/file.txt";
    assert_eq!(MediaPath::new(s), MediaPath::new(s));
    assert_eq!(MediaPath::new(s).as_str(), "caf\u{00E9}/file.txt");
}
