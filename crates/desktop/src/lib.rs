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
use tauri::Manager;
use tauri_specta::{Builder, collect_commands};

use crate::commands::{DbEventHandler, LogEventHandler};
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

            // WHY eager open + Arc-wrap: `SqliteMetadataRepository` holds a
            // `Mutex<Connection>` and is deliberately shared — commands
            // clone the same `Arc` into the background `MetadataQueue`
            // worker during scans. Running `open_and_migrate` here
            // guarantees V001..V005 migrations run before the first
            // command fires; WAL mode makes later re-opens free.
            let db_path = cfg.data_dir.join("perima.db");
            let metadata_conn = open_and_migrate(&db_path)?;
            let metadata_repo = Arc::new(SqliteMetadataRepository::new(metadata_conn));

            // WHY second open: `SqliteTagRepository` also holds a
            // `Mutex<Connection>`. Under WAL mode the second open is
            // instant (no migration work — V005 already ran above).
            // Separating the two connections avoids cross-locking the
            // metadata and tag Mutexes on every tag command.
            let tag_conn = open_and_migrate(&db_path)?;
            let tag_repo = Arc::new(SqliteTagRepository::new(tag_conn));

            // WHY third open: `SqliteSearchRepository` needs its own
            // `Mutex<Connection>`. Under WAL mode concurrent readers are
            // never blocked by writers, so the extra handle is free.
            let search_conn = open_and_migrate(&db_path)?;
            let search_repo = Arc::new(SqliteSearchRepository::new(search_conn));

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

            let container = build_container(
                &db_path,
                Arc::clone(&metadata_repo),
                Arc::clone(&tag_repo),
                Arc::clone(&search_repo),
                vec![log_handler, db_handler, tauri_emitter],
            )?;

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
fn build_container(
    db_path: &Path,
    metadata_repo: Arc<SqliteMetadataRepository>,
    tag_repo: Arc<SqliteTagRepository>,
    search_repo: Arc<SqliteSearchRepository>,
    handlers: Vec<Arc<dyn EventBus>>,
) -> Result<Arc<AppContainer>, perima_core::CoreError> {
    // WHY open fresh connections for files / volumes: the existing
    // `metadata_repo`, `tag_repo`, and `search_repo` Arcs wrap per-purpose
    // `Mutex<Connection>` handles that we deliberately share with the
    // legacy `AppState` fields. The `AppContainer` still needs its own
    // `FileRepository` + `VolumeRepository` handles — both absent from
    // the pre-Batch-B `AppState` surface. Under WAL mode two extra opens
    // cost a directory-stat; the writer-actor in Batch C consolidates
    // this to a single writer + read-pool and removes the multi-open
    // pattern entirely.
    let files: Arc<dyn FileRepository> =
        Arc::new(SqliteFileRepository::new(open_and_migrate(db_path)?));
    let volumes: Arc<dyn VolumeRepository> =
        Arc::new(SqliteVolumeRepository::new(open_and_migrate(db_path)?));
    let tags: Arc<dyn TagRepository> = tag_repo;
    let metadata: Arc<dyn MetadataRepository> = metadata_repo;
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

    Ok(AppContainer::new(deps, handlers))
}
