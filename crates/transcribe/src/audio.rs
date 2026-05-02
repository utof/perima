//! Audio extraction shim. Wraps ffmpeg as either a Tauri-bundled
//! sidecar binary (desktop) or a PATH-discovered system binary (CLI).
//!
//! See spec § "Audio extraction — `AudioPipeline`" + § "ffmpeg sourcing
//! strategy".

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::Arc;

use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;

/// Errors from audio extraction.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// ffmpeg binary not found on PATH (CLI) or in the sidecar bundle (desktop).
    #[error("ffmpeg binary not found: {0}")]
    BinaryNotFound(String),

    /// ffmpeg returned a non-zero exit code.
    #[error("ffmpeg exited with status {status}: {stderr}")]
    NonZeroExit {
        /// Exit status code.
        status: i32,
        /// Captured stderr for diagnostics.
        stderr: String,
    },

    /// I/O failure spawning or reading from ffmpeg.
    #[error("ffmpeg I/O failure: {0}")]
    Io(#[from] std::io::Error),

    /// Cancelled by caller via [`CancellationToken`].
    #[error("audio extraction cancelled by caller")]
    Cancelled,
}

/// Audio extraction operations used by transcription adapters.
pub trait AudioPipeline: Send + Sync {
    /// Drop video stream, downmix to mono Opus 32 kbps, return a [`NamedTempFile`]
    /// that auto-deletes on Drop. Used by cloud adapters before upload.
    /// Cancel-aware: kills the ffmpeg child if `cancel` fires.
    ///
    /// # Errors
    /// Returns [`AudioError`] for ffmpeg failures.
    fn remux_for_upload(
        &self,
        input: &std::path::Path,
        cancel: &CancellationToken,
    ) -> Result<NamedTempFile, AudioError>;

    // Future (local slice, V013): extract_pcm_16k_mono(...)
}

/// Strategy for invoking ffmpeg. Two impls: one for desktop (uses Tauri's
/// path resolver to locate the bundled sidecar binary), one for CLI
/// (uses [`which`]).
pub trait FfmpegInvoker: Send + Sync {
    /// Spawn `ffmpeg <args>` and return a handle whose Drop kills the
    /// child. Output streamed to the returned reader; stderr captured
    /// for error context.
    ///
    /// # Errors
    /// Returns [`std::io::Error`] for spawn failures.
    fn spawn(&self, args: &[&str]) -> std::io::Result<FfmpegChild>;
}

/// Handle to a running ffmpeg child. Drop kills the child.
#[derive(Debug)]
pub struct FfmpegChild {
    /// The underlying OS process. `Option` so `wait_with_output` can `take`
    /// ownership while `Drop` can still observe `None` and skip the kill.
    pub(crate) child: Option<Child>,
}

impl FfmpegChild {
    /// Construct from a spawned `Child`.
    #[must_use]
    pub const fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    /// Wait for the child to exit; return its [`std::process::Output`] and captured stderr.
    ///
    /// # Panics
    /// Panics if called more than once (the child handle is moved on the first
    /// call; subsequent calls would have nothing to wait on).
    ///
    /// # Errors
    /// I/O errors waiting on the child.
    pub fn wait_with_output(mut self) -> std::io::Result<std::process::Output> {
        let child = self.child.take().expect("child already taken");
        child.wait_with_output()
    }

    /// Kill the child immediately.
    ///
    /// # Errors
    /// I/O errors while killing.
    pub fn kill(&mut self) -> std::io::Result<()> {
        if let Some(child) = self.child.as_mut() {
            child.kill()?;
        }
        Ok(())
    }
}

