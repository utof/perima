# Phase 1a — Core types, ports, scan-without-DB

**Status:** draft awaiting reviewer pass #2
**Date:** 2026-04-16
**Parent:** meta-plan `2026-04-15-meta-plan-design.md`, phase 1 entry.
**Prior phase:** `phase-0-complete` tag.
**Siblings:** 1b (DB + `perima ls`), 1c (volumes + manifest +
`perima volumes`). Written just-in-time after each predecessor lands.

---

## Goal

Land the hexagonal core (domain types + trait ports) and the two
purest adapters (hash, fs walker), plus enough of the CLI to exercise
them. `perima scan --dry-run <path>` walks the tree, BLAKE3-hashes
each file, and prints `<hex-hash>  <size>  <relative-path>` to stdout
plus a summary to stderr. **No database writes in 1a.** 1b wires the
real repository; 1c wires volume identification.

All cross-cutting concerns (config, logging, errors, Ctrl-C handler,
panic hook) land in 1a because retrofitting them later would touch
every command.

## Non-goals for 1a

- rusqlite, refinery, schema, migrations (1b).
- Volume detection, `volume_mounts`, `.perima/manifest.db` (1c).
- `perima ls`, `perima volumes` subcommands (1b, 1c).
- File watching, `EventBus`, `Asset<State>` (phase 3; deferred).
- Thumbnails, tags, FTS5, Tauri (later phases).

---

## Architecture

```
crates/core
├── ids.rs              // UUIDv7 helpers (new_id, parse)
├── errors.rs           // CoreError (zero adapter deps)
├── types.rs            // BlakeHash, FileSize, MediaPath,
│                       // VolumeId, DeviceId, DiscoveredFile,
│                       // HashedFile
├── ports/
│   ├── hash.rs         // HashService trait
│   ├── scanner.rs      // Scanner trait
│   ├── file_repo.rs    // FileRepository trait (SHAPE ONLY in 1a)
│   └── volume_repo.rs  // VolumeRepository trait (SHAPE ONLY in 1a)
└── lib.rs              // re-exports

crates/hash
├── errors.rs           // hash::Error
└── blake3_service.rs   // Blake3Service impls HashService (rayon)

crates/fs
├── errors.rs           // fs::Error
├── paths.rs            // MediaPath normalization (NFC + /)
└── walker.rs           // WalkdirScanner impls Scanner

crates/cli
├── config.rs           // data_dir / config_dir / device_id
├── logging.rs          // tracing-subscriber init
├── signals.rs          // Ctrl-C handler (AtomicBool flag)
├── panic.rs            // std::panic::set_hook → tracing::error
├── cmd/
│   └── scan.rs         // `perima scan` (dry-run only in 1a)
└── main.rs             // clap root + dispatch
```

Trait ports for `FileRepository` and `VolumeRepository` are defined
in 1a (signatures only) so the scan command can depend on abstract
interfaces. 1a's CLI does not instantiate them — `scan --dry-run`
short-circuits before the repository boundary.

### Concurrency

Sync throughout. `rayon` for parallel file hashing inside the scan
loop. No tokio, no async. `HashService: Send + Sync` permits a future
async adapter without breaking the trait.

### Data flow (`perima scan --dry-run <path>`)

1. `main.rs` installs panic hook + Ctrl-C handler; initializes
   logging; parses CLI; resolves config.
2. `scan::run(path, dry_run=true)` validates the path (exists + is a
   directory; else exit 2).
3. `WalkdirScanner` produces an iterator of `DiscoveredFile`.
4. For each, `rayon::par_iter` with a buffered channel drains
   `DiscoveredFile` → `Blake3Service::full_hash` → `HashedFile`.
5. Each `HashedFile` is printed to stdout as
   `<hex-hash>  <size-bytes>  <relative-path>`.
6. On completion (or on Ctrl-C drain), print to stderr:
   `scanned <N> files (dry-run; DB not yet wired)`.
7. Exit 0 on success; exit 130 if interrupted (Ctrl-C); exit 1 on
   unrecoverable error.

