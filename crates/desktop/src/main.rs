//! Desktop binary entry point.
//!
//! Thin wrapper around [`perima_desktop::run`] so `cargo build`'s `--bins`
//! filter (which `tauri build` uses) finds a target to build. The library
//! crate keeps its `cdylib`/`staticlib` shapes for future mobile shells.

fn main() {
    // WHY: `AppContainer::new` calls raw `tokio::spawn(...)` to start
    // EventBus handler tasks. `tokio::spawn` looks up the current runtime
    // via thread-local storage. Tauri 2's `setup(...)` callback runs on
    // the main thread but does NOT enter `tauri::async_runtime` into TLS
    // — Tauri-managed code uses `tauri::async_runtime::spawn` directly
    // via a `Handle`. Without an explicit TLS-entered runtime here,
    // `AppContainer::new` panics with "there is no reactor running".
    //
    // We build one multi-threaded runtime, register it as Tauri's
    // `async_runtime` so both layers share it (no duplicate worker pools),
    // and hold an `EnterGuard` for the lifetime of `main`.
    //
    // The proper fix is to make `AppContainer::new` accept an explicit
    // `tokio::runtime::Handle` parameter — tracked as a follow-up issue.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    tauri::async_runtime::set(runtime.handle().clone());
    let _guard = runtime.enter();

    if let Err(err) = perima_desktop::run() {
        // WHY allow(clippy::print_stderr): this is the terminal error path in a
        // binary entry point — writing to stderr before exit is the correct UX.
        // The CLI crate suppresses the same lint with the same rationale.
        #[allow(clippy::print_stderr)]
        {
            eprintln!("perima-desktop exited with error: {err:#}");
        }
        std::process::exit(1);
    }
}
