//! Round-trip tests for the JSON shapes that cross the IPC boundary.
//!
//! WHY: `bindings.ts` is generated from `#[derive(specta::Type)]` on these
//! types but the runtime serialization comes from `#[derive(Serialize)]`.
//! These tests pin the wire shape so a future field rename or struct
//! variant tweak fails here loudly instead of silently breaking the
//! frontend's `parseCoreError` matcher.

use perima_core::{
    BlakeHash, CoreError, FileLocationRecord, FileSize, LocationStatus, MediaPath, VolumeId,
    VolumeRecord,
};

#[test]
fn core_error_not_found_serializes_with_kind_and_data() {
    let err = CoreError::NotFound("file 42".to_owned());
    let json = serde_json::to_string(&err).expect("serialize");
    let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(v["kind"], "NotFound");
    assert_eq!(v["data"], "file 42");
}

#[test]
fn core_error_io_serializes_as_struct_variant_with_kind_and_message() {
    let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    let err: CoreError = io_err.into();
    let json = serde_json::to_string(&err).expect("serialize");
    let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(v["kind"], "Io");
    assert_eq!(v["data"]["kind"], "PermissionDenied");
    assert!(
        v["data"]["message"]
            .as_str()
            .expect("message field present and string-typed")
            .contains("denied")
    );
}

#[test]
fn core_error_implements_clone() {
    // WHY: Lowering Io to a struct variant unblocks Clone, which is
    // useful for the bindings-compile test fixture and future
    // event-replay scenarios (Batch E).
    // WHY: compile-time proof is sufficient — a runtime .clone() would be flagged
    // as redundant_clone by clippy since `err` is never used after the call.
    fn _assert_clone<T: Clone>() {}
    let _ = _assert_clone::<CoreError>;
}

// ── IPC domain-type wire-shape tests (6 types) ─────────────────────────────
// WHY: Specta derives inform the TypeScript types; serde derives drive the
// runtime wire format. Both must agree, so we pin the JSON shape here.
// A field rename or wrapper change causes a loud failure instead of a silent
// frontend type mismatch.

#[test]
fn blake_hash_serializes_as_lowercase_hex_string() {
    // All-zeros hash → 64 '0' characters.
    let h = BlakeHash::from_bytes([0u8; 32]);
    let v = serde_json::to_value(h).expect("serialize BlakeHash");
    let s = v.as_str().expect("BlakeHash JSON must be a string");
    assert_eq!(
        s.len(),
        64,
        "BlakeHash JSON string must be exactly 64 chars"
    );
    assert!(
        s.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "BlakeHash JSON string must be lowercase hex"
    );
    assert_eq!(
        s,
        "0".repeat(64),
        "all-zeros hash must produce 64 '0' chars"
    );
}

#[test]
fn file_size_serializes_as_number() {
    let fs = FileSize(42);
    let v = serde_json::to_value(fs).expect("serialize FileSize");
    assert_eq!(
        v,
        serde_json::json!(42),
        "FileSize JSON must be a bare number"
    );
}

#[test]
fn media_path_serializes_as_string() {
    let p = MediaPath::new("photos/img.jpg");
    let v = serde_json::to_value(&p).expect("serialize MediaPath");
    assert_eq!(
        v,
        serde_json::json!("photos/img.jpg"),
        "MediaPath JSON must be the normalized path string"
    );
}

#[test]
fn volume_id_serializes_as_uuid_string() {
    let id = VolumeId(uuid::Uuid::nil());
    let v = serde_json::to_value(id).expect("serialize VolumeId");
    assert_eq!(
        v,
        serde_json::json!("00000000-0000-0000-0000-000000000000"),
        "VolumeId JSON must be a hyphenated UUID string"
    );
}

#[test]
fn file_location_record_serializes_with_string_typed_fields() {
    let record = FileLocationRecord {
        hash: BlakeHash::from_bytes([0xabu8; 32]),
        size: FileSize(1024),
        volume_id: VolumeId(uuid::Uuid::nil()),
        relative_path: MediaPath::new("docs/spec.md"),
        status: LocationStatus::Active,
        first_seen: "2026-04-22T00:00:00Z".to_owned(),
    };
    let v = serde_json::to_value(&record).expect("serialize FileLocationRecord");
    // Verify object shape: all expected keys present and typed correctly.
    assert!(v["hash"].is_string(), "hash must serialize as string");
    assert_eq!(v["hash"].as_str().expect("hash is string").len(), 64);
    assert!(v["size"].is_number(), "size must serialize as number");
    assert_eq!(v["size"], serde_json::json!(1024));
    assert!(
        v["volume_id"].is_string(),
        "volume_id must serialize as string"
    );
    assert!(
        v["relative_path"].is_string(),
        "relative_path must serialize as string"
    );
    assert_eq!(v["relative_path"], serde_json::json!("docs/spec.md"));
    assert_eq!(v["status"], serde_json::json!("Active"));
    assert_eq!(v["first_seen"], serde_json::json!("2026-04-22T00:00:00Z"));
}

#[test]
fn volume_record_serializes_with_object_shape() {
    use std::path::PathBuf;
    let record = VolumeRecord {
        id: VolumeId(uuid::Uuid::nil()),
        label: Some("MyDrive".to_owned()),
        capacity_bytes: 1_000_000,
        is_removable: true,
        mounts_on_this_machine: vec![PathBuf::from("/mnt/vol")],
        last_seen: "2026-04-22T00:00:00Z".to_owned(),
    };
    let v = serde_json::to_value(&record).expect("serialize VolumeRecord");
    // Verify object shape: all expected keys present.
    assert!(v["id"].is_string(), "id must serialize as string");
    assert_eq!(
        v["id"],
        serde_json::json!("00000000-0000-0000-0000-000000000000")
    );
    assert_eq!(v["label"], serde_json::json!("MyDrive"));
    assert_eq!(v["capacity_bytes"], serde_json::json!(1_000_000_u64));
    assert_eq!(v["is_removable"], serde_json::json!(true));
    assert!(
        v["mounts_on_this_machine"].is_array(),
        "mounts_on_this_machine must serialize as array"
    );
    assert_eq!(v["last_seen"], serde_json::json!("2026-04-22T00:00:00Z"));
}
