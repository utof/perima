//! Integration tests: `perima tag` subcommand + `perima ls --tag` filter.
//!
//! WHY subprocess-based: the tag subcommand wires together `TagRepository`,
//! `FileRepository`, volume detection, and path resolution in `main.rs`.
//! Testing through the binary is the only way to verify the full dispatch
//! path including DB schema migrations (V005 tags tables).

use std::io::Write;
use std::path::Path;
use std::process::Command;

const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

/// Create two plain-text fixture files in `dir`.
fn mk_fixture(dir: &Path) {
    for (name, content) in [
        ("file1.txt", b"hello world" as &[u8]),
        ("file2.txt", b"bye"),
    ] {
        let path = dir.join(name);
        std::fs::File::create(&path)
            .expect("create fixture")
            .write_all(content)
            .expect("write fixture");
    }
}

fn run_scan(td: &Path, env_dir: &Path) {
    let out = Command::new(bin())
        .args(["scan", "--no-thumbnails"])
        .arg(td)
        .env("PERIMA_CONFIG_DIR", env_dir)
        .env("PERIMA_DATA_DIR", env_dir)
        .output()
        .expect("spawn perima scan");
    assert!(
        out.status.success(),
        "scan failed\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_tag_add(file: &Path, tags: &[&str], env_dir: &Path) {
    let mut cmd = Command::new(bin());
    cmd.arg("tag").arg("add").arg(file);
    for t in tags {
        cmd.arg(t);
    }
    cmd.env("PERIMA_CONFIG_DIR", env_dir)
        .env("PERIMA_DATA_DIR", env_dir);
    let out = cmd.output().expect("spawn perima tag add");
    assert!(
        out.status.success(),
        "tag add failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_tag_rm(file: &Path, tag: &str, env_dir: &Path) {
    let out = Command::new(bin())
        .args(["tag", "rm"])
        .arg(file)
        .arg(tag)
        .env("PERIMA_CONFIG_DIR", env_dir)
        .env("PERIMA_DATA_DIR", env_dir)
        .output()
        .expect("spawn perima tag rm");
    assert!(
        out.status.success(),
        "tag rm failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_tag_ls_json(env_dir: &Path) -> Vec<serde_json::Value> {
    let out = Command::new(bin())
        .args(["tag", "ls", "--json"])
        .env("PERIMA_CONFIG_DIR", env_dir)
        .env("PERIMA_DATA_DIR", env_dir)
        .output()
        .expect("spawn perima tag ls --json");
    assert!(
        out.status.success(),
        "tag ls --json failed\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    serde_json::from_str(&stdout).expect("deserialize tag ls json")
}

fn run_ls_tag_json(tag: &str, env_dir: &Path) -> Vec<serde_json::Value> {
    let out = Command::new(bin())
        .args(["ls", "--json", "--tag", tag])
        .env("PERIMA_CONFIG_DIR", env_dir)
        .env("PERIMA_DATA_DIR", env_dir)
        .output()
        .expect("spawn perima ls --tag --json");
    assert!(
        out.status.success(),
        "ls --tag --json failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    serde_json::from_str(&stdout).expect("deserialize ls --tag json")
}

/// Scan two files, add two tags to file1, verify counts, remove one tag,
/// verify updated counts, and filter via `ls --tag`.
#[test]
fn tag_add_rm_ls_and_filter() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());

    // Step 1: index the files.
    run_scan(td.path(), env_dir.path());

    let file1 = td.path().join("file1.txt");

    // Step 2: tag file1 with "vacation" and "sunset".
    run_tag_add(&file1, &["vacation", "sunset"], env_dir.path());

    // Step 3: verify tag ls shows both tags with count=1.
    let rows = run_tag_ls_json(env_dir.path());
    let vacation_row = rows
        .iter()
        .find(|r| r["name"] == "vacation")
        .expect("vacation tag in ls");
    let sunset_row = rows
        .iter()
        .find(|r| r["name"] == "sunset")
        .expect("sunset tag in ls");
    assert_eq!(vacation_row["count"], 1, "vacation should have count 1");
    assert_eq!(sunset_row["count"], 1, "sunset should have count 1");

    // Step 4: remove "sunset" from file1.
    run_tag_rm(&file1, "sunset", env_dir.path());

    // Step 5: verify sunset now has count=0, vacation still count=1.
    let rows = run_tag_ls_json(env_dir.path());
    let vacation_row = rows
        .iter()
        .find(|r| r["name"] == "vacation")
        .expect("vacation after rm");
    let sunset_row = rows
        .iter()
        .find(|r| r["name"] == "sunset")
        .expect("sunset after rm");
    assert_eq!(
        vacation_row["count"], 1,
        "vacation count unchanged after sunset rm"
    );
    assert_eq!(sunset_row["count"], 0, "sunset count should be 0 after rm");

    // Step 6: ls --tag vacation should return exactly file1.
    let ls_rows = run_ls_tag_json("vacation", env_dir.path());
    assert_eq!(
        ls_rows.len(),
        1,
        "ls --tag vacation should return exactly 1 file, got: {ls_rows:?}"
    );
    // The relative_path field should end with "file1.txt".
    let rel_path = ls_rows[0]["relative_path"]
        .as_str()
        .expect("relative_path is string");
    assert!(
        rel_path.ends_with("file1.txt"),
        "expected file1.txt, got: {rel_path}"
    );
}

/// `tag add` is idempotent — tagging the same file twice with the same label
/// must not double-count.
#[test]
fn tag_add_is_idempotent() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    run_scan(td.path(), env_dir.path());

    let file1 = td.path().join("file1.txt");
    run_tag_add(&file1, &["nature"], env_dir.path());
    run_tag_add(&file1, &["nature"], env_dir.path());

    let rows = run_tag_ls_json(env_dir.path());
    let nature = rows
        .iter()
        .find(|r| r["name"] == "nature")
        .expect("nature tag");
    assert_eq!(nature["count"], 1, "idempotent add must not double-count");
}

/// `ls --tag` for a tag that carries no files returns an empty array.
#[test]
fn ls_tag_empty_after_rm() {
    let td = tempfile::tempdir().expect("tempdir");
    let env_dir = tempfile::tempdir().expect("env dir");
    mk_fixture(td.path());
    run_scan(td.path(), env_dir.path());

    let file1 = td.path().join("file1.txt");
    run_tag_add(&file1, &["archive"], env_dir.path());
    run_tag_rm(&file1, "archive", env_dir.path());

    // The tag still exists (soft-deleted attachment), but no files carry it.
    // ls --tag archive must return an empty array (not an error).
    // WHY: `ls --tag` with zero matches is different from "tag not found";
    // the tag was created, so NotFound would be wrong. The filter returns [].
    let ls_rows = run_ls_tag_json("archive", env_dir.path());
    assert!(
        ls_rows.is_empty(),
        "expected empty ls --tag after rm, got: {ls_rows:?}"
    );
}
