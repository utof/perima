//! Build-script: capture short git SHA into the `GIT_SHA` rustc-env so
//! `perima --debug-report` can report the binary's source version.
//!
//! Falls back to "unknown" if git is unavailable or .git is missing
//! (e.g. vendored / source-tarball builds).

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".into(), |s| s.trim().to_string());
    println!("cargo:rustc-env=GIT_SHA={sha}");
    // Re-run when HEAD moves so builds during dev pick up new commits.
    println!("cargo:rerun-if-changed=.git/HEAD");
}
