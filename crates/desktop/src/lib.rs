//! Tauri desktop backend for perima.
//!
//! Exposes `scan`, `list_files`, `list_files_with_metadata`,
//! `list_volumes`, `start_watch`, `stop_watch`, `is_watching`,
//! `list_tags`, `attach_tag`, `detach_tag`, `list_files_with_tags`,
//! `search`, and `search_rebuild` as Tauri IPC commands.
//!
//! `AppState` holds the resolved `Config` (data dir + device id), the
//! shared `Arc<AppContainer>` hub every migrated handler delegates to,
//! and transitional `Arc<Sqlite*Repository>` handles retained for
//! test-only `_inner` helpers (see `state.rs` WHY-blocks). `WatcherState`
//! holds the active [`perima_fs::DebouncedWatcher`] and its cancellation
//! token.

#![forbid(unsafe_code)]

pub mod commands;
pub mod config;
pub mod events;
pub mod payloads;
pub mod state;

use std::path::Path;
use std::sync::Arc;

use perima_app::{AppContainer, AppDeps, LogEventHandler};
use perima_core::{
    EventBus, FileRepository, HashService, MetadataRepository, Scanner, SearchRepository,
    TagRepository, VolumeRepository,
};
use perima_db::{
    ReadPool, SqliteFileRepository, SqliteMetadataRepository, SqliteSearchRepository,
    SqliteTagRepository, SqliteVolumeRepository, SqliteWriter, SqliteWriterHandle,
    open_and_migrate,
};
use perima_fs::WalkdirScanner;
use perima_hash::Blake3Service;
use perima_media::ThumbnailGenerator;
use tauri::Manager;
use tauri_specta::{Builder, collect_commands};

use crate::commands::DbEventHandler;
use crate::events::TauriEventEmitter;

/// Boxed error type used by [`run`].
///
/// WHY: the `run()` body assembles errors from three distinct origins —
/// `perima_core::CoreError` (config resolution), `specta_typescript`'s
/// export error (debug-only binding dump), and `tauri::Error` (event-loop
/// failure). A boxed trait object is the minimum-friction union that
/// accepts `?` from all three and matches what Tauri's own `.setup(...)`
/// callback uses, so there is no second conversion layer to maintain.
pub type RunError = Box<dyn std::error::Error + Send + Sync>;

