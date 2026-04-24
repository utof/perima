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

use perima_core::{
    BlakeHash, CoreError, DeviceId, HashedFile, LocationStatus, MediaMetadata, MediaPath, Tag,
    UpsertOutcome, VolumeId, VolumeIdentifiers,
};
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
    /// Cooperative shutdown signal — when the writer thread receives
    /// this it exits its loop. Sent by `SqliteWriterHandle::join` and
    /// the `Drop` impl.
    ///
    /// WHY explicit shutdown signal: prior to its introduction,
    /// shutdown depended on every cloned `Sender<WriteCmd>` (held by
    /// repos / handlers) being dropped before `writer.join()` was
    /// called — otherwise the channel never closed and the writer
    /// parked in `recv` forever, hanging `pthread_join`. The pattern
    /// produced "magic-drop" callsites that listed N explicit drops
    /// matching how many senders the function had cloned (e.g. GH
    /// #131's 3-of-3 fix in `run_scan_inner_with_metadata`). With the
    /// explicit `Shutdown` variant, sender clones become harmless —
    /// after the writer breaks, subsequent sends fail with
    /// `Disconnected` instead of deadlocking on shutdown.
    Shutdown,
}

impl WriteCmd {
    /// Short kind name for tracing spans. WHY: enum Debug print is too noisy;
    /// `?cmd` would dump full bodies into spans. (Batch I Task 5.)
    pub(crate) const fn kind_str(&self) -> &'static str {
        match self {
            Self::Volume(_) => "volume",
            Self::Tag(_) => "tag",
            Self::Metadata(_) => "metadata",
            Self::File(_) => "file",
            Self::Search(_) => "search",
            Self::Shutdown => "shutdown",
        }
    }
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
///
/// WHY `UpdateThumbnail` sits alongside `UpsertMetadata` (rather than
/// being folded into the latter): the thumbnail-worker path writes
/// independently from the extractor path — same row, different logical
/// event. `upsert_metadata`'s Unchanged/Updated equivalence proxy
/// compares `device_id` + `mime_type` only, so a thumbnail status flip
/// `pending → ready` would be classified Unchanged and lost. Mirror of
/// the pre-Batch-C `MetadataRepository::update_thumbnail` rationale.
#[derive(Debug)]
pub enum MetadataWriteCmd {
    /// Insert or update the metadata row keyed by `record.hash`.
    ///
    /// Thumbnail columns are deliberately NOT touched by this command —
    /// see `crates/db/src/writer/metadata.rs::upsert_metadata_impl` for
    /// the decoupling rationale (utof/perima#15 HIGH #4).
    UpsertMetadata {
        /// Metadata to persist. The `hash` field is the content-
        /// addressed PK; every other field is nullable / optional.
        record: MediaMetadata,
        /// Device that initiated the upsert.
        device: DeviceId,
        /// Reply channel carrying the classification
        /// (Inserted / Updated / Unchanged).
        reply: ReplyTx<UpsertOutcome>,
    },
    /// Update the thumbnail columns on an existing `file_metadata` row.
    ///
    /// `path` is carried as `Option<String>` (nullable in SQL) —
    /// thumbnail-failed rows store `path = NULL` with `status =
    /// "failed"`. The writer transmits `Option<String>` rather than
    /// `Option<&str>` because the command crosses a thread boundary
    /// via `flume`; `'static` is the simplest lifetime contract.
    UpdateThumbnail {
        /// Content hash of the file whose thumbnail row to update.
        hash: BlakeHash,
        /// Thumbnail path (WebP under the thumbnail root) or `None`
        /// when the generation failed.
        path: Option<String>,
        /// Status literal — one of `pending` / `ready` / `failed`.
        status: String,
        /// Device that initiated the update.
        device: DeviceId,
        /// Reply channel carrying `rows_changed` (0 if no metadata
        /// row exists for `hash`; 1 otherwise).
        reply: ReplyTx<u64>,
    },
}

