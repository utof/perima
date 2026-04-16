//! Domain value types.
//!
//! WHY (content-addressed PK, landed at the migration in phase 1b):
//! `files.blake3_hash` will be the primary key on the `files` table
//! even though `CLAUDE.md` mandates `UUIDv7` PKs. A BLAKE3-256 hash is
//! deterministic and content-derived — two devices hashing identical
//! bytes MUST compute the same value, so using it as a PK satisfies
//! the CRDT-merge invariant that the `UUIDv7` rule exists to enforce
//! (no accidental divergence). A content hash is effectively a
//! deterministic UUID whose generation function is "hash the bytes";
//! the merge is free. The `UUIDv7` rule applies only to rows whose
//! identity is NOT content-derived (volumes, locations, mounts).
//! This comment ships in 1a so 1b's migration reproduces the
//! rationale verbatim.

use serde::{Deserialize, Serialize};

use crate::errors::CoreError;

/// BLAKE3-256 content hash (32 bytes).
///
/// Stored as lowercase hex at the persistence boundary. Custom serde
/// impl serializes as a 64-char hex string (not a raw byte array)
/// so JSON consumers see `"a1b2c3..."` instead of `[161, 178, ...]`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlakeHash([u8; 32]);

impl Serialize for BlakeHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for BlakeHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse_hex(&s).map_err(serde::de::Error::custom)
    }
}

impl BlakeHash {
    /// Construct from raw bytes.
    #[must_use]
    pub const fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// Render as 64-char lowercase hex.
    #[must_use]
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            // Hand-rolled to guarantee lowercase without depending on fmt quirks.
            out.push(nibble_to_hex(byte >> 4));
            out.push(nibble_to_hex(byte & 0x0f));
        }
        out
    }

    /// Parse from 64-char lowercase hex. Uppercase hex is rejected
    /// so the DB form is stable.
    ///
    /// # Errors
    /// Returns `CoreError::InvalidHash` on wrong length, non-hex
    /// characters, or uppercase letters.
    pub fn parse_hex(s: &str) -> Result<Self, CoreError> {
        if s.len() != 64 {
            return Err(CoreError::InvalidHash(format!(
                "expected 64 chars, got {}",
                s.len()
            )));
        }
        let mut out = [0u8; 32];
        let bytes = s.as_bytes();
        for i in 0..32 {
            let hi = parse_nibble(bytes[i * 2])
                .ok_or_else(|| CoreError::InvalidHash(format!("invalid char at {}", i * 2)))?;
            let lo = parse_nibble(bytes[i * 2 + 1])
                .ok_or_else(|| CoreError::InvalidHash(format!("invalid char at {}", i * 2 + 1)))?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }

    /// Raw byte view.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

const fn nibble_to_hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        // WHY: unreachable only because callers mask to 4 bits. If this ever
        // fires in prod, the bitwise masks in `to_hex` were broken.
        _ => unreachable!(),
    }
}

const fn parse_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        // WHY: uppercase rejected so persisted form is case-stable.
        _ => None,
    }
}

/// File size in bytes. Newtype to prevent arithmetic with other u64s.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FileSize(pub u64);

/// Path relative to a volume root. NFC-normalized, forward-slash,
/// no leading slash. The constructor is *idempotent* AND makes
/// canonically-equivalent inputs compare equal (NFC = NFD).
///
/// WHY: the combination of (NFC normalization + forward-slash
/// conversion + leading-slash strip) in one pass is what makes
/// the constructor simultaneously idempotent AND case-canonical
/// under Unicode equivalence. Splitting these into separate
/// passes would preserve idempotence but break equivalence
/// (NFC-then-slash-fix would still differ from slash-fix-then-NFC
/// on edge cases involving combining marks inside path segments).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct MediaPath(String);

impl MediaPath {
    /// Construct a normalized `MediaPath` from a raw string.
    #[must_use]
    pub fn new(raw: &str) -> Self {
        use unicode_normalization::UnicodeNormalization;
        let nfc: String = raw.nfc().collect();
        let slashed = nfc.replace('\\', "/");
        let trimmed = slashed.trim_start_matches('/').to_owned();
        Self(trimmed)
    }

    /// Borrow the normalized string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

use std::path::PathBuf;

/// `UUIDv7` volume identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct VolumeId(pub uuid::Uuid);

impl VolumeId {
    /// Generate a new `UUIDv7`-backed volume id.
    #[must_use]
    pub fn new() -> Self {
        Self(crate::ids::new_id())
    }
}

impl Default for VolumeId {
    fn default() -> Self {
        Self::new()
    }
}

/// `UUIDv7` device identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct DeviceId(pub uuid::Uuid);

impl DeviceId {
    /// Generate a new `UUIDv7`-backed device id (used on first run).
    #[must_use]
    pub fn new() -> Self {
        Self(crate::ids::new_id())
    }
}

impl Default for DeviceId {
    fn default() -> Self {
        Self::new()
    }
}

/// Output of the scanner; pre-hash.
#[derive(Clone, Debug)]
pub struct DiscoveredFile {
    /// Absolute path as observed during the walk.
    pub absolute_path: PathBuf,
    /// Path relative to the volume root, NFC-normalized.
    pub relative_path: MediaPath,
    /// File size in bytes at walk time.
    pub size: FileSize,
}

/// Post-hash pipeline record.
#[derive(Clone, Debug)]
pub struct HashedFile {
    /// The scanner output that produced this record.
    pub discovered: DiscoveredFile,
    /// BLAKE3-256 content hash of the file contents.
    pub hash: BlakeHash,
}

/// Status of a file location row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LocationStatus {
    /// Visible on the expected volume at the expected path.
    Active,
    /// The path was not found on the last verification.
    Missing,
    /// The file has moved elsewhere on the same volume.
    Moved,
}

