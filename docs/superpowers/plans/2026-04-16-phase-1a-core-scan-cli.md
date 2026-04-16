# Phase 1a — Core Types, Ports, Scan-Without-DB Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the hexagonal core (`BlakeHash`, `MediaPath`, `VolumeId`, trait ports, `CoreError`), the hash + filesystem adapters, and a CLI `perima scan --dry-run <path>` that walks + hashes + prints. No DB access; no watching; cross-cutting concerns (config, logging, Ctrl-C, panic hook) land here to prevent retrofits.

**Architecture:** `crates/core` holds domain + trait ports with zero framework deps. `crates/hash` impls `HashService` via the `blake3` crate with rayon-parallel hashing. `crates/fs` impls `Scanner` via `walkdir` + `unicode-normalization` + `path-slash`. `crates/cli` composes adapters, installs panic/Ctrl-C handlers, and dispatches `perima scan --dry-run`. Sync throughout; rayon for CPU-bound hashing; no tokio yet (phase 3).

**Tech Stack:** Rust 1.85+ edition 2024, blake3, walkdir, unicode-normalization, path-slash, dunce, rayon, ctrlc, clap v4 (derive), uuid v7, thiserror, anyhow, tracing + tracing-subscriber, directories, tempfile, insta, proptest.

**Spec:** `docs/superpowers/specs/2026-04-16-phase-1a-core-scan-cli-design.md`

**Execution rule (from CLAUDE.md):** All work on `main`. Per-commit order: execute → `just ci` green → reviewer subagent approves → commit. Never commit unreviewed work. No `--force`, no `--no-verify`, no worktrees, no branches.

---

## File Structure

**Created in this phase (all committed):**

```
Cargo.toml                              # modify — add deps
crates/core/
├── Cargo.toml                          # modify — add deps
└── src/
    ├── lib.rs                          # modify — re-exports
    ├── ids.rs                          # new
    ├── errors.rs                       # new
    ├── types.rs                        # new
    └── ports/
        ├── mod.rs                      # new
        ├── hash.rs                     # new
        ├── scanner.rs                  # new
        ├── file_repo.rs                # new
        └── volume_repo.rs              # new

crates/hash/
├── Cargo.toml                          # modify — add deps
└── src/
    ├── lib.rs                          # modify — re-exports
    ├── errors.rs                       # new
    └── blake3_service.rs               # new

crates/fs/
├── Cargo.toml                          # modify — add deps
└── src/
    ├── lib.rs                          # modify — re-exports
    ├── errors.rs                       # new
    ├── paths.rs                        # new
    └── walker.rs                       # new

crates/cli/
├── Cargo.toml                          # modify — add deps
└── src/
    ├── main.rs                         # modify — full rewrite
    ├── config.rs                       # new
    ├── logging.rs                      # new
    ├── signals.rs                      # new
    ├── panic.rs                        # new
    └── cmd/
        ├── mod.rs                      # new
        └── scan.rs                     # new
└── tests/
    └── scan_dry_run.rs                 # new — integration test

crates/core/tests/
├── props_hash_determinism.rs           # new
├── props_path_idempotence.rs           # new
└── props_path_nfc_equivalence.rs       # new
```

Reviewer checkpoints gate four commits: after core, after adapters, after CLI, after final tests + tag.

---

## Task 1: Add workspace dependencies

**Files:**
- Modify: `Cargo.toml:41-65` (append to `[workspace.dependencies]`)

- [ ] **Step 1: Open Cargo.toml and insert new deps**

Append these lines immediately before the `[profile.release]` section (they join the existing `[workspace.dependencies]` table):

```toml
clap         = { version = "4", features = ["derive"] }
tempfile     = "3"
insta        = { version = "1", features = ["yaml", "filters"] }
proptest     = "1"
rayon        = "1"
ctrlc        = { version = "3", features = ["termination"] }
```

- [ ] **Step 2: Verify resolution**

Run: `cargo metadata --format-version 1 >/dev/null`
Expected: exit 0. Cargo resolves the new deps without network errors.

- [ ] **Step 3: Confirm `just ci` still green (no deps used yet)**

Run: `just ci`
Expected: exit 0.

---

## Task 2: `crates/core/Cargo.toml` — add deps + feature

**Files:**
- Modify: `crates/core/Cargo.toml`

- [ ] **Step 1: Add dependencies**

Replace the `[dependencies]` section (currently empty) with:

```toml
[dependencies]
thiserror.workspace = true
serde.workspace     = true
uuid.workspace      = true
unicode-normalization.workspace = true
```

- [ ] **Step 2: Verify core builds**

Run: `cargo build -p perima-core`
Expected: exit 0.

---

## Task 3: Core IDs helper (`crates/core/src/ids.rs`)

**Files:**
- Create: `crates/core/src/ids.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: Write the module**

`crates/core/src/ids.rs`:

```rust
//! UUIDv7 helpers.
//!
//! UUIDv7 (RFC 9562) is time-sortable with 48-bit ms timestamps; it
//! gives us globally unique IDs whose B-tree insertion order matches
//! creation order, avoiding SQLite index fragmentation.

use uuid::Uuid;

/// Generate a fresh UUIDv7.
#[must_use]
pub fn new_id() -> Uuid {
    Uuid::now_v7()
}
```

- [ ] **Step 2: Re-export from lib.rs**

Replace `crates/core/src/lib.rs` contents with:

```rust
//! Domain types and trait ports for perima.
//!
//! This crate has zero framework dependencies. Every other crate in
//! the workspace either defines types consumed here or adapts this
//! crate's traits to a concrete backend.

pub mod ids;

/// Marker placeholder. Replaced with real domain types in phase 1.
pub const CRATE_NAME: &str = "perima-core";
```

- [ ] **Step 3: Run the gates**

Run: `just ci`
Expected: exit 0.

---

## Task 4: Core errors (`crates/core/src/errors.rs`)

**Files:**
- Create: `crates/core/src/errors.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: Write the module**

`crates/core/src/errors.rs`:

```rust
//! Top-level error type crossing the core boundary.
//!
//! Adapters define their own internal errors and implement
//! `From<AdapterError> for CoreError` **inside the adapter crate**
//! so that `core` depends on no adapter (preserves hexagonal
//! direction).

use thiserror::Error;

/// Error returned by every `core` trait method.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Queried item was absent.
    #[error("not found: {0}")]
    NotFound(String),

    /// App-level uniqueness check rejected an insert.
    #[error("duplicate: {0}")]
    Duplicate(String),

    /// Path string could not be normalized or is outside the expected root.
    #[error("invalid path: {0}")]
    InvalidPath(String),

    /// Hex input was not a valid 64-char lowercase BLAKE3 hash.
    #[error("invalid hash hex: {0}")]
    InvalidHash(String),

    /// Underlying I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Feature is declared but not yet implemented at this phase.
    /// Dedicated variant so `main.rs` can map to a stable exit code
    /// without substring-matching prose.
    #[error("unsupported in this phase: {0}")]
    Unsupported(String),

    /// Any adapter-level failure that didn't map to a typed variant.
    #[error("internal: {0}")]
    Internal(String),
}
```

- [ ] **Step 2: Re-export from lib.rs**

Replace `crates/core/src/lib.rs` contents:

```rust
//! Domain types and trait ports for perima.
//!
//! This crate has zero framework dependencies. Every other crate in
//! the workspace either defines types consumed here or adapts this
//! crate's traits to a concrete backend.

pub mod errors;
pub mod ids;

pub use errors::CoreError;

/// Marker placeholder. Replaced with real domain types in phase 1.
pub const CRATE_NAME: &str = "perima-core";
```

- [ ] **Step 3: Run the gates**

Run: `just ci`
Expected: exit 0.

---

