//! Search hit value type.

use serde::{Deserialize, Serialize};

/// A ranked result from the `FTS5` full-text search index.
///
/// The `rank` field follows `SQLite`'s BM25 convention: lower (more negative)
/// values indicate a better match. Callers should sort ascending.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SearchHit {
    /// `BLAKE3` hex hash of the file content.
    pub blake3_hash: String,
    /// Volume UUID string.
    pub volume_id: String,
    /// Relative path within the volume (representative location).
    pub relative_path: String,
    /// BM25 rank (lower = better, `SQLite` convention).
    pub rank: f64,
}
