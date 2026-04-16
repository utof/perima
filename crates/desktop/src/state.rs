//! Shared application state injected into every Tauri command.

use std::path::PathBuf;

use perima_core::DeviceId;

/// State shared across all Tauri commands via `tauri::State<AppState>`.
///
/// WHY: only `data_dir` and `device_id` are held here — no DB connection.
/// Each command opens its own connection to avoid lifetime / `Send` issues
/// with Tauri's async command system. Under `SQLite` WAL mode the second
/// open is instant because migrations already ran.
pub struct AppState {
    /// Resolved data directory (where `perima.db` lives).
    pub data_dir: PathBuf,
    /// Stable device identifier.
    pub device_id: DeviceId,
}
