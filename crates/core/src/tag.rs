//! Tag value type + name normalization.

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::CoreError;

/// Maximum tag length after normalization, in Unicode scalar values.
pub const MAX_TAG_LEN: usize = 64;

/// A tag — user-assignable label on content.
///
/// WHY only `id`/`name`/`first_seen` exposed: CRDT bookkeeping
/// (`updated_at`/`device_id`/`deleted_at`) is repo-internal; UI and
/// future FFI/HTTP adapters don't need it. Keeps the domain type
/// minimal and stable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    /// `UUIDv7` primary key.
    pub id: Uuid,
    /// NFC-normalized lowercase, trimmed.
    pub name: String,
    /// ISO 8601 UTC timestamp of first sighting.
    pub first_seen: String,
}

/// Normalize a user-supplied tag name to its canonical form.
///
/// Steps: trim whitespace, NFC-normalize, lowercase. Reject empty
/// or longer than [`MAX_TAG_LEN`] chars post-normalization.
///
/// WHY core-level (not repo-level): shared by CLI + future HTTP API
/// + FFI adapters. Mirrors `MediaPath::new`'s design.
///
/// # Errors
/// [`CoreError::InvalidTag`] on empty, whitespace-only, or overlong
/// input.
pub fn normalize(raw: &str) -> Result<String, CoreError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CoreError::InvalidTag("tag name is empty".into()));
    }
    let normalized: String = trimmed.nfc().flat_map(char::to_lowercase).collect();
    if normalized.chars().count() > MAX_TAG_LEN {
        return Err(CoreError::InvalidTag(format!(
            "tag exceeds {MAX_TAG_LEN} chars: {raw:?}"
        )));
    }
    Ok(normalized)
}

#[allow(
    clippy::unwrap_used,
    reason = "tests: unwrap is the assertion — a panic is a failing test by design"
)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lowercases_and_nfc() {
        assert_eq!(normalize("Vacation").unwrap(), "vacation");
        // Precomposed "café" vs decomposed should collapse.
        let decomposed: String = "cafe\u{0301}".nfd().collect();
        assert_eq!(normalize(&decomposed).unwrap(), "caf\u{00e9}");
    }

    #[test]
    fn normalize_trims() {
        assert_eq!(normalize("  tag  ").unwrap(), "tag");
    }

    #[test]
    fn normalize_rejects_empty() {
        assert!(matches!(normalize(""), Err(CoreError::InvalidTag(_))));
        assert!(matches!(normalize("   "), Err(CoreError::InvalidTag(_))));
    }

    #[test]
    fn normalize_rejects_too_long() {
        let too_long = "x".repeat(MAX_TAG_LEN + 1);
        assert!(matches!(
            normalize(&too_long),
            Err(CoreError::InvalidTag(_))
        ));
    }
}
