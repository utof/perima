//! Tauri desktop backend for perima.
//!
//! Exposes `scan`, `list_files`, `list_files_with_metadata`,
//! `list_volumes`, `start_watch`, `stop_watch`, `is_watching`,
//! `list_tags`, `attach_tag`, `detach_tag`, and `list_files_with_tags`
//! as Tauri IPC commands.
//! `AppState` holds the resolved `Config` (data dir + device id) plus
//! shared `Arc<SqliteMetadataRepository>` and `Arc<SqliteTagRepository>`
//! handles, injected into every command via `tauri::State`.
//! `WatcherState` holds the active [`perima_fs::DebouncedWatcher`] and its
//! cancellation token.

pub mod commands;
pub mod config;
pub mod events;
pub mod payloads;
pub mod state;

use std::sync::Arc;

use perima_db::{SqliteMetadataRepository, SqliteTagRepository, open_and_migrate};
use tauri::Manager;
use tauri_specta::{Builder, collect_commands};

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

            let app_state =
                state::AppState::new(cfg.data_dir, cfg.device_id, metadata_repo, tag_repo);
            app.manage(app_state);

            Ok(())
        })
        .run(tauri::generate_context!())?;
    Ok(())
}