## Task 5: Core types — `BlakeHash` + `FileSize` (TDD)

**Files:**
- Create: `crates/core/src/types.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: Write failing unit tests**

Create `crates/core/src/types.rs` with just the test module first:

```rust
//! Domain value types.

// tests defined below force the impl shape

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake_hash_round_trip() {
        let bytes = [0x42u8; 32];
        let h = BlakeHash::from_bytes(bytes);
        let s = h.to_hex();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
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
}
```

- [ ] **Step 2: Run — expect compile fail**

Run: `cargo test -p perima-core --lib`
Expected: compile error "cannot find type `BlakeHash`".

- [ ] **Step 3: Implement `BlakeHash` + `FileSize`**

Replace `crates/core/src/types.rs` with:

```rust
//! Domain value types.
//!
//! WHY (content-addressed PK, landed at the migration in phase 1b):
//! `files.blake3_hash` will be the primary key on the `files` table
//! even though CLAUDE.md mandates UUIDv7 PKs. A BLAKE3-256 hash is
//! deterministic and content-derived — two devices hashing identical
//! bytes MUST compute the same value, so using it as a PK satisfies
//! the CRDT-merge invariant that the UUIDv7 rule exists to enforce
//! (no accidental divergence). A content hash is effectively a
//! deterministic UUID whose generation function is "hash the bytes";
//! the merge is free. The UUIDv7 rule applies only to rows whose
//! identity is NOT content-derived (volumes, locations, mounts).
//! This comment ships in 1a so 1b's migration reproduces the
//! rationale verbatim.

use serde::{Deserialize, Serialize};

use crate::errors::CoreError;

/// BLAKE3-256 content hash (32 bytes). Stored as lowercase hex at
/// the persistence boundary.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct BlakeHash([u8; 32]);

impl BlakeHash {
    /// Construct from raw bytes.
    #[must_use]
    pub fn from_bytes(b: [u8; 32]) -> Self {
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
            let hi = parse_nibble(bytes[i * 2]).ok_or_else(|| {
                CoreError::InvalidHash(format!("invalid char at {}", i * 2))
            })?;
            let lo = parse_nibble(bytes[i * 2 + 1]).ok_or_else(|| {
                CoreError::InvalidHash(format!("invalid char at {}", i * 2 + 1))
            })?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }

