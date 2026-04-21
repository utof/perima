//! Shell-agnostic observability handlers.
//!
//! WHY this module exists: `LogEventHandler` was duplicated across the
//! CLI (`crates/cli/src/cmd/watch.rs`) and Desktop
//! (`crates/desktop/src/commands.rs`) shells pre-Batch-B. Task 10 of the
//! Batch B plan hoists it alongside [`crate::AppContainer`] so both
//! shells (and future `api` / `ffi` shells) construct the container
//! uniformly — the canonical home for shell-agnostic event handlers.
//!
//! `DbEventHandler` is deliberately NOT hoisted here: it touches
//! `SqliteFileRepository` which lives in `crates/db`, so hoisting it
//! would force `perima-app` to depend on a concrete adapter. It stays
//! shell-local in both CLI and Desktop.

use perima_core::{CoreError, EventBus, FileEvent};

/// Logs every filesystem event at INFO level via `tracing`.
///
/// WHY `Default` + `Debug`: wire-up sites construct this with
/// `Arc::new(LogEventHandler)` today; derived `Default` keeps the
/// zero-field shape future-proof, and `Debug` makes the handler
/// printable inside `AppContainer` diagnostics without special-casing.
#[derive(Debug, Default)]
pub struct LogEventHandler;

impl EventBus for LogEventHandler {
    fn emit(&self, event: &FileEvent) -> Result<(), CoreError> {
        tracing::info!(?event, "file event");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use perima_core::{MediaPath, VolumeId};
    use uuid::Uuid;

    #[test]
    fn log_handler_never_errors_on_created() {
        let handler = LogEventHandler;
        let event = FileEvent::Created {
            path: MediaPath::new("foo.txt"),
            volume: VolumeId(Uuid::nil()),
        };
        assert!(handler.emit(&event).is_ok());
    }

    #[test]
    fn log_handler_never_errors_on_renamed() {
        // WHY `LogEventHandler` (not `::default()`): clippy's
        // `default_constructed_unit_structs` flags the latter on zero-field
        // structs. The derived `Default` still matters for symmetry with
        // future non-unit shapes and for `#[derive]` consumers.
        let handler = LogEventHandler;
        let event = FileEvent::Renamed {
            from: MediaPath::new("a.txt"),
            to: MediaPath::new("b.txt"),
            volume: VolumeId(Uuid::nil()),
        };
        assert!(handler.emit(&event).is_ok());
    }
}
