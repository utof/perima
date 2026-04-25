//! Cross-shell data-directory resolution.
//!
//! Both `perima` (CLI) and `perima-desktop` call [`resolve_data_dir`] so a
//! tag added via one shell is visible in the other. Closes GH #154.

use std::path::PathBuf;

use directories::BaseDirs;
use perima_core::CoreError;

/// The Tauri bundle identifier — single source of truth for the data-dir
/// segment that both shells share.
pub const BUNDLE_ID: &str = "dev.perima.desktop";

/// Resolve perima's data dir for the current OS, matching Tauri's
/// bundle-id-based `app.path().app_data_dir()` resolution exactly so
/// CLI + desktop write to the same database.
///
/// - Linux:   `~/.local/share/dev.perima.desktop/perima/`
/// - macOS:   `~/Library/Application Support/dev.perima.desktop/perima/`
/// - Windows: `%APPDATA%\dev.perima.desktop\perima\`
///
/// # Errors
///
/// Returns [`CoreError::Internal`] if `directories::BaseDirs::new()` cannot
/// resolve the per-OS data root (very rare — would mean a broken HOME).
pub fn resolve_data_dir() -> Result<PathBuf, CoreError> {
    let base =
        BaseDirs::new().ok_or_else(|| CoreError::Internal("could not resolve base dirs".into()))?;
    Ok(base.data_dir().join(BUNDLE_ID).join("perima"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_data_dir_contains_bundle_id() {
        let path = resolve_data_dir().expect("resolve");
        let s = path.to_string_lossy();
        assert!(
            s.contains(BUNDLE_ID),
            "path {s} missing BUNDLE_ID {BUNDLE_ID}"
        );
    }

    #[test]
    fn resolve_data_dir_ends_with_perima() {
        let path = resolve_data_dir().expect("resolve");
        let last = path.file_name().unwrap_or_default().to_string_lossy();
        assert_eq!(last, "perima", "path should end in /perima");
    }
}
