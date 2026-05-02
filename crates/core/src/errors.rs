//! Top-level error type crossing the core boundary.
//!
//! Adapters define their own internal errors and implement
//! `From<AdapterError> for CoreError` **inside the adapter crate**
//! so that `core` depends on no adapter (preserves hexagonal
//! direction).

/// Stable, serializable error type that crosses the Tauri IPC boundary.
///
/// WHY discriminated union (`#[serde(tag = "kind", content = "data")]`):
/// the TypeScript binding compiles to `{ kind: "NotFound"; data: string } | ...`
/// which the frontend pattern-matches via `switch (err.kind)`. Replaces the
/// pre-Batch-D `Result<T, String>` + regex-on-prose discrimination
/// (audit §3.11 + §4.3).
///
/// WHY no `miette::Diagnostic` derive: `miette` is a binary-UX-only
/// concern (L7 landed it in `crates/cli` + `crates/desktop` only). Adding
/// it to `crates/core` violates the "no framework deps in core" rule.
/// Binaries can wrap `CoreError` in `miette::Report` at their own edge.
/// (Umbrella spec §1.4 #2.)
#[derive(Debug, Clone, thiserror::Error, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", content = "data")]
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

    /// Tag name failed normalization (empty, whitespace-only, or too long).
    #[error("invalid tag: {0}")]
    InvalidTag(String),

    /// Underlying I/O failure, lowered from `std::io::Error`.
    ///
    /// WHY struct variant: `std::io::Error` is `!Serialize` and `!Clone`.
    /// Capture `kind()` (e.g. `"NotFound"`, `"PermissionDenied"`) + display
    /// message at the conversion site so the frontend can branch on
    /// recoverable vs not.
    #[error("io [{kind}]: {message}")]
    Io {
        /// The `std::io::ErrorKind` debug name (e.g. `"PermissionDenied"`).
        kind: String,
        /// The original display message from the `std::io::Error`.
        message: String,
    },

    /// Feature is declared but not yet implemented at this phase.
    /// Dedicated variant so `main.rs` can map to a stable exit code
    /// without substring-matching prose.
    #[error("unsupported in this phase: {0}")]
    Unsupported(String),

    /// Transcription failure (cloud or local STT backend).
    #[error("transcription failed: {0}")]
    Transcription(#[from] crate::transcription::TranscriptionError),

    /// Any adapter-level failure that didn't map to a typed variant.
    #[error("internal: {0}")]
    Internal(String),

    /// A `full_hash` was requested but cannot be produced.
    ///
    /// WHY separate variant (not `Internal`): the frontend branches on the
    /// inner `reason` to show distinct user-facing messages — e.g., mount the
    /// volume vs wait for the backfill worker. `Internal` would prevent that.
    #[error("full hash unavailable: {reason:?}")]
    FullHashUnavailable {
        /// The specific reason the full hash is not available.
        reason: FullHashUnavailableReason,
    },

    /// Database backup failed. The `reason` enum carries a typed cause;
    /// frontend pattern-matches on `reason.kind` for targeted UX.
    ///
    /// WHY a dedicated variant (not reusing `Io`): backup failures need
    /// targeted UX. `TargetExists` says "pass --force"; `Io` would say
    /// "something went wrong" — much worse.
    ///
    /// WHY `{reason}` not `{reason:?}` in `#[error]`: `BackupFailureReason`
    /// itself derives `thiserror::Error`, so its per-variant `#[error]`
    /// produces the human-friendly message via Display. Debug printing
    /// (`{reason:?}`) would emit `TargetExists { path: "..." }` which is
    /// uglier and not user-facing.
    #[error("backup failed: {reason}")]
    BackupFailed {
        /// The specific reason the backup failed.
        reason: BackupFailureReason,
    },
}

/// Why a `full_hash` could not be produced.
///
/// WHY `#[serde(tag = "kind")]` (internal tagging, no content key): struct
/// variants already carry named fields inline; internal tagging produces
/// `{ "kind": "NotMounted", "volume_id": "…" }` — a TypeScript discriminated
/// union the frontend switches on cleanly.
///
/// WHY `thiserror::Error`: each variant needs a `Display` impl so
/// `CoreError::FullHashUnavailable` can format its `reason` field via `{reason:?}`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind")]
pub enum FullHashUnavailableReason {
    /// The volume that holds this file is not currently mounted.
    #[error("volume not mounted: {volume_id}")]
    NotMounted {
        /// UUID string of the unmounted volume.
        volume_id: String,
    },
    /// The full hash has not been computed yet (backfill not yet run).
    #[error("full hash has not been computed for this file yet")]
    NotComputed,
    /// An I/O error occurred while computing the full hash.
    #[error("io error: {message}")]
    IoError {
        /// The original I/O error message.
        message: String,
    },
}

// WHY explicit From, not #[from]: Io is now a struct variant capturing
// kind+message. The pre-Batch-D `#[from] std::io::Error` pattern requires
// the variant to wrap io::Error directly — which conflicts with both
// the Serialize derive (io::Error is !Serialize) and the Clone derive
// (io::Error is !Clone). DO NOT switch back to #[from].
impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io {
            kind: format!("{:?}", e.kind()),
            message: e.to_string(),
        }
    }
}

/// Typed reasons a `BackupDatabaseUseCase::execute` call can fail.
///
/// WHY `path: String` (not `PathBuf`): specta serialises both as TS
/// `string`, but `String` avoids the `PathBuf -> Display -> serialize`
/// step where a non-UTF8 path would become `"<path with bad utf8>"`.
/// Backups are user-facing artifacts; we only care about display, and
/// `String` is the cleaner type at the IPC seam.
#[derive(thiserror::Error, Debug, Clone, serde::Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "kind", content = "data")]
pub enum BackupFailureReason {
    /// `--to <path>` was supplied AND the target already exists, AND
    /// `--force` was not passed.
    #[error("target already exists: {path}; pass --force to overwrite")]
    TargetExists {
        /// The path that already exists.
        path: String,
    },

    /// Filesystem refused the write — permissions, read-only mount,
    /// missing parent dir we couldn't create, broken symlink whose
    /// target is gone, etc.
    #[error("target unwritable: {path}: {message}")]
    TargetUnwritable {
        /// The path that could not be written.
        path: String,
        /// The underlying error message.
        message: String,
    },

    /// Disk full / out of space.
    #[error("disk full while writing {path}")]
    DiskFull {
        /// The path being written when the disk filled.
        path: String,
    },

    /// A backup is already running on this `BackupDatabaseUseCase` instance;
    /// concurrent attempts are refused. Try again when the in-flight backup
    /// completes.
    #[error("a backup is already in progress")]
    AlreadyInProgress,

    /// Wrapped `SQLite` or unexpected-state error message that doesn't fit
    /// any of the typed buckets above.
    ///
    /// WHY a catch-all: `SQLite` returns dozens of distinct error codes;
    /// only the four above are user-actionable in slice 1. Future
    /// telemetry can mine `Internal(...)` messages to add typed reasons.
    #[error("internal backup error: {0}")]
    Internal(String),
}
