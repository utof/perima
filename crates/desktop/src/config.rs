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

/// Resolve config using the Tauri-provided `app_data_dir`.
///
/// WHY a second entry point (not just an override on [`resolve_config`]):
/// `tauri.conf.json`'s `assetProtocol.scope` literal is
/// `$APPDATA/perima/thumbnails/**`, where `$APPDATA` resolves via the
/// Tauri bundle identifier (`dev.perima.desktop`) to e.g.
/// `~/.local/share/dev.perima.desktop` on Linux. The `directories`
/// crate, in contrast, resolves `ProjectDirs::from("dev","perima","perima")`
/// to `~/.local/share/perima` — a different subtree. Before this fix,
/// the runtime wrote thumbnails under the `directories`-derived path
/// while the scope allowlisted the Tauri-derived path; every
/// `convertFileSrc(thumbnail_path)` therefore returned 404.
///
/// The Tauri-resolved `app_data_dir` is the single source of truth the
/// desktop backend now threads through. `data_dir` is
/// `<app_data_dir>/perima` so the existing scope literal
/// (`$APPDATA/perima/thumbnails/**`) matches the runtime's thumbnail
/// root (`<app_data_dir>/perima/thumbnails/...`). The `perima`
/// subdirectory also keeps the CLI's `~/.local/share/perima` layout
/// conceptually aligned: the directory basename stays `perima`, only
/// the parent tree changes to the Tauri bundle-id subtree.
///
/// `config_dir` reuses `app_data_dir` (no separate XDG config subtree)
/// because the Tauri bundle-id tree is already a single dedicated
/// directory per app; splitting config vs data there buys nothing.
/// `device_id.txt` lives at `<app_data_dir>/device_id.txt`.
///
/// # Errors
/// Returns `CoreError::Io` on filesystem failures creating the directories
/// or reading / writing the device-id sidecar.
pub fn resolve_with_app_data_dir(app_data_dir: &Path) -> Result<Config, CoreError> {
    let config_dir = app_data_dir.to_path_buf();
    let data_dir = app_data_dir.join("perima");

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

    /// `data_dir` ends in a `perima` segment so the runtime thumbnail
    /// root `<data_dir>/thumbnails/...` falls inside the asset-protocol
    /// scope literal `$APPDATA/perima/thumbnails/**` declared in
    /// `tauri.conf.json`. If this assertion breaks, `convertFileSrc`
    /// silently 404s in the `WebView`.
    #[test]
    fn app_data_dir_derives_perima_subtree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Simulate Tauri's `app.path().app_data_dir()` returning
        // `<tmp>/dev.perima.desktop`.
        let app_data_dir = tmp.path().join("dev.perima.desktop");
        let cfg = resolve_with_app_data_dir(&app_data_dir).expect("resolve");

        assert_eq!(
            cfg.data_dir,
            app_data_dir.join("perima"),
            "data_dir must be <app_data_dir>/perima so $APPDATA/perima/thumbnails/** matches",
        );
        let thumb_root = cfg.data_dir.join("thumbnails");
        assert!(
            thumb_root.starts_with(&app_data_dir),
            "thumbnail root {} must fall under app_data_dir {}",
            thumb_root.display(),
            app_data_dir.display(),
        );
    }
}
