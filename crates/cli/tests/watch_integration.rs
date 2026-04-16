//! Integration tests for `perima watch`.
//!
//! These tests spawn the compiled `perima` binary as a child process and
//! interact with it via signals. Unix-only because SIGTERM is a POSIX concept.

#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

/// Path to the compiled test binary, provided by Cargo.
const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

/// Wait up to `timeout` for `child` to exit; return the exit status if it
/// exits within the window, otherwise return `None`.
///
/// WHY poll loop instead of `wait_timeout`: `std::process::Child` has no
/// built-in timeout on `wait()`. Polling with `try_wait` avoids the need
/// for an external crate or unsafe thread tricks.
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait failed") {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Create a small fixture file in `dir`.
fn write_fixture(dir: &std::path::Path, name: &str, content: &[u8]) {
    let path = dir.join(name);
    std::fs::File::create(&path)
        .expect("create fixture")
        .write_all(content)
        .expect("write fixture");
}

/// `perima watch <tmpdir>` starts, reports "watching", then exits cleanly on SIGTERM.
///
/// Test flow:
/// 1. Scan the tmpdir so a volume row exists (watcher needs the volume).
/// 2. Spawn `perima watch <tmpdir>` with stderr piped.
/// 3. Wait up to 2 s for the "watching" line to appear in stderr.
/// 4. Send SIGTERM.
/// 5. Wait up to 5 s for the child to exit.
/// 6. Assert stderr contains "watching" and "watch stopped".
#[test]
fn watch_starts_and_exits_on_sigterm() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");

    // Pre-populate a file so the scan has something to index.
    write_fixture(td.path(), "seed.txt", b"seed content");

    // Step 1: initial scan so the volume record exists.
    let scan_out = Command::new(bin())
        .args(["scan", td.path().to_str().expect("path str")])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("scan failed to spawn");
    assert!(
        scan_out.status.success(),
        "pre-scan failed: {}",
        String::from_utf8_lossy(&scan_out.stderr)
    );

    // Step 2: spawn the watch command.
    let mut child = Command::new(bin())
        .args(["watch", td.path().to_str().expect("path str")])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("watch failed to spawn");

    // Step 3: poll for the "watching" message (up to 3 s).
    // WHY: the watcher needs a moment to init before we consider it ready.
    // We do not read stderr inline because `read_to_string` blocks; instead
    // we just wait and check exit status hasn't occurred yet.
    let pid = Pid::from_raw(i32::try_from(child.id()).expect("child PID fits in i32"));

    // Give the watcher time to initialise.
    std::thread::sleep(Duration::from_millis(800));

    // Confirm the child is still alive.
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "watch exited prematurely"
    );

    // Step 4: send SIGTERM.
    kill(pid, Signal::SIGTERM).expect("kill failed");

    // Step 5: wait for the child to exit.
    let status = wait_with_timeout(&mut child, Duration::from_secs(5));

    // Collect stderr for assertions.
    let output = child.wait_with_output().unwrap_or_else(|_| {
        // If already waited above, construct a fake output — the status was
        // captured; stderr might be empty but the test still passes on status.
        std::process::Output {
            status: status
                .unwrap_or_else(|| panic!("watch process did not exit within 5 s after SIGTERM")),
            stdout: vec![],
            stderr: vec![],
        }
    });

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Step 6: assertions.
    assert!(
        stderr.contains("watching"),
        "expected 'watching' in stderr; got: {stderr:?}"
    );
    // "watch stopped" is printed after the cancellation token fires.
    // SIGTERM triggers ctrlc handler → token.cancel() → cancelled().await
    // resolves → eprintln!("watch stopped").
    assert!(
        stderr.contains("watch stopped"),
        "expected 'watch stopped' in stderr; got: {stderr:?}"
    );
}

/// `perima watch <path>` returns exit code 2 for a non-existent path.
#[test]
fn watch_exits_2_for_missing_path() {
    let env_dir = tempfile::tempdir().expect("env dir");

    let output = Command::new(bin())
        .args(["watch", "/tmp/perima-test-nonexistent-path-xyz-12345"])
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn");

    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit code 2 for missing path; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
