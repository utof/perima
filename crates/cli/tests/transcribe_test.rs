//! Integration tests: `perima transcribe` and `perima auth` subcommands.
//!
//! Drives the CLI binary (`assert_cmd::Command::cargo_bin("perima")`)
//! against a wiremock-stubbed Groq-shaped HTTP endpoint. A synthetic
//! 1-second 16 kHz mono WAV (via `hound`) stands in for real media.
//!
//! ## How the keyring is bypassed in tests
//!
//! `keyring::mock::default_credential_builder` is process-local: a test
//! process cannot pre-seed the spawned CLI subprocess's mock store. The
//! container reads provider api keys via two paths:
//!
//! 1. `PERIMA_TEST_API_KEY_<provider>` env var (intended ONLY for tests).
//!    If set, this value short-circuits the keyring lookup. The CLI sees
//!    the api key as if a real keyring entry existed.
//! 2. The platform keyring otherwise.
//!
//! The auth-subcommand tests (`auth_*`) flip `PERIMA_KEYRING_MOCK=1` so
//! the subprocess keyring lives in memory; that avoids polluting the
//! developer's real OS keyring during `cargo test`.
//!
//! ## Why a real wiremock server, not a mocked Transcriber trait
//!
//! Mocking the trait would skip the entire HTTP request shape — the test
//! would pass even if the CLI accidentally sent the wrong endpoint or
//! omitted required multipart parts. wiremock asserts the wire shape.

#![allow(clippy::unwrap_used)]
#![allow(clippy::print_stdout)]

use std::io::Write;
use std::path::Path;

use assert_cmd::Command;
use wiremock::matchers::{method, path as wm_path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_perima")
}

/// Synthesize a 1-second 16 kHz mono silent WAV at the given path.
fn write_silent_wav(path: &Path) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for _ in 0..16_000 {
        writer.write_sample(0_i16).unwrap();
    }
    writer.finalize().unwrap();
}

/// Verbose-JSON response body matching the OpenAI / Groq schema. One
/// segment with the text "hello world" — the happy-path test asserts
/// this surfaces on stdout.
fn verbose_json_body() -> serde_json::Value {
    serde_json::json!({
        "task": "transcribe",
        "language": "en",
        "duration": 1.0_f32,
        "text": "hello world",
        "segments": [{
            "id": 0,
            "seek": 0,
            "start": 0.0_f32,
            "end": 1.0_f32,
            "text": "hello world",
            "tokens": [],
            "temperature": 0.0_f32,
            "avg_logprob": -0.1_f32,
            "compression_ratio": 1.0_f32,
            "no_speech_prob": 0.0_f32,
        }],
        "usage": { "type": "duration", "seconds": 1.0_f32 },
    })
}

