//! Tag repository port (implementation lands in `perima-db`).

use std::collections::HashMap;

use crate::{BlakeHash, CoreError, DeviceId, Tag};

/// Persistence boundary for `tags` + `file_tags`.
///
/// WHY `&self` (not `&mut self`): matches the `MetadataRepository`
/// pattern — interior mutability via `Mutex<Connection>` inside the
/// adapter. Allows `Arc<SqliteTagRepository>` sharing in desktop state.
pub trait TagRepository: Send + Sync {
    /// Insert or look up a tag by its normalized name. Returns the
    /// (possibly pre-existing) tag. Name is normalized via
    /// `perima_core::tag::normalize` before persistence.
    ///
    /// # Errors
    /// [`CoreError::InvalidTag`] on name normalization failure.
    /// Adapter-level failures surface as [`CoreError::Internal`].
    fn upsert_tag(&self, name: &str, device: DeviceId) -> Result<Tag, CoreError>;

    /// Soft-delete a tag by id. Attachments in `file_tags` are
    /// **not** deleted — CRDT semantics preserve link history.
    ///
    /// # Errors
    /// Adapter-level failures surface as [`CoreError::Internal`].
    fn delete_tag(&self, tag_id: uuid::Uuid, device: DeviceId) -> Result<(), CoreError>;

    /// Attach a tag to a content hash. Idempotent — re-attaching an
    /// already-active `(hash, tag_id)` pair is a no-op.
    ///
    /// # Errors
    /// Adapter-level failures surface as [`CoreError::Internal`].
    fn attach(
        &self,
        hash: &BlakeHash,
        tag_id: uuid::Uuid,
        device: DeviceId,
    ) -> Result<(), CoreError>;

    /// Remove a tag from a content hash (soft-delete the `file_tags`
    /// row).
    ///
    /// # Errors
    /// Adapter-level failures surface as [`CoreError::Internal`].
    fn detach(
        &self,
        hash: &BlakeHash,
        tag_id: uuid::Uuid,
        device: DeviceId,
    ) -> Result<(), CoreError>;

    /// List all active (non-deleted) tags, sorted by name.
    ///
    /// # Errors
    /// Adapter-level failures surface as [`CoreError::Internal`].
    fn list_tags(&self) -> Result<Vec<Tag>, CoreError>;

    /// Batch-fetch tags for a set of content hashes. Returns a map
    /// from hash to its active tags.
    ///
    /// **MUST short-circuit on empty `hashes`** — `SQL IN ()` is a
    /// parse error in `SQLite`, not just an empty result.
    ///
    /// # Errors
    /// Adapter-level failures surface as [`CoreError::Internal`].
    fn tags_for_hashes(
        &self,
        hashes: &[BlakeHash],
    ) -> Result<HashMap<BlakeHash, Vec<Tag>>, CoreError>;

    /// Return content hashes that carry a given tag.
    ///
    /// WHY this method exists despite the spec saying "no
    /// `files_with_tag`": the CLI's `ls --tag` command needs
    /// server-side filtering — it cannot trivially post-filter the
    /// existing `list_file_locations` output. The desktop layer does
    /// NOT use this method; it filters client-side from the merged
    /// `list_files_with_tags` response.
    ///
    /// # Errors
    /// Adapter-level failures surface as [`CoreError::Internal`].
    fn files_with_tag(&self, tag_id: uuid::Uuid) -> Result<Vec<BlakeHash>, CoreError>;

    /// Count active attachments for a tag. Used by CLI `tag ls` to
    /// show per-tag file counts without fetching full hash lists.
    ///
    /// # Errors
    /// Adapter-level failures surface as [`CoreError::Internal`].
    fn count_files_for_tag(&self, tag_id: uuid::Uuid) -> Result<u64, CoreError>;
}
