// WHY: tauri_build::build() reads tauri.conf.json at compile time to embed
// the Tauri context (window config, capabilities, etc.) and validates the
// frontendDist path exists. Must run before the lib compiles.
//
// WHY no doc comment on `main`: build scripts are binary entry points, not
// library API. The workspace's `missing_docs = "deny"` applies to public
// items in libraries; the build script is exempt by convention. The `allow`
// below makes the intent explicit.
#![allow(missing_docs)]
fn main() {
    tauri_build::build();
}