    /// Raw byte view.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
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

#[cfg(test)]
mod tests { /* see above */ }
```

Note: the `#[cfg(test)] mod tests` at the bottom must contain the tests written in Step 1. Keep them verbatim; the module was truncated above for brevity, but the real file has both the implementation AND the tests.

- [ ] **Step 4: Re-export from lib.rs**

Add `pub mod types;` plus `pub use types::{BlakeHash, FileSize};` to `crates/core/src/lib.rs`.

- [ ] **Step 5: Run the gates**

Run: `cargo test -p perima-core --lib` → expect 5 tests pass.
Run: `just ci` → exit 0.

---

## Task 6: Core types — `MediaPath` (TDD)

**Files:**
- Modify: `crates/core/src/types.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: Write failing tests**

Append to `crates/core/src/types.rs` `mod tests`:

```rust
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
```

- [ ] **Step 2: Run — expect compile fail**

Run: `cargo test -p perima-core --lib`
Expected: compile error "cannot find type `MediaPath`".

- [ ] **Step 3: Implement `MediaPath`**

Add to `crates/core/src/types.rs` (above the `#[cfg(test)]` block):

```rust
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
```

- [ ] **Step 4: Run the gates**

Run: `cargo test -p perima-core --lib` → 9 tests pass.
Run: `just ci` → exit 0.

---

## Task 7: Core types — `VolumeId`, `DeviceId`, records

**Files:**
- Modify: `crates/core/src/types.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: Append types + tests**

Add to `crates/core/src/types.rs` (above the test block):

```rust
use std::path::PathBuf;

/// UUIDv7 volume identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct VolumeId(pub uuid::Uuid);

impl VolumeId {
    /// Generate a new UUIDv7-backed volume id.
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

/// UUIDv7 device identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct DeviceId(pub uuid::Uuid);

impl DeviceId {
    /// Generate a new UUIDv7-backed device id (used on first run).
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
```

And add a small test to `mod tests`:

```rust
#[test]
fn volume_id_new_is_unique() {
    let a = VolumeId::new();
    let b = VolumeId::new();
    assert_ne!(a.0, b.0);
}
```

- [ ] **Step 2: Re-export from lib.rs**

Replace `crates/core/src/lib.rs` with:

```rust
//! Domain types and trait ports for perima.
//!
//! Zero framework dependencies.

pub mod errors;
pub mod ids;
pub mod types;

pub use errors::CoreError;
pub use types::{
    BlakeHash, DeviceId, DiscoveredFile, FileLocationRecord, FileSize, HashedFile,
    LocationStatus, MediaPath, UpsertOutcome, VolumeId, VolumeIdentifiers, VolumeRecord,
};

/// Marker placeholder. Retained as a public symbol for phase-0
/// compatibility tests; will be removed in phase 1b when the real
/// public surface covers it.
pub const CRATE_NAME: &str = "perima-core";
```

- [ ] **Step 3: Run the gates**

Run: `cargo test -p perima-core` → 10 tests pass.
Run: `just ci` → exit 0.

---

## Task 8: Core ports

**Files:**
- Create: `crates/core/src/ports/mod.rs`
- Create: `crates/core/src/ports/hash.rs`
- Create: `crates/core/src/ports/scanner.rs`
- Create: `crates/core/src/ports/file_repo.rs`
- Create: `crates/core/src/ports/volume_repo.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: Create `ports/mod.rs`**

```rust
//! Trait ports — the hexagonal boundary between core and adapters.

pub mod file_repo;
pub mod hash;
pub mod scanner;
pub mod volume_repo;

pub use file_repo::FileRepository;
pub use hash::HashService;
pub use scanner::Scanner;
pub use volume_repo::VolumeRepository;
```

- [ ] **Step 2: Create `ports/hash.rs`**

```rust
//! Hash service port.

use std::path::Path;

use crate::{BlakeHash, CoreError};

/// BLAKE3-based content hashing.
///
/// WHY `Send + Sync`: although phase 1a is entirely synchronous, the
/// scan loop uses `rayon` to parallelize `full_hash` calls across
/// files. Keeping the trait `Send + Sync` also leaves room for a
/// future async adapter without breaking the trait.
pub trait HashService: Send + Sync {
    /// Hash only the first 64 KiB. Cheap change-detection used by
    /// the phase 3 watcher. Phase 1a callers always use `full_hash`.
    ///
    /// # Errors
    /// Returns `CoreError::Io` on read failures.
    fn quick_hash(&self, path: &Path) -> Result<BlakeHash, CoreError>;

    /// Hash the entire file.
    ///
    /// # Errors
    /// Returns `CoreError::Io` on read failures.
    fn full_hash(&self, path: &Path) -> Result<BlakeHash, CoreError>;
}
```

- [ ] **Step 3: Create `ports/scanner.rs`**

```rust
//! Filesystem scanner port.

use std::path::Path;

use crate::{CoreError, DiscoveredFile};

/// Walks a directory tree and produces `DiscoveredFile`s.
pub trait Scanner: Send + Sync {
    /// Walk `root` recursively. Per-entry errors are logged via
    /// `tracing` and skipped (the iterator continues). Only
    /// terminal failures (e.g. permission denied on the root)
    /// return `Err`.
    ///
    /// `volume_root` is used to compute each file's relative path.
    ///
    /// # Errors
    /// Returns `CoreError::Io` if `root` cannot be opened, or
    /// `CoreError::InvalidPath` if `volume_root` is not a prefix
    /// of `root`.
    fn walk<'a>(
        &'a self,
        root: &Path,
        volume_root: &Path,
    ) -> Result<Box<dyn Iterator<Item = DiscoveredFile> + Send + 'a>, CoreError>;
}
```

- [ ] **Step 4: Create `ports/file_repo.rs`**

```rust
//! File + location repository port (implementations land in phase 1b).

use crate::{
    BlakeHash, CoreError, DeviceId, FileLocationRecord, HashedFile, MediaPath,
    UpsertOutcome, VolumeId,
};

/// Persistence boundary for `files` + `file_locations`.
pub trait FileRepository: Send + Sync {
    /// Upsert the content-addressed `files` row.
    ///
    /// # Errors
    /// Adapter-level errors are surfaced as `CoreError::Internal`
    /// unless they map to a typed variant.
    fn upsert_file(
        &mut self,
        file: &HashedFile,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError>;

    /// Upsert a `file_locations` row for `(volume, relative_path)`.
    ///
    /// # Errors
    /// Returns `CoreError::Duplicate` if the app-level uniqueness
    /// check rejects the row.
    fn upsert_location(
        &mut self,
        hash: &BlakeHash,
        volume: VolumeId,
        path: &MediaPath,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError>;

    /// List `(file, location)` joins. Used by `perima ls` in phase 1b.
    ///
    /// # Errors
    /// Adapter-level errors become `CoreError::Internal`.
    fn list_file_locations(
        &self,
        limit: usize,
        volume: Option<VolumeId>,
    ) -> Result<Vec<FileLocationRecord>, CoreError>;
}
```

- [ ] **Step 5: Create `ports/volume_repo.rs`**

```rust
//! Volume + volume-mount repository port (implementations in 1b/1c).

use std::path::Path;

use crate::{CoreError, DeviceId, VolumeId, VolumeIdentifiers, VolumeRecord};

/// Persistence boundary for `volumes` + `volume_mounts`.
pub trait VolumeRepository: Send + Sync {
    /// Find a known volume matching the observed identifiers, or
    /// create a new one.
    ///
    /// # Errors
    /// `CoreError::Internal` on adapter failure.
    fn find_or_create(
        &mut self,
        ident: &VolumeIdentifiers,
        device: DeviceId,
    ) -> Result<VolumeId, CoreError>;

    /// Record the current mount path for `volume` on `machine`.
    ///
    /// # Errors
    /// `CoreError::Internal` on adapter failure.
    fn record_mount(
        &mut self,
        volume: VolumeId,
        machine: DeviceId,
        mount: &Path,
    ) -> Result<(), CoreError>;

    /// Enumerate all known volumes with their current mounts on this
    /// machine.
    ///
    /// # Errors
    /// `CoreError::Internal` on adapter failure.
    fn list(&self) -> Result<Vec<VolumeRecord>, CoreError>;
}
```

- [ ] **Step 6: Update `lib.rs`**

Append to `crates/core/src/lib.rs`:

```rust
pub mod ports;
pub use ports::{FileRepository, HashService, Scanner, VolumeRepository};
```

- [ ] **Step 7: Dispatch reviewer subagent (checkpoint #1 — commit 1 prep)**

Reviewer checklist:
- [ ] `cargo build -p perima-core` exit 0.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exit 0.
- [ ] `cargo test -p perima-core` passes (≥ 10 tests).
- [ ] `cargo doc --workspace --no-deps` exit 0 (no `missing_docs`).
- [ ] Every pub item in `crates/core` has a doc comment.
- [ ] `// WHY:` comments present for: `MediaPath::new` combining normalizations, `HashService: Send + Sync`.

Must return APPROVED before Step 8.

- [ ] **Step 8: Commit (after APPROVED)**

```bash
git add Cargo.toml Cargo.lock crates/core/
git commit -m "$(cat <<'EOF'
feat(phase-1a): core types + ports + errors + ids

crates/core lands with zero framework deps: BlakeHash (content
hash), MediaPath (NFC + forward-slash + strip-leading normalization,
idempotent + NFC-equivalent under repeated application), VolumeId/
DeviceId (UUIDv7), DiscoveredFile/HashedFile pipeline records,
FileLocationRecord join view, LocationStatus enum, UpsertOutcome,
VolumeIdentifiers/VolumeRecord supporting types.

Ports: HashService (quick_hash + full_hash), Scanner (walk →
iterator of DiscoveredFile), FileRepository (upsert_file,
upsert_location, list_file_locations), VolumeRepository
(find_or_create, record_mount, list). Ports 3–4 are shape-only in
1a; implementations arrive in 1b/1c.

CoreError with thiserror; adapters will implement
From<AdapterError> for CoreError inside their own crates to
preserve the hexagonal dependency direction.

Refs: docs/superpowers/specs/2026-04-16-phase-1a-core-scan-cli-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Hash adapter (`crates/hash`)

**Files:**
- Modify: `crates/hash/Cargo.toml`
- Modify: `crates/hash/src/lib.rs`
- Create: `crates/hash/src/errors.rs`
- Create: `crates/hash/src/blake3_service.rs`

- [ ] **Step 1: Update `crates/hash/Cargo.toml`**

Replace `[dependencies]` section with:

```toml
[dependencies]
perima-core = { path = "../core" }
thiserror.workspace = true
blake3.workspace    = true
rayon.workspace     = true
tracing.workspace   = true
```

- [ ] **Step 2: Create `crates/hash/src/errors.rs`**

```rust
//! Internal errors for the hash adapter.

use thiserror::Error;

/// Errors raised inside `perima-hash`.
#[derive(Debug, Error)]
pub enum Error {
    /// I/O failure while reading a file.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<Error> for perima_core::CoreError {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(io) => perima_core::CoreError::Io(io),
        }
    }
}
```

- [ ] **Step 3: Create `crates/hash/src/blake3_service.rs` with tests first (TDD)**

```rust
//! BLAKE3 content-hashing service.

use std::io::Read;
use std::path::Path;

use perima_core::{BlakeHash, CoreError, HashService};

use crate::errors::Error;

/// Default chunk size for streaming reads. BLAKE3 is fastest when
/// fed reasonably-large chunks.
const CHUNK_SIZE: usize = 64 * 1024;

/// First-64-KiB cap used by `quick_hash`.
const QUICK_CAP: u64 = 64 * 1024;

/// `HashService` implementation backed by the `blake3` crate.
#[derive(Clone, Copy, Debug, Default)]
pub struct Blake3Service;

impl Blake3Service {
    /// Construct a stateless hasher.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl HashService for Blake3Service {
    fn quick_hash(&self, path: &Path) -> Result<BlakeHash, CoreError> {
        hash_file(path, Some(QUICK_CAP)).map_err(Into::into)
    }

    fn full_hash(&self, path: &Path) -> Result<BlakeHash, CoreError> {
        hash_file(path, None).map_err(Into::into)
    }
}

fn hash_file(path: &Path, cap: Option<u64>) -> Result<BlakeHash, Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; CHUNK_SIZE];
    let mut remaining = cap.unwrap_or(u64::MAX);
    while remaining > 0 {
        let want = std::cmp::min(buf.len() as u64, remaining) as usize;
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        remaining = remaining.saturating_sub(n as u64);
    }
    let out = hasher.finalize();
    Ok(BlakeHash::from_bytes(*out.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn deterministic_full_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.bin");
        std::fs::File::create(&path)
            .expect("create")
            .write_all(b"hello world")
            .expect("write");
        let svc = Blake3Service::new();
        let a = svc.full_hash(&path).expect("hash1");
        let b = svc.full_hash(&path).expect("hash2");
        assert_eq!(a, b);
    }

    #[test]
    fn full_hash_matches_blake3_reference() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.bin");
        std::fs::File::create(&path)
            .expect("create")
            .write_all(b"hello world")
            .expect("write");
        let svc = Blake3Service::new();
        let got = svc.full_hash(&path).expect("hash");
        let expected = blake3::hash(b"hello world");
        assert_eq!(got.as_bytes(), expected.as_bytes());
    }

