//! Mock-ffmpeg unit tests for the audio pipeline.

#![allow(clippy::unwrap_used)] // WHY: test code; panics are acceptable assertions.

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use perima_transcribe::audio::{
    AudioError, AudioPipeline, FfmpegAudioPipeline, FfmpegChild, FfmpegInvoker,
};

/// Records the args passed and spawns a real process (true/false) to simulate
/// success or failure based on a config flag.
struct MockFfmpeg {
    captured_args: Mutex<Vec<String>>,
    fail_with_status: Option<i32>,
}

impl MockFfmpeg {
    fn new(fail_with_status: Option<i32>) -> Self {
        Self {
            captured_args: Mutex::new(Vec::new()),
            fail_with_status,
        }
    }

    fn captured(&self) -> Vec<String> {
        self.captured_args.lock().unwrap().clone()
    }
}

impl FfmpegInvoker for MockFfmpeg {
    fn spawn(&self, args: &[&str]) -> std::io::Result<FfmpegChild> {
        self.captured_args
            .lock()
            .unwrap()
            .extend(args.iter().map(|s| (*s).to_string()));

        // Simulate success with `true` or failure with `false` (exit 1).
        let cmd = if self.fail_with_status.is_none() {
            "true"
        } else {
            "false"
        };
        let child = std::process::Command::new(cmd)
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        Ok(FfmpegChild::new(child))
    }
}

#[test]
fn remux_for_upload_passes_canonical_flags() {
    let mock = Arc::new(MockFfmpeg::new(None));
    let pipeline = FfmpegAudioPipeline::new(Arc::clone(&mock));
    let input = std::env::temp_dir().join("test-input.mp4");
    std::fs::write(&input, b"placeholder").unwrap();

    let cancel = CancellationToken::new();
    let _result = pipeline.remux_for_upload(&input, &cancel).unwrap();

    let args = mock.captured();
    assert!(
        args.contains(&"-vn".to_string()),
        "expected -vn in args: {args:?}"
    );
    assert!(
        args.contains(&"-ac".to_string()) && args.contains(&"1".to_string()),
        "expected -ac 1 in args: {args:?}"
    );
    assert!(
        args.contains(&"-c:a".to_string()) && args.contains(&"libopus".to_string()),
        "expected -c:a libopus in args: {args:?}"
    );
    assert!(
        args.contains(&"-b:a".to_string()) && args.contains(&"32k".to_string()),
        "expected -b:a 32k in args: {args:?}"
    );
    assert!(
        args.contains(&"-i".to_string()),
        "expected -i in args: {args:?}"
    );
    assert!(
        args.contains(&input.to_string_lossy().to_string()),
        "expected input path in args: {args:?}"
    );
}

#[test]
fn remux_returns_non_zero_exit_when_ffmpeg_fails() {
    let mock = Arc::new(MockFfmpeg::new(Some(1)));
    let pipeline = FfmpegAudioPipeline::new(Arc::clone(&mock));
    let input = std::env::temp_dir().join("test-input-fail.mp4");
    std::fs::write(&input, b"placeholder").unwrap();

    let cancel = CancellationToken::new();
    let result = pipeline.remux_for_upload(&input, &cancel);

    assert!(matches!(result, Err(AudioError::NonZeroExit { .. })));
}

#[test]
fn remux_aborts_on_cancel() {
    // Use `sleep 30` to simulate a long-running ffmpeg invocation.
    struct SlowMock;
    impl FfmpegInvoker for SlowMock {
        fn spawn(&self, _args: &[&str]) -> std::io::Result<FfmpegChild> {
            let child = std::process::Command::new("sleep")
                .arg("30")
                .stderr(std::process::Stdio::piped())
                .spawn()?;
            Ok(FfmpegChild::new(child))
        }
    }
    let pipeline = FfmpegAudioPipeline::new(Arc::new(SlowMock));
    let input = std::env::temp_dir().join("test-input-slow.mp4");
    std::fs::write(&input, b"placeholder").unwrap();

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        cancel_clone.cancel();
    });

    let result = pipeline.remux_for_upload(&input, &cancel);
    assert!(matches!(result, Err(AudioError::Cancelled)));
}
