//! Search hit value type.

use serde::{Deserialize, Serialize};

use crate::FileUuid;

/// A ranked result from the `FTS5` full-text search index.
///
/// The `rank` field follows `SQLite`'s BM25 convention: lower (more negative)
/// values indicate a better match. Callers should sort ascending.
///
/// WHY `file_uuid` non-nullable + `blake3_hash` nullable (Task 11, spec §4.8):
/// `file_uuid` is the stable surrogate for every row in `files` from V011 on,
/// so it is always present. `blake3_hash` (== `full_hash` in v0.6.x) is `None`
/// for files whose `full_hash` has not yet been computed (pending dedup).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct SearchHit {
    /// Stable surrogate identifier for the file row (`UUIDv7`).
    pub file_uuid: FileUuid,
    /// `BLAKE3` hex hash of the file content. `None` when `full_hash` has not
    /// yet been computed for this file (pending verification).
    pub blake3_hash: Option<String>,
    /// Volume UUID string.
    pub volume_id: String,
    /// Relative path within the volume (representative location).
    pub relative_path: String,
    /// BM25 rank (lower = better, `SQLite` convention).
    pub rank: f64,
}
