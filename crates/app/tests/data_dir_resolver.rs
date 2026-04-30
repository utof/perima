//! Smoke tests for the shared data-dir resolver (GH #154).

#![allow(clippy::unwrap_used)] // WHY: integration test; panics are assertion failures, not prod bugs.

use perima_app::config::{BUNDLE_ID, resolve_data_dir};

#[test]
fn resolve_data_dir_yields_path_with_bundle_id_segment() {
    let path = resolve_data_dir().expect("resolve");
    let s = path.to_string_lossy();
    assert!(
        s.contains(BUNDLE_ID),
        "path {s} missing BUNDLE_ID {BUNDLE_ID}"
    );
}

#[test]
fn resolve_data_dir_ends_with_perima_component() {
    let path = resolve_data_dir().expect("resolve");
    let last = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    assert_eq!(last, "perima", "path {path:?} should end in /perima");
}

#[test]
fn resolve_data_dir_is_deterministic_within_process() {
    let a = resolve_data_dir().expect("first");
    let b = resolve_data_dir().expect("second");
    assert_eq!(a, b, "resolve_data_dir must be idempotent");
}

#[test]
fn resolve_data_dir_parent_is_bundle_id() {
    let path = resolve_data_dir().expect("resolve");
    let parent = path
        .parent()
        .expect("data_dir should have a parent")
        .file_name()
        .expect("parent should have a filename")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        parent, BUNDLE_ID,
        "data_dir parent should be BUNDLE_ID, got {parent}"
    );
}
