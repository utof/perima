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

/// Build and run the Tauri application.
///
/// Wires `AppState` and `WatcherState`, registers IPC commands, and starts
/// the event loop.
///
/// # Panics
/// Panics if platform directories cannot be resolved (only occurs on systems
/// where `directories::ProjectDirs::from` returns `None`, which is highly
/// unusual in production). This is a fatal misconfiguration; propagating it
/// as a `Result` would not help since the app cannot run without a data dir.
///
/// # Errors
/// Returns a [`tauri::Error`] if the app fails to initialize or the event
/// loop exits with an error.
pub fn run() -> Result<(), tauri::Error> {
    let cfg = config::resolve_config().expect("failed to resolve perima config");

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
    specta_builder
        .export(
            specta_typescript::Typescript::default(),
            "../../apps/desktop/src/bindings.ts",
        )
        .expect("failed to export TypeScript bindings");

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
        .manage(state::WatcherState::new())
        .invoke_handler(specta_builder.invoke_handler())
        .setup(move |app| {
            specta_builder.mount_events(app);
            Ok(())
        })
        .run(tauri::generate_context!())
}