/// File-repo write commands. Populated by Task 5.
///
/// WHY `UpsertFile` reply is `ReplyTx<UpsertOutcome>` (not `()`):
/// `ScanUseCase` classifies Inserted / Updated / Unchanged to decide
/// whether to trigger downstream thumbnail generation and metadata
/// extraction. The outcome signal must cross the writer boundary.
///
/// WHY three inherent-method variants (`UpdateLocationStatus`,
/// `UpdateLocationPath`, `MigrateSentinelRow`) sit alongside the two
/// port-trait variants: `DbEventHandler` (desktop + CLI) calls them on
/// `Arc<SqliteFileRepository>` without going through the `FileRepository`
/// trait. Keeping them as `WriteCmd` variants means a single writer-actor
/// serializes ALL writes to `file_locations`, including the watcher's
/// status flips — no second writable connection needed.
#[derive(Debug)]
pub enum FileWriteCmd {
    /// Upsert the content-addressed `files` row keyed by `file.hash`.
    ///
    /// Binds `files.hlc` on INSERT and on UPDATE. Skips all writes on
    /// Unchanged (prior `hlc` is preserved).
    UpsertFile {
        /// Hashed file (hash + discovered metadata).
        file: HashedFile,
        /// Device that initiated the upsert.
        device: DeviceId,
        /// Reply channel carrying `Inserted / Updated / Unchanged`.
        reply: ReplyTx<UpsertOutcome>,
    },
    /// Upsert a `file_locations` row for `(volume, path)`.
    ///
    /// Binds `file_locations.hlc` on INSERT, UPDATE, and the
    /// collision-path soft-delete. Skips writes on Unchanged.
    UpsertLocation {
        /// Content hash linking to the `files` row.
        hash: BlakeHash,
        /// Volume the location lives on.
        volume: VolumeId,
        /// Relative path within the volume.
        path: MediaPath,
        /// Device that initiated the upsert.
        device: DeviceId,
        /// Reply channel carrying `Inserted / Updated / Unchanged`.
        reply: ReplyTx<UpsertOutcome>,
    },
    /// Update the status of a non-deleted `file_locations` row.
    ///
    /// WHY inherent (not port-trait): called by `DbEventHandler` in
    /// response to already-emitted `FileEvent`s from the filesystem
    /// watcher; not part of the `FileRepository` port surface.
    /// Binds `file_locations.hlc` on the UPDATE.
    UpdateLocationStatus {
        /// Volume the location lives on.
        volume: VolumeId,
        /// Relative path within the volume.
        path: MediaPath,
        /// New status value.
        status: LocationStatus,
        /// Device that initiated the update.
        device: DeviceId,
        /// Reply channel carrying `rows_changed` (`0` or `1`).
        reply: ReplyTx<u64>,
    },
    /// Rename a `file_locations` row and reset status to `active`.
    ///
    /// WHY inherent: called by `DbEventHandler` on
    /// `FileEvent::Renamed`. Binds `file_locations.hlc` on the UPDATE
    /// or collision-path soft-delete.
    UpdateLocationPath {
        /// Volume the location lives on.
        volume: VolumeId,
        /// Current (old) relative path.
        old_path: MediaPath,
        /// Target (new) relative path.
        new_path: MediaPath,
        /// Device that initiated the update.
        device: DeviceId,
        /// Reply channel carrying `rows_changed` (`0` or `1`).
        reply: ReplyTx<u64>,
    },
    /// Migrate a sentinel (`volume_id = nil-UUID`) `file_locations` row
    /// to the real volume after scan phase 1c resolves the volume.
    ///
    /// WHY inherent: called by the scan sentinel-migration closure
    /// (CLI `dispatch_scan` + desktop scan path). Not part of the
    /// `FileRepository` port surface. Binds `file_locations.hlc` on
    /// the UPDATE.
    MigrateSentinelRow {
        /// Relative path of the sentinel row to migrate.
        path: MediaPath,
        /// Resolved real volume to assign.
        real_volume: VolumeId,
        /// Device that initiated the migration.
        device: DeviceId,
        /// Reply channel carrying `rows_changed` (`0` or `1`).
        reply: ReplyTx<u64>,
    },
}

/// Search-repo write commands. Populated by Task 6.
///
/// WHY only one variant: `search` is read-only (pool); `rebuild` is the
/// sole write path — wipe + reseed the FTS5 index from source rows.
/// Per-row FTS maintenance runs via `SQLite` triggers on `file_metadata` /
/// `file_tags` / `file_locations`; those fires are captured inside the
/// respective Task 3-5 writer handlers. No additional write variants are
/// needed for search.
#[derive(Debug)]
pub enum SearchWriteCmd {
    /// Drop + reseed the FTS5 index (`search_content` table) from scratch.
    ///
    /// WHY `ReplyTx<()>`: the caller blocks until the rebuild completes
    /// (CLI `perima search --rebuild`; Desktop `search_rebuild` command).
    /// The result carries no payload — either it succeeded (`Ok(())`) or
    /// propagated a [`perima_core::CoreError`].
    Rebuild {
        /// Reply channel; writer sends `Ok(())` on success.
        reply: ReplyTx<()>,
    },
}
