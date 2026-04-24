//! FTS5 trigger codegen — single source of truth for the 16 sync triggers.
//!
//! See `docs/superpowers/specs/2026-04-23-arch-audit-batch-F-fts-codegen-design.md`
//! for the design rationale + the V006→V007→V008 bug class this closes.
//!
//! Public surface:
//! - [`spec::FtsAggregation`] — one trigger entry.
//! - [`spec::FTS_AGGREGATIONS`] — the 16 entries.
//! - [`spec::LEGACY_TRIGGER_NAMES`] — historical names dropped but no longer created.
//! - [`render_fts_triggers`] — render the install body to a `String`.
//! - [`install_fts_triggers`] — execute the rendered SQL on a `Connection`.

pub mod spec;

pub use spec::{BodyKind, FTS_AGGREGATIONS, FtsAggregation, LEGACY_TRIGGER_NAMES, TriggerEvent};

use std::sync::OnceLock;

use minijinja::{Environment, context};
use rusqlite::Connection;

use perima_core::CoreError;

static TEMPLATE_SOURCE: &str = include_str!("templates/fts_triggers.sql.j2");

fn build_env() -> Environment<'static> {
    // WHY: `Environment::new` is `#[deprecated]` without the `serde` feature;
    // our pin disables default-features (workspace `Cargo.toml` + `crates/db`).
    let mut env = Environment::empty();
    env.add_template("fts_triggers.sql.j2", TEMPLATE_SOURCE)
        .expect("template registers");
    env
}

fn env() -> &'static Environment<'static> {
    static ENV: OnceLock<Environment<'static>> = OnceLock::new();
    ENV.get_or_init(build_env)
}

/// Render the full FTS-trigger install SQL: legacy `DROP`s + current
/// `DROP`s + 16 `CREATE TRIGGER` statements.
///
/// Pure; no I/O. Used by [`install_fts_triggers`] AND by snapshot tests.
#[must_use]
pub fn render_fts_triggers() -> String {
    render_internal(spec::FTS_AGGREGATIONS, spec::LEGACY_TRIGGER_NAMES)
}

/// Render a per-source-table subset (used by snapshot tests).
///
/// Filters [`spec::FTS_AGGREGATIONS`] by `source_table`, omits the prologue
/// `DROP` block. Useful for per-entity PR diffs.
#[cfg(test)]
pub(crate) fn render_for_source(source: &str) -> String {
    let aggs: Vec<_> = spec::FTS_AGGREGATIONS
        .iter()
        .filter(|a| a.source_table == source)
        .copied()
        .collect();
    render_internal(&aggs, &[])
}

fn render_internal(aggs: &[spec::FtsAggregation], legacy: &[&str]) -> String {
    let tmpl = env()
        .get_template("fts_triggers.sql.j2")
        .expect("template registered");
    // Serialise BodyKind to its variant name for in-template dispatch.
    let aggs_ctx: Vec<_> = aggs
        .iter()
        .map(|a| {
            context! {
                name => a.name,
                source_table => a.source_table,
                when => a.when,
                event => event_ctx(&a.event),
                body => body_kind_name(a.body),
            }
        })
        .collect();
    tmpl.render(context! {
        aggregations => aggs_ctx,
        legacy_trigger_names => legacy,
    })
    .expect("template render")
}

fn event_ctx(e: &spec::TriggerEvent) -> minijinja::Value {
    match e {
        spec::TriggerEvent::Insert => context! { kind => "Insert" },
        spec::TriggerEvent::Update => context! { kind => "Update" },
        spec::TriggerEvent::UpdateOf(col) => context! { kind => "UpdateOf", col => *col },
        spec::TriggerEvent::Delete => context! { kind => "Delete" },
    }
}

const fn body_kind_name(b: spec::BodyKind) -> &'static str {
    match b {
        spec::BodyKind::SearchContentAfterInsert => "SearchContentAfterInsert",
        spec::BodyKind::SearchContentAfterUpdate => "SearchContentAfterUpdate",
        spec::BodyKind::SearchContentAfterDelete => "SearchContentAfterDelete",
        spec::BodyKind::FileLocationsInsert => "FileLocationsInsert",
        spec::BodyKind::LocationHashChangeRetire => "LocationHashChangeRetire",
        spec::BodyKind::LocationHashChangeSeed => "LocationHashChangeSeed",
        spec::BodyKind::RenameRepresentative => "RenameRepresentative",
        spec::BodyKind::LocationSoftDelete => "LocationSoftDelete",
        spec::BodyKind::LocationRestore => "LocationRestore",
        spec::BodyKind::MetadataInsert => "MetadataInsert",
        spec::BodyKind::MetadataUpdate => "MetadataUpdate",
        spec::BodyKind::FileTagsInsert => "FileTagsInsert",
        spec::BodyKind::FileTagsUpdate => "FileTagsUpdate",
        spec::BodyKind::TagsNameUpdate => "TagsNameUpdate",
        spec::BodyKind::TagsSoftDeleteOrRestore => "TagsSoftDeleteOrRestore",
        spec::BodyKind::TagsDelete => "TagsDelete",
    }
}

/// Install the rendered FTS-trigger set on `conn`.
///
/// Idempotent: every `CREATE` is preceded by a `DROP IF EXISTS` for the same
/// name (and for every name in [`spec::LEGACY_TRIGGER_NAMES`]). Safe to call on
/// every writer init.
///
/// # Errors
///
/// Returns [`CoreError::Internal`] if `execute_batch` fails (e.g. malformed
/// generated SQL — programmer error, fix the template).
pub fn install_fts_triggers(conn: &Connection) -> Result<(), CoreError> {
    let sql = render_fts_triggers();
    conn.execute_batch(&sql)
        .map_err(|e| CoreError::Internal(format!("install_fts_triggers: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_search_content_sync_triggers() {
        insta::assert_snapshot!(
            "fts_search_content_sync",
            render_for_source("search_content")
        );
    }

    #[test]
    fn snapshot_file_locations_triggers() {
        insta::assert_snapshot!("fts_file_locations", render_for_source("file_locations"));
    }

    #[test]
    fn snapshot_file_metadata_triggers() {
        insta::assert_snapshot!("fts_file_metadata", render_for_source("file_metadata"));
    }

    #[test]
    fn snapshot_file_tags_triggers() {
        insta::assert_snapshot!("fts_file_tags", render_for_source("file_tags"));
    }

    #[test]
    fn snapshot_tags_triggers() {
        insta::assert_snapshot!("fts_tags", render_for_source("tags"));
    }

    #[test]
    fn render_includes_all_legacy_drops() {
        let sql = render_fts_triggers();
        for legacy in spec::LEGACY_TRIGGER_NAMES {
            assert!(
                sql.contains(&format!("DROP TRIGGER IF EXISTS {legacy};")),
                "render output missing legacy DROP for {legacy}"
            );
        }
    }
}