Ctrl-C: the handler flips an `AtomicBool`. The scan loop polls it
between batches; when set, the iterator stops early, the drain
completes printing already-hashed entries, and the summary reflects
the partial count.

---

## Domain types (`crates/core/types.rs`)

All pub items have doc comments (workspace `missing_docs = "deny"`).
WHY-comments call out non-obvious design choices per CLAUDE.md.

```rust
/// BLAKE3-256 content hash (32 bytes). Stored as lowercase hex at
/// the persistence boundary.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct BlakeHash([u8; 32]);
impl BlakeHash {
    pub fn from_bytes(b: [u8; 32]) -> Self;
    pub fn to_hex(&self) -> String;           // 64-char lowercase
    pub fn parse_hex(s: &str) -> Result<Self, CoreError>;
    pub fn as_bytes(&self) -> &[u8; 32];
}

/// File size in bytes. Newtype to prevent arithmetic with other u64s.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FileSize(pub u64);

/// Path relative to a volume root. NFC-normalized, forward-slash,
/// no leading slash. The constructor is *idempotent* AND makes
/// canonically-equivalent inputs compare equal (NFC = NFD).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct MediaPath(String);
impl MediaPath {
    pub fn new(raw: &str) -> Self;
    pub fn as_str(&self) -> &str;
}

/// UUIDv7 volume identifier (phase 1c populates; 1a only declares).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct VolumeId(pub uuid::Uuid);
impl VolumeId { pub fn new() -> Self; }       // uuid::Uuid::now_v7()

/// UUIDv7 device identifier. Read from or created at
/// `<config_dir>/perima/device_id.txt` on first run.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct DeviceId(pub uuid::Uuid);

/// Output of the scanner; pre-hash.
pub struct DiscoveredFile {
    pub absolute_path: std::path::PathBuf,
    pub relative_path: MediaPath,
    pub size: FileSize,
}

/// Post-hash pipeline record.
pub struct HashedFile {
    pub discovered: DiscoveredFile,
    pub hash: BlakeHash,
}
```

## Trait ports

```rust
// crates/core/ports/hash.rs
pub trait HashService: Send + Sync {
    /// Hash only the first 64 KiB. Cheap change-detection. Phase 3's
    /// watcher uses this; 1a's scan always calls `full_hash`.
    fn quick_hash(&self, path: &std::path::Path) -> Result<BlakeHash, CoreError>;
    /// Hash the entire file.
    fn full_hash(&self, path: &std::path::Path) -> Result<BlakeHash, CoreError>;
}

// crates/core/ports/scanner.rs
pub trait Scanner: Send + Sync {
    /// Walk `root` recursively. Per-entry I/O errors are logged via
    /// tracing and skipped; the iterator continues. Only terminal
    /// failures (permission-denied on the root, etc.) return Err.
    fn walk(
        &self,
        root: &std::path::Path,
        volume_root: &std::path::Path,
    ) -> Result<Box<dyn Iterator<Item = DiscoveredFile> + '_>, CoreError>;
}

// crates/core/ports/file_repo.rs (SHAPE ONLY — 1a does not
// instantiate; 1b wires the real impl.)
pub trait FileRepository: Send + Sync {
    fn upsert_file(
        &mut self,
        file: &HashedFile,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError>;
    fn upsert_location(
        &mut self,
        hash: &BlakeHash,
        volume: VolumeId,
        path: &MediaPath,
        device: DeviceId,
    ) -> Result<UpsertOutcome, CoreError>;
    /// Phase 1b uses this for `perima ls`. Returns one row per
    /// (hash, volume, relative_path) with the `status` attached.
    fn list_file_locations(
        &self,
        limit: usize,
        volume: Option<VolumeId>,
    ) -> Result<Vec<FileLocationRecord>, CoreError>;
}

pub enum UpsertOutcome { Inserted, Updated, Unchanged }

/// A joined view of `files` + `file_locations`. The `status` field
/// belongs to the *location*, not the file — a single file's hash
/// may live on multiple volumes with independent statuses.
pub struct FileLocationRecord {
    pub hash: BlakeHash,
    pub size: FileSize,
    pub volume_id: VolumeId,
    pub relative_path: MediaPath,
    pub status: LocationStatus,
    pub first_seen: String,   // ISO 8601 UTC
}

pub enum LocationStatus { Active, Missing, Moved }

// crates/core/ports/volume_repo.rs (SHAPE ONLY)
pub trait VolumeRepository: Send + Sync {
    fn find_or_create(
        &mut self,
        ident: &VolumeIdentifiers,
        device: DeviceId,
    ) -> Result<VolumeId, CoreError>;
    fn record_mount(
        &mut self,
        volume: VolumeId,
        machine: DeviceId,
        mount: &std::path::Path,
    ) -> Result<(), CoreError>;
    fn list(&self) -> Result<Vec<VolumeRecord>, CoreError>;
}

pub struct VolumeIdentifiers {
    pub gpt_partition_guid: Option<String>,
    pub fs_uuid: Option<String>,
    pub label: Option<String>,
    pub capacity_bytes: u64,
    pub is_removable: bool,
}

pub struct VolumeRecord {
    pub id: VolumeId,
    pub label: Option<String>,
    pub capacity_bytes: u64,
    pub is_removable: bool,
    pub mounts_on_this_machine: Vec<std::path::PathBuf>,
    pub last_seen: String,    // ISO 8601 UTC
}
```

