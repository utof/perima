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

use flume::Sender;

use perima_core::CoreError;

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
#[derive(Debug)]
#[non_exhaustive]
pub enum VolumeWriteCmd {}

/// Tag-repo write commands. Populated by Task 3.
#[derive(Debug)]
#[non_exhaustive]
pub enum TagWriteCmd {}

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
