//! `perima` command-line entry point.

mod cmd;
mod config;
mod logging;
mod panic;
mod signals;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use perima_core::VolumeId;
use perima_db::{SqliteFileRepository, open_and_migrate};
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
}

fn main() -> ExitCode {
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

    // WHY: sentinel VolumeId (all-zeros UUID) is used until phase 1c
    // wires real volume detection. Phase 1c will UPDATE file_locations
    // SET volume_id = <real> WHERE volume_id = '00000000-...' after
    // resolving actual volumes.
    let volume = VolumeId(uuid::Uuid::nil());

    if dry_run {
        // WHY turbofish: repo = None so the type parameter R is never
        // instantiated, but Rust needs a concrete type for
        // monomorphisation. SqliteFileRepository is the production impl;
        // using it here is a zero-cost hint with no allocation because
        // the None branch never calls it.
        map_scan_result(cmd::scan::run::<_, _, SqliteFileRepository>(
            &scanner,
            &hasher,
            None,
            config.device_id,
            volume,
            cancel,
            &args,
        ))
    } else {
        let db_path = config.data_dir.join("perima.db");
        let conn = match open_and_migrate(&db_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("perima: database: {e}");
                return ExitCode::from(1);
            }
        };
        let mut repo = SqliteFileRepository::new(conn);
        map_scan_result(cmd::scan::run(
            &scanner,
            &hasher,
            Some(&mut repo),
            config.device_id,
            volume,
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
                .map(VolumeId)
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
