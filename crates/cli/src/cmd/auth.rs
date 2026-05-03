//! `perima auth` subcommand — manage transcription-provider keyring entries.
//!
//! Service: `"perima.transcription"` (matches the constant the
//! `AppContainer` reads from at startup; spec §"Settings + auth").
//!
//! Sub-actions:
//! - `set <PROVIDER>`     — prompt for password (blind input, no echo);
//!   stores under service `perima.transcription`, account `<PROVIDER>`.
//!   Reads from stdin when stdin is non-TTY (CI / piped input).
//! - `delete <PROVIDER>`  — remove the keyring entry; idempotent (no error
//!   when the entry never existed).
//! - `has <PROVIDER>`     — exit 0 if entry exists, 1 if not.
//! - `list`               — prints active provider + every provider with a
//!   keyring entry; one per line.

use std::io::{IsTerminal, Read, Write};
use std::path::Path;

use clap::{Args, Subcommand};
use perima_app::config::transcription::TranscriptionConfig;
use perima_core::CoreError;

/// Keyring service name. PINNED — must match
/// `perima_app::container::KEYRING_SERVICE`.
pub(crate) const KEYRING_SERVICE: &str = "perima.transcription";

/// Arguments for `perima auth`.
#[derive(Args, Debug)]
pub(crate) struct AuthArgs {
    /// The auth sub-action to perform.
    #[command(subcommand)]
    pub action: AuthAction,
}

/// Individual actions under `perima auth`.
#[derive(Subcommand, Debug)]
pub(crate) enum AuthAction {
    /// Store an API key under the given provider name. Prompts for the
    /// key with hidden input on a TTY; reads from stdin otherwise.
    Set {
        /// Provider name as it appears in `[transcription.providers.*]`.
        provider: String,
    },
    /// Remove the keyring entry for the given provider. Idempotent.
    Delete {
        /// Provider name to remove.
        provider: String,
    },
    /// Exit 0 if a keyring entry exists for the given provider, 1 if not.
    Has {
        /// Provider name to check.
        provider: String,
    },
    /// Print the active provider and every provider with a stored key.
    List,
}

fn keyring_entry(provider: &str) -> Result<keyring::Entry, CoreError> {
    keyring::Entry::new(KEYRING_SERVICE, provider)
        .map_err(|e| CoreError::Internal(format!("keyring entry: {e}")))
}

fn read_password_blind(provider: &str) -> Result<String, CoreError> {
    if std::io::stdin().is_terminal() {
        // WHY dialoguer's Password widget on TTY: hides typed bytes from
        // shoulder-surfing observers and disables echo on Unix terminals.
        // The non-TTY fallback below is for piped input (CI, automation).
        dialoguer::Password::new()
            .with_prompt(format!("API key for provider {provider}"))
            .interact()
            .map_err(|e| CoreError::Internal(format!("dialoguer: {e}")))
    } else {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(CoreError::from)?;
        // Strip a single trailing \n if present; preserve any other whitespace.
        let trimmed = buf
            .strip_suffix("\r\n")
            .or_else(|| buf.strip_suffix('\n'))
            .unwrap_or(&buf)
            .to_owned();
        if trimmed.is_empty() {
            return Err(CoreError::Internal(
                "empty password (read from non-TTY stdin)".into(),
            ));
        }
        Ok(trimmed)
    }
}

/// Run `perima auth <ACTION>`.
///
/// # Errors
/// Returns [`CoreError::Internal`] on keyring or I/O failures.
pub(crate) fn run(args: &AuthArgs, config_dir: &Path) -> Result<u8, CoreError> {
    match &args.action {
        AuthAction::Set { provider } => run_set(provider),
        AuthAction::Delete { provider } => run_delete(provider),
        AuthAction::Has { provider } => run_has(provider),
        AuthAction::List => run_list(config_dir),
    }
}

fn run_set(provider: &str) -> Result<u8, CoreError> {
    let password = read_password_blind(provider)?;
    let entry = keyring_entry(provider)?;
    entry
        .set_password(&password)
        .map_err(|e| CoreError::Internal(format!("keyring set: {e}")))?;
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "stored API key for provider {provider}");
    Ok(0)
}

fn run_delete(provider: &str) -> Result<u8, CoreError> {
    let entry = keyring_entry(provider)?;
    match entry.delete_credential() {
        Ok(()) => {
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "deleted keyring entry for {provider}");
            Ok(0)
        }
        Err(keyring::Error::NoEntry) => {
            // Idempotent — nothing to delete is success.
            let mut stderr = std::io::stderr();
            let _ = writeln!(stderr, "no keyring entry for {provider} (already absent)");
            Ok(0)
        }
        Err(e) => Err(CoreError::Internal(format!("keyring delete: {e}"))),
    }
}

fn run_has(provider: &str) -> Result<u8, CoreError> {
    let entry = keyring_entry(provider)?;
    match entry.get_password() {
        Ok(_) => Ok(0),
        Err(keyring::Error::NoEntry) => Ok(1),
        Err(e) => Err(CoreError::Internal(format!("keyring get: {e}"))),
    }
}

fn run_list(config_dir: &Path) -> Result<u8, CoreError> {
    let cfg = TranscriptionConfig::load(config_dir)?;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    let active_label = cfg.active_provider.as_deref().unwrap_or("(none)");
    writeln!(handle, "active provider: {active_label}").map_err(CoreError::from)?;

    if cfg.providers.is_empty() {
        writeln!(handle, "no providers configured (edit config.toml)").map_err(CoreError::from)?;
        return Ok(0);
    }

    writeln!(handle, "providers:").map_err(CoreError::from)?;
    // Sort for deterministic output.
    let mut names: Vec<&String> = cfg.providers.keys().collect();
    names.sort();
    for name in names {
        let mark = match keyring_entry(name).and_then(|e| {
            e.get_password()
                .map_err(|err| CoreError::Internal(format!("keyring: {err}")))
        }) {
            Ok(_) => "[has key]",
            Err(_) => "[no key] ",
        };
        let active_mark = if cfg.active_provider.as_deref() == Some(name.as_str()) {
            "*"
        } else {
            " "
        };
        writeln!(handle, "  {active_mark} {mark} {name}").map_err(CoreError::from)?;
    }
    Ok(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// `keyring::set_default_credential_builder` is set ONCE per process.
    /// Tests that need the mock should run with `--test-threads 1` OR
    /// register the mock via a `#[ctor]` — but for now we accept that
    /// only the first test through here decides the backend. The CLI
    /// integration tests under `tests/` use a fresh subprocess each time
    /// so they don't conflict.
    #[test]
    fn keyring_entry_constructs_with_canonical_service() {
        // Doesn't actually touch the OS keyring — just builds the Entry.
        let entry = keyring_entry("test-provider-construction").unwrap();
        // Sanity: the builder accepted the (service, account) tuple.
        let _ = entry; // keyring::Entry has no public accessors for these fields.
    }
}
