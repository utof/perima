//! `perima` command-line entry point.

mod cmd;
mod config;
mod logging;
mod panic;
mod signals;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use perima_fs::WalkdirScanner;
use perima_hash::Blake3Service;

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

    /// Override the main database directory. Not used in phase 1a
    /// (DB lands in 1b); accepted for forward compatibility.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Walk a directory, hash every file, and print the results.
    Scan {
        /// Directory to walk.
        root: PathBuf,

        /// Dry-run mode: hash and print, skip DB writes.
        /// In phase 1a this is the only supported mode; 1b makes it optional.
        #[arg(long)]
        dry_run: bool,

        /// Suppress per-file stdout lines.
        #[arg(long)]
        quiet: bool,
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

    let _config = match config::Config::resolve(cli.data_dir.clone()) {
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
        } => {
            let args = cmd::scan::ScanArgs {
                root,
                dry_run,
                quiet,
            };
            let scanner = WalkdirScanner::new();
            let hasher = Blake3Service::new();
            match cmd::scan::run(&scanner, &hasher, &cancel, &args) {
                Ok(cmd::scan::ExitCode::Success) => ExitCode::from(0),
                Ok(cmd::scan::ExitCode::Interrupted) => ExitCode::from(130),
                Err(
                    perima_core::CoreError::InvalidPath(msg)
                    | perima_core::CoreError::Unsupported(msg),
                ) => {
                    eprintln!("perima: {msg}");
                    ExitCode::from(2)
                }
                Err(e) => {
                    eprintln!("perima: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}
