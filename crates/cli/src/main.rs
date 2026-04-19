//! `perima` command-line entry point.

#![forbid(unsafe_code)]
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "CLI's purpose is user-facing output to stdout/stderr. Migrate to `tracing` for structured logs in a future wave; user-facing output stays on println!/eprintln!."
)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "Binary crate: modules are pub(crate) to satisfy unreachable_pub; items inside are also pub(crate) for explicit scope signaling. redundant_pub_crate fires on pub(crate)-inside-pub(crate) but the intent is explicit, not accidental."
)]

pub(crate) mod cmd;
pub(crate) mod config;
pub(crate) mod logging;
pub(crate) mod panic;
pub(crate) mod signals;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use perima_core::MetadataRepository;
use perima_db::{
    SqliteFileRepository, SqliteMetadataRepository, SqliteSearchRepository, SqliteTagRepository,
    SqliteVolumeRepository, open_and_migrate,
};
use perima_fs::WalkdirScanner;
use perima_hash::Blake3Service;
use perima_media::ThumbnailGenerator;

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

        /// Skip the bounded post-walk drain of the metadata queue.
        #[arg(long)]
        no_wait_metadata: bool,

        /// Disable WebP thumbnail generation for image / video files.
        #[arg(long)]
        no_thumbnails: bool,
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

        /// Include media metadata columns.
        #[arg(long)]
        with_metadata: bool,

        /// Filter to files carrying this tag.
        #[arg(long)]
        tag: Option<String>,
    },

    /// Tag management: add, remove, and list tags.
    Tag(cmd::tag::TagArgs),

    /// Full-text search over indexed file metadata and tags.
    Search(cmd::search::SearchArgs),

    /// List known volumes and their mount paths on this machine.
    Volumes,

    /// Watch a directory for filesystem changes and update the database.
    Watch {
        /// Directory to watch.
        root: PathBuf,
    },

    /// Re-extract and print media metadata for a specific file.
    Metadata {
        /// Path to the file whose metadata should be re-extracted.
        path: PathBuf,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
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
            no_wait_metadata,
            no_thumbnails,
        } => {
            dispatch_scan(
                root,
                dry_run,
                quiet,
                no_wait_metadata,
                no_thumbnails,
                &config,
                &cancel,
            )
            .await
        }

        Command::Ls {
            volume,
            limit,
            json,
            with_metadata,
            tag,
        } => dispatch_ls(volume, limit, json, with_metadata, tag, &config),

        Command::Tag(args) => dispatch_tag(&args, &config),

        Command::Search(args) => dispatch_search(&args, &config),

        Command::Volumes => dispatch_volumes(&config),

        Command::Watch { root } => dispatch_watch(root, &config, &cancel).await,

        Command::Metadata { path, json } => dispatch_metadata(path, json, &config).await,
    }
}

