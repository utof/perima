//! Round-trip tests for the JSON shapes that cross the IPC boundary.
//!
//! WHY: `bindings.ts` is generated from `#[derive(specta::Type)]` on these
//! types but the runtime serialization comes from `#[derive(Serialize)]`.
//! These tests pin the wire shape so a future field rename or struct
//! variant tweak fails here loudly instead of silently breaking the
//! frontend's `parseCoreError` matcher.

use perima_core::CoreError;

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
