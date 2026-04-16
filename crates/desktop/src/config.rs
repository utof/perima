//! Runtime configuration for the Tauri desktop backend.
//!
//! WHY: duplicated from `crates/cli/src/config.rs` rather than sharing via
//! a new crate. The CLI config has identical logic; extracting it to a shared
//! crate would require `directories` to land in `perima-core` (violating the
//! zero-framework-deps rule) or introducing a tiny `crates/config` crate
//! (premature abstraction for ~40 lines). Duplication is the correct call for
//! phase 2; revisit after phase 3 when the crate graph is more stable.

use std::path::{Path, PathBuf};

use perima_core::{CoreError, DeviceId, ids};

/// Resolved configuration for a desktop run.
///
/// WHY `struct_field_names` allow: `config_dir` vs `data_dir` naming is
/// intentional domain vocabulary; the lint would fire regardless of name.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Debug)]
pub struct Config {
    /// Where the main database (`perima.db`) lives.
    pub data_dir: PathBuf,
    /// Where `device_id.txt` lives.
    pub config_dir: PathBuf,
    /// Stable device identifier loaded from or written to `device_id.txt`.
    pub device_id: DeviceId,
}

/// Resolve config from platform dirs, then env overrides.
///
/// Creates `<config_dir>/device_id.txt` on first run. No CLI flag overrides
/// because the Tauri app has no flag parsing.
///
/// # Errors
/// Returns `CoreError::Internal` if platform dirs cannot be resolved, or
/// `CoreError::Io` on filesystem failures.
pub fn resolve_config() -> Result<Config, CoreError> {
    let dirs = directories::ProjectDirs::from("dev", "perima", "perima")
        .ok_or_else(|| CoreError::Internal("cannot resolve project dirs".into()))?;

    let config_dir = std::env::var_os("PERIMA_CONFIG_DIR")
        .map_or_else(|| dirs.config_dir().to_path_buf(), PathBuf::from);
    let data_dir = std::env::var_os("PERIMA_DATA_DIR")
        .map_or_else(|| dirs.data_dir().to_path_buf(), PathBuf::from);

    std::fs::create_dir_all(&config_dir)?;
    std::fs::create_dir_all(&data_dir)?;

    let device_id = load_or_create_device_id(&config_dir)?;
    Ok(Config {
        data_dir,
        config_dir,
        device_id,
    })
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

/// Resolve using explicit directories (used in tests to avoid unsafe env mutation).
///
/// WHY: `std::env::set_var` is unsafe in Rust ≥ 1.86; the workspace
/// denies `unsafe_code`, so tests use this helper instead.
#[cfg(test)]
pub(crate) fn resolve_with_dirs(
    config_dir: PathBuf,
    data_dir: PathBuf,
) -> Result<Config, CoreError> {
    std::fs::create_dir_all(&config_dir)?;
    std::fs::create_dir_all(&data_dir)?;
    let device_id = load_or_create_device_id(&config_dir)?;
    Ok(Config {
        data_dir,
        config_dir,
        device_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_persists_across_calls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = resolve_with_dirs(tmp.path().to_path_buf(), tmp.path().to_path_buf())
            .expect("resolve 1");
        let b = resolve_with_dirs(tmp.path().to_path_buf(), tmp.path().to_path_buf())
            .expect("resolve 2");
        assert_eq!(a.device_id.0, b.device_id.0);
    }
}
