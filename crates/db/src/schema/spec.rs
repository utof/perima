//! Static spec for all FTS5 sync triggers. Edit this file + the template
//! to add or change a trigger; never edit the rendered SQL by hand.

/// Trigger event clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerEvent {
    /// `AFTER INSERT ON <table>`
    Insert,
    /// `AFTER UPDATE ON <table>`
    Update,
    /// `AFTER UPDATE OF <col> ON <table>`
    UpdateOf(&'static str),
    /// `AFTER DELETE ON <table>`
    Delete,
}

/// Which template macro composition to invoke for the body.
///
/// Each variant maps to a `{% macro body_<kind>() %}` block in
/// `templates/fts_triggers.sql.j2`. Adding a variant requires also adding
/// the matching macro; missing macro = template render error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    /// `sc_after_insert` — search_content → search_index sync on INSERT.
    SearchContentAfterInsert,
    /// `sc_after_update` — sync on UPDATE (delete + reinsert).
    SearchContentAfterUpdate,
    /// `sc_after_delete` — sync on DELETE.
    SearchContentAfterDelete,
    /// `search_after_file_locations_insert` — seed search_content from joined live state.
    FileLocationsInsert,
    /// `search_after_location_hash_change_retire` — DELETE OLD search_content row if no sibling.
    LocationHashChangeRetire,
    /// `search_after_location_hash_change_seed` — INSERT-OR-IGNORE + UPDATE NEW row from live state.
    LocationHashChangeSeed,
    /// `search_after_location_rename` — V007 trigger 2b. Consumes NEW.* directly
    /// (WHEN-guarded to representative). DO NOT replace with representative_path() macro.
    RenameRepresentative,
    /// `search_after_location_soft_delete` — repoint to surviving sibling or retire.
    LocationSoftDelete,
    /// `search_after_location_restore` — recreate from joined live state.
    LocationRestore,
    /// `search_after_metadata_insert` — UPSERT search_content row + refresh metadata cols.
    MetadataInsert,
    /// `search_after_metadata_update` — CASE on deleted_at to refresh-or-clear cols.
    MetadataUpdate,
    /// `search_after_file_tags_insert` — UPSERT row + refresh tags agg.
    FileTagsInsert,
    /// `search_after_file_tags_update` — refresh tags agg.
    FileTagsUpdate,
    /// `search_after_tags_name_update` — refresh tags agg for every holder of this tag.
    TagsNameUpdate,
    /// `search_after_tag_soft_delete_or_restore` — refresh tags agg for every holder.
    TagsSoftDeleteOrRestore,
    /// `search_after_tags_delete` — refresh tags agg for every holder (post-DELETE OLD.id).
    TagsDelete,
}

/// One FTS-trigger spec entry. Drives the template render loop.
#[derive(Debug, Clone, Copy)]
pub struct FtsAggregation {
    /// Trigger name (e.g. `search_after_file_tags_insert`).
    pub name: &'static str,
    /// Source table (e.g. `file_tags`).
    pub source_table: &'static str,
    /// Trigger event clause: `INSERT`, `UPDATE`, `UPDATE OF <col>`, `DELETE`.
    pub event: TriggerEvent,
    /// Optional `WHEN ...` clause (e.g. `OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL`).
    pub when: Option<&'static str>,
    /// Which body macro to invoke from the template.
    pub body: BodyKind,
}

/// Trigger names V006/V007/V008 created that are NOT in `FTS_AGGREGATIONS`.
/// Every install body DROPs these names so existing dev DBs converge.
///
/// Adding a name to `FTS_AGGREGATIONS` is fine; REMOVING a name from it
/// requires adding the removed name here so existing DBs converge.
pub const LEGACY_TRIGGER_NAMES: &[&str] = &[
    "search_after_location_hash_change", // V007; V008 split into _retire + _seed
];

