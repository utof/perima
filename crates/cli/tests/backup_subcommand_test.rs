//! `perima backup` subcommand integration test.
//!
//! WHY subprocess-based: verifies the full dispatch path including DB schema
//! migrations and clap argument parsing through the real binary entry point.

#![allow(clippy::unwrap_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn perima_cmd(data_dir: &std::path::Path, config_dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("perima").expect("bin");
    cmd.env("PERIMA_DATA_DIR", data_dir);
    cmd.env("PERIMA_CONFIG_DIR", config_dir);
    cmd
}

#[test]
fn backup_default_path_succeeds_and_prints_path() {
    let tmp = TempDir::new().expect("tempdir");
    let data_dir = tmp.path().join("data");
    let config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&data_dir).expect("data dir");
    std::fs::create_dir_all(&config_dir).expect("config dir");

    perima_cmd(&data_dir, &config_dir)
        .args(["backup"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Saved to"))
        .stdout(predicate::str::contains("backups"))
        .stdout(predicate::str::contains(".sqlite"));

    let backups_dir = data_dir.join("backups");
    let entries: Vec<_> = std::fs::read_dir(&backups_dir)
        .expect("read backups dir")
        .collect();
    assert!(!entries.is_empty(), "default backup file should exist");
}

#[test]
fn backup_to_existing_path_without_force_fails() {
    let tmp = TempDir::new().expect("tempdir");
    let data_dir = tmp.path().join("data");
    let config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&data_dir).expect("data dir");
    std::fs::create_dir_all(&config_dir).expect("config dir");

    let target = tmp.path().join("out.sqlite");
    perima_cmd(&data_dir, &config_dir)
        .args(["backup", "--to", target.to_str().unwrap()])
        .assert()
        .success();

    perima_cmd(&data_dir, &config_dir)
        .args(["backup", "--to", target.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));

    perima_cmd(&data_dir, &config_dir)
        .args(["backup", "--to", target.to_str().unwrap(), "--force"])
        .assert()
        .success();
}