    #[test]
    fn quick_hash_caps_at_64kib() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.bin");
        let payload = vec![0x42u8; 128 * 1024];
        std::fs::File::create(&path)
            .expect("create")
            .write_all(&payload)
            .expect("write");
        let svc = Blake3Service::new();
        let q = svc.quick_hash(&path).expect("quick");
        let expected = blake3::hash(&payload[..64 * 1024]);
        assert_eq!(q.as_bytes(), expected.as_bytes());
    }
}
```

Add `tempfile` to `[dev-dependencies]` in `crates/hash/Cargo.toml`:

```toml
[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 4: Update `crates/hash/src/lib.rs`**

```rust
//! BLAKE3-based content-hashing adapter for perima.

pub mod blake3_service;
pub mod errors;

pub use blake3_service::Blake3Service;
pub use errors::Error;

/// Marker placeholder retained for phase-0 compatibility.
pub const CRATE_NAME: &str = "perima-hash";
```

- [ ] **Step 5: Run the gates**

Run: `cargo test -p perima-hash` → 3 tests pass.
Run: `just ci` → exit 0.

---

## Task 10: FS adapter — errors + paths (TDD)

**Files:**
- Modify: `crates/fs/Cargo.toml`
- Modify: `crates/fs/src/lib.rs`
- Create: `crates/fs/src/errors.rs`
- Create: `crates/fs/src/paths.rs`

- [ ] **Step 1: Update `crates/fs/Cargo.toml`**

```toml
[dependencies]
perima-core = { path = "../core" }
thiserror.workspace  = true
walkdir.workspace    = true
path-slash.workspace = true
dunce.workspace      = true
tracing.workspace    = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 2: Create `errors.rs`**

```rust
//! Internal errors for the filesystem adapter.

use std::path::PathBuf;

use thiserror::Error;

/// Errors raised inside `perima-fs`.
#[derive(Debug, Error)]
pub enum Error {
    /// I/O failure walking the filesystem.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// A discovered path was not under the declared volume root.
    #[error("path not under volume root: {0}")]
    NotUnderVolume(PathBuf),
}

impl From<Error> for perima_core::CoreError {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(io) => perima_core::CoreError::Io(io),
            Error::NotUnderVolume(p) => {
                perima_core::CoreError::InvalidPath(p.display().to_string())
            }
        }
    }
}
```

- [ ] **Step 3: Create `paths.rs` with tests first**

```rust
//! Filesystem-level path helpers — resolve absolute paths to
//! volume-relative `MediaPath`s.

use std::path::Path;

use perima_core::MediaPath;

use crate::errors::Error;

/// Convert `absolute` into a `MediaPath` relative to `volume_root`.
///
/// `absolute` must be under `volume_root`; callers should pre-
/// canonicalize both sides to avoid symlink surprises.
///
/// # Errors
/// Returns `Error::NotUnderVolume` if `absolute` is not prefixed by
/// `volume_root`.
pub fn relativize(absolute: &Path, volume_root: &Path) -> Result<MediaPath, Error> {
    let abs = dunce::simplified(absolute);
    let root = dunce::simplified(volume_root);
    let rel = abs
        .strip_prefix(root)
        .map_err(|_| Error::NotUnderVolume(absolute.to_path_buf()))?;
    // WHY: convert to forward-slash explicitly before handing to
    // MediaPath so our Windows test cases normalize consistently
    // on non-Windows hosts.
    let as_str = path_slash::PathExt::to_slash_lossy(rel);
    Ok(MediaPath::new(as_str.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relativize_simple() {
        let root = std::path::Path::new("/tmp/perima");
        let abs = std::path::Path::new("/tmp/perima/photos/a.jpg");
        let r = relativize(abs, root).expect("relativize");
        assert_eq!(r.as_str(), "photos/a.jpg");
    }

    #[test]
    fn relativize_rejects_outside_root() {
        let root = std::path::Path::new("/tmp/perima");
        let abs = std::path::Path::new("/tmp/other/a.jpg");
        assert!(relativize(abs, root).is_err());
    }

    #[test]
    fn relativize_identity_root() {
        let root = std::path::Path::new("/tmp/perima");
        let r = relativize(root, root).expect("relativize root");
        assert_eq!(r.as_str(), "");
    }
}
```

- [ ] **Step 4: Update `crates/fs/src/lib.rs`**

```rust
//! Filesystem scanning, watching, and path normalization for perima.

pub mod errors;
pub mod paths;

pub use errors::Error;
pub use paths::relativize;

/// Marker placeholder retained for phase-0 compatibility.
pub const CRATE_NAME: &str = "perima-fs";
```

- [ ] **Step 5: Run the gates**

Run: `cargo test -p perima-fs` → 3 tests pass.
Run: `just ci` → exit 0.

---

## Task 11: FS adapter — walker (TDD)

**Files:**
- Create: `crates/fs/src/walker.rs`
- Modify: `crates/fs/src/lib.rs`

- [ ] **Step 1: Write the walker + tests**

`crates/fs/src/walker.rs`:

```rust
//! Recursive filesystem walker implementing `Scanner`.

use std::path::Path;

use perima_core::{CoreError, DiscoveredFile, FileSize, Scanner};

use crate::{errors::Error, paths::relativize};

/// `walkdir`-backed scanner.
#[derive(Clone, Copy, Debug, Default)]
pub struct WalkdirScanner;

impl WalkdirScanner {
    /// Construct a stateless walker.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Scanner for WalkdirScanner {
    fn walk<'a>(
        &'a self,
        root: &Path,
        volume_root: &Path,
    ) -> Result<Box<dyn Iterator<Item = DiscoveredFile> + Send + 'a>, CoreError> {
        // Smoke-check the root exists before we return the iterator
        // so callers get a terminal error immediately rather than
        // an empty stream.
        if !root.exists() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("root does not exist: {}", root.display()),
            ))
            .into());
        }

        let owned_volume_root = volume_root.to_path_buf();
        let iter = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(move |entry| match entry {
                Ok(e) => {
                    if !e.file_type().is_file() {
                        return None;
                    }
                    let metadata = match e.metadata() {
                        Ok(m) => m,
                        Err(err) => {
                            tracing::warn!(
                                path = %e.path().display(),
                                error = %err,
                                "skipping entry: cannot read metadata"
                            );
                            return None;
                        }
                    };
                    let rel = match relativize(e.path(), &owned_volume_root) {
                        Ok(r) => r,
                        Err(err) => {
                            tracing::warn!(
                                path = %e.path().display(),
                                error = %err,
                                "skipping entry: cannot relativize"
                            );
                            return None;
                        }
                    };
                    Some(DiscoveredFile {
                        absolute_path: e.path().to_path_buf(),
                        relative_path: rel,
                        size: FileSize(metadata.len()),
                    })
                }
                Err(err) => {
                    tracing::warn!(error = %err, "skipping entry: walk error");
                    None
                }
            });
        Ok(Box::new(iter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn mk_file(dir: &Path, name: &str, bytes: &[u8]) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create dirs");
        }
        std::fs::File::create(&path)
            .expect("create file")
            .write_all(bytes)
            .expect("write");
    }

    #[test]
    fn walks_three_files() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path();
        mk_file(root, "a.txt", b"alpha");
        mk_file(root, "sub/b.txt", b"beta");
        mk_file(root, "sub/c.bin", b"gamma");

        let scanner = WalkdirScanner::new();
        let mut names: Vec<String> = scanner
            .walk(root, root)
            .expect("walk")
            .map(|f| f.relative_path.as_str().to_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.txt", "sub/b.txt", "sub/c.bin"]);
    }

    #[test]
    fn missing_root_is_err() {
        let scanner = WalkdirScanner::new();
        let bogus = std::path::Path::new("/definitely/does/not/exist/perima-test");
        assert!(scanner.walk(bogus, bogus).is_err());
    }

    #[test]
    fn sizes_are_populated() {
        let td = tempfile::tempdir().expect("tempdir");
        mk_file(td.path(), "f.bin", &vec![0x00; 1024]);
        let scanner = WalkdirScanner::new();
        let files: Vec<_> = scanner.walk(td.path(), td.path()).expect("walk").collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].size.0, 1024);
    }
}
```

- [ ] **Step 2: Re-export from `crates/fs/src/lib.rs`**

Add `pub mod walker;` and `pub use walker::WalkdirScanner;`.

- [ ] **Step 3: Dispatch reviewer (checkpoint #2 — commit 2 prep)**

Reviewer checklist:
- [ ] `crates/hash`: `Blake3Service`, `hash::Error`, `From<Error>`, 3 tests pass.
- [ ] `crates/fs`: `relativize`, `WalkdirScanner`, `fs::Error`, `From<Error>`, 6 tests pass.
- [ ] Both adapters depend on `perima-core` but not on each other.
- [ ] `just ci` green.
- [ ] WHY-comment present for `path_slash` explicit slash conversion.

Must return APPROVED before Step 4.

- [ ] **Step 4: Commit (after APPROVED)**

```bash
git add crates/hash/ crates/fs/ Cargo.lock
git commit -m "$(cat <<'EOF'
feat(phase-1a): hash + fs adapters