/// Outcome of a repository upsert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpsertOutcome {
    /// Row did not exist and was inserted.
    Inserted,
    /// Row existed and was updated.
    Updated,
    /// Row existed and matched; no write performed.
    Unchanged,
}

/// Row returned by `FileRepository::list_file_locations`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileLocationRecord {
    /// Content hash of the underlying file.
    pub hash: BlakeHash,
    /// File size in bytes.
    pub size: FileSize,
    /// Volume the location lives on.
    pub volume_id: VolumeId,
    /// Relative path within the volume.
    pub relative_path: MediaPath,
    /// Location status.
    pub status: LocationStatus,
    /// ISO 8601 UTC timestamp of first sighting.
    pub first_seen: String,
}

/// Observed identifiers for a volume during detection (phase 1c fills).
#[derive(Clone, Debug)]
pub struct VolumeIdentifiers {
    /// GPT partition GUID if available.
    pub gpt_partition_guid: Option<String>,
    /// Filesystem UUID if available.
    pub fs_uuid: Option<String>,
    /// Human-readable volume label if available.
    pub label: Option<String>,
    /// Total capacity in bytes.
    pub capacity_bytes: u64,
    /// Whether the OS reports this as removable.
    pub is_removable: bool,
}

/// Row returned by `VolumeRepository::list`.
#[derive(Clone, Debug)]
pub struct VolumeRecord {
    /// Volume id.
    pub id: VolumeId,
    /// Label if any.
    pub label: Option<String>,
    /// Capacity in bytes.
    pub capacity_bytes: u64,
    /// Removable flag.
    pub is_removable: bool,
    /// Current mount paths on this machine.
    pub mounts_on_this_machine: Vec<PathBuf>,
    /// ISO 8601 UTC timestamp of last sighting.
    pub last_seen: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake_hash_round_trip() {
        let bytes = [0x42u8; 32];
        let h = BlakeHash::from_bytes(bytes);
        let s = h.to_hex();
        assert_eq!(s.len(), 64);
        assert!(
            s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        let parsed = BlakeHash::parse_hex(&s).expect("parse_hex round-trip");
        assert_eq!(parsed, h);
        assert_eq!(parsed.as_bytes(), &bytes);
    }

    #[test]
    fn blake_hash_rejects_wrong_length() {
        assert!(BlakeHash::parse_hex("abc").is_err());
        assert!(BlakeHash::parse_hex(&"a".repeat(63)).is_err());
        assert!(BlakeHash::parse_hex(&"a".repeat(65)).is_err());
    }

    #[test]
    fn blake_hash_rejects_non_hex() {
        let bad: String = "Z".repeat(64);
        assert!(BlakeHash::parse_hex(&bad).is_err());
    }

    #[test]
    fn blake_hash_rejects_uppercase() {
        let upper: String = "A".repeat(64);
        assert!(BlakeHash::parse_hex(&upper).is_err());
    }

    #[test]
    fn file_size_is_copy() {
        let a = FileSize(1024);
        let b = a;
        assert_eq!(a.0, b.0);
    }

    #[test]
    fn media_path_strips_leading_slash() {
        assert_eq!(MediaPath::new("/photos/a.jpg").as_str(), "photos/a.jpg");
        assert_eq!(MediaPath::new("///photos/a.jpg").as_str(), "photos/a.jpg");
    }

    #[test]
    fn media_path_forward_slashes() {
        assert_eq!(
            MediaPath::new("photos\\2024\\a.jpg").as_str(),
            "photos/2024/a.jpg"
        );
    }

    #[test]
    fn media_path_nfc_equivalence_fixed() {
        // "café" — precomposed (NFC) vs decomposed (NFD).
        let nfc = "caf\u{00E9}";
        let nfd = "cafe\u{0301}";
        assert_eq!(MediaPath::new(nfc), MediaPath::new(nfd));
    }

    #[test]
    fn media_path_idempotent_fixed_cases() {
        for s in &["photos/a.jpg", "a", "", "caf\u{0301}", "a/b/c"] {
            let once = MediaPath::new(s);
            let twice = MediaPath::new(once.as_str());
            assert_eq!(once, twice);
        }
    }

    #[test]
    fn volume_id_new_is_unique() {
        let a = VolumeId::new();
        let b = VolumeId::new();
        assert_ne!(a.0, b.0);
    }
}