---

## Error taxonomy

`core` stays framework-free. Adapters each define their own `Error`
and a `From<Error> for CoreError` conversion **inside the adapter
crate** (adapter depends on core; never the other way).

```rust
// crates/core/errors.rs
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("not found: {0}")] NotFound(String),
    #[error("duplicate: {0}")] Duplicate(String),
    #[error("invalid path: {0}")] InvalidPath(String),
    #[error("invalid hash hex: {0}")] InvalidHash(String),
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("internal: {0}")] Internal(String),
}

// crates/hash/errors.rs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")] Io(#[from] std::io::Error),
}
impl From<Error> for perima_core::CoreError { /* … */ }

// crates/fs/errors.rs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("path not under volume root: {0}")]
    NotUnderVolume(std::path::PathBuf),
}
impl From<Error> for perima_core::CoreError { /* … */ }
```

Phase 1b will add richer DB-error mapping
(`rusqlite::Error::QueryReturnedNoRows` → `CoreError::NotFound`,
constraint failures → `CoreError::Duplicate`). Called out here so
1b's spec picks it up.

---

## CLI in 1a — `perima scan --dry-run <path>`

Only one subcommand in 1a, and only its `--dry-run` form.

Options:
- `<path>` (positional, required) — directory to walk.
- `--dry-run` (required in 1a; 1b makes it optional, default off).
- `--data-dir <path>` — accepted but unused in 1a; reserved for 1b.
- `-v` / `-vv` — tracing level bumps.
- `--quiet` — suppress per-file stdout lines, print summary only.

Behavior: walk + hash + print per-file to stdout; summary to stderr.
No DB access at all. The `--dry-run` flag is the autonomously-
verifiable proxy for "no repository used": if `--dry-run` is absent
in 1a, the binary exits 2 with
`phase 1a ships only 'scan --dry-run'; real scan arrives in 1b`.
1b removes that guard and makes `--dry-run` optional.

---

## Cross-cutting concerns

### Config (`crates/cli/config.rs`)

```rust
pub struct Config {
    pub data_dir: std::path::PathBuf,
    pub config_dir: std::path::PathBuf,
    pub device_id: perima_core::DeviceId,
}
impl Config {
    /// Resolve from the `directories` crate, then env overrides
    /// (`PERIMA_DATA_DIR`, `PERIMA_CONFIG_DIR`), then CLI flag
    /// overrides. Creates `<config_dir>/device_id.txt` on first run.
    pub fn resolve(
        cli_data_dir: Option<std::path::PathBuf>,
    ) -> Result<Self, perima_core::CoreError>;
}
```

### Logging (`crates/cli/logging.rs`)

```rust
/// Init `tracing-subscriber`. Reads `PERIMA_LOG` (env filter,
/// default "info"); `PERIMA_LOG_JSON=1` for JSON output (else
/// human-readable text). Writes to stderr. `verbosity_bump` comes
/// from CLI `-v` count and is additive on the `perima` target.
pub fn init(verbosity_bump: u8) -> Result<(), perima_core::CoreError>;
```

