//! Windows-scoped path-canonicalization helpers.
//!
//! `dunce` works around two Windows bugs in `std::fs::canonicalize`:
//! (1) `\\?\` UNC-prefix pollution of user-displayable paths and
//! (2) rejection of valid legacy drive-letter paths. On non-Windows
//! `dunce` is documented to pass through to `std::fs::canonicalize` /
//! identity `simplified`, so we avoid pulling it as a direct dep
//! outside crates/fs — the `#[cfg(windows)]` branch below is the
//! only place dunce is reachable from.

use std::io;
use std::path::{Path, PathBuf};

/// Canonicalize a path, stripping `\\?\` on Windows.
///
/// On non-Windows this is `std::fs::canonicalize`.
///
/// # Errors
/// Returns [`std::io::Error`] if the path does not exist or cannot be
/// resolved by the OS.
#[inline]
pub fn canonicalize(p: &Path) -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        dunce::canonicalize(p)
    }
    #[cfg(not(windows))]
    {
        std::fs::canonicalize(p)
    }
}

/// Return `p` with `\\?\` prefix stripped on Windows; identity elsewhere.
// WHY allow: clippy suggests `const fn` based on the non-Windows identity
// branch, but the #[cfg(windows)] branch calls dunce::simplified which is
// not const. Silencing here is cleaner than splitting into two cfg-gated fns.
#[allow(clippy::missing_const_for_fn)]
#[inline]
#[must_use]
pub fn simplified(p: &Path) -> &Path {
    #[cfg(windows)]
    {
        dunce::simplified(p)
    }
    #[cfg(not(windows))]
    {
        p
    }
}
