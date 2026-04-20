//! Logging initialization.

use perima_core::CoreError;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Init `tracing-subscriber`. Reads `PERIMA_LOG` (env filter,
/// default "info"); `PERIMA_LOG_JSON=1` for JSON output (else
/// human-readable text). Writes to stderr. `verbosity_bump` comes
/// from CLI `-v` count.
///
/// # Errors
/// Returns `CoreError::Internal` if the global subscriber is already
/// set (tests should tolerate this).
pub(crate) fn init(verbosity_bump: u8) -> Result<(), CoreError> {
    let base = std::env::var("PERIMA_LOG").unwrap_or_else(|_| "info".into());
    let bump_level = match verbosity_bump {
        0 => None,
        1 => Some("debug"),
        _ => Some("trace"),
    };
    let filter_str = match bump_level {
        Some(lvl) => format!("{base},perima={lvl}"),
        None => base,
    };
    let filter = EnvFilter::try_new(&filter_str)
        .map_err(|e| CoreError::Internal(format!("env filter: {e}")))?;

    let json = std::env::var("PERIMA_LOG_JSON").is_ok_and(|v| v == "1");

    let registry = tracing_subscriber::registry().with(filter);
    if json {
        registry
            .with(fmt::layer().json().with_writer(std::io::stderr))
            .try_init()
            .map_err(|e| CoreError::Internal(format!("subscriber: {e}")))
    } else {
        registry
            .with(fmt::layer().with_writer(std::io::stderr))
            .try_init()
            .map_err(|e| CoreError::Internal(format!("subscriber: {e}")))
    }
}
