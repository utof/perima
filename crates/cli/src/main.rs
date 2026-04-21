//! `perima` command-line entry point.

#![forbid(unsafe_code)]
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "CLI's purpose is user-facing output to stdout/stderr. Migrate to `tracing` for structured logs in a future wave; user-facing output stays on println!/eprintln!."
)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "Binary crate. clippy::nursery::redundant_pub_crate fires on pub(crate) items inside private `mod` blocks because pub(crate) is technically redundant there (the private mod already restricts visibility). We keep items pub(crate) anyway for explicit scope signalling and suppress the nursery lint crate-wide."
)]

mod cmd;
mod config;
mod logging;
mod panic;
mod signals;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use perima_app::{AppContainer, AppDeps};
use perima_core::{
    EventBus, FileRepository, HashService, MetadataRepository, Scanner, SearchRepository,
    TagRepository, VolumeRepository,
};
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

/// Entry point.
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
        } => dispatch_ls(volume, limit, json, with_metadata, tag, &config).await,

        Command::Tag(args) => dispatch_tag(&args, &config).await,

        Command::Search(args) => dispatch_search(&args, &config).await,

        Command::Volumes => dispatch_volumes(&config).await,

        Command::Watch { root } => dispatch_watch(root, &config, &cancel).await,

        Command::Metadata { path, json } => dispatch_metadata(path, json, &config).await,
    }
}

// ---------------------------------------------------------------------------
// AppContainer construction
// ---------------------------------------------------------------------------

/// Build an [`AppContainer`] for a given database path.
///
/// `extra_handlers` lets callers inject additional [`EventBus`] implementations
/// before the single [`perima_app::CompositeEventBus`] is constructed inside
/// [`AppContainer::new`]. The `watch` dispatcher uses this to inject its
/// `DbEventHandler` so that filesystem events can mutate location rows via the
/// shared bus without constructing a second `CompositeEventBus` in the shell.
///
/// WHY one connection per repo (not one `Arc<Mutex<Connection>>` shared):
/// each `Sqlite*Repository` owns its own `Mutex<Connection>` today (see
/// `crates/db/src/*_repo.rs`). Under WAL mode opening multiple connections
/// to the same file is cheap and avoids a second layer of `Mutex` that none
/// of the repo constructors accept. Batch C (connection-actor) will
/// consolidate this to a single writer + read pool.
fn build_container(
    db_path: &Path,
    extra_handlers: Vec<Arc<dyn EventBus>>,
) -> Result<Arc<AppContainer>, perima_core::CoreError> {
    let files: Arc<dyn FileRepository> =
        Arc::new(SqliteFileRepository::new(open_and_migrate(db_path)?));
    let volumes: Arc<dyn VolumeRepository> =
        Arc::new(SqliteVolumeRepository::new(open_and_migrate(db_path)?));
    let tags: Arc<dyn TagRepository> =
        Arc::new(SqliteTagRepository::new(open_and_migrate(db_path)?));
    let metadata: Arc<dyn MetadataRepository> =
        Arc::new(SqliteMetadataRepository::new(open_and_migrate(db_path)?));
    let search: Arc<dyn SearchRepository> =
        Arc::new(SqliteSearchRepository::new(open_and_migrate(db_path)?));
    let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
    let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());

    // WHY the thumbnailer is chosen at container-build time: the container
    // is constructed once per command dispatch (via `build_container`),
    // and each dispatcher overrides the thumbnailer *flag* via the
    // `FullScan { no_thumbnails }` field at UseCase call time. The wired
    // generator here stays enabled by default; `--no-thumbnails` flips
    // to `disabled()` inside the UseCase.
    let thumbnailer = Arc::new(ThumbnailGenerator::new(
        db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
    ));

    let deps = AppDeps {
        files,
        volumes,
        tags,
        metadata,
        search,
        hasher,
        scanner,
        thumbnailer,
    };

    // WHY log handler always first: every command benefits from tracing
    // event emissions; `extra_handlers` (injected by the watch dispatcher)
    // are appended after so the log entry always fires before DB writes.
    let log_handler: Arc<dyn EventBus> = Arc::new(perima_app::LogEventHandler);
    let mut handlers: Vec<Arc<dyn EventBus>> = vec![log_handler];
    handlers.extend(extra_handlers);
    Ok(AppContainer::new(deps, handlers))
}

/// Build a `DbEventHandler` for the `watch` command, wrapped as `Arc<dyn EventBus>`.
///
/// WHY a dedicated helper: `dispatch_watch` must construct the handler
/// before calling `build_container` so it can be passed as an `extra_handler`.
/// Extracting it here keeps `dispatch_watch` focused on control-flow and makes
/// the single-connection justification easy to find.
fn build_watch_db_handler(
    db_path: &Path,
    device_id: perima_core::DeviceId,
) -> Result<Arc<dyn EventBus>, perima_core::CoreError> {
    // WHY a fresh connection: `watch` needs its own `SqliteFileRepository`
    // to mutate location rows as filesystem events arrive. Under WAL mode
    // opening an additional connection is cheap and safe.
    let file_conn = open_and_migrate(db_path)?;
    let file_repo = Arc::new(SqliteFileRepository::new(file_conn));
    Ok(crate::cmd::watch::make_db_event_handler(
        file_repo, device_id,
    ))
}

