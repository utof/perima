//! `perima` command-line entry point.

mod cmd;
mod config;
mod logging;
mod panic;
mod signals;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use perima_db::{SqliteFileRepository, SqliteVolumeRepository, open_and_migrate};
use perima_fs::WalkdirScanner;
use perima_hash::Blake3Service;

use crate::config::Config;
use crate::signals::Cancellation;

/// Cross-platform media asset manager.
#[derive(Parser, Debug)]
#[command(
    name = "perima",
    version,
    about = "Index your media across drives by content hash"
)]
struct Cli {
    /// Bump tracing verbosity; repeatable (-v, -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Override the main database directory.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Walk a directory, hash every file, and persist to the database.
    Scan {
        /// Directory to walk.
        root: PathBuf,

        /// Dry-run mode: hash and print, skip DB writes.
        #[arg(long)]
        dry_run: bool,

        /// Suppress per-file stdout lines.
        #[arg(long)]
        quiet: bool,
    },

    /// List indexed files.
    Ls {
        /// Filter to a specific volume UUID.
        #[arg(long)]
        volume: Option<String>,

        /// Maximum rows to return.
        #[arg(long, default_value = "100")]
        limit: usize,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// List known volumes and their mount paths on this machine.
    Volumes,

    /// Watch a directory for filesystem changes and update the database.
    Watch {
        /// Directory to watch.
        root: PathBuf,
    },
}

/// Entry point. Tokio runtime is required for the `watch` command (phase 3a)
/// and for `CancellationToken::cancelled().await` in future sub-commands.
/// All existing sync commands (`scan`, `ls`, `volumes`) run directly on the
/// main task without blocking — they complete before yielding.
///
/// WHY `#[tokio::main]`: `CancellationToken::cancelled()` is an async future;
/// we need a runtime even when the current command is sync. The cost is one
/// thread-pool allocation that goes unused for sync commands — acceptable.
#[tokio::main]
async fn main() -> ExitCode {
    panic::install();
    let cli = Cli::parse();

    if let Err(e) = logging::init(cli.verbose) {
        eprintln!("perima: logging init failed: {e}");
        return ExitCode::from(1);
    }

    let cancel = match signals::install() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: signal handler install failed: {e}");
            return ExitCode::from(1);
        }
    };

    let config = match config::Config::resolve(cli.data_dir.clone()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: config resolution failed: {e}");
            return ExitCode::from(1);
        }
    };

    match cli.command {
        Command::Scan {
            root,
            dry_run,
            quiet,
        } => dispatch_scan(root, dry_run, quiet, &config, &cancel),

        Command::Ls {
            volume,
            limit,
            json,
        } => dispatch_ls(volume, limit, json, &config),

        Command::Volumes => dispatch_volumes(&config),

        Command::Watch { root } => dispatch_watch(root, &config, &cancel).await,
    }
}