/// The 16 trigger entries the codegen renders.
///
/// Order matters for fire-order on combined-transaction UPDATE statements
/// (SQLite fires triggers in CREATE order). See spec §7.1 + V007 inline
/// comment "fire-order: 2a, 2b, 2c".
pub const FTS_AGGREGATIONS: &[FtsAggregation] = &[
    // search_content → search_index sync (V007).
    FtsAggregation {
        name: "sc_after_insert",
        source_table: "search_content",
        event: TriggerEvent::Insert,
        when: None,
        body: BodyKind::SearchContentAfterInsert,
    },
    FtsAggregation {
        name: "sc_after_update",
        source_table: "search_content",
        event: TriggerEvent::Update,
        when: None,
        body: BodyKind::SearchContentAfterUpdate,
    },
    FtsAggregation {
        name: "sc_after_delete",
        source_table: "search_content",
        event: TriggerEvent::Delete,
        when: None,
        body: BodyKind::SearchContentAfterDelete,
    },
    // file_locations triggers (V007/V008).
    FtsAggregation {
        name: "search_after_file_locations_insert",
        source_table: "file_locations",
        event: TriggerEvent::Insert,
        when: Some("NEW.deleted_at IS NULL"),
        body: BodyKind::FileLocationsInsert,
    },
    FtsAggregation {
        name: "search_after_location_hash_change_retire",
        source_table: "file_locations",
        event: TriggerEvent::UpdateOf("blake3_hash"),
        when: Some("OLD.blake3_hash != NEW.blake3_hash"),
        body: BodyKind::LocationHashChangeRetire,
    },
    FtsAggregation {
        name: "search_after_location_hash_change_seed",
        source_table: "file_locations",
        event: TriggerEvent::UpdateOf("blake3_hash"),
        when: Some("OLD.blake3_hash != NEW.blake3_hash AND NEW.deleted_at IS NULL"),
        body: BodyKind::LocationHashChangeSeed,
    },
    FtsAggregation {
        name: "search_after_location_rename",
        source_table: "file_locations",
        event: TriggerEvent::UpdateOf("relative_path"),
        when: Some(
            "OLD.relative_path != NEW.relative_path \
             AND NEW.deleted_at IS NULL \
             AND NEW.id = (SELECT id FROM file_locations \
                           WHERE blake3_hash = NEW.blake3_hash AND deleted_at IS NULL \
                           ORDER BY first_seen ASC, id ASC LIMIT 1)",
        ),
        body: BodyKind::RenameRepresentative,
    },
    FtsAggregation {
        name: "search_after_location_soft_delete",
        source_table: "file_locations",
        event: TriggerEvent::UpdateOf("deleted_at"),
        when: Some("OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL"),
        body: BodyKind::LocationSoftDelete,
    },
    FtsAggregation {
        name: "search_after_location_restore",
        source_table: "file_locations",
        event: TriggerEvent::UpdateOf("deleted_at"),
        when: Some("OLD.deleted_at IS NOT NULL AND NEW.deleted_at IS NULL"),
        body: BodyKind::LocationRestore,
    },
    // file_metadata triggers (V008).
    FtsAggregation {
        name: "search_after_metadata_insert",
        source_table: "file_metadata",
        event: TriggerEvent::Insert,
        when: Some("NEW.deleted_at IS NULL"),
        body: BodyKind::MetadataInsert,
    },
    FtsAggregation {
        name: "search_after_metadata_update",
        source_table: "file_metadata",
        event: TriggerEvent::Update,
        when: None,
        body: BodyKind::MetadataUpdate,
    },
    // file_tags triggers (V008).
    FtsAggregation {
        name: "search_after_file_tags_insert",
        source_table: "file_tags",
        event: TriggerEvent::Insert,
        when: Some("NEW.deleted_at IS NULL"),
        body: BodyKind::FileTagsInsert,
    },
    FtsAggregation {
        name: "search_after_file_tags_update",
        source_table: "file_tags",
        event: TriggerEvent::Update,
        when: None,
        body: BodyKind::FileTagsUpdate,
    },
    // tags triggers (V008).
    FtsAggregation {
        name: "search_after_tags_name_update",
        source_table: "tags",
        event: TriggerEvent::UpdateOf("name"),
        when: Some("OLD.name != NEW.name"),
        body: BodyKind::TagsNameUpdate,
    },
    FtsAggregation {
        name: "search_after_tag_soft_delete_or_restore",
        source_table: "tags",
        event: TriggerEvent::UpdateOf("deleted_at"),
        when: Some("(OLD.deleted_at IS NULL) != (NEW.deleted_at IS NULL)"),
        body: BodyKind::TagsSoftDeleteOrRestore,
    },
    FtsAggregation {
        name: "search_after_tags_delete",
        source_table: "tags",
        event: TriggerEvent::Delete,
        when: None,
        body: BodyKind::TagsDelete,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_aggregations_has_sixteen_entries() {
        assert_eq!(
            FTS_AGGREGATIONS.len(),
            16,
            "expected 16 trigger entries — see spec §7.1"
        );
    }

    #[test]
    fn legacy_trigger_names_includes_v007_hash_change() {
        assert!(
            LEGACY_TRIGGER_NAMES.contains(&"search_after_location_hash_change"),
            "V007's pre-split name must remain in LEGACY for existing-DB convergence"
        );
    }

    #[test]
    fn no_overlap_between_legacy_and_current() {
        let current: std::collections::HashSet<_> =
            FTS_AGGREGATIONS.iter().map(|a| a.name).collect();
        for legacy in LEGACY_TRIGGER_NAMES {
            assert!(
                !current.contains(legacy),
                "{legacy:?} appears in BOTH LEGACY and FTS_AGGREGATIONS — pick one"
            );
        }
    }
}
