//! `perima migrate-data-dir` — one-shot migration of legacy CLI data into the
//! canonical (desktop-shared) data dir.
//!
//! v0.6.x users may have CLI data in `~/.local/share/perima/` (the path that
//! `directories::ProjectDirs::from("dev","perima","perima")` resolves to on
//! Linux). After GH #154, both CLI and desktop share the Tauri bundle-id path
//! `~/.local/share/dev.perima.desktop/perima/`. This command moves the legacy
//! DB files into the canonical path so users can switch without losing data.
//!
//! Refuses to migrate if the canonical path already contains `perima.db`, to
//! prevent overwriting an active desktop database.

use std::path::{Path, PathBuf};

use clap::Parser;
use perima_core::CoreError;

/// Arguments for the `migrate-data-dir` subcommand.
#[derive(Parser, Debug)]
pub(crate) struct MigrateDataDirArgs {
    /// Print what would happen without making any changes.
    #[arg(long)]
    pub dry_run: bool,
}

/// Legacy data directory — the path `directories::ProjectDirs::from("dev","perima","perima")`
/// resolves to on each platform, which is what the CLI used before GH #154.
///
/// WHY inline here (not in `resolve_data_dir`): the shared resolver MUST NOT
/// return the legacy path; it returns the canonical bundle-id path. We keep the
/// old lookup here only so we can find existing user data to migrate it.
fn legacy_data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "perima", "perima").map(|d| d.data_dir().to_path_buf())
}

/// Run the `migrate-data-dir` subcommand (thin shim — resolves platform paths
/// then delegates to [`migrate`]).
///
/// The canonical destination respects the `PERIMA_DATA_DIR` env var so that
/// tests and CI can redirect the target without touching real user data.
///
/// # Errors
/// Returns `CoreError::Internal` if the canonical path cannot be resolved, or
/// `CoreError::Io` on filesystem failures (dir creation, rename).
pub(crate) fn run(args: &MigrateDataDirArgs) -> Result<(), CoreError> {
    let Some(legacy) = legacy_data_dir() else {
        println!("migrate-data-dir: cannot resolve legacy path; nothing to migrate.");
        return Ok(());
    };
    // WHY honour PERIMA_DATA_DIR: mirrors the precedence in Config::resolve,
    // allowing tests + CI to redirect the canonical path without touching the
    // user's real DB. When PERIMA_DATA_DIR is set the migration target is the
    // override; otherwise it is the shared bundle-id path from resolve_data_dir.
    let canonical = std::env::var_os("PERIMA_DATA_DIR")
        .map(PathBuf::from)
        .map_or_else(perima_app::config::resolve_data_dir, Ok)?;
    migrate(&legacy, &canonical, args.dry_run)
}