/// Run the `scan` subcommand.
fn dispatch_scan(
    root: PathBuf,
    dry_run: bool,
    quiet: bool,
    config: &Config,
    cancel: &Cancellation,
) -> ExitCode {
    let args = cmd::scan::ScanArgs {
        root,
        dry_run,
        quiet,
    };
    let scanner = WalkdirScanner::new();
    let hasher = Blake3Service::new();

    if dry_run {
        // WHY turbofish: both repos are None so the type parameters FR and VR
        // are never instantiated, but Rust needs concrete types for
        // monomorphisation. SqliteFileRepository / SqliteVolumeRepository are
        // the production impls; using them here is a zero-cost hint with no
        // allocation because the None branches never call them.
        map_scan_result(cmd::scan::run::<
            _,
            _,
            SqliteFileRepository,
            SqliteVolumeRepository,
        >(
            &scanner,
            &hasher,
            None,
            None,
            None,
            config.device_id,
            cancel,
            &args,
        ))
    } else {
        let db_path = config.data_dir.join("perima.db");
        // WHY two separate open_and_migrate calls: SqliteFileRepository and
        // SqliteVolumeRepository each take owned Connections wrapped in
        // Mutex<Connection>. Rather than introduce Arc<Mutex<Connection>>
        // complexity, we open the DB twice. Under WAL mode SQLite allows
        // multiple concurrent readers; the second open is instant because
        // migrations already ran on the first connection.
        let file_conn = match open_and_migrate(&db_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("perima: database: {e}");
                return ExitCode::from(1);
            }
        };
        let vol_conn = match open_and_migrate(&db_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("perima: database (volume repo): {e}");
                return ExitCode::from(1);
            }
        };
        let mut file_repo = SqliteFileRepository::new(file_conn);
        let mut vol_repo = SqliteVolumeRepository::new(vol_conn);

        // WHY closure for sentinel migration: migrate_sentinel_row is an
        // impl-specific method on SqliteFileRepository (not on the
        // FileRepository trait). Passing it as a closure lets scan.rs stay
        // generic over FR while still invoking the concrete migration in the
        // production path.
        //
        // WHY second DB connection for sentinel migration: the closure and
        // the FileRepository mutable borrow cannot alias in safe Rust. We
        // open a third lightweight connection for the sentinel UPDATE queries
        // only; under WAL mode this is a cheap SELECT + UPDATE path with no
        // contention against the scan writer.
        let sentinel_conn = match open_and_migrate(&db_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("perima: database (sentinel migration): {e}");
                return ExitCode::from(1);
            }
        };
        let sentinel_repo = SqliteFileRepository::new(sentinel_conn);
        let device = config.device_id;
        let on_persist = |path: &perima_core::MediaPath,
                          volume: perima_core::VolumeId,
                          dev: perima_core::DeviceId| {
            if let Err(e) = sentinel_repo.migrate_sentinel_row(path, volume, dev) {
                tracing::warn!(error = %e, "sentinel migration failed (non-fatal)");
            }
        };

        map_scan_result(cmd::scan::run(
            &scanner,
            &hasher,
            Some(&mut file_repo),
            Some(&mut vol_repo),
            Some(&on_persist),
            device,
            cancel,
            &args,
        ))
    }
}

/// Convert a scan result to a process `ExitCode`.
fn map_scan_result(
    res: Result<(cmd::scan::ExitCode, cmd::scan::ScanStats), perima_core::CoreError>,
) -> ExitCode {
    match res {
        Ok((cmd::scan::ExitCode::Success, _)) => ExitCode::from(0),
        Ok((cmd::scan::ExitCode::Interrupted, _)) => ExitCode::from(130),
        Err(perima_core::CoreError::InvalidPath(msg)) => {
            eprintln!("perima: {msg}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("perima: {e}");
            ExitCode::from(1)
        }
    }
}

/// Run the `ls` subcommand.
fn dispatch_ls(volume: Option<String>, limit: usize, json: bool, config: &Config) -> ExitCode {
    let volume_id = volume
        .map(|v| {
            uuid::Uuid::parse_str(&v)
                .map(perima_core::VolumeId)
                .map_err(|e| format!("bad volume UUID: {e}"))
        })
        .transpose();
    let volume_id = match volume_id {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("perima: {msg}");
            return ExitCode::from(2);
        }
    };
    let db_path = config.data_dir.join("perima.db");
    let conn = match open_and_migrate(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database: {e}");
            return ExitCode::from(1);
        }
    };
    let repo = SqliteFileRepository::new(conn);
    let ls_args = cmd::ls::LsArgs {
        volume: volume_id,
        limit,
        json,
    };
    match cmd::ls::run(&repo, &ls_args) {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("perima: {e}");
            ExitCode::from(1)
        }
    }
}

/// Run the `watch` subcommand.
async fn dispatch_watch(root: PathBuf, config: &Config, cancel: &Cancellation) -> ExitCode {
    match cmd::watch::run(&config.data_dir, config.device_id, &root, cancel).await {
        Ok(()) => ExitCode::from(0),
        Err(perima_core::CoreError::InvalidPath(msg)) => {
            eprintln!("perima: {msg}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("perima: {e}");
            ExitCode::from(1)
        }
    }
}

/// Run the `volumes` subcommand.
fn dispatch_volumes(config: &Config) -> ExitCode {
    let db_path = config.data_dir.join("perima.db");
    let conn = match open_and_migrate(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database: {e}");
            return ExitCode::from(1);
        }
    };
    let repo = SqliteVolumeRepository::new(conn);
    match cmd::volumes::run(&repo, config.device_id) {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("perima: {e}");
            ExitCode::from(1)
        }
    }
}
