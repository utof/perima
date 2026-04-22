//! Sanity check that the tauri-specta builder produces a parseable
//! TypeScript export containing the expected IPC type graph.
//!
//! WHY a separate integration test (not relying on `lib.rs::run`'s
//! `#[cfg(debug_assertions)]` path): this isolates the
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

/// Verifies that the tauri-specta builder accepts all 13 IPC commands
/// and produces a non-empty TypeScript file without panicking or
/// erroring. This is the baseline: derives compile, exporter succeeds.
#[test]
fn tauri_specta_builder_exports_to_string_without_error() {
    let builder = Builder::<tauri::Wry>::new().commands(collect_commands![
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
    ]);

    let tmp = tempfile::NamedTempFile::new().expect("create tempfile for bindings export");
    builder
        .export(Typescript::default(), tmp.path())
        .expect("specta TypeScript export to tempfile must not fail");

    let ts = std::fs::read_to_string(tmp.path()).expect("read generated TypeScript from tempfile");

    assert!(
        !ts.is_empty(),
        "generated TypeScript bindings must be non-empty"
    );

    // Mirror types present in the current (pre-Task-8) handler signatures.
    // These are wire-wrapper structs in crates/desktop/src/commands.rs
    // and crates/desktop/src/payloads.rs, each carrying `specta::Type`.
    assert!(
        ts.contains("ScanResult"),
        "ScanResult missing from bindings"
    );
    assert!(ts.contains("FileEntry"), "FileEntry missing from bindings");
    assert!(
        ts.contains("VolumeEntry"),
        "VolumeEntry missing from bindings"
    );
    assert!(
        ts.contains("SearchHitPayload"),
        "SearchHitPayload missing from bindings"
    );
}

/// Verifies that the core domain types — `CoreError`, `FileLocationRecord`,
/// `Tag`, `SearchHit`, `FileEvent`, `BlakeHash` — appear in the generated
/// TypeScript after Task 8 flips all 13 handlers to `Result<T, CoreError>`
/// and deletes the 1:1 wire-mirror structs.
///
/// WHY `#[ignore]`: the assertions below WILL fail until Task 8 lands,
/// because current handlers return `Result<T, String>` and the domain
/// types are not reachable from any handler return type. This is not a
/// `specta::Type` derive failure — it is the expected pre-Task-8 state.
/// Re-enable (remove `#[ignore]`) when Task 8 is complete.
// TODO(Batch D Task 8): remove `#[ignore]` once all handlers return
// `Result<T, CoreError>` and FileLocationRecord/BlakeHash/FileEvent/SearchHit
// replace their wire mirrors in handler signatures.
#[ignore]
#[test]
fn bindings_contain_core_domain_types() {
    let builder = Builder::<tauri::Wry>::new().commands(collect_commands![
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
    ]);

    let tmp = tempfile::NamedTempFile::new().expect("create tempfile for bindings export");
    builder
        .export(Typescript::default(), tmp.path())
        .expect("specta TypeScript export to tempfile must not fail");

    let ts = std::fs::read_to_string(tmp.path()).expect("read generated TypeScript from tempfile");

    // All of these appear transitively via CoreError (error variant on every
    // handler) and the domain-typed return values once Task 8 lands.
    assert!(ts.contains("CoreError"), "CoreError missing from bindings");
    assert!(
        ts.contains("FileLocationRecord"),
        "FileLocationRecord missing from bindings"
    );
    assert!(ts.contains("Tag"), "Tag missing from bindings");
    assert!(ts.contains("SearchHit"), "SearchHit missing from bindings");
    assert!(ts.contains("FileEvent"), "FileEvent missing from bindings");
    assert!(ts.contains("BlakeHash"), "BlakeHash missing from bindings");
}