### Ctrl-C handler (`crates/cli/signals.rs`)

```rust
/// Install a SIGINT/SIGTERM handler that flips a global AtomicBool.
/// The scan loop polls `cancelled()` between batches and exits
/// gracefully, letting already-hashed entries finish printing.
/// Returns a `Cancellation` guard that owns the registered handler;
/// dropping it removes the handler (important for tests).
pub fn install() -> Result<Cancellation, perima_core::CoreError>;

pub struct Cancellation;
impl Cancellation {
    pub fn cancelled(&self) -> bool;
}
```

Implementation via the `ctrlc` crate (widely used, small). Add to
`[workspace.dependencies]`.

### Panic hook (`crates/cli/panic.rs`)

```rust
/// Install a panic hook that routes panics through
/// `tracing::error!` with backtrace + thread info. Replaces the
/// default "thread 'xxx' panicked at …" so background rayon
/// threads don't die silently.
pub fn install();
```

---

## WHY-comments required in code

Per CLAUDE.md, non-obvious choices get `// WHY:` comments. 1a's code
must include WHY-comments for at least:

- `MediaPath::new` combining NFC + forward-slash + leading-slash
  strip — WHY: the combination is what makes the constructor
  idempotent AND makes NFC-vs-NFD inputs compare equal.
- `HashService: Send + Sync` despite sync-only phase — WHY: leaves
  room for a future async adapter without breaking the trait.
- Ctrl-C handler flipping an AtomicBool rather than aborting —
  WHY: lets already-hashed entries finish printing so stdout isn't
  truncated mid-line.