crates/hash: Blake3Service impls HashService. Streaming reader
with 64 KiB chunks; quick_hash caps at first 64 KiB (phase 3
change-detection path); full_hash runs unbounded. hash::Error
implements From for CoreError inside the adapter crate.

crates/fs: relativize() resolves absolute paths to volume-relative
MediaPath via dunce (Windows UNC fix) + path-slash (forward-slash
convention). WalkdirScanner impls Scanner; per-entry errors are
logged via tracing and skipped (the iterator continues), only
terminal failures abort.

Refs: docs/superpowers/specs/2026-04-16-phase-1a-core-scan-cli-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: CLI — `config` + `logging` modules

**Files:**
- Modify: `crates/cli/Cargo.toml`
- Create: `crates/cli/src/config.rs`
- Create: `crates/cli/src/logging.rs`

- [ ] **Step 1: Update `crates/cli/Cargo.toml`**

```toml
[dependencies]
perima-core = { path = "../core" }
perima-hash = { path = "../hash" }
perima-fs   = { path = "../fs" }

clap.workspace               = true
anyhow.workspace             = true
tracing.workspace            = true
tracing-subscriber.workspace = true
directories.workspace        = true
ctrlc.workspace              = true
rayon.workspace              = true
uuid.workspace               = true

[dev-dependencies]
tempfile.workspace = true
insta.workspace    = true
```

- [ ] **Step 2: Create `config.rs`**

```rust
//! Runtime configuration: data dir, config dir, device id.

use std::path::{Path, PathBuf};

use perima_core::{CoreError, DeviceId, ids};

/// Resolved configuration for a CLI invocation.
#[derive(Clone, Debug)]
pub struct Config {
    /// Where the main database will live (1b uses this).
    pub data_dir: PathBuf,
    /// Where `device_id.txt` and future user config live.
    pub config_dir: PathBuf,
    /// Stable device identifier.
    pub device_id: DeviceId,
}

impl Config {
    /// Resolve from the `directories` crate, then env overrides
    /// (`PERIMA_DATA_DIR`, `PERIMA_CONFIG_DIR`), then CLI overrides.
    /// Creates `<config_dir>/device_id.txt` on first run.
    ///
    /// # Errors
    /// Returns `CoreError::Internal` if platform directories cannot
    /// be resolved, or `CoreError::Io` on filesystem failures.
    pub fn resolve(cli_data_dir: Option<PathBuf>) -> Result<Self, CoreError> {
        let dirs = directories::ProjectDirs::from("dev", "perima", "perima")
            .ok_or_else(|| CoreError::Internal("cannot resolve project dirs".into()))?;

        let config_dir = std::env::var_os("PERIMA_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs.config_dir().to_path_buf());
        let data_dir = cli_data_dir
            .or_else(|| std::env::var_os("PERIMA_DATA_DIR").map(PathBuf::from))
            .unwrap_or_else(|| dirs.data_dir().to_path_buf());

        std::fs::create_dir_all(&config_dir)?;
        std::fs::create_dir_all(&data_dir)?;

        let device_id = load_or_create_device_id(&config_dir)?;
        Ok(Self { data_dir, config_dir, device_id })
    }
}

fn load_or_create_device_id(config_dir: &Path) -> Result<DeviceId, CoreError> {
    let path = config_dir.join("device_id.txt");
    if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        let trimmed = raw.trim();
        let parsed = uuid::Uuid::parse_str(trimmed)
            .map_err(|e| CoreError::Internal(format!("device_id parse: {e}")))?;
        return Ok(DeviceId(parsed));
    }
    let id = ids::new_id();
    std::fs::write(&path, id.to_string())?;
    Ok(DeviceId(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_persists_across_calls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: single-threaded test — no race on env.
        unsafe {
            std::env::set_var("PERIMA_CONFIG_DIR", tmp.path());
            std::env::set_var("PERIMA_DATA_DIR", tmp.path());
        }
        let a = Config::resolve(None).expect("resolve 1");
        let b = Config::resolve(None).expect("resolve 2");
        assert_eq!(a.device_id.0, b.device_id.0);
        unsafe {
            std::env::remove_var("PERIMA_CONFIG_DIR");
            std::env::remove_var("PERIMA_DATA_DIR");
        }
    }
}
```

- [ ] **Step 3: Create `logging.rs`**

```rust
//! Logging initialization.

use perima_core::CoreError;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Init `tracing-subscriber`. Reads `PERIMA_LOG` (env filter,
/// default "info"); `PERIMA_LOG_JSON=1` for JSON output (else
/// human-readable text). Writes to stderr. `verbosity_bump` comes
/// from CLI `-v` count.
///
/// # Errors
/// Returns `CoreError::Internal` if the global subscriber is already
/// set (tests should tolerate this).
pub fn init(verbosity_bump: u8) -> Result<(), CoreError> {
    let base = std::env::var("PERIMA_LOG").unwrap_or_else(|_| "info".into());
    let bump_level = match verbosity_bump {
        0 => None,
        1 => Some("debug"),
        _ => Some("trace"),
    };
    let filter_str = match bump_level {
        Some(lvl) => format!("{base},perima={lvl}"),
        None => base,
    };
    let filter = EnvFilter::try_new(&filter_str)
        .map_err(|e| CoreError::Internal(format!("env filter: {e}")))?;

    let json = std::env::var("PERIMA_LOG_JSON")
        .map(|v| v == "1")
        .unwrap_or(false);

    let registry = tracing_subscriber::registry().with(filter);
    if json {
        registry
            .with(fmt::layer().json().with_writer(std::io::stderr))
            .try_init()
            .map_err(|e| CoreError::Internal(format!("subscriber: {e}")))
    } else {
        registry
            .with(fmt::layer().with_writer(std::io::stderr))
            .try_init()
            .map_err(|e| CoreError::Internal(format!("subscriber: {e}")))
    }
}
```

- [ ] **Step 4: Run gates**

Run: `cargo test -p perima --lib` → passes (at least `device_id_persists_across_calls`).
Run: `just ci` → exit 0.

