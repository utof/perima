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

            // WHY resolve db_path up-front: used for the watch writer
            // (DbEventHandler) and for build_container (primary writer).
            let db_path = cfg.data_dir.join("perima.db");

            // WHY build the AppContainer here, not per-command: a single
            // container is reused across every Tauri command dispatch via
            // `manage(state)`. CLI builds one per dispatch (short-lived
            // process); Desktop builds once because the process is
            // long-running.
            //
            // WHY a dedicated watch writer for DbEventHandler: the
            // `DbEventHandler` holds `Arc<SqliteFileRepository>`, which
            // requires a concrete (not trait-object) type. The handler
            // must be constructed BEFORE `build_container` so it can be
            // passed as an extra_handler into `AppContainer::new` — the
            // single CompositeEventBus construction site. Opening a
            // separate writer+pool pair here is cheap under WAL mode;
            // SQLite WAL serialises concurrent writers at the OS level.
            //
            // WHY the `TauriEventEmitter` + `DbEventHandler` are wired
            // at setup (not at `start_watch` time): the single-bus-
            // construction invariant (spec §4) says exactly one
            // `CompositeEventBus::new` call in the codebase, inside
            // `AppContainer::new`. When no watcher is active neither
            // handler fires — events originate only from
            // `DebouncedWatcher` which only runs while `start_watch`
            // has been invoked.
            struct WatchNoopBus;
            impl EventBus for WatchNoopBus {
                fn emit(&self, _: &perima_core::FileEvent) -> Result<(), perima_core::CoreError> {
                    Ok(())
                }
            }
            let watch_writer =
                SqliteWriter::start(&db_path, Arc::new(WatchNoopBus) as Arc<dyn EventBus>)
                    .map_err(|e| format!("watch writer: {e}"))?;
            let watch_reads = ReadPool::open(&db_path).map_err(|e| format!("watch pool: {e}"))?;
            let watch_file_repo = Arc::new(SqliteFileRepository::new(
                watch_writer.sender(),
                watch_reads,
            ));
            let db_handler: Arc<dyn EventBus> = Arc::new(DbEventHandler::new(
                Arc::clone(&watch_file_repo),
                cfg.device_id,
            ));
            let tauri_emitter: Arc<dyn EventBus> = Arc::new(TauriEventEmitter {
                app_handle: app.handle().clone(),
            });
            let log_handler: Arc<dyn EventBus> = Arc::new(LogEventHandler);

            let (container, writer_handle, tag_repo, metadata_repo, search_repo) =
                build_container(&db_path, vec![log_handler, db_handler, tauri_emitter])?;

            // WHY `manage(writer_handle)`: the writer thread stays
            // alive as long as at least one `flume::Sender<WriteCmd>`
            // clone exists. Every sender lives inside the repo adapters
            // on the container; storing the handle lets a future
            // `shutdown` command call `handle.join()` explicitly.
            // WHY NOT manage `watch_writer`: the watch writer's sender
            // lives inside `watch_file_repo` → `DbEventHandler` → the
            // composite bus on the container — kept alive by
            // `manage(app_state)`. The handle is intentionally dropped
            // here; thread reaps when all senders drop at process exit.
            drop(watch_writer);
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
/// keeps the shared-handle wiring and extra-handler plumbing in one place so
/// the Tauri `.setup` closure stays focused on control-flow.
///
/// WHY `tag_repo` + `metadata_repo` + `search_repo` are returned: `AppState`
/// retains concrete-typed `Arc`s for the `_inner` test-helper seam — those
/// helpers construct their own repos per-call and need the concrete type for
/// methods not exposed by the trait object.
fn build_container(
    db_path: &Path,
    handlers: Vec<Arc<dyn EventBus>>,
) -> Result<
    (
        Arc<AppContainer>,
        SqliteWriterHandle,
        Arc<SqliteTagRepository>,
        Arc<SqliteMetadataRepository>,
        Arc<SqliteSearchRepository>,
    ),
    perima_core::CoreError,
> {
    // WHY a `NoopBus` to the writer: the writer's after-COMMIT emission
    // path is scaffolded but file-event emission is handled by the
    // composite bus wired into `AppContainer`. Batch E's `async-broadcast`
    // will re-plumb this once the single-construction-site invariant is
    // relaxed. Spec §§3.3 + 4.8 (A4.8 first bullet).
    struct NoopBus;
    impl EventBus for NoopBus {
        fn emit(&self, _: &perima_core::FileEvent) -> Result<(), perima_core::CoreError> {
            Ok(())
        }
    }
    let writer_bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(db_path, writer_bus)?;
    let reads = ReadPool::open(db_path)?;

    // WHY clone `reads` for each adapter: `ReadPool` is cheap to
    // [`Clone`] (inner `r2d2::Pool` is `Arc`-backed).
    let files: Arc<dyn FileRepository> =
        Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
    let volumes: Arc<dyn VolumeRepository> =
        Arc::new(SqliteVolumeRepository::new(writer.sender(), reads.clone()));
    let tag_repo = Arc::new(SqliteTagRepository::new(writer.sender(), reads.clone()));
    let metadata_repo = Arc::new(SqliteMetadataRepository::new(
        writer.sender(),
        reads.clone(),
    ));
    let search_repo = Arc::new(SqliteSearchRepository::new(writer.sender(), reads));
    // WHY explicit `Arc<dyn _>` bindings: `AppDeps::{tags,metadata,search}`
    // are `Arc<dyn _>`; assigning the cloned concrete-typed `Arc`s to
    // the typed locals triggers the unsize coercion.
    let tags: Arc<dyn TagRepository> = Arc::clone(&tag_repo);
    let metadata: Arc<dyn MetadataRepository> = Arc::clone(&metadata_repo);
    let search: Arc<dyn SearchRepository> = Arc::clone(&search_repo);
    let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
    let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());

    // WHY thumbnailer rooted at `data_dir` (db parent): the Tauri
    // asset-protocol scope (`tauri.conf.json`) exposes
    // `$APPDATA/perima/thumbnails/**`; resolving the generator to the
    // same directory tree keeps `convertFileSrc` calls from the
    // frontend working end-to-end.
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
        search_repo,
    ))
}