/// Run the `scan` subcommand.
//
// WHY `#[allow(clippy::future_not_send)]`: propagates from
// `scan::run` (`on_persist` captures a non-Sync closure). This task
// is awaited directly from `#[tokio::main]` — never sent between
// threads — so the non-Send future is acceptable here.
// WHY allow(fn_params_excessive_bools): each bool corresponds to a
// distinct `--flag` on `perima scan`. Collapsing them into an enum
// would either merge orthogonal axes or lose the 1:1 CLI mapping.
#[allow(clippy::future_not_send)]
#[allow(clippy::too_many_arguments)]
#[allow(clippy::fn_params_excessive_bools)]
async fn dispatch_scan(
    root: PathBuf,
    dry_run: bool,
    quiet: bool,
    no_wait_metadata: bool,
    no_thumbnails: bool,
    config: &Config,
    cancel: &Cancellation,
) -> ExitCode {
    let args = cmd::scan::ScanArgs {
        root,
        dry_run,
        quiet,
        no_wait_metadata,
        no_thumbnails,
    };
    let scanner = WalkdirScanner::new();
    let hasher = Blake3Service::new();

    if dry_run {
        // WHY turbofish: both repos are None so the type parameters FR and VR
        // are never instantiated, but Rust needs concrete types for
        // monomorphisation. SqliteFileRepository / SqliteVolumeRepository are
        // the production impls; using them here is a zero-cost hint with no
        // allocation because the None branches never call them.
        map_scan_result(
            cmd::scan::run::<_, _, SqliteFileRepository, SqliteVolumeRepository>(
                &scanner,
                &hasher,
                None,
                None,
                None,
                None,
                None,
                config.device_id,
                cancel,
                &args,
            )
            .await,
        )
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

        // WHY separate metadata connection: SqliteMetadataRepository
        // owns a Mutex<Connection>. Under WAL mode another open is
        // instant; sharing across repos would require Arc<Mutex<>>
        // layering none of the existing constructors accept. The same
        // rationale applies to file_repo/vol_repo above.
        let metadata_conn = match open_and_migrate(&db_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("perima: database (metadata repo): {e}");
                return ExitCode::from(1);
            }
        };
        let metadata_repo: Arc<dyn MetadataRepository> =
            Arc::new(SqliteMetadataRepository::new(metadata_conn));

        // WHY build the thumbnailer here: `config.data_dir` is the
        // root the rest of `scan::run` uses to resolve `perima.db`, so
        // thumbnails co-locating under `<data_dir>/thumbnails/...` is
        // the simplest layout. `--no-thumbnails` short-circuits to a
        // no-op generator that returns `Ok(None)` from `generate`.
        let thumbnailer: Arc<ThumbnailGenerator> = Arc::new(if no_thumbnails {
            ThumbnailGenerator::disabled()
        } else {
            ThumbnailGenerator::new(config.data_dir.clone())
        });

        map_scan_result(
            cmd::scan::run(
                &scanner,
                &hasher,
                Some(&mut file_repo),
                Some(&mut vol_repo),
                Some(metadata_repo),
                Some(thumbnailer),
                Some(&on_persist),
                device,
                cancel,
                &args,
            )
            .await,
        )
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
fn dispatch_ls(
    volume: Option<String>,
    limit: usize,
    json: bool,
    with_metadata: bool,
    tag: Option<String>,
    config: &Config,
) -> ExitCode {
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
    let file_conn = match open_and_migrate(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database: {e}");
            return ExitCode::from(1);
        }
    };
    let meta_conn = match open_and_migrate(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database (metadata): {e}");
            return ExitCode::from(1);
        }
    };
    let tag_conn = match open_and_migrate(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database (tag repo): {e}");
            return ExitCode::from(1);
        }
    };
    let repo = SqliteFileRepository::new(file_conn);
    let metadata_repo = SqliteMetadataRepository::new(meta_conn);
    let tag_repo = SqliteTagRepository::new(tag_conn);
    let ls_args = cmd::ls::LsArgs {
        volume: volume_id,
        limit,
        json,
        with_metadata,
        tag,
    };
    match cmd::ls::run(&repo, &metadata_repo, &tag_repo, &ls_args) {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("perima: {e}");
            ExitCode::from(1)
        }
    }
}

/// Run the `tag` subcommand.
fn dispatch_tag(args: &cmd::tag::TagArgs, config: &Config) -> ExitCode {
    let db_path = config.data_dir.join("perima.db");
    let tag_conn = match open_and_migrate(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database (tag repo): {e}");
            return ExitCode::from(1);
        }
    };
    let file_conn = match open_and_migrate(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database (file repo): {e}");
            return ExitCode::from(1);
        }
    };
    let tag_repo = SqliteTagRepository::new(tag_conn);
    let file_repo = SqliteFileRepository::new(file_conn);
    match cmd::tag::run(&tag_repo, &file_repo, config.device_id, args) {
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

/// Run the `metadata` subcommand.
async fn dispatch_metadata(path: PathBuf, json: bool, config: &Config) -> ExitCode {
    let args = cmd::metadata::MetadataArgs { path, json };
    match cmd::metadata::run(&config.data_dir, config.device_id, &args).await {
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

/// Run the `search` subcommand.
fn dispatch_search(args: &cmd::search::SearchArgs, config: &Config) -> ExitCode {
    let db_path = config.data_dir.join("perima.db");
    let conn = match open_and_migrate(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database (search repo): {e}");
            return ExitCode::from(1);
        }
    };
    let repo = SqliteSearchRepository::new(conn);
    match cmd::search::run(&repo, args) {
        Ok(()) => ExitCode::from(0),
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