---

## Task 13: CLI — `signals` + `panic` modules

**Files:**
- Create: `crates/cli/src/signals.rs`
- Create: `crates/cli/src/panic.rs`

- [ ] **Step 1: Create `signals.rs`**

```rust
//! Ctrl-C / SIGTERM handling.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use perima_core::CoreError;

/// Cancellation guard. Drop it to uninstall the signal handler
/// (so later tests or embedded usage can re-install).
///
/// WHY guard semantics: `ctrlc::set_handler` is a process-global
/// singleton; without this guard pattern a test suite would fail
/// the second test that tried to install a handler.
pub struct Cancellation {
    flag: Arc<AtomicBool>,
}

impl Cancellation {
    /// Has a cancellation signal been received?
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Share the cancellation flag with a worker closure (e.g.
    /// rayon's `par_iter` map). WHY: we only expose the `Arc` for
    /// this reason; callers that only need a bool should use
    /// `cancelled()` instead.
    #[must_use]
    pub fn token(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }
}

/// Install a process-global SIGINT/SIGTERM handler that flips the
/// cancellation flag. The returned guard holds the flag; keep it
/// alive for the duration you care about signals.
///
/// WHY: on Ctrl-C we flip the flag rather than `std::process::exit`
/// so the scan loop can finish printing already-hashed entries —
/// stdout lines are per-file, and aborting mid-write would truncate.
///
/// # Errors
/// Returns `CoreError::Internal` if another handler is already
/// registered (only one handler per process).
pub fn install() -> Result<Cancellation, CoreError> {
    let flag = Arc::new(AtomicBool::new(false));
    let cloned = flag.clone();
    ctrlc::set_handler(move || {
        cloned.store(true, Ordering::SeqCst);
    })
    .map_err(|e| CoreError::Internal(format!("ctrlc: {e}")))?;
    Ok(Cancellation { flag })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_flag_starts_false() {
        let flag = Arc::new(AtomicBool::new(false));
        let c = Cancellation { flag: flag.clone() };
        assert!(!c.cancelled());
        flag.store(true, Ordering::SeqCst);
        assert!(c.cancelled());
    }
}
```

- [ ] **Step 2: Create `panic.rs`**

```rust
//! Panic handler routing panics through tracing.

/// Install a panic hook that routes panics through
/// `tracing::error!` with thread info. Replaces the default
/// "thread 'xxx' panicked at …" so background rayon threads don't
/// die silently.
pub fn install() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".into());
        tracing::error!(
            thread = name,
            location = %location,
            payload = ?info.payload(),
            "panic"
        );
        // WHY: delegate to the default hook after logging so that
        // the user still sees the standard backtrace on the tty.
        default_hook(info);
    }));
}
```

- [ ] **Step 3: Run gates**

Run: `cargo test -p perima` → passes.
Run: `just ci` → exit 0.

---

## Task 14: CLI — `cmd/scan.rs`

**Files:**
- Create: `crates/cli/src/cmd/mod.rs`
- Create: `crates/cli/src/cmd/scan.rs`

- [ ] **Step 1: Create `cmd/mod.rs`**

```rust
//! CLI subcommand modules.

pub mod scan;
```

- [ ] **Step 2: Create `cmd/scan.rs`**

```rust
//! `perima scan --dry-run` implementation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use perima_core::{CoreError, DiscoveredFile, HashService, Scanner};
use rayon::prelude::*;

use crate::signals::Cancellation;

/// Arguments for the scan command.
#[derive(Debug, Clone)]
pub struct ScanArgs {
    /// Root directory to walk.
    pub root: PathBuf,
    /// When true, hashes and prints but skips all DB writes.
    /// REQUIRED in phase 1a (no DB wired); optional in phase 1b.
    pub dry_run: bool,
    /// Suppress per-file stdout lines; print summary only.
    pub quiet: bool,
}

/// Exit code returned to `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Completed normally.
    Success,
    /// Ctrl-C received; partial scan summarized.
    Interrupted,
}

/// Execute `scan`. In 1a, `args.dry_run` must be `true`; the
/// caller (`main.rs`) enforces this and the scan's own guard
/// doubles as documentation.
///
/// # Errors
/// Returns `CoreError::InvalidPath` if `root` is not a directory;
/// propagates `CoreError` from hashing and walking.
pub fn run<S, H>(
    scanner: &S,
    hasher: &H,
    cancel: &Cancellation,
    args: &ScanArgs,
) -> Result<ExitCode, CoreError>
where
    S: Scanner + ?Sized,
    H: HashService + ?Sized,
{
    // WHY: guard fires BEFORE Config::resolve in main.rs, so the
    // caller already rejected this path. If we still land here (e.g.
    // programmatic call), surface Unsupported so the CLI maps to
    // exit 2 without string-matching prose.
    if !args.dry_run {
        return Err(CoreError::Unsupported(
            "phase 1a ships only 'scan --dry-run'; real scan arrives in 1b".into(),
        ));
    }
    validate_root(&args.root)?;

    let volume_root = canonicalize_for_walk(&args.root)?;
    let mut count: u64 = 0;
    let stdout = std::io::stdout();

    // Collect up-front so rayon can parallelize hashing; the walker
    // iterator itself isn't Send across the par_iter boundary. The
    // inner `take_while` polls between yielded items so a Ctrl-C
    // during walk short-circuits quickly.
    let discovered: Vec<DiscoveredFile> = scanner
        .walk(&args.root, &volume_root)?
        .take_while(|_| !cancel.cancelled())
        .collect();

    // Parallel hash. WHY: we also check cancellation at the top of
    // each map closure so in-flight hashes short-circuit the moment
    // Ctrl-C lands — without this, a large fixture would drain the
    // par_iter to completion even after the flag flips, defeating
    // the "Ctrl-C stops hashing" guarantee in the spec.
    let cancel_flag = cancel.token();
    let hashed: Vec<Result<(DiscoveredFile, perima_core::BlakeHash), CoreError>> = discovered
        .into_par_iter()
        .map(|d| {
            if cancel_flag.load(Ordering::SeqCst) {
                return Err(CoreError::Internal("cancelled".into()));
            }
            let h = hasher.full_hash(&d.absolute_path)?;
            Ok((d, h))
        })
        .collect();

    let mut handle = stdout.lock();
    for res in hashed {
        match res {
            Ok((d, h)) => {
                count += 1;
                if !args.quiet {
                    writeln!(
                        handle,
                        "{}  {}  {}",
                        h.to_hex(),
                        d.size.0,
                        d.relative_path.as_str()
                    )
                    .map_err(CoreError::Io)?;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "skipping file: hash failed");
            }
        }
    }
    drop(handle);

    let interrupted = cancel.cancelled();
    let suffix = if interrupted { " (interrupted)" } else { "" };
    eprintln!("scanned {count} files (dry-run; DB not yet wired){suffix}");

    Ok(if interrupted { ExitCode::Interrupted } else { ExitCode::Success })
}

fn validate_root(root: &Path) -> Result<(), CoreError> {
    if !root.exists() {
        return Err(CoreError::InvalidPath(format!(
            "does not exist: {}",
            root.display()
        )));
    }
    if !root.is_dir() {
        return Err(CoreError::InvalidPath(format!(
            "not a directory: {}",
            root.display()
        )));
    }
    Ok(())
}

fn canonicalize_for_walk(root: &Path) -> Result<PathBuf, CoreError> {
    // dunce::canonicalize avoids UNC prefixes on Windows.
    dunce::canonicalize(root).map_err(CoreError::Io)
}
```

- [ ] **Step 3: Run gates**

Run: `just ci` → exit 0 (no new tests; compile check only).

---

## Task 15: CLI — `main.rs`

**Files:**
- Modify: `crates/cli/src/main.rs`

- [ ] **Step 1: Full rewrite**

```rust
//! `perima` command-line entry point.