/// Core migration logic accepting explicit paths.
///
/// WHY explicit-path signature: enables unit tests with `TempDir`-backed paths
/// without mutating env vars (which are `unsafe` since Rust 1.86 and banned
/// workspace-wide by `#![forbid(unsafe_code)]`).
///
/// # Errors
/// Returns `CoreError::Internal` when the canonical DB already exists (refuses
/// to overwrite), or `CoreError::Io` on filesystem failures.
#[allow(clippy::module_name_repetitions)] // WHY: `migrate_data_dir::migrate` is the natural name; lint is overzealous here.
pub(crate) fn migrate(legacy: &Path, canonical: &Path, dry_run: bool) -> Result<(), CoreError> {
    // If legacy and canonical are the same directory (shouldn't happen post-#154
    // but handle it gracefully), there is nothing to do.
    if legacy == canonical {
        println!(
            "migrate-data-dir: legacy and canonical paths are identical — nothing to migrate."
        );
        println!("  path: {}", legacy.display());
        return Ok(());
    }

    let legacy_db = legacy.join("perima.db");

    // No legacy DB → nothing to move.
    if !legacy_db.exists() {
        println!(
            "migrate-data-dir: no legacy database found at {}; nothing to migrate.",
            legacy.display()
        );
        return Ok(());
    }

    let canonical_db = canonical.join("perima.db");

    // Canonical DB already present → refuse to overwrite.
    if canonical_db.exists() {
        eprintln!(
            "migrate-data-dir: canonical database already exists at {}.",
            canonical_db.display()
        );
        eprintln!("  Refusing to overwrite. Manually back up or remove the existing DB first.");
        return Err(CoreError::Internal(
            "canonical database already exists; refusing to overwrite".into(),
        ));
    }

    println!(
        "migrate-data-dir: found legacy database at {}",
        legacy.display()
    );
    println!("  → canonical path: {}", canonical.display());

    // Enumerate files to migrate (db + WAL side-cars).
    let files_to_move: Vec<(&str, PathBuf, PathBuf)> =
        ["perima.db", "perima.db-shm", "perima.db-wal"]
            .iter()
            .filter_map(|name| {
                let src = legacy.join(name);
                if src.exists() {
                    let dst = canonical.join(name);
                    Some((*name, src, dst))
                } else {
                    None
                }
            })
            .collect();

    for (name, src, dst) in &files_to_move {
        if dry_run {
            println!(
                "  [dry-run] would move {} → {}",
                src.display(),
                dst.display()
            );
        } else {
            println!("  moving {name} ...");
        }
    }

    if dry_run {
        println!("migrate-data-dir: dry-run complete; no files were moved.");
        return Ok(());
    }

    // Create the canonical directory tree if it doesn't exist yet.
    std::fs::create_dir_all(canonical)?;

    for (_name, src, dst) in &files_to_move {
        // WHY copy+remove fallback: `fs::rename` is atomic and preferred but
        // fails with `CrossesDevices` (EXDEV / os error 18) when `legacy` and
        // `canonical` live on different filesystems (e.g. home on ext4 vs
        // tmp on tmpfs in tests). The fallback is not atomic but safe for
        // migration: if interrupted, the source file stays intact.
        if let Err(e) = std::fs::rename(src, dst) {
            if e.kind() == std::io::ErrorKind::CrossesDevices {
                std::fs::copy(src, dst)?;
                std::fs::remove_file(src)?;
            } else {
                return Err(CoreError::from(e));
            }
        }
    }

    println!(
        "migrate-data-dir: migration complete. {} file(s) moved to {}.",
        files_to_move.len(),
        canonical.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// No legacy DB present → returns Ok, no files created in canonical dir.
    #[test]
    fn migrate_nothing_when_legacy_db_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let canonical = tmp.path().join("canonical");
        fs::create_dir_all(&legacy).expect("create legacy dir");

        migrate(&legacy, &canonical, false).expect("should succeed");

        // Canonical dir was NOT created when there was nothing to migrate.
        assert!(
            !canonical.join("perima.db").exists(),
            "canonical perima.db should not exist"
        );
    }

    /// Legacy DB present, canonical dir empty → files are moved.
    #[test]
    fn migrate_moves_db_when_legacy_has_data() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let canonical = tmp.path().join("canonical");
        fs::create_dir_all(&legacy).expect("create legacy dir");

        // Create a fake legacy DB + WAL.
        fs::write(legacy.join("perima.db"), b"fake-db").expect("write db");
        fs::write(legacy.join("perima.db-wal"), b"fake-wal").expect("write wal");

        migrate(&legacy, &canonical, false).expect("migrate");

        assert!(
            canonical.join("perima.db").exists(),
            "canonical perima.db should exist after migration"
        );
        assert!(
            canonical.join("perima.db-wal").exists(),
            "canonical perima.db-wal should exist after migration"
        );
        assert!(
            !legacy.join("perima.db").exists(),
            "legacy perima.db should be gone after migration"
        );
    }

    /// Dry run: files remain in legacy dir, canonical dir untouched.
    #[test]
    fn migrate_dry_run_does_not_move_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let canonical = tmp.path().join("canonical");
        fs::create_dir_all(&legacy).expect("create legacy dir");
        fs::write(legacy.join("perima.db"), b"fake-db").expect("write db");

        migrate(&legacy, &canonical, true).expect("dry-run");

        assert!(
            legacy.join("perima.db").exists(),
            "legacy db should be untouched after dry-run"
        );
        assert!(
            !canonical.join("perima.db").exists(),
            "canonical db should not exist after dry-run"
        );
    }

    /// Canonical DB already present → returns Err to prevent overwrite.
    #[test]
    fn migrate_refuses_when_canonical_db_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let legacy = tmp.path().join("legacy");
        let canonical = tmp.path().join("canonical");
        fs::create_dir_all(&legacy).expect("create legacy");
        fs::create_dir_all(&canonical).expect("create canonical");
        fs::write(legacy.join("perima.db"), b"old-db").expect("write legacy db");
        fs::write(canonical.join("perima.db"), b"new-db").expect("write canonical db");

        let result = migrate(&legacy, &canonical, false);
        assert!(
            result.is_err(),
            "should refuse to overwrite existing canonical DB"
        );
    }

    /// Same legacy and canonical path → reports nothing to do.
    #[test]
    fn migrate_same_paths_is_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("data");
        fs::create_dir_all(&path).expect("create dir");
        fs::write(path.join("perima.db"), b"db").expect("write db");

        // Same path for both — should succeed without error.
        migrate(&path, &path, false).expect("same-path should be noop");
    }
}