// ---------------------------------------------------------------------------
// Dispatchers
// ---------------------------------------------------------------------------

/// Run the `scan` subcommand.
//
// WHY allow(fn_params_excessive_bools): each bool corresponds to a
// distinct `--flag` on `perima scan`.
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

    let db_path = config.data_dir.join("perima.db");

    // WHY dry-run takes the same container path: the UseCase's
    // `FullScan { dry_run: true }` branch skips every DB write and
    // volume detection internally — no split path needed. Building the
    // container still requires migrations to have run, which is
    // harmless for a fresh dry-run against an empty data dir.
    let container = match build_container(&db_path, vec![]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: {e}");
            return ExitCode::from(1);
        }
    };

    // WHY sentinel migration closure stays in the shell: the
    // `FileRepository` trait doesn't expose `migrate_sentinel_row`; it's
    // an impl-specific method on `SqliteFileRepository`. The UseCase
    // accepts an opaque `OnPersist = Arc<dyn Fn + Send + Sync>` hook,
    // and the CLI constructs one here with its own short-lived
    // `SqliteFileRepository`.
    let on_persist: cmd::scan::OnPersistFactory = if dry_run {
        None
    } else {
        let sentinel_conn = match open_and_migrate(&db_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("perima: database (sentinel migration): {e}");
                return ExitCode::from(1);
            }
        };
        let sentinel_repo = Arc::new(SqliteFileRepository::new(sentinel_conn));
        Some(Arc::new(
            move |path: &perima_core::MediaPath,
                  volume: perima_core::VolumeId,
                  dev: perima_core::DeviceId| {
                if let Err(e) = sentinel_repo.migrate_sentinel_row(path, volume, dev) {
                    tracing::warn!(error = %e, "sentinel migration failed (non-fatal)");
                }
            },
        ) as perima_app::OnPersist)
    };

    map_scan_result(cmd::scan::run(&container, config.device_id, cancel, on_persist, &args).await)
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
async fn dispatch_ls(
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
    let container = match build_container(&db_path, vec![]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database: {e}");
            return ExitCode::from(1);
        }
    };
    let ls_args = cmd::ls::LsArgs {
        volume: volume_id,
        limit,
        json,
        with_metadata,
        tag,
    };
    match cmd::ls::run(&container, &config.data_dir, config.device_id, &ls_args).await {
        Ok(()) => ExitCode::from(0),
        Err(perima_core::CoreError::NotFound(msg)) => {
            eprintln!("perima: {msg}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("perima: {e}");
            ExitCode::from(1)
        }
    }
}

/// Run the `tag` subcommand.
async fn dispatch_tag(args: &cmd::tag::TagArgs, config: &Config) -> ExitCode {
    let db_path = config.data_dir.join("perima.db");
    let container = match build_container(&db_path, vec![]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database: {e}");
            return ExitCode::from(1);
        }
    };
    match cmd::tag::run(&container, &config.data_dir, config.device_id, args).await {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("perima: {e}");
            ExitCode::from(1)
        }
    }
}

/// Run the `watch` subcommand.
async fn dispatch_watch(root: PathBuf, config: &Config, cancel: &Cancellation) -> ExitCode {
    let db_path = config.data_dir.join("perima.db");

    // WHY build the DbEventHandler here and inject via extra_handlers:
    // watch needs a DB handler so filesystem events mutate location rows.
    // Constructing it here (before AppContainer::new) keeps CompositeEventBus
    // construction in exactly one place — container.rs §4 acceptance criterion.
    let db_handler = match build_watch_db_handler(&db_path, config.device_id) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("perima: database (watch handler): {e}");
            return ExitCode::from(1);
        }
    };

    let container = match build_container(&db_path, vec![db_handler]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database: {e}");
            return ExitCode::from(1);
        }
    };
    match cmd::watch::run(
        &container,
        &config.data_dir,
        config.device_id,
        &root,
        cancel,
    )
    .await
    {
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

/// Run the `metadata` subcommand (single-file re-extract — NOT migrated to
/// a `UseCase` in Batch B; the single-file extraction path is post-v1 work).
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
async fn dispatch_search(args: &cmd::search::SearchArgs, config: &Config) -> ExitCode {
    let db_path = config.data_dir.join("perima.db");
    let container = match build_container(&db_path, vec![]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database (search): {e}");
            return ExitCode::from(1);
        }
    };
    match cmd::search::run(&container, args).await {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("perima: {e}");
            ExitCode::from(1)
        }
    }
}

/// Run the `volumes` subcommand.
async fn dispatch_volumes(config: &Config) -> ExitCode {
    let db_path = config.data_dir.join("perima.db");
    let container = match build_container(&db_path, vec![]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database: {e}");
            return ExitCode::from(1);
        }
    };
    match cmd::volumes::run(&container, config.device_id).await {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("perima: {e}");
            ExitCode::from(1)
        }
    }
}
