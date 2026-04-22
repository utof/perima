//! Round-trip tests for the JSON shapes that cross the IPC boundary.
//!
//! WHY: `bindings.ts` is generated from `#[derive(specta::Type)]` on these
//! types but the runtime serialization comes from `#[derive(Serialize)]`.
//! These tests pin the wire shape so a future field rename or struct
//! variant tweak fails here loudly instead of silently breaking the
//! frontend's `parseCoreError` matcher.

use perima_core::{
    BlakeHash, CoreError, FileEvent, FileLocationRecord, FileSize, LocationStatus, MediaMetadata,
    MediaPath, SearchHit, Tag, VolumeId, VolumeRecord,
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

// ── E4: single-type file IPC shapes (4 types) ──────────────────────────────
// WHY: MediaMetadata, Tag, SearchHit, FileEvent each live in their own file
// and cross the IPC boundary. Pinning their wire shapes here catches field
// renames and serde-tag changes before they silently break the frontend.

#[test]
fn media_metadata_serializes_as_object() {
    let meta = MediaMetadata {
        hash: BlakeHash::from_bytes([0u8; 32]),
        width: Some(1920),
        height: Some(1080),
        duration_ms: None,
        captured_at: Some("2026-04-22T12:00:00Z".to_owned()),
        camera_make: None,
        camera_model: None,
        codec: None,
        bitrate_bps: None,
        mime_type: Some("image/jpeg".to_owned()),
        thumbnail_path: None,
        thumbnail_status: None,
    };
    let v = serde_json::to_value(&meta).expect("serialize MediaMetadata");
    assert!(v.is_object(), "MediaMetadata JSON must be an object");
    assert!(v["hash"].is_string(), "hash must serialize as string");
    assert_eq!(v["width"], serde_json::json!(1920));
    assert_eq!(v["height"], serde_json::json!(1080));
    assert_eq!(v["duration_ms"], serde_json::Value::Null);
    assert_eq!(v["captured_at"], serde_json::json!("2026-04-22T12:00:00Z"));
    assert_eq!(v["mime_type"], serde_json::json!("image/jpeg"));
}

#[test]
fn tag_serializes_with_id_name_first_seen() {
    // WHY: Tag.first_seen (not created_at) — confirmed via MCP discovery.
    let tag = Tag {
        id: uuid::Uuid::nil(),
        name: "nature".to_owned(),
        first_seen: "2026-04-22T00:00:00Z".to_owned(),
    };
    let v = serde_json::to_value(&tag).expect("serialize Tag");
    assert!(v.is_object(), "Tag JSON must be an object");
    assert_eq!(
        v["id"],
        serde_json::json!("00000000-0000-0000-0000-000000000000")
    );
    assert_eq!(v["name"], serde_json::json!("nature"));
    assert_eq!(v["first_seen"], serde_json::json!("2026-04-22T00:00:00Z"));
}

#[test]
fn search_hit_serializes_with_blake3_hash_and_rank() {
    let hit = SearchHit {
        blake3_hash: "abc123".to_owned(),
        volume_id: "00000000-0000-0000-0000-000000000000".to_owned(),
        relative_path: "photos/img.jpg".to_owned(),
        rank: -1.5_f64,
    };
    let v = serde_json::to_value(&hit).expect("serialize SearchHit");
    assert!(v.is_object(), "SearchHit JSON must be an object");
    assert_eq!(v["blake3_hash"], serde_json::json!("abc123"));
    assert_eq!(
        v["volume_id"],
        serde_json::json!("00000000-0000-0000-0000-000000000000")
    );
    assert_eq!(v["relative_path"], serde_json::json!("photos/img.jpg"));
    assert_eq!(v["rank"], serde_json::json!(-1.5_f64));
}

#[test]
fn file_event_created_serializes_with_kind_and_data() {
    // WHY: FileEvent uses #[serde(tag = "type")] inline (no content key),
    // matching the pre-Batch-D FileEventPayload mirror in desktop.
    // This is DIFFERENT from CoreError (tag="kind", content="data").
    let event = FileEvent::Created {
        path: MediaPath::new("photos/img.jpg"),
        volume: VolumeId(uuid::Uuid::nil()),
    };
    let v = serde_json::to_value(&event).expect("serialize FileEvent::Created");
    assert!(v.is_object(), "FileEvent JSON must be an object");
    assert_eq!(
        v["type"],
        serde_json::json!("Created"),
        "FileEvent must serialize with 'type' discriminant key"
    );
    // Inline tag: path and volume live at the top level, no 'data' wrapper.
    assert!(
        v["path"].is_string(),
        "path must be inlined at the top level"
    );
    assert_eq!(v["path"], serde_json::json!("photos/img.jpg"));
    assert!(
        v["volume"].is_string(),
        "volume must be inlined at the top level"
    );
    assert_eq!(
        v["volume"],
        serde_json::json!("00000000-0000-0000-0000-000000000000")
    );
}