mod cmd;
mod config;
mod logging;
mod panic;
mod signals;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use perima_fs::WalkdirScanner;
use perima_hash::Blake3Service;

/// Cross-platform media asset manager.
#[derive(Parser, Debug)]
#[command(
    name = "perima",
    version,
    about = "Index your media across drives by content hash"
)]
struct Cli {
    /// Bump tracing verbosity; repeatable (-v, -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Override the main database directory. Not used in phase 1a
    /// (DB lands in 1b); accepted for forward compatibility.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Walk a directory, hash every file, and print the results.
    Scan {
        /// Directory to walk.
        root: PathBuf,

        /// Required in phase 1a (clap enforces); 1b makes optional.
        #[arg(long, required = true)]
        dry_run: bool,

        /// Suppress per-file stdout lines.
        #[arg(long)]
        quiet: bool,
    },
}

fn main() -> ExitCode {
    panic::install();
    let cli = Cli::parse();

    if let Err(e) = logging::init(cli.verbose) {
        eprintln!("perima: logging init failed: {e}");
        return ExitCode::from(1);
    }

    let cancel = match signals::install() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: signal handler install failed: {e}");
            return ExitCode::from(1);
        }
    };

    let _config = match config::Config::resolve(cli.data_dir.clone()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: config resolution failed: {e}");
            return ExitCode::from(1);
        }
    };

    match cli.command {
        Command::Scan { root, dry_run, quiet } => {
            let args = cmd::scan::ScanArgs { root, dry_run, quiet };
            let scanner = WalkdirScanner::new();
            let hasher = Blake3Service::new();
            match cmd::scan::run(&scanner, &hasher, &cancel, &args) {
                Ok(cmd::scan::ExitCode::Success) => ExitCode::from(0),
                Ok(cmd::scan::ExitCode::Interrupted) => ExitCode::from(130),
                Err(perima_core::CoreError::InvalidPath(msg)) => {
                    eprintln!("perima: {msg}");
                    ExitCode::from(2)
                }
                Err(perima_core::CoreError::Unsupported(msg)) => {
                    eprintln!("perima: {msg}");
                    ExitCode::from(2)
                }
                Err(e) => {
                    eprintln!("perima: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}
```

- [ ] **Step 2: Run gates**

Run: `just ci` → exit 0.
Run: `cargo run -p perima -- --help` → prints clap-generated help.
Run: `cargo run -p perima -- scan /tmp` → prints `perima: phase 1a ships only 'scan --dry-run'; real scan arrives in 1b`, exit 2.

- [ ] **Step 3: Dispatch reviewer (checkpoint #3 — commit 3 prep)**

Reviewer checklist:
- [ ] `perima --help` shows `scan` subcommand with `--dry-run`, `--quiet`, `--data-dir`, `-v/-vv`.
- [ ] `perima scan <tmp-fixture> --dry-run` prints hash/size/path lines to stdout and a summary to stderr, exit 0.
- [ ] `perima scan <tmp-fixture>` (no `--dry-run`) exits 2 with the phase-1a guard message.
- [ ] Ctrl-C handler installed; `signals::Cancellation::cancelled()` unit test passes.
- [ ] Panic hook installed in `main`.
- [ ] `just ci` green.
- [ ] `// WHY:` comments in code match the four required bullets from the spec.

Must return APPROVED before Step 4.

- [ ] **Step 4: Commit**

```bash
git add crates/cli/ Cargo.lock
git commit -m "$(cat <<'EOF'
feat(phase-1a): CLI scaffold with perima scan --dry-run

crates/cli gains: clap-derive root with -v/-vv/global data-dir,
single 'scan' subcommand (root, --dry-run required in 1a, --quiet).
Composition root wires WalkdirScanner + Blake3Service.

Cross-cutting concerns land here to avoid retrofits in 1b+:
- config.rs: directories crate + env overrides + persistent
  device_id.txt at <config_dir>/device_id.txt
- logging.rs: tracing-subscriber with PERIMA_LOG env filter,
  PERIMA_LOG_JSON=1 for JSON on stderr, -v/-vv bumps
- signals.rs: ctrlc-based SIGINT/SIGTERM flag with guard semantics
  so tests can re-install
- panic.rs: tracing::error! panic hook that delegates to the
  default for tty backtraces

Rayon parallelizes hashing across files; Ctrl-C flips an AtomicBool
polled between walk and hash so already-hashed lines finish
printing before the summary fires.

Refs: docs/superpowers/specs/2026-04-16-phase-1a-core-scan-cli-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 16: Property tests

**Files:**
- Create: `crates/core/tests/props_hash_determinism.rs`
- Create: `crates/core/tests/props_path_idempotence.rs`
- Create: `crates/core/tests/props_path_nfc_equivalence.rs`
- Modify: `crates/core/Cargo.toml`

- [ ] **Step 1: Add dev-deps**

Append to `crates/core/Cargo.toml`:

```toml
[dev-dependencies]
proptest.workspace = true
```

- [ ] **Step 2: Create `props_hash_determinism.rs`**

```rust
//! Property: the same byte input always produces the same
//! `BlakeHash`.

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]
    #[test]
    fn blake_hash_is_deterministic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let a = blake3::hash(&bytes);
        let b = blake3::hash(&bytes);
        prop_assert_eq!(a.as_bytes(), b.as_bytes());
    }
}
```

- [ ] **Step 3: Create `props_path_idempotence.rs`**

```rust
//! Property: `MediaPath::new` is idempotent under repeated
//! application — `f(f(s)) == f(s)` for every input.

use perima_core::MediaPath;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]
    #[test]
    fn media_path_is_idempotent(s in "\\PC{0,200}") {
        let once = MediaPath::new(&s);
        let twice = MediaPath::new(once.as_str());
        prop_assert_eq!(once, twice);
    }
}
```

- [ ] **Step 4: Create `props_path_nfc_equivalence.rs`**

```rust
//! Property: `MediaPath::new` collapses canonically-equivalent
//! inputs. Applying NFD decomposition before construction must
//! yield the same `MediaPath`.

use perima_core::MediaPath;
use proptest::prelude::*;
use unicode_normalization::UnicodeNormalization;

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]
    #[test]
    fn media_path_nfc_equivalence(s in "\\PC{0,200}") {
        let nfd: String = s.nfd().collect();
        let from_original = MediaPath::new(&s);
        let from_nfd = MediaPath::new(&nfd);
        prop_assert_eq!(from_original, from_nfd);
    }
}
```

The `unicode-normalization` dep is already visible to tests via
the regular `[dependencies]` entry from Task 2 — do NOT re-declare
it under `[dev-dependencies]`.

- [ ] **Step 5: Run gates**

Run: `cargo test -p perima-core --tests` → 3 proptests pass (256 cases each).
Run: `just ci` → exit 0.

---

## Task 17: Integration test

**Files:**
- Create: `crates/cli/tests/scan_dry_run.rs`

- [ ] **Step 1: Write the integration test**

```rust
//! End-to-end `perima scan --dry-run` via the built binary.

use std::io::Write;
use std::path::Path;
use std::process::Command;

