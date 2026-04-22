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

use perima_core::{AppEvent, CoreError, EventBus};

/// Logs every application event at INFO level via `tracing`.
///
/// WHY `Default` + `Debug`: wire-up sites construct this with
/// `Arc::new(LogEventHandler)` today; derived `Default` keeps the
/// zero-field shape future-proof, and `Debug` makes the handler
/// printable inside `AppContainer` diagnostics without special-casing.
#[derive(Debug, Default)]
pub struct LogEventHandler;

impl EventBus for LogEventHandler {
    fn emit(&self, event: &AppEvent) -> Result<(), CoreError> {
        match event {
            AppEvent::File(file_event) => {
                tracing::info!(event = ?file_event, "file event");
            }
            AppEvent::ScanCompleted {
                volume,
                files_new,
                files_seen,
                duration_ms,
            } => {
                tracing::info!(
                    ?volume,
                    files_new,
                    files_seen,
                    duration_ms,
                    "scan completed"
                );
            }
            AppEvent::IndexInvalidated { reason } => {
                tracing::info!(?reason, "index invalidated");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use perima_core::{FileEvent, InvalidationReason, MediaPath, VolumeId};
    use uuid::Uuid;

    #[test]
    fn log_handler_never_errors_on_file_created() {
        let handler = LogEventHandler;
        let event = AppEvent::File(FileEvent::Created {
            path: MediaPath::new("foo.txt"),
            volume: VolumeId(Uuid::nil()),
        });
        assert!(handler.emit(&event).is_ok());
    }

    #[test]
    fn log_handler_never_errors_on_file_renamed() {
        // WHY `LogEventHandler` (not `::default()`): clippy's
        // `default_constructed_unit_structs` flags the latter on zero-field
        // structs. The derived `Default` still matters for symmetry with
        // future non-unit shapes and for `#[derive]` consumers.
        let handler = LogEventHandler;
        let event = AppEvent::File(FileEvent::Renamed {
            from: MediaPath::new("a.txt"),
            to: MediaPath::new("b.txt"),
            volume: VolumeId(Uuid::nil()),
        });
        assert!(handler.emit(&event).is_ok());
    }

    #[test]
    fn log_handler_never_errors_on_scan_completed() {
        let handler = LogEventHandler;
        let event = AppEvent::ScanCompleted {
            volume: VolumeId(Uuid::nil()),
            files_seen: 10,
            files_new: 3,
            duration_ms: 500,
        };
        assert!(handler.emit(&event).is_ok());
    }

    #[test]
    fn log_handler_never_errors_on_index_invalidated() {
        let handler = LogEventHandler;
        let event = AppEvent::IndexInvalidated {
            reason: InvalidationReason::TagsChanged,
        };
        assert!(handler.emit(&event).is_ok());
    }
}
