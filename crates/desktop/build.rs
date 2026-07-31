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
    // WHY: forward cargo's TARGET env var (e.g. "x86_64-unknown-linux-gnu")
    // into the compiled binary so the runtime can resolve the bundled
    // ffmpeg sidecar at the path Tauri's `externalBin` rewriter actually
    // uses (`binaries/ffmpeg-{target-triple}`). std::env::consts::ARCH
    // gives only the architecture ("x86_64"), which would never match the
    // sidecar filename and silently break the T12 bundling story.
    println!(
        "cargo:rustc-env=TARGET={}",
        std::env::var("TARGET").expect("cargo always sets TARGET for build scripts")
    );
    tauri_build::build();
}
