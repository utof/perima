//! Runtime configuration: data dir, config dir, device id.

use std::path::{Path, PathBuf};

use perima_core::{CoreError, DeviceId, ids};

/// Resolved configuration for a CLI invocation.
// WHY allow struct_field_names: `config_dir` intentionally mirrors the
// OS concept "config directory for this tool"; changing it to `dir`
// alone would lose the domain clarity that `data_dir` vs `config_dir`
// provides. Phase 1b reads all three fields; the lint would fire
// regardless of the field name choice here.
#[allow(clippy::struct_field_names)]
#[derive(Clone, Debug)]
pub(crate) struct Config {
    /// Where the main database will live (1b uses this).
    // WHY allow dead_code: `data_dir`, `config_dir`, and `device_id`
    // are forwarded to the DB layer in phase 1b; declaring them here
    // now avoids a struct layout change. The binary target does not yet
    // read these fields, but tests and phase-1b will.
    #[allow(dead_code)]
    pub data_dir: PathBuf,
    /// Where `device_id.txt` and future user config live.
    #[allow(dead_code)]
    pub config_dir: PathBuf,
    /// Stable device identifier.
    #[allow(dead_code)]
    pub device_id: DeviceId,
}

impl Config {
    /// Resolve from the shared [`perima_app::config::resolve_data_dir`] (which
    /// matches Tauri's bundle-id path), then env overrides (`PERIMA_DATA_DIR`,
    /// `PERIMA_CONFIG_DIR`), then CLI `--data-dir` override.
    /// Creates `<config_dir>/device_id.txt` on first run.
    ///
    /// WHY use `resolve_data_dir` from `perima-app`: both CLI and desktop must
    /// read/write the same `SQLite` database. GH #154 — before this change, CLI
    /// used `directories::ProjectDirs::from("dev","perima","perima")` →
    /// `~/.local/share/perima/`, while desktop used the Tauri bundle-id path
    /// `~/.local/share/dev.perima.desktop/perima/`. Tags added via one shell
    /// were invisible to the other. The shared resolver locks both shells to
    /// the Tauri bundle-id path so they converge on the same DB file.
    ///
    /// The `--data-dir` CLI flag and the `PERIMA_DATA_DIR` env var still take
    /// precedence (for testing, CI, and custom-path installs).
    ///
    /// # Errors
    /// Returns `CoreError::Internal` if platform directories cannot
    /// be resolved, or `CoreError::Io` on filesystem failures.
    pub(crate) fn resolve(cli_data_dir: Option<PathBuf>) -> Result<Self, CoreError> {
        // WHY config_dir still uses ProjectDirs: the config dir (device_id.txt)
        // predates #154 and is machine-local only — it does not need to match
        // the desktop path. The data_dir (the DB) is what must match.
        let dirs = directories::ProjectDirs::from("dev", "perima", "perima")
            .ok_or_else(|| CoreError::Internal("cannot resolve project dirs".into()))?;

        let config_dir = std::env::var_os("PERIMA_CONFIG_DIR")
            .map_or_else(|| dirs.config_dir().to_path_buf(), PathBuf::from);

        // WHY `resolve_data_dir()` as default: aligns CLI with the desktop
        // bundle-id path. `PERIMA_DATA_DIR` env override + `--data-dir` flag
        // still take precedence for tests and custom installs.
        let canonical_data_dir = perima_app::config::resolve_data_dir()?;
        let data_dir = cli_data_dir
            .or_else(|| std::env::var_os("PERIMA_DATA_DIR").map(PathBuf::from))
            .unwrap_or(canonical_data_dir);

        std::fs::create_dir_all(&config_dir)?;
        std::fs::create_dir_all(&data_dir)?;

        let device_id = load_or_create_device_id(&config_dir)?;
        Ok(Self {
            data_dir,
            config_dir,
            device_id,
        })
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

/// Resolve using explicit directories (used in tests to avoid unsafe env mutation).
///
/// WHY: `std::env::set_var` is unsafe in Rust ≥ 1.86; the workspace
/// denies `unsafe_code`, so tests go through this helper instead.
#[cfg(test)]
fn resolve_with_dirs(config_dir: PathBuf, data_dir: PathBuf) -> Result<Config, CoreError> {
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
        // WHY: call resolve_with_dirs instead of mutating env, because
        // std::env::set_var is unsafe since Rust 1.86 and the workspace
        // denies unsafe_code.
        let a = resolve_with_dirs(tmp.path().to_path_buf(), tmp.path().to_path_buf())
            .expect("resolve 1");
        let b = resolve_with_dirs(tmp.path().to_path_buf(), tmp.path().to_path_buf())
            .expect("resolve 2");
        assert_eq!(a.device_id.0, b.device_id.0);
    }
}