/// Build and run the Tauri application.
///
/// Wires `AppState` and `WatcherState`, registers IPC commands, and starts
/// the event loop.
///
/// # Errors
/// Returns a [`RunError`] if config resolution fails, if TypeScript binding
/// export fails in debug builds, or if the Tauri event loop exits with an
/// error. No panic paths remain; all previously `.expect()`-ed sites now
/// propagate via `?`.
pub fn run() -> Result<(), RunError> {
    // WHY: tauri-specta Builder collects #[specta::specta]-annotated commands
    // and generates TypeScript bindings at build time (debug only). The invoke
    // handler is then wired into tauri::Builder so the frontend can call typed
    // `invoke("scan", ...)` etc.
    let specta_builder = Builder::<tauri::Wry>::new().commands(collect_commands![
        commands::scan,
        commands::list_files,
        commands::list_files_with_metadata,
        commands::list_volumes,
        commands::start_watch,
        commands::stop_watch,
        commands::is_watching,
        commands::list_tags,
        commands::attach_tag,
        commands::detach_tag,
        commands::list_files_with_tags,
        commands::search,
        commands::search_rebuild,
    ]);

    // Export TypeScript bindings in debug builds only.
    #[cfg(debug_assertions)]
    specta_builder.export(
        specta_typescript::Typescript::default(),
        "../../apps/desktop/src/bindings.ts",
    )?;

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state::WatcherState::new())
        .invoke_handler(specta_builder.invoke_handler())
        .setup(move |app| {
            specta_builder.mount_events(app);

            // WHY resolve config INSIDE .setup() (v0.4.3): the Tauri
            // `assetProtocol.scope` literal in `tauri.conf.json` is
            // `$APPDATA/perima/thumbnails/**`, where `$APPDATA`
            // resolves via the Tauri bundle identifier
            // (`dev.perima.desktop`). Prior versions resolved config
            // via the `directories` crate instead, producing a
            // different subtree — the scope never matched runtime
            // thumbnail paths and `convertFileSrc` silently 404ed
            // every thumbnail in the grid. `app.path().app_data_dir()`
            // is the single source of truth: every downstream path
            // (DB location, thumbnail root, device-id sidecar) is
            // derived from it so the scope literal matches end-to-end.
            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("resolve app_data_dir: {e}"))?;
            let cfg = config::resolve_with_app_data_dir(&app_data_dir)?;

            // WHY resolve db_path up-front: used for the writer actor,
            // the read pool, and the legacy search/file opens below.
            //
            // WHY new_legacy for search: Task 7 migrates this to
            // SqliteSearchRepository::new(writer, reads). Under WAL mode
            // concurrent readers are never blocked by the writer.
            let db_path = cfg.data_dir.join("perima.db");
            let search_conn = open_and_migrate(&db_path)?;
            #[allow(deprecated)]
            let search_repo = Arc::new(SqliteSearchRepository::new_legacy(search_conn));

            // WHY build the AppContainer here, not per-command: a single
            // container is reused across every Tauri command dispatch via
            // `manage(state)`. CLI builds one per dispatch (short-lived
            // process); Desktop builds once because the process is
            // long-running and re-opening the repo Arcs on every command
            // would defeat the shared `Mutex<Connection>` pattern that
            // `SqliteMetadataRepository` and friends rely on.
            //
            // WHY the `TauriEventEmitter` + `DbEventHandler` are wired
            // into `extra_handlers` at setup (rather than at `start_watch`
            // time): the single-bus-construction invariant (spec §4
            // acceptance) says exactly one `CompositeEventBus::new` call
            // in the codebase, inside `AppContainer::new`. The desktop
            // bus therefore needs every handler at container-build time.
            // `AppHandle` is available on `app.handle()` here, and a
            // dedicated `SqliteFileRepository` connection for the DB
            // handler is a cheap WAL re-open. When no watcher is active,
            // neither handler fires — events originate only from
            // `DebouncedWatcher`, which is only running while
            // `start_watch` has been invoked.
            let watch_file_conn = open_and_migrate(&db_path)?;
            let watch_file_repo = Arc::new(SqliteFileRepository::new(watch_file_conn));
            let db_handler: Arc<dyn EventBus> = Arc::new(DbEventHandler::new(
                Arc::clone(&watch_file_repo),
                cfg.device_id,
            ));
            let tauri_emitter: Arc<dyn EventBus> = Arc::new(TauriEventEmitter {
                app_handle: app.handle().clone(),
            });
            let log_handler: Arc<dyn EventBus> = Arc::new(LogEventHandler);

            let (container, writer_handle, tag_repo, metadata_repo) = build_container(
                &db_path,
                Arc::clone(&search_repo),
                vec![log_handler, db_handler, tauri_emitter],
            )?;

            // WHY `manage(writer_handle)`: the writer thread stays
            // alive as long as at least one `flume::Sender<WriteCmd>`
            // clone exists. Every sender today lives inside
            // `SqliteVolumeRepository` + `SqliteTagRepository` +
            // `SqliteMetadataRepository` on the container, which lives
            // inside `AppState` — all kept alive by `manage(state)`.
            // Storing the handle itself lets a future `shutdown`
            // command call `handle.join()` explicitly rather than
            // relying on drop order at process exit.
            app.manage(writer_handle);

            let app_state = state::AppState::new(
                cfg.data_dir,
                cfg.device_id,
                metadata_repo,
                tag_repo,
                search_repo,
                container,
            );
            app.manage(app_state);

            Ok(())
        })
        .run(tauri::generate_context!())?;
    Ok(())
}