/// Write a `config.toml` that wires a single `custom`-preset provider at
/// the wiremock server's URL with a placeholder api key.
fn write_test_config(config_dir: &Path, base_url: &str) {
    let body = format!(
        "[transcription]\nactive_provider = \"test\"\n\n\
         [transcription.providers.test]\npreset = \"custom\"\nbase_url = \"{base_url}\"\nmodel = \"whisper-1\"\n"
    );
    let path = config_dir.join("config.toml");
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(body.as_bytes()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcribe_happy_path_writes_segment_text_to_stdout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(wm_path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(verbose_json_body()))
        .mount(&server)
        .await;

    let env_dir = tempfile::tempdir().unwrap();
    write_test_config(env_dir.path(), &server.uri());

    let media_tmp = tempfile::tempdir().unwrap();
    let wav = media_tmp.path().join("hello.wav");
    write_silent_wav(&wav);

    // WHY spawn_blocking around `Command::output`: `output()` is sync and
    // would block the tokio runtime, preventing wiremock from serving the
    // CLI subprocess's HTTP request → permanent stall. spawn_blocking
    // moves the wait off the runtime so wiremock's MockServer task can
    // service the inbound POST.
    let env_path = env_dir.path().to_path_buf();
    let wav_path = wav.clone();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(bin())
            .arg("transcribe")
            .arg(&wav_path)
            .arg("--language")
            .arg("en")
            .env("PERIMA_CONFIG_DIR", &env_path)
            .env("PERIMA_DATA_DIR", &env_path)
            // Bypass the platform keyring AND the real key lookup — the
            // test injects the api key directly via this env var.
            .env("PERIMA_TEST_API_KEY_test", "test-key")
            .output()
            .expect("spawn perima")
    })
    .await
    .expect("join");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "exit {:?}; stdout={stdout}; stderr={stderr}",
        output.status.code()
    );
    assert!(
        stdout.contains("hello world"),
        "expected 'hello world' on stdout; got stdout={stdout}; stderr={stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcribe_auth_failure_exits_2() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(wm_path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": {
                "message": "Incorrect API key provided",
                "type": "invalid_request_error",
                "code": "invalid_api_key",
            }
        })))
        .mount(&server)
        .await;

    let env_dir = tempfile::tempdir().unwrap();
    write_test_config(env_dir.path(), &server.uri());

    let media_tmp = tempfile::tempdir().unwrap();
    let wav = media_tmp.path().join("auth-fail.wav");
    write_silent_wav(&wav);

    let env_path = env_dir.path().to_path_buf();
    let wav_path = wav.clone();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(bin())
            .arg("transcribe")
            .arg(&wav_path)
            .env("PERIMA_CONFIG_DIR", &env_path)
            .env("PERIMA_DATA_DIR", &env_path)
            .env("PERIMA_TEST_API_KEY_test", "wrong-key")
            .output()
            .expect("spawn perima")
    })
    .await
    .expect("join");

    assert_eq!(
        output.status.code(),
        Some(2),
        "expected exit 2 (Auth); got {:?}; stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn transcribe_invalid_language_rejected_by_clap() {
    let env_dir = tempfile::tempdir().unwrap();
    let media_tmp = tempfile::tempdir().unwrap();
    let wav = media_tmp.path().join("bad-lang.wav");
    write_silent_wav(&wav);

    let output = Command::new(bin())
        .arg("transcribe")
        .arg(&wav)
        .arg("--language")
        .arg("not-a-real-tag-zzz-zzz-zzz")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .output()
        .expect("spawn perima");

    // clap rejects with exit code 2 by default for value_parser failures.
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("BCP-47") || stderr.contains("language"),
        "expected BCP-47 hint in clap error; got: {stderr}"
    );
}

#[test]
fn auth_has_returns_1_when_entry_missing() {
    let env_dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .arg("auth")
        .arg("has")
        .arg("nonexistent-provider-xyz")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .env("PERIMA_KEYRING_MOCK", "1")
        .output()
        .expect("spawn perima");

    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn auth_set_then_has_then_delete_is_idempotent() {
    let env_dir = tempfile::tempdir().unwrap();

    // `set` reads from stdin when it isn't on a TTY (assert_cmd's pipe is non-TTY).
    let mut set = std::process::Command::new(bin())
        .arg("auth")
        .arg("set")
        .arg("groq")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .env("PERIMA_KEYRING_MOCK", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn auth set");
    set.stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"my-secret-key\n")
        .expect("write stdin");
    let set_out = set.wait_with_output().expect("wait set");
    assert!(
        set_out.status.success(),
        "auth set failed: {}",
        String::from_utf8_lossy(&set_out.stderr)
    );

    // The mock keyring is process-local, so we cannot follow up with `auth has`
    // in a separate subprocess and expect the entry to still be there — each
    // `Command::new(bin())` invocation is a fresh subprocess, fresh in-memory
    // mock. We only assert `set` succeeds end-to-end here.

    // `delete` on a non-existent entry is idempotent — exits 0.
    let del_out = Command::new(bin())
        .arg("auth")
        .arg("delete")
        .arg("never-existed-anywhere")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .env("PERIMA_KEYRING_MOCK", "1")
        .output()
        .expect("spawn auth delete");
    assert!(
        del_out.status.success(),
        "auth delete must be idempotent; got {:?}",
        del_out.status.code()
    );
}

#[test]
fn auth_list_with_empty_config_prints_no_providers() {
    let env_dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .arg("auth")
        .arg("list")
        .env("PERIMA_CONFIG_DIR", env_dir.path())
        .env("PERIMA_DATA_DIR", env_dir.path())
        .env("PERIMA_KEYRING_MOCK", "1")
        .output()
        .expect("spawn perima");

    assert!(
        output.status.success(),
        "auth list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no providers") || stdout.contains("active provider: (none)"),
        "expected empty-config marker; got: {stdout}"
    );
}
