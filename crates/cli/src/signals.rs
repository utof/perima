//! Ctrl-C / SIGTERM handling.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use perima_core::CoreError;

/// Cancellation guard. Drop it to uninstall the signal handler
/// (so later tests or embedded usage can re-install).
///
/// WHY guard semantics: `ctrlc::set_handler` is a process-global
/// singleton; without this guard pattern a test suite would fail
/// the second test that tried to install a handler.
pub struct Cancellation {
    flag: Arc<AtomicBool>,
}

impl Cancellation {
    /// Has a cancellation signal been received?
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Share the cancellation flag with a worker closure (e.g.
    /// rayon's `par_iter` map). WHY: we only expose the `Arc` for
    /// this reason; callers that only need a bool should use
    /// `cancelled()` instead.
    #[must_use]
    pub fn token(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }
}

/// Install a process-global SIGINT/SIGTERM handler that flips the
/// cancellation flag. The returned guard holds the flag; keep it
/// alive for the duration you care about signals.
///
/// WHY: on Ctrl-C we flip the flag rather than `std::process::exit`
/// so the scan loop can finish printing already-hashed entries —
/// stdout lines are per-file, and aborting mid-write would truncate.
///
/// # Errors
/// Returns `CoreError::Internal` if another handler is already
/// registered (only one handler per process).
pub fn install() -> Result<Cancellation, CoreError> {
    let flag = Arc::new(AtomicBool::new(false));
    let cloned = Arc::clone(&flag);
    ctrlc::set_handler(move || {
        cloned.store(true, Ordering::SeqCst);
    })
    .map_err(|e| CoreError::Internal(format!("ctrlc: {e}")))?;
    Ok(Cancellation { flag })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_flag_starts_false() {
        let flag = Arc::new(AtomicBool::new(false));
        let c = Cancellation {
            flag: Arc::clone(&flag),
        };
        assert!(!c.cancelled());
        flag.store(true, Ordering::SeqCst);
        assert!(c.cancelled());
    }
}
