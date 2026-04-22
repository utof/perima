//! Sanity check that the tauri-specta builder produces a parseable
//! TypeScript export containing the expected IPC type graph.
//!
//! WHY a separate integration test (not relying on `lib.rs::run`'s
//! `#[cfg(feature = "specta-export")]` path): this isolates the
//! "derives compile cleanly + exporter doesn't choke on our type
//! graph" assertion from the full Tauri runtime initialization.
//! Catches `specta::Type` derive failures separately from `cargo build`
//! and surfaces a meaningful diagnostic when a new core type was added
//! but its `specta` derive forgot the feature gate or some required
//! attribute.
//!
//! WHY `tempfile` (not `export_str`): `tauri-specta =2.0.0-rc.24` only
//! exposes `Builder::export(language, path)` — the `export_str` method
//! visible in rc.21 docs was removed before rc.24. We write to a
//! `NamedTempFile` and read back to string; the file is deleted when
//! the `NamedTempFile` handle drops at end-of-test.

use specta_typescript::Typescript;
use tauri_specta::{Builder, collect_commands};

use perima_desktop::commands;

/// Builds the same 13-command tauri-specta `Builder` that `lib.rs::run`
/// constructs. Centralised so future handler renames or additions are
/// caught loudly at compile time in exactly one place.
fn build_test_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![
        commands::scan,
        commands::list_files,
        commands::list_files_with_metadata,
        commands::list_volumes,
        commands::start_watch,
        commands::stop_watch,
        commands::is_watching,
        commands::list_tags,
        commands::attach_tag,
        commands::detach_tag,
        commands::list_files_with_tags,
        commands::search,
        commands::search_rebuild,
    ])
}

/// Verifies that the tauri-specta builder accepts all 13 IPC commands
/// and produces a non-empty TypeScript file containing every core
/// domain type that crosses the IPC boundary post-Batch-D.
///
/// Coverage rationale: `CoreError` (Result error on every handler);
/// `ScanReport` (scan handler return); `FileLocationRecord` /
/// `VolumeRecord` / `Tag` / `SearchHit` (handler returns); `FileEvent`
/// (emitted via `TauriEventEmitter`); `BlakeHash` (transitive field
/// of `FileLocationRecord`); composite payloads `FileWithMetadataPayload`
/// + `FileWithTagsPayload` (retained per spec §8 #6).
#[test]
fn tauri_specta_builder_exports_full_ipc_type_graph() {
    let tmp = tempfile::NamedTempFile::new().expect("create tempfile for bindings export");
    build_test_builder()
        .export(Typescript::default(), tmp.path())
        .expect("specta TypeScript export to tempfile must not fail");

    let ts = std::fs::read_to_string(tmp.path()).expect("read generated TypeScript from tempfile");

    assert!(
        !ts.is_empty(),
        "generated TypeScript bindings must be non-empty"
    );

    // Core types on the wire: error variant + handler returns + emitted
    // event payload + transitive fields.
    for ty in [
        "CoreError",
        "ScanReport",
        "FileLocationRecord",
        "VolumeRecord",
        "SearchHit",
        "FileEvent",
        "BlakeHash",
    ] {
        assert!(ts.contains(ty), "{ty} missing from bindings");
    }

    // WHY tightened "Tag" check: bare substring matches `TagOutput`,
    // `Tagged`, `FileWithTagsPayload` etc. — pin to a top-level TS
    // declaration boundary so a missing core `Tag` re-export is caught.
    assert!(
        ts.contains("type Tag ") || ts.contains("type Tag\n") || ts.contains("interface Tag "),
        "Tag missing from bindings (top-level declaration)"
    );

    // Composite payloads retained (deliberate flat composites with no
    // clean 1:1 core analogue, per spec §8 #6).
    for ty in ["FileWithMetadataPayload", "FileWithTagsPayload"] {
        assert!(ts.contains(ty), "{ty} missing from bindings");
    }
}