/// Build the [`AppContainer`] that backs every migrated Tauri handler.
///
/// WHY a dedicated helper (mirrors `crates/cli/src/main.rs::build_container`):
/// keeps the shared-handle wiring (metadata / tag / search) and extra-handler
/// plumbing in one place so the Tauri `.setup` closure stays focused on
/// control-flow. Under WAL mode the extra per-repo opens below are cheap.
///
/// WHY `tag_repo` + `metadata_repo` are constructed here (not passed in
/// like search): post-Batch-C Tasks 3 + 4, both adapters require the
/// writer sender + read pool that this helper assembles. Returning the
/// `Arc<SqliteTagRepository>` + `Arc<SqliteMetadataRepository>`
/// alongside the container lets `AppState` retain its `_inner`
/// test-helper seam (same rationale as retaining `search_repo`).
fn build_container(
    db_path: &Path,
    search_repo: Arc<SqliteSearchRepository>,
    handlers: Vec<Arc<dyn EventBus>>,
) -> Result<
    (
        Arc<AppContainer>,
        SqliteWriterHandle,
        Arc<SqliteTagRepository>,
        Arc<SqliteMetadataRepository>,
    ),
    perima_core::CoreError,
> {
    // WHY hybrid state post-Batch-C Task 4: Volume + Tag + Metadata
    // migrated to writer+pool; File/Search still take an owned
    // `Connection`. Tasks 5-6 migrate the remaining two repos.
    //
    // WHY a `NoopBus` to the writer (Task 4): the writer's after-COMMIT
    // emission path is scaffolded but NO command emits events today —
    // none of volume / tag / metadata commands are on the `FileEvent`
    // bus surface. Tasks 5-6 re-plumb this to the container's event
    // bus once Batch E replaces `CompositeEventBus` with
    // `async-broadcast` (the current single-construction-site
    // invariant forbids a second composite here).
    struct NoopBus;
    impl EventBus for NoopBus {
        fn emit(&self, _: &perima_core::FileEvent) -> Result<(), perima_core::CoreError> {
            Ok(())
        }
    }
    let writer_bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(db_path, writer_bus)?;
    let reads = ReadPool::open(db_path)?;

    let files: Arc<dyn FileRepository> =
        Arc::new(SqliteFileRepository::new(open_and_migrate(db_path)?));
    // WHY clone `reads`: `ReadPool` is cheap to [`Clone`] (inner
    // `r2d2::Pool` is `Arc`-backed).
    let volumes: Arc<dyn VolumeRepository> =
        Arc::new(SqliteVolumeRepository::new(writer.sender(), reads.clone()));
    let tag_repo = Arc::new(SqliteTagRepository::new(writer.sender(), reads.clone()));
    let metadata_repo = Arc::new(SqliteMetadataRepository::new(writer.sender(), reads));
    // WHY explicit `Arc<dyn _>` bindings: `AppDeps::{tags,metadata}`
    // are `Arc<dyn _>`; assigning the cloned concrete-typed `Arc`s to
    // the typed locals triggers the unsize coercion.
    let tags: Arc<dyn TagRepository> = Arc::clone(&tag_repo);
    let metadata: Arc<dyn MetadataRepository> = Arc::clone(&metadata_repo);
    let search: Arc<dyn SearchRepository> = search_repo;
    let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
    let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());

    // WHY thumbnailer rooted at `data_dir` (db parent): the Tauri
    // asset-protocol scope (`tauri.conf.json`) exposes
    // `$APPDATA/perima/thumbnails/**`; resolving the generator to the
    // same directory tree keeps `convertFileSrc` calls from the
    // frontend working end-to-end. Matches the pre-Batch-B wiring in
    // `commands::run_scan_inner_with_metadata`.
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

    Ok((
        AppContainer::new(deps, handlers),
        writer,
        tag_repo,
        metadata_repo,
    ))
}