- `files.blake3_hash` as PK (deferred to 1b's migration) — WHY: a
  content hash is deterministic and content-derived; two devices
  hashing identical bytes MUST produce the same row under any CRDT
  merge strategy, so a content-address PK is effectively a
  deterministic UUID and satisfies the "no accidental divergence
  under merge" invariant that the UUIDv7 rule exists to enforce.
  1a doesn't land the migration but this WHY is documented here so
  1b reproduces it verbatim at the migration site.

---

## Test strategy (1a)

All tests run under `cargo test --workspace --all-targets` as part
of `just ci` — no new harness needed.

### Unit tests

- `crates/core/src/types.rs`:
  - `BlakeHash::parse_hex` round-trips with `to_hex`.
  - `BlakeHash::parse_hex` rejects wrong length and non-hex chars.
  - `FileSize` derive traits compile (trivial).
- `crates/fs/src/paths.rs`:
  - `MediaPath::new` forward-slash conversion on Windows-style input.
  - `MediaPath::new` strips leading slash.
  - `MediaPath::new("café")` equals `MediaPath::new("cafe\u{0301}")`
    (fixed-case NFC equivalence — paired with the proptest below).

### Property tests (in `crates/core/tests/props_*.rs`)

- **Hash determinism**: `proptest!` with 256 cases — same `Vec<u8>`
  input produces the same `BlakeHash` across calls.
- **Path idempotence**: `proptest!` with 256 cases —
  `MediaPath::new` applied twice equals applied once.
- **Path NFC equivalence**: `proptest!` with 256 cases — for any
  string `s`, `MediaPath::new(s) == MediaPath::new(to_nfd(s))`.
  *This is the property that actually prevents de-dup misses when
  macOS (NFD) and Linux (NFC) paths reference the same asset.*

### Integration tests (`crates/cli/tests/scan_dry_run.rs`)

- Create a `tempfile::tempdir()` with three fixture files of known
  contents.
- Invoke `perima scan --dry-run <tmpdir>` via `std::process::Command`
  pointing at the built binary (`env!("CARGO_BIN_EXE_perima")`).
- Assert:
  - Exit code 0.
  - Stdout, **sorted line-wise**, contains three lines each matching
    `^[0-9a-f]{64}  \d+  .+$`. (Sort first because rayon parallel
    hashing means output order is non-deterministic.)
  - Stderr ends with `scanned 3 files (dry-run; DB not yet wired)`.
  - Running twice produces identical *sorted* stdout (determinism
    through the CLI, not just the library).
  - `perima scan <tmpdir>` (no `--dry-run`) exits 2 with stderr
    containing `phase 1a ships only 'scan --dry-run'`.

- Snapshot the summary line via `insta` (hash values are
  content-derived and stable, so no redaction needed for hashes;
  the temp-dir path in relative-path components IS redacted via
  an `insta::with_settings!` filter).

---

## Dependencies to add

Append to workspace `[workspace.dependencies]`:

```toml
clap     = { version = "4", features = ["derive"] }
tempfile = "3"
insta    = { version = "1", features = ["yaml", "filters"] }
proptest = "1"
rayon    = "1"
ctrlc    = { version = "3", features = ["termination"] }
```

Existing workspace deps (uuid, tracing, tracing-subscriber, blake3,
walkdir, unicode-normalization, path-slash, dunce, thiserror,
anyhow, serde, serde_json, directories) cover everything else.

---

## Exit criteria (phase 1a, autonomously verifiable)

1. `cargo build --workspace` — exit 0.
2. `cargo clippy --workspace --all-targets -- -D warnings` — exit 0.
3. `cargo test --workspace` — exit 0; new property tests pass with
   default 256 cases; integration test passes.
4. `cargo doc --workspace --no-deps` — exit 0 (every new public item
   has a doc comment).
5. `perima scan --dry-run <fixture>` stdout has one
   `^[0-9a-f]{64}  \d+  .+$` line per fixture file and stderr ends
   with the expected summary; `perima scan <fixture>` (without
   `--dry-run`) exits 2 with the phase-1a guard message.
6. Ctrl-C handling: unit test of the `Cancellation` flag passes on
   all platforms. Signal-dispatch integration test gated behind
   `#[cfg(unix)]` passes on Linux/macOS CI; skipped on Windows.
7. `just ci` green.
8. `grep -rE '^\s*//\s*WHY:' crates/` finds **at least 4** WHY
   comments (covering the four bullets in "WHY-comments required in
   code"; autonomously-verifiable floor).
9. Pre-commit hook + GitHub Actions remain green after push.

---

## Open decisions pushed to 1b

- `rusqlite` journal mode: **WAL** + `synchronous = NORMAL` set via
  `PRAGMA` at connection open.
- `rusqlite` distribution: **bundled + glibc desktop targets only**
  for v1; musl / cross-compile deferred to post-v1 packaging.
- Integration-test DB assertions: **use `rusqlite` from the test**
  (not `sqlite3` CLI — removes one install dependency).
- Rich rusqlite-error mapping: `QueryReturnedNoRows` →
  `CoreError::NotFound`, constraint failures → `CoreError::Duplicate`,
  everything else → `CoreError::Internal`.

## Open decisions pushed to 1c

- Volume identifier priority chain concrete algorithm (GPT GUID → fs
  UUID → volume label) AND **conflict** resolution (GUID wins when
  different identifiers each match a different known volume).
- Per-drive manifest schema (subset of main DB: `manifest_meta` +
  `manifest_files`).

---

## Risks (1a-specific)

- **Rayon + per-file stdout ordering.** Parallel hashing means
  output order won't match walk order. Mitigation: accept the
  non-determinism; integration test sorts lines before comparing.
- **NFC proptest generating exotic Unicode.** `proptest::string::*`
  ranges can produce combining characters that blow assumptions.
  Mitigation: curated BMP-restricted strategy plus one fixed case
  per known equivalence pair (the deterministic cases catch
  regressions; random cases catch surprises).
- **Ctrl-C on Windows.** `ctrlc` is cross-platform but signal
  semantics differ. Mitigation: dispatch test `#[cfg(unix)]`;
  Windows coverage limited to the `Cancellation`-flag unit test.
- **Clippy pedantic noise.** Pedantic lints may fire on idiomatic
  patterns (`must_use_candidate`). Mitigation: document any lint
  additions to the allow-list in `DECISIONS.md` as encountered;
  do NOT pre-emptively allow.
