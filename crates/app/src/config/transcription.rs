//! Transcription provider config (TOML-backed). Loaded at app startup;
//! parsed via `toml`. NOT in `crates/core` because core is framework-free
//! (no `toml` dep allowed).
//!
//! Schema: see spec § "Settings + auth".

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use perima_core::CoreError;

/// Wrapper for the top-level on-disk `config.toml` shape.
///
/// WHY a private wrapper struct: the on-disk file may carry tables unrelated
/// to `[transcription]` (e.g. future `[telemetry]`). Parsing through a wrapper
/// that ignores unknown sections preserves them on round-trip when callers
/// go through `save` immediately afterwards.
#[derive(Deserialize)]
struct Root {
    transcription: Option<TranscriptionConfig>,
}

/// Top-level transcription config (one TOML table).
///
/// Lives at `<config_dir>/config.toml` under the `[transcription]` table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscriptionConfig {
    /// Active provider name (must match one of `providers.*` keys).
    /// Optional so first-run state (no provider configured yet) is
    /// representable without `Option<TranscriptionConfig>` at the caller.
    pub active_provider: Option<String>,
    /// Per-provider configuration table. Empty = no providers wired up yet.
    #[serde(default)]
    pub providers: HashMap<String, ProviderEntry>,
}

/// One provider's TOML entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// Preset name from `KNOWN_PROVIDERS` (e.g. "groq", "openai", "custom").
    pub preset: String,
    /// Optional model override; falls back to the preset's default.
    pub model: Option<String>,
    /// Optional `base_url` override (required for `preset = "custom"`).
    pub base_url: Option<String>,
    /// Auth scheme override for custom providers:
    /// `"bearer"` | `"x-api-key:<header>"` | `"none"`.
    pub auth_scheme: Option<String>,
}

impl TranscriptionConfig {
    /// Load from `<config_dir>/config.toml`, parsing the `[transcription]`
    /// table only.
    ///
    /// Returns [`Self::default()`] if the file is missing or the table is
    /// absent — that is not an error, just "no providers configured yet."
    ///
    /// # Errors
    /// Returns [`CoreError::Internal`] if the file exists but is malformed
    /// TOML or unreadable.
    pub fn load(config_dir: &Path) -> Result<Self, CoreError> {
        let path = config_dir.join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let body = std::fs::read_to_string(&path).map_err(|e| {
            CoreError::Internal(format!("read transcription config {}: {e}", path.display()))
        })?;
        let root: Root = toml::from_str(&body).map_err(|e| {
            CoreError::Internal(format!(
                "parse transcription config {}: {e}",
                path.display()
            ))
        })?;
        Ok(root.transcription.unwrap_or_default())
    }

    /// Persist to `<config_dir>/config.toml`. Replaces only the
    /// `[transcription]` table; preserves any other tables already on disk.
    ///
    /// # Errors
    /// Returns [`CoreError::Internal`] for I/O or TOML serialization failures.
    pub fn save(&self, config_dir: &Path) -> Result<(), CoreError> {
        let path = config_dir.join("config.toml");
        // Read the existing file (if any), merge the transcription table, write back.
        let existing: toml::Value = if path.exists() {
            let body = std::fs::read_to_string(&path).map_err(|e| {
                CoreError::Internal(format!("read transcription config {}: {e}", path.display()))
            })?;
            // WHY unwrap_or_else: a malformed pre-existing config.toml should
            // not block writing `[transcription]`; we treat it as empty for
            // the merge. The caller (settings UI) is the recovery path for
            // a corrupted file outside our table.
            toml::from_str(&body).unwrap_or_else(|_| toml::Value::Table(toml::value::Table::new()))
        } else {
            toml::Value::Table(toml::value::Table::new())
        };
        let mut table = match existing {
            toml::Value::Table(t) => t,
            _ => toml::value::Table::new(),
        };
        let our_value = toml::Value::try_from(self)
            .map_err(|e| CoreError::Internal(format!("serialize transcription config: {e}")))?;
        table.insert("transcription".to_owned(), our_value);
        let body = toml::to_string_pretty(&toml::Value::Table(table))
            .map_err(|e| CoreError::Internal(format!("encode transcription config: {e}")))?;
        std::fs::create_dir_all(config_dir).map_err(|e| {
            CoreError::Internal(format!("mkdir config dir {}: {e}", config_dir.display()))
        })?;
        std::fs::write(&path, body).map_err(|e| {
            CoreError::Internal(format!(
                "write transcription config {}: {e}",
                path.display()
            ))
        })?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.
mod tests {
    use super::*;

    #[test]
    fn load_returns_default_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = TranscriptionConfig::load(tmp.path()).unwrap();
        assert!(cfg.active_provider.is_none());
        assert!(cfg.providers.is_empty());
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = TranscriptionConfig::default();
        cfg.active_provider = Some("groq".to_owned());
        cfg.providers.insert(
            "groq".to_owned(),
            ProviderEntry {
                preset: "groq".to_owned(),
                model: Some("whisper-large-v3-turbo".to_owned()),
                base_url: None,
                auth_scheme: None,
            },
        );

        cfg.save(tmp.path()).unwrap();
        let round = TranscriptionConfig::load(tmp.path()).unwrap();
        assert_eq!(round.active_provider.as_deref(), Some("groq"));
        let entry = round.providers.get("groq").expect("groq provider missing");
        assert_eq!(entry.preset, "groq");
        assert_eq!(entry.model.as_deref(), Some("whisper-large-v3-turbo"));
    }

    #[test]
    fn save_preserves_unrelated_tables() {
        let tmp = tempfile::tempdir().unwrap();
        // Pre-write an unrelated table to ensure save() doesn't clobber it.
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[other]\nfoo = \"bar\"\n").unwrap();

        let cfg = TranscriptionConfig {
            active_provider: Some("openai".to_owned()),
            providers: HashMap::new(),
        };
        cfg.save(tmp.path()).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("[other]") && body.contains("foo"),
            "save() must not clobber unrelated tables; got body:\n{body}"
        );
        assert!(
            body.contains("[transcription]"),
            "save() must add [transcription]; got body:\n{body}"
        );
    }

    #[test]
    fn load_returns_internal_error_on_malformed_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "this is = not = valid = toml").unwrap();

        let err = TranscriptionConfig::load(tmp.path()).expect_err("malformed TOML must error");
        assert!(matches!(err, CoreError::Internal(_)), "got {err:?}");
    }
}
