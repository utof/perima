//! Dedup-related core types — exposed to shells via specta-typed bindings.
//!
//! See spec §4.7.1.

use serde::{Deserialize, Serialize};

use crate::{BlakeHash, CoreError, FileLocationRecord, FileUuid};

// WHY CoreError is Serialize-only: std::io::Error is !Clone and !Deserialize;
// CoreError::Io lowers the io::Error at construction and is itself !Deserialize
// (only derives Serialize). FullHashOutcome::Failed holds a CoreError, so
// FullHashOutcome cannot derive Deserialize either. It is an outbound-only event
// payload (frontend never sends it back) so Serialize alone is the correct bound.

/// Stable identifier for a `compute_full_hash_batch` operation.
///
/// UUIDv7-derived so batch IDs sort chronologically and do not collide
/// across devices.
// WHY specta(transparent): inner field is `uuid::Uuid`; specta maps
// `Uuid` → `string` via the "uuid" feature so TS sees `string`, not `{ 0: string }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[cfg_attr(feature = "specta", specta(transparent))]
pub struct BatchId(pub uuid::Uuid);

impl BatchId {
    /// Generate a fresh `UUIDv7` batch id.
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl Default for BatchId {
    fn default() -> Self {
        Self::new()
    }
}

/// Returned from `compute_full_hash_batch` — used by the frontend to
/// subscribe to per-file progress events.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct BatchHandle {
    /// Stable id for this batch run.
    pub batch_id: BatchId,
    /// Total number of files queued for full-hash computation.
    pub total: u32,
}

/// What kind of physical storage backs a volume.
///
/// WHY perima-owned (NOT `sysinfo::DiskKind`): `crates/core` has zero
/// framework dependencies. Adapters convert `sysinfo::DiskKind` → this
/// enum at the volume adapter boundary, keeping the domain type stable
/// even if `sysinfo` is swapped out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum DeviceKind {
    /// Spinning rust (HDD).
    Hdd,
    /// SATA / `NVMe` / SD / generic non-rotational.
    Ssd,
    /// Could not be determined; treat conservatively (defaults to SSD path
    /// per spec §4.5.3 — more common in 2026 hardware; HDD penalty for
    /// a falsely-Unknown SSD is worse than the reverse).
    Unknown,
}

/// A group of files whose `quick_hash` matches — candidate duplicates.
///
/// `verified_state` tracks whether the group has been confirmed by
/// `full_hash` comparison (see [`VerifiedState`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct CollisionGroup {
    /// The shared quick-hash fingerprint of every file in this group.
    pub quick_hash: BlakeHash,
    /// All known file locations whose quick-hash is this value.
    pub files: Vec<FileLocationRecord>,
    /// Current verification state of this group.
    pub verified_state: VerifiedState,
}

/// State of a candidate group's verification.
///
/// WHY plain external tagging (no `#[serde(tag)]`): this is a unit-only
/// enum; internal tagging would force a TS object envelope for zero gain.
/// If a future variant grows fields, switch to internal tagging then.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub enum VerifiedState {
    /// Group has not been compared by `full_hash`.
    Unverified,
    /// Every file in the group produced the same `full_hash` (true duplicate).
    VerifiedDuplicate,
    /// At least one file differs by `full_hash` (quick-hash collision, not duplicate).
    VerifiedDistinct,
    /// Some files have been verified, others haven't (partial batch completion).
    Mixed,
}

/// Per-file outcome inside `AppEvent::VerifyProgress`.
///
/// WHY `#[serde(tag = "outcome", content = "data")]`: matches `CoreError`'s
/// internal-tagging-with-content pattern, producing a TypeScript discriminated
/// union `{ outcome: "Computed"; data: … } | { outcome: "Failed"; data: … }`.
///
/// WHY only `Serialize` (no `Deserialize`): `FullHashOutcome::Failed` holds a
/// `CoreError`, and `CoreError` is `!Deserialize` because `CoreError::Io` lowers
/// `std::io::Error` (which is `!Deserialize`) at construction time. Since
/// `FullHashOutcome` is an outbound-only event payload the frontend never sends
/// back, `Serialize` alone is the correct bound.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(tag = "outcome", content = "data")]
pub enum FullHashOutcome {
    /// Full hash was successfully computed for this file.
    Computed {
        /// The stable file identifier.
        file_uuid: FileUuid,
        /// The computed full BLAKE3-256 hash.
        hash: BlakeHash,
    },
    /// Full hash computation failed for this file.
    Failed {
        /// The stable file identifier.
        file_uuid: FileUuid,
        /// The error that caused the failure.
        error: CoreError,
    },
}
