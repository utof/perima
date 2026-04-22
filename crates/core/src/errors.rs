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

    /// Any adapter-level failure that didn't map to a typed variant.
    #[error("internal: {0}")]
    Internal(String),
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
