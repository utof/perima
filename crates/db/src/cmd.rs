//! Write-command envelope for the `SQLite` writer actor.
//!
//! Each sub-enum variant carries a [`flume::Sender`] reply channel
//! (`bounded(1)`). Sub-enums are `#[non_exhaustive]` + empty in Task 1
//! and populated incrementally per repo in Tasks 2-6.
//!
//! WHY `flume::Sender` over `tokio::sync::oneshot::Sender`:
//! `oneshot::Receiver::blocking_recv` panics inside a tokio runtime
//! context ("Cannot block the current thread from within a runtime").
//! `flume::Receiver::recv` is runtime-agnostic and works in sync OR
//! async callers. A single `flume` dep also covers the command channel.

use std::path::PathBuf;

use flume::Sender;

use perima_core::{BlakeHash, CoreError, DeviceId, Tag, VolumeId, VolumeIdentifiers};
use uuid::Uuid;

/// Reply channel alias for writer-to-caller responses.
pub type ReplyTx<T> = Sender<Result<T, CoreError>>;

/// Top-level write-command envelope consumed by [`crate::SqliteWriter`].
///
/// Each variant wraps a per-repo sub-enum carrying the actual SQL
/// payload and a reply channel. Tasks 2-6 populate the sub-enums.
#[derive(Debug)]
#[non_exhaustive]
pub enum WriteCmd {
    /// Volume-repo writes (populated Task 2).
    Volume(VolumeWriteCmd),
    /// Tag-repo writes (populated Task 3).
    Tag(TagWriteCmd),
    /// Metadata-repo writes (populated Task 4).
    Metadata(MetadataWriteCmd),
    /// File-repo writes (populated Task 5).
    File(FileWriteCmd),
    /// Search-repo writes (populated Task 6).
    Search(SearchWriteCmd),
}

/// Volume-repo write commands. Populated by Task 2.
///
/// WHY `ReplyTx<T>` carries `Debug`: `flume::Sender` implements `Debug`
/// (verified crate docs), and every payload type on the in-flight side
/// (`VolumeIdentifiers`, `DeviceId`, `VolumeId`, `PathBuf`) is `Debug`.
/// Keeping `#[derive(Debug)]` on this enum lets the writer loop's
/// `tracing::debug!` prints render a full command view without a manual
/// impl.
#[derive(Debug)]
pub enum VolumeWriteCmd {
    /// Find an existing volume matching `identifiers` or insert a new
    /// row. Updates `last_seen` + `updated_at` + `device_id` on match.
    FindOrCreate {
        /// Observed identifiers (GUID / `fs_uuid` / label+capacity).
        identifiers: VolumeIdentifiers,
        /// Device that observed the volume.
        device: DeviceId,
        /// Reply channel carrying the resolved [`VolumeId`].
        reply: ReplyTx<VolumeId>,
    },
    /// Record (or refresh) the current mount path for `(volume, device)`.
    RecordMount {
        /// Volume being mounted.
        volume: VolumeId,
        /// Device that observed the mount.
        device: DeviceId,
        /// Absolute mount-point path on the device.
        mount: PathBuf,
        /// Reply channel acknowledging the write.
        reply: ReplyTx<()>,
    },
}

/// Tag-repo write commands. Populated by Task 3.
///
/// WHY `ReplyTx<u64>` on `Attach` / `Detach` / `DeleteTag` (not `()`):
/// the writer learns `rusqlite::Connection::changes()` after each UPDATE
/// / INSERT, and callers occasionally want "did anything actually
/// happen?" signal. The current [`perima_core::TagRepository`] port
/// returns `Result<(), CoreError>` on attach/detach/delete, so the
/// adapter drops the `u64` on the floor today; keeping the wider reply
/// channel now means surfacing the count later is a port-only change.
#[derive(Debug)]
pub enum TagWriteCmd {
    /// Insert a new tag row (or look up an existing active one by
    /// normalized name). Returns the resolved [`Tag`].
    ///
    /// Name normalization is performed by the adapter before the command
    /// is sent — the writer receives the already-normalized `name`.
    UpsertTag {
        /// Normalized tag name (post `perima_core::normalize_tag`).
        name: String,
        /// Device that initiated the upsert.
        device: DeviceId,
        /// Reply channel carrying the resolved [`Tag`].
        reply: ReplyTx<Tag>,
    },
    /// Soft-delete a tag by id. Attachments in `file_tags` survive —
    /// CRDT semantics preserve link history (see port trait doc).
    DeleteTag {
        /// Tag UUID.
        tag_id: Uuid,
        /// Device that initiated the delete.
        device: DeviceId,
        /// Reply channel carrying `rows_changed` (0 if already deleted).
        reply: ReplyTx<u64>,
    },
    /// Attach a tag to a content hash. Idempotent at the DB level —
    /// re-attaching an already-active `(hash, tag_id)` pair is a no-op.
    Attach {
        /// Content hash.
        hash: BlakeHash,
        /// Tag UUID.
        tag_id: Uuid,
        /// Device that initiated the attach.
        device: DeviceId,
        /// Reply channel carrying `rows_changed` (1 on new insert, 0 on
        /// idempotent repeat).
        reply: ReplyTx<u64>,
    },
    /// Soft-delete the `file_tags` row linking `hash` → `tag_id`.
    Detach {
        /// Content hash.
        hash: BlakeHash,
        /// Tag UUID.
        tag_id: Uuid,
        /// Device that initiated the detach.
        device: DeviceId,
        /// Reply channel carrying `rows_changed` (0 if nothing was
        /// active for the pair).
        reply: ReplyTx<u64>,
    },
}

/// Metadata-repo write commands. Populated by Task 4.
#[derive(Debug)]
#[non_exhaustive]
pub enum MetadataWriteCmd {}

/// File-repo write commands. Populated by Task 5.
#[derive(Debug)]
#[non_exhaustive]
pub enum FileWriteCmd {}

/// Search-repo write commands. Populated by Task 6.
#[derive(Debug)]
#[non_exhaustive]
pub enum SearchWriteCmd {}
