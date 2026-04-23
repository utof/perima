//! FTS5 trigger codegen — single source of truth for the 16 sync triggers.
//!
//! See `docs/superpowers/specs/2026-04-23-arch-audit-batch-F-fts-codegen-design.md`
//! for the design rationale + the V006→V007→V008 bug class this closes.
//!
//! Public surface:
//! - [`spec::FtsAggregation`] — one trigger entry.
//! - [`spec::FTS_AGGREGATIONS`] — the 16 entries.
//! - [`spec::LEGACY_TRIGGER_NAMES`] — historical names dropped but no longer created.
//! - `render_fts_triggers` — render the install body to a `String` (added in Task 4).
//! - `install_fts_triggers` — execute the rendered SQL on a `Connection` (added in Task 4).

pub mod spec;

pub use spec::{BodyKind, FTS_AGGREGATIONS, FtsAggregation, LEGACY_TRIGGER_NAMES, TriggerEvent};

// render_fts_triggers + install_fts_triggers added in Task 4.