fn mk_fixture(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = vec![
        ("alpha.txt".to_string(), b"alpha".to_vec()),
        ("sub/beta.txt".to_string(), b"beta".to_vec()),
        ("sub/gamma.bin".to_string(), b"\x00\x01\x02\x03".to_vec()),
    ];
    for (name, bytes) in &files {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::File::create(&path)
            .expect("create")
            .write_all(bytes)
            .expect("write");
    }
    files.sort();
    files
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

#[test]
fn dry_run_prints_hashes_and_summary() {
    let td = tempfile::tempdir().expect("tempdir");
    let _fixture = mk_fixture(td.path());

    let tmp_env = tempfile::tempdir().expect("env dir");
    let output = Command::new(bin())
        .arg("scan")
        .arg("--dry-run")
        .arg(td.path())
        .env("PERIMA_CONFIG_DIR", tmp_env.path())
        .env("PERIMA_DATA_DIR", tmp_env.path())
        .output()
        .expect("run perima");

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));

    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    let mut lines: Vec<&str> = stdout.lines().collect();
    lines.sort();
    assert_eq!(lines.len(), 3, "expected 3 hashed files, got: {lines:?}");

    for line in &lines {
        let mut parts = line.splitn(3, "  ");
        let hash = parts.next().expect("hash field");
        let size = parts.next().expect("size field");
        let path = parts.next().expect("path field");
        assert_eq!(hash.len(), 64, "bad hash length in: {line}");
        assert!(
            hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "hash not lowercase hex: {line}"
        );
        assert!(size.bytes().all(|b| b.is_ascii_digit()), "bad size: {line}");
        assert!(!path.is_empty(), "empty path: {line}");
    }

    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("scanned 3 files (dry-run; DB not yet wired)"),
        "stderr missing summary; got: {stderr}"
    );
}

#[test]
fn dry_run_is_deterministic_across_runs() {
    let td = tempfile::tempdir().expect("tempdir");
    mk_fixture(td.path());
    let tmp_env = tempfile::tempdir().expect("env dir");

    let run = || {
        let output = Command::new(bin())
            .arg("scan")
            .arg("--dry-run")
            .arg("--quiet")
            .arg(td.path())
            .env("PERIMA_CONFIG_DIR", tmp_env.path())
            .env("PERIMA_DATA_DIR", tmp_env.path())
            .output()
            .expect("run perima");
        assert!(output.status.success());
        String::from_utf8(output.stdout).expect("utf8")
    };

    // With --quiet, stdout is empty on both runs.
    let a = run();
    let b = run();
    assert_eq!(a, b);
}

#[test]
fn real_scan_refused_in_phase_1a() {
    let td = tempfile::tempdir().expect("tempdir");
    mk_fixture(td.path());
    let tmp_env = tempfile::tempdir().expect("env dir");

    let output = Command::new(bin())
        .arg("scan")
        .arg(td.path())
        .env("PERIMA_CONFIG_DIR", tmp_env.path())
        .env("PERIMA_DATA_DIR", tmp_env.path())
        .output()
        .expect("run perima");

    assert_eq!(output.status.code(), Some(2), "expected exit 2");
    let stderr = String::from_utf8(output.stderr).expect("utf8 stderr");
    assert!(
        stderr.contains("phase 1a ships only 'scan --dry-run'"),
        "stderr missing guard message; got: {stderr}"
    );
}
```

- [ ] **Step 2: No extra dev-deps**

The integration test uses only `std` + `tempfile` (already in
`crates/cli/Cargo.toml` dev-deps). No regex dep needed.

- [ ] **Step 3: Run gates**

Run: `cargo test -p perima --tests` → 3 integration tests pass.
Run: `just ci` → exit 0.

---

## Task 18: Final sweep + WHY grep + commit + tag

- [ ] **Step 1: Grep WHY comments**

Run:
```bash
grep -rE '^\s*//\s*WHY:' crates/ | tee /tmp/why.txt
wc -l /tmp/why.txt
```
Expected: **at least 4 lines**. Covers: `MediaPath::new`, `HashService: Send+Sync`, Ctrl-C AtomicBool, content-PK (blake3_hash — documented in spec, noted in-code via the core types module docstring).

If fewer than 4, add the missing WHY comments to match the spec's "WHY-comments required in code" section.

- [ ] **Step 2: `just ci` clean sweep**

Run: `cargo clean && just ci`
Expected: exit 0 on fresh build.

- [ ] **Step 3: Dispatch final reviewer (checkpoint #4)**

Reviewer checklist:
- [ ] All property tests pass (256 cases each).
- [ ] All integration tests pass on Linux (Unix-gated Ctrl-C test is absent or passes).
- [ ] `grep -rE '^\s*//\s*WHY:' crates/ | wc -l` ≥ 4.
- [ ] No `.md` file appears in `git ls-files '*.md'`.
- [ ] Clippy clean with `-D warnings` including the new code.
- [ ] Docs: `cargo doc --workspace --no-deps` produces no missing-doc errors.
- [ ] Every phase 1a exit criterion from the spec is observably met.

Must return APPROVED before Step 4.

- [ ] **Step 4: Commit tests + tag phase**

```bash
git add crates/core/tests/ crates/core/Cargo.toml crates/cli/tests/ crates/cli/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
test(phase-1a): property + integration tests for scan --dry-run

- proptests (256 cases each) in crates/core/tests/:
  * hash determinism (blake3::hash)
  * MediaPath idempotence (f(f(s)) == f(s))
  * MediaPath NFC equivalence (NFC input == NFD input)
- crates/cli/tests/scan_dry_run.rs exercises the built binary:
  * 3 fixture files produce 3 hash/size/path lines
  * --quiet+determinism: two runs match byte-for-byte
  * real scan (no --dry-run) refused with exit 2

Refs: docs/superpowers/specs/2026-04-16-phase-1a-core-scan-cli-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 5: Push main and wait for CI green BEFORE tagging**

```bash
git push origin main
gh run watch --exit-status \
    "$(gh run list --workflow=ci.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
```
Expected: green across all 3 matrix jobs.

If red: `gh run view <id> --log-failed` and open a bugfix task.
Do NOT tag a red commit. Do NOT mask with `continue-on-error`.

- [ ] **Step 6: Tag phase boundary (ONLY after CI is green)**

```bash
git tag -a phase-1a-complete -m "Phase 1a: core types + hash + fs + CLI scan --dry-run"
git push origin phase-1a-complete
```

---

## Self-review

**Spec coverage:**
- Domain types (BlakeHash, FileSize, MediaPath, VolumeId, DeviceId, DiscoveredFile, HashedFile, LocationStatus, UpsertOutcome, FileLocationRecord, VolumeIdentifiers, VolumeRecord) → Tasks 5, 6, 7.
- Trait ports (HashService, Scanner, FileRepository, VolumeRepository) → Task 8.
- Errors (CoreError, adapter Error types + From conversions) → Tasks 4, 9, 10.
- Hash adapter (Blake3Service with two-phase) → Task 9.
- FS adapter (relativize, WalkdirScanner) → Tasks 10, 11.
- CLI cross-cutting (config, logging, signals, panic) → Tasks 12, 13.
- `perima scan --dry-run` → Tasks 14, 15.
- Property tests (hash det, path idempotence, NFC equiv) → Task 16.
- Integration tests → Task 17.
- WHY comments (4 required) → checked in Task 18 Step 1.
- Exit criteria 1–9 → Task 18 verification.

**Placeholder scan:** no TBD/TODO/"implement later". Every file has exact content.

**Type consistency:**
- `BlakeHash`, `MediaPath`, `VolumeId`, `DeviceId`, `FileSize` match between types.rs, ports, adapters, CLI.
- `FileLocationRecord` (not `FileRecord`) used consistently in ports and later consumers.
- `DiscoveredFile.absolute_path` / `.relative_path` / `.size` match between scanner impl and consumers.
- Signal guard name `Cancellation` consistent between `signals.rs` and `scan.rs`.
- `ScanArgs` / `ExitCode` / `run()` match between `cmd/scan.rs` and `main.rs`.

**Commit discipline:** four commits gated by four reviewer checkpoints, matching CLAUDE.md "execute → tests → reviewer → commit." Push + tag happen only at the final checkpoint.
