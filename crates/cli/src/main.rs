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
use perima_app::{AppContainer, AppDeps, EventHandler};
use perima_core::{
    FileRepository, HashService, MetadataRepository, Scanner, SearchRepository, TagRepository,
    VolumeRepository,
};
use perima_db::{
    ReadPool, SqliteFileRepository, SqliteMetadataRepository, SqliteSearchRepository,
    SqliteTagRepository, SqliteVolumeRepository, SqliteWriter,
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
/// `extra_handlers` lets callers inject additional [`EventHandler`] implementations
/// before the single [`perima_app::Bus`] is constructed inside
/// [`AppContainer::new`]. The `watch` dispatcher uses this to inject its
/// `DbEventHandler` so that filesystem events can mutate location rows via the
/// shared bus without constructing a second bus in the shell.
fn build_container(
    db_path: &Path,
    extra_handlers: Vec<Box<dyn EventHandler>>,
) -> Result<Arc<AppContainer>, perima_core::CoreError> {
    // WHY a `NoopBus` passed to the writer: the writer's after-COMMIT
    // emission path is scaffolded but file-event emission is handled by
    // the async-broadcast Bus wired into `AppContainer`. The writer bus
    // is separate from the handler list — it receives post-COMMIT events
    // from the writer thread (std::thread, not tokio), so it must be
    // Arc<dyn EventBus> (sync emit). Spec §§3.3 + 4.8 (A4.8 first bullet).
    struct NoopBus;
    impl perima_core::events::EventBus for NoopBus {
        fn emit(&self, _: &perima_core::AppEvent) -> Result<(), perima_core::CoreError> {
            Ok(())
        }
    }
    let writer_bus: Arc<dyn perima_core::events::EventBus> = Arc::new(NoopBus);

    let writer = SqliteWriter::start(db_path, writer_bus)?;
    let reads = ReadPool::open(db_path)?;

    // WHY clone `reads` for each adapter: `ReadPool` is cheap to
    // [`Clone`] (inner `r2d2::Pool` is `Arc`-backed).
    let files: Arc<dyn FileRepository> =
        Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
    let volumes: Arc<dyn VolumeRepository> =
        Arc::new(SqliteVolumeRepository::new(writer.sender(), reads.clone()));
    let tags: Arc<dyn TagRepository> =
        Arc::new(SqliteTagRepository::new(writer.sender(), reads.clone()));
    let metadata: Arc<dyn MetadataRepository> = Arc::new(SqliteMetadataRepository::new(
        writer.sender(),
        reads.clone(),
    ));
    let search: Arc<dyn SearchRepository> =
        Arc::new(SqliteSearchRepository::new(writer.sender(), reads));
    let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
    let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());
    // WHY no explicit `writer` keep-alive: every adapter above holds a
    // cloned `flume::Sender<WriteCmd>` via `writer.sender()`. When
    // `build_container` returns, the local `writer` handle drops, but the
    // sender clones inside the repos keep the writer thread running for
    // the container's lifetime. At CLI process exit, all senders drop and
    // the thread observes `Disconnected` + returns.

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
    let log_handler: Box<dyn EventHandler> = Box::new(perima_app::LogEventHandler);
    let mut handlers: Vec<Box<dyn EventHandler>> = vec![log_handler];
    handlers.extend(extra_handlers);
    Ok(AppContainer::new(deps, handlers))
}

/// Build a `DbEventHandler` for the `watch` command, boxed as `Box<dyn EventHandler>`.
///
/// WHY a dedicated helper: `dispatch_watch` must construct the handler
/// before calling `build_container` so it can be passed as an `extra_handler`.
/// Extracting it here keeps `dispatch_watch` focused on control-flow and makes
/// the single-connection justification easy to find.
///
/// WHY a fresh writer+pool pair here: `dispatch_watch` builds the
/// `DbEventHandler` BEFORE calling `build_container`, so the handler's
/// `SqliteFileRepository` must own its own sender. Both senders ride
/// into the Bus via `extra_handlers` and the container respectively —
/// the writer thread keeps running while any sender lives.
fn build_watch_db_handler(
    db_path: &Path,
    device_id: perima_core::DeviceId,
) -> Result<Box<dyn EventHandler>, perima_core::CoreError> {
    struct NoopBus;
    impl perima_core::events::EventBus for NoopBus {
        fn emit(&self, _: &perima_core::AppEvent) -> Result<(), perima_core::CoreError> {
            Ok(())
        }
    }
    let writer = SqliteWriter::start(
        db_path,
        Arc::new(NoopBus) as Arc<dyn perima_core::events::EventBus>,
    )?;
    let reads = ReadPool::open(db_path)?;
    let file_repo = Arc::new(SqliteFileRepository::new(writer.sender(), reads));
    // WHY writer handle dropped here: the sender inside `file_repo` keeps
    // the writer thread alive; the extra handle is not needed for join.
    drop(writer);
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
    // `SqliteFileRepository` backed by a dedicated writer+pool pair.
    //
    // WHY a separate writer for the sentinel (not sharing container's
    // writer): `migrate_sentinel_row` is an inherent method on the
    // concrete `SqliteFileRepository`, not on the `FileRepository` trait;
    // calling it requires an `Arc<SqliteFileRepository>`, not
    // `Arc<dyn FileRepository>`. Constructing a fresh writer+pool pair
    // here is cheap (WAL mode; migrations already ran) and is the only
    // path that avoids either widening the port trait or leaking concrete
    // types through the container.
    let on_persist: cmd::scan::OnPersistFactory = if dry_run {
        None
    } else {
        struct NoopBus;
        impl perima_core::events::EventBus for NoopBus {
            fn emit(&self, _: &perima_core::AppEvent) -> Result<(), perima_core::CoreError> {
                Ok(())
            }
        }
        let sentinel_writer = match SqliteWriter::start(
            &db_path,
            Arc::new(NoopBus) as Arc<dyn perima_core::events::EventBus>,
        ) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("perima: database (sentinel migration): {e}");
                return ExitCode::from(1);
            }
        };
        let sentinel_reads = match ReadPool::open(&db_path) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("perima: database (sentinel pool): {e}");
                return ExitCode::from(1);
            }
        };
        let sentinel_repo = Arc::new(SqliteFileRepository::new(
            sentinel_writer.sender(),
            sentinel_reads,
        ));
        // WHY drop the handle: the sender inside sentinel_repo keeps the
        // thread alive; the handle is not needed for join.
        drop(sentinel_writer);
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
    // Constructing it here (before AppContainer::new) keeps Bus
    // construction in exactly one place — Batch E spec §2.1 single-
    // construction-site invariant.
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
    // WHY build_container here: `cmd::metadata::run` now consumes
    // `AppContainer.volumes` for `find_or_create` (Batch C Task 2).
    let db_path = config.data_dir.join("perima.db");
    let container = match build_container(&db_path, vec![]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perima: database: {e}");
            return ExitCode::from(1);
        }
    };
    match cmd::metadata::run(&container, &config.data_dir, config.device_id, &args).await {
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