impl Drop for FfmpegChild {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            // Best-effort kill on drop; ignore errors (process may have already exited).
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Desktop ffmpeg invoker: holds the absolute path to the bundled sidecar
/// binary, resolved at app startup via `app.path().resolve(...)`.
#[derive(Debug)]
pub struct DesktopFfmpegInvoker {
    binary_path: PathBuf,
}

impl DesktopFfmpegInvoker {
    /// Construct from a pre-resolved binary path (typically from
    /// `app.path().resolve("ffmpeg-{target-triple}", BaseDirectory::Resource)`).
    #[must_use]
    pub const fn new(binary_path: PathBuf) -> Self {
        Self { binary_path }
    }
}

impl FfmpegInvoker for DesktopFfmpegInvoker {
    fn spawn(&self, args: &[&str]) -> std::io::Result<FfmpegChild> {
        let child = std::process::Command::new(&self.binary_path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(FfmpegChild::new(child))
    }
}

/// CLI ffmpeg invoker: holds the PATH-discovered binary location.
#[derive(Debug)]
pub struct CliFfmpegInvoker {
    binary_path: PathBuf,
}

impl CliFfmpegInvoker {
    /// Discover ffmpeg on PATH via [`which::which`]. Caches the resolved path.
    ///
    /// # Errors
    /// Returns [`AudioError::BinaryNotFound`] if ffmpeg is not on PATH.
    pub fn discover() -> Result<Self, AudioError> {
        let path = which::which("ffmpeg").map_err(|_| {
            AudioError::BinaryNotFound(
                "ffmpeg not found on PATH; install via apt/brew/winget or use the desktop app"
                    .to_owned(),
            )
        })?;
        Ok(Self { binary_path: path })
    }
}

impl FfmpegInvoker for CliFfmpegInvoker {
    fn spawn(&self, args: &[&str]) -> std::io::Result<FfmpegChild> {
        let child = std::process::Command::new(&self.binary_path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(FfmpegChild::new(child))
    }
}

/// [`AudioPipeline`] impl built on a generic [`FfmpegInvoker`].
#[derive(Debug)]
pub struct FfmpegAudioPipeline<I: FfmpegInvoker> {
    invoker: Arc<I>,
}

impl<I: FfmpegInvoker> FfmpegAudioPipeline<I> {
    /// Construct from an invoker. Caller chooses Desktop vs Cli at app startup.
    #[must_use]
    pub const fn new(invoker: Arc<I>) -> Self {
        Self { invoker }
    }
}

impl<I: FfmpegInvoker + 'static> AudioPipeline for FfmpegAudioPipeline<I> {
    fn remux_for_upload(
        &self,
        input: &std::path::Path,
        cancel: &CancellationToken,
    ) -> Result<NamedTempFile, AudioError> {
        let output = NamedTempFile::with_suffix(".opus")?;
        let output_path = output.path().to_owned();

        let input_str = input.to_str().ok_or_else(|| {
            AudioError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "input path is not valid UTF-8",
            ))
        })?;
        let output_str = output_path.to_str().ok_or_else(|| {
            AudioError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "output tempfile path is not valid UTF-8",
            ))
        })?;

        // WHY this flag order: -y (overwrite), -i (input), -vn (no video),
        // -ac 1 (downmix to mono), -c:a libopus (codec), -b:a 32k (bitrate).
        // Produces an Opus file that Whisper backends accept and is well
        // under the 25 MiB file-size ceiling even for hour-long media.
        let args = [
            "-y", "-i", input_str, "-vn", "-ac", "1", "-c:a", "libopus", "-b:a", "32k", output_str,
        ];

        let mut child = self.invoker.spawn(&args)?;

        // WHY poll-and-sleep (50 ms) rather than tokio::process::Command +
        // select!: the port trait is sync (no async fn), and spinning up a
        // nested tokio runtime inside a sync fn violates the one-runtime-per-
        // binary rule. A future local-adapter slice can move this to an async
        // fn on a dedicated worker thread.
        loop {
            if cancel.is_cancelled() {
                let _ = child.kill();
                return Err(AudioError::Cancelled);
            }
            match child.child.as_mut().expect("child taken").try_wait()? {
                Some(status) => {
                    if status.success() {
                        return Ok(output);
                    }
                    let mut stderr = String::new();
                    if let Some(err_pipe) = child.child.as_mut().and_then(|c| c.stderr.take()) {
                        let mut reader = err_pipe;
                        let _ = reader.read_to_string(&mut stderr);
                    }
                    return Err(AudioError::NonZeroExit {
                        status: status.code().unwrap_or(-1),
                        stderr,
                    });
                }
                None => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    }
}
