//! Tauri desktop backend for perima.
//!
//! Exposes `scan`, `list_files`, `list_volumes`, `start_watch`, `stop_watch`,
//! and `is_watching` as Tauri IPC commands.
//! `AppState` holds the resolved `Config` (data dir + device id) and is
//! injected into every command via `tauri::State`.
//! `WatcherState` holds the active [`perima_fs::DebouncedWatcher`] and its
//! cancellation token.

pub mod commands;
pub mod config;
pub mod events;
pub mod state;

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
    let cfg = config::resolve_config()?;

    let app_state = state::AppState {
        data_dir: cfg.data_dir,
        device_id: cfg.device_id,
    };

    // WHY: tauri-specta Builder collects #[specta::specta]-annotated commands
    // and generates TypeScript bindings at build time (debug only). The invoke
    // handler is then wired into tauri::Builder so the frontend can call typed
    // `invoke("scan", ...)` etc.
    let specta_builder = Builder::<tauri::Wry>::new().commands(collect_commands![
        commands::scan,
        commands::list_files,
        commands::list_volumes,
        commands::start_watch,
        commands::stop_watch,
        commands::is_watching,
    ]);

    // Export TypeScript bindings in debug builds only.
    #[cfg(debug_assertions)]
    specta_builder.export(
        specta_typescript::Typescript::default(),
        "../../apps/desktop/src/bindings.ts",
    )?;

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
        .manage(state::WatcherState::new())
        .invoke_handler(specta_builder.invoke_handler())
        .setup(move |app| {
            specta_builder.mount_events(app);
            Ok(())
        })
        .run(tauri::generate_context!())?;
    Ok(())
}
