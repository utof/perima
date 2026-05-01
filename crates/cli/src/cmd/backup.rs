//! `perima backup` — produce a single-file `SQLite` snapshot of the
//! current database via `VACUUM INTO`.

use std::path::PathBuf;

use clap::Parser;
use perima_app::{AppContainer, BackupCommand};
use perima_core::CoreError;

/// Arguments for `perima backup`.
#[derive(Parser, Debug)]
pub(crate) struct BackupArgs {
    /// Where to write the backup file.
    /// Default: `<data_dir>/backups/perima-<ISO8601>.sqlite`
    #[arg(long)]
    pub to: Option<PathBuf>,

    /// Overwrite the target if it already exists.
    /// Has no effect when `--to` is omitted (default path is timestamped).
    #[arg(long)]
    pub force: bool,
}

/// Run the backup subcommand.
///
/// Delegates to [`BackupDatabaseUseCase::execute`] and prints `"Saved to
/// <path> (X.X MB)"` on success.
///
/// # Errors
///
/// Returns [`CoreError::BackupFailed`] if the backup fails; the caller
/// maps to a non-zero exit + stderr message.
///
/// [`BackupDatabaseUseCase::execute`]: perima_app::BackupDatabaseUseCase::execute
pub(crate) async fn run(container: &AppContainer, args: &BackupArgs) -> Result<(), CoreError> {
    let out = container
        .backup
        .execute(BackupCommand {
            target: args.to.clone(),
            force: args.force,
        })
        .await?;

    #[allow(clippy::cast_precision_loss)] // WHY: MB display precision acceptable for u64 → f64
    let mb = (out.size_bytes as f64) / (1024.0 * 1024.0);
    println!("Saved to {} ({:.1} MB)", out.absolute_path.display(), mb);
    Ok(())
}
