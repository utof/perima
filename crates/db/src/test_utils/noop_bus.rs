//! Shared test-utility bus that does nothing on emit.
//!
//! WHY shared: pre-Batch-E the same struct was duplicated 9 times across
//! `crates/db/src/{file_repo,tag_repo,metadata_repo,search_repo,
//! volume_repo}.rs`, `writer/mod.rs`, and 3 test files. Batch E
//! consolidates into one canonical impl.

use perima_core::{AppEvent, CoreError, EventBus};

/// Minimal `EventBus` impl that drops every event. Used by writer tests
/// + repo tests that don't care about event observability.
#[derive(Debug, Default)]
pub struct NoopBus;

impl EventBus for NoopBus {
    fn emit(&self, _event: &AppEvent) -> Result<(), CoreError> {
        Ok(())
    }
}
