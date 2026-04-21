//! Ctrl-C / SIGTERM handling.

use perima_core::CoreError;
use tokio_util::sync::CancellationToken;

/// Cancellation guard backed by a `CancellationToken`.
///
/// WHY `CancellationToken` instead of `AtomicBool`: phase 3a introduces
/// async `perima watch`, which needs `cancel.token().cancelled().await`.
/// `CancellationToken` is the idiomatic tokio-util primitive for this;
/// it is cheaply cloneable and integrates with tokio's `select!` and
/// `CancellationToken::cancelled_owned()` futures.
pub(crate) struct Cancellation {
    token: CancellationToken,
}

impl Cancellation {
    /// Clone the underlying [`CancellationToken`] for use in a rayon closure
    /// or an async task.
    ///
    /// WHY cloned not referenced: `CancellationToken` is an `Arc`-wrapped
    /// inner handle, so `clone()` is O(1) and the clone shares state with the
    /// original. Callers needing a boolean check can call `.is_cancelled()`
    /// on the cloned token directly.
    #[must_use]
    pub(crate) fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

/// Install a process-global SIGINT/SIGTERM handler that cancels the token.
/// The returned guard holds the token; keep it alive for the duration you
/// care about signals.
///
/// WHY: on Ctrl-C we cancel the token rather than `std::process::exit`
/// so the scan loop can finish printing already-hashed entries — stdout
/// lines are per-file, and aborting mid-write would truncate output.
///
/// # Errors
/// Returns `CoreError::Internal` if another handler is already registered
/// (only one `ctrlc` handler per process).
pub(crate) fn install() -> Result<Cancellation, CoreError> {
    let token = CancellationToken::new();
    let cloned = token.clone();
    ctrlc::set_handler(move || {
        cloned.cancel();
    })
    .map_err(|e| CoreError::Internal(format!("ctrlc: {e}")))?;
    Ok(Cancellation { token })
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    #[test]
    fn cancellation_flag_starts_false() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
    }
}
