//! `TagUseCase` — orchestrates tag CRUD and file-tag queries.
//!
//! This is the `crates/app` port of the tag orchestration that previously
//! lived in `crates/cli/src/cmd/tag.rs` (224 LOC) and
//! `crates/desktop/src/commands.rs::{list_tags_inner, attach_tag_inner,
//! detach_tag_inner, list_files_with_tags_inner}`.
//!
//! Zero generics: dependency ports are carried as `Arc<dyn Port>` fields;
//! a single `async fn execute(&self, cmd: TagCommand) ->
//! Result<TagOutput, CoreError>` exposes all four operations.
//!
//! # Why a third field (`metadata`)
//!
//! `ListFilesWithTags` merges file-location rows (from `MetadataRepository`)
//! with their attached tags (from `TagRepository`). The plan skeleton shows
//! only the two primary fields (`tags`, `events`), but the
//! `ListFilesWithTags` variant is architecturally impossible without a
//! third port. The alternative — embedding a `MetadataRepository` reference
//! inside `TagRepository` — would violate single-responsibility. Adding the
//! field here is the smallest-surprise approach; `AppContainer` (Task 7)
//! wires it from the same `AppDeps`.
//!
//! # `Attached(u64)` / `Detached(u64)` semantics
//!
//! The `TagRepository::attach` + `detach` port methods return `Result<(),
//! CoreError>` — no rows-changed signal. A successful call counts as 1
//! affected operation (idempotent re-attach is a no-op at DB level but
//! still returns `Ok(())`). The `UseCase` therefore returns `Attached(1)` /
//! `Detached(1)` for every successful invocation. This matches
//! `sqlite::changes()` semantics well enough for shell-layer logging; when
//! the Batch-C writer actor exposes an explicit rows-changed channel the
//! value can be refined. See [`TagOutput::Attached`] doc for the contract.
//!
//! See [`TagUseCase::execute`] for the workflow body.

use std::sync::Arc;

use perima_core::{
    BlakeHash, CoreError, DeviceId, EventBus, FileLocationRecord, MediaMetadata,
    MetadataRepository, Tag, TagRepository, VolumeId,
};

/// A `(file-location, optional-metadata, tags)` triple returned by
/// [`TagCommand::ListFilesWithTags`].
///
/// WHY defined here (not in `perima-core`): `perima-core` contains domain
/// types and trait ports with zero framework deps. `FileWithTags` is an
/// aggregation convenience assembled by the app layer from three separate
/// repository results; it does not belong in the domain. Desktop's
/// `FileWithTagsPayload` + CLI's table-print code derive their own
/// presentation from this struct. Compare with `SearchOutput` (also
/// app-layer, not core).
#[derive(Debug, Clone)]
pub struct FileWithTags {
    /// File location record (hash + relative path + volume + status).
    pub location: FileLocationRecord,
    /// Optional media metadata (None if extractor has not run yet).
    pub metadata: Option<MediaMetadata>,
    /// Active tags for this content hash.
    pub tags: Vec<Tag>,
}

/// Filter parameters for [`TagCommand::ListFilesWithTags`].
///
/// WHY defined in app (not core): purely an orchestration concern that
/// packs the two parameters `list_files_with_tags_inner` historically
/// took as positional args. Core ports are kept minimal; filter shapes
/// belong at the call site.
#[derive(Debug, Clone)]
pub struct TagFilter {
    /// Maximum number of files to return.
    pub limit: u32,
    /// Optional volume to restrict results to.
    pub volume: Option<VolumeId>,
}

impl Default for TagFilter {
    /// Default: up to 500 files, no volume filter.
    ///
    /// WHY 500: matches the desktop command's implicit upper-bound and avoids
    /// unbounded scans in the app layer without requiring callers to specify a
    /// limit explicitly.
    fn default() -> Self {
        Self {
            limit: 500,
            volume: None,
        }
    }
}

/// Inputs to [`TagUseCase::execute`].
#[derive(Debug, Clone)]
pub enum TagCommand {
    /// List all active (non-deleted) tags, sorted by name.
    List,

    /// Upsert a tag by `name`, then attach it to the file identified by
    /// `hash`. Idempotent — re-attaching an already-active `(hash, name)`
    /// pair is a no-op at the DB level.
    Attach {
        /// Content hash of the target file.
        hash: BlakeHash,
        /// Tag name (will be normalized via `perima_core::normalize_tag`
        /// inside `TagRepository::upsert_tag`).
        name: String,
        /// Device that initiated the operation (CRDT bookkeeping).
        device: DeviceId,
    },

    /// Look up `name` via upsert (harmless if it doesn't exist), then
    /// soft-delete the `file_tags` row linking `hash` to that tag.
    Detach {
        /// Content hash of the target file.
        hash: BlakeHash,
        /// Tag name to detach.
        name: String,
        /// Device that initiated the operation (CRDT bookkeeping).
        device: DeviceId,
    },

    /// Return files with their associated tags, optionally filtered.
    ///
    /// `None` filter → [`TagFilter::default()`] (500 rows, no volume
    /// restriction).
    ListFilesWithTags {
        /// Optional filter; defaults to 500 rows, all volumes.
        filter: Option<TagFilter>,
    },
}

/// Output of a successful tag operation.
#[derive(Debug, Clone)]
pub enum TagOutput {
    /// Response to [`TagCommand::List`] — all active tags sorted by name.
    Tags(Vec<Tag>),

    /// Response to [`TagCommand::Attach`].
    ///
    /// The `u64` is always `1` for a successful attach (matches
    /// `sqlite::changes()` semantics — see module doc for rationale).
    Attached(u64),

    /// Response to [`TagCommand::Detach`].
    ///
    /// The `u64` is always `1` for a successful detach call. A detach
    /// targeting a non-existent or already-deleted link is still
    /// `Detached(1)` at the `UseCase` boundary — the port contract returns
    /// `Ok(())` in both cases.
    Detached(u64),

    /// Response to [`TagCommand::ListFilesWithTags`].
    FilesWithTags(Vec<FileWithTags>),
}

impl TagCommand {
    /// Short kind name for tracing spans. WHY: enum Debug print is too noisy;
    /// `?cmd` would dump full bodies into spans. (Batch I Task 5.)
    pub(crate) const fn kind_str(&self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Attach { .. } => "attach",
            Self::Detach { .. } => "detach",
            Self::ListFilesWithTags { .. } => "list_files_with_tags",
        }
    }
}

/// Orchestrator: tag list, attach, detach, and file-tag queries.
///
/// Dependencies are carried as `Arc<dyn Port>` fields; there are zero
/// generic parameters on the struct itself. See [`TagUseCase::execute`]
/// for the workflow body.
pub struct TagUseCase {
    tags: Arc<dyn TagRepository>,
    metadata: Arc<dyn MetadataRepository>,
    // WHY `events` is held but unused in the orchestration body today:
    // Batch E will emit `FileEvent::TagAttached` / `TagDetached` from here
    // once the async-broadcast bus lands. Holding the handle at construction
    // makes the Batch-E diff a single-file addition rather than a signature
    // churn across every caller. The field is silenced below with a
    // `_ = &self.events` one-liner (preferred zero-cost form — no refcount
    // increment on each call, unlike `Arc::clone`).
    events: Arc<dyn EventBus>,
}

impl std::fmt::Debug for TagUseCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TagUseCase").finish_non_exhaustive()
    }
}

impl TagUseCase {
    /// Construct a `TagUseCase` with the given dependency ports.
    ///
    /// The container (Task 7) calls this once and shares the resulting
    /// `Arc<TagUseCase>` across surfaces.
    ///
    /// `metadata` is required for [`TagCommand::ListFilesWithTags`]; it merges
    /// file-location rows from `MetadataRepository` with tag rows from
    /// `TagRepository` (two-query merge in Rust — see module doc).
    #[must_use]
    pub fn new(
        tags: Arc<dyn TagRepository>,
        metadata: Arc<dyn MetadataRepository>,
        events: Arc<dyn EventBus>,
    ) -> Self {
        Self {
            tags,
            metadata,
            events,
        }
    }

    /// Execute the tag command.
    ///
    /// # Errors
    /// - [`CoreError::InvalidTag`] if `Attach`/`Detach` `name` fails
    ///   normalization (empty, whitespace-only, overlong).
    /// - [`CoreError::Internal`] on `SQLite` failures from any repository.
    // WHY allow unused_async: `TagRepository` + `MetadataRepository` methods
    // are synchronous today; the `async fn` signature is mandated by the
    // UseCase contract so the Batch-C connection-actor swap (async write
    // channel) can evolve the impl without touching callers. Removing `async`
    // now would force a caller-side churn when the trait gains async variants.
    #[allow(clippy::unused_async)]
    #[tracing::instrument(name = "tag", skip(self, cmd), fields(cmd_kind = cmd.kind_str()), err(level = "warn", Display))]
    pub async fn execute(&self, cmd: TagCommand) -> Result<TagOutput, CoreError> {
        // WHY touch self.events: held for the Batch-E event-emit path;
        // reference the field so `unused` lints don't fire before Batch E
        // wires the emissions.
        let _ = &self.events;

        match cmd {
            TagCommand::List => {
                let tags = self.tags.list_tags()?;
                Ok(TagOutput::Tags(tags))
            }

            TagCommand::Attach { hash, name, device } => {
                let tag = self.tags.upsert_tag(&name, device)?;
                self.tags.attach(&hash, tag.id, device)?;
                Ok(TagOutput::Attached(1))
            }

            TagCommand::Detach { hash, name, device } => {
                // WHY upsert_tag for detach: we need the tag's UUID to call
                // `detach`. `upsert_tag` is idempotent — if the tag doesn't
                // exist we create it (harmless), then `detach` finds no active
                // row (no-op soft-delete). Mirrors `crates/cli/src/cmd/tag.rs::
                // run_rm` rationale.
                let tag = self.tags.upsert_tag(&name, device)?;
                self.tags.detach(&hash, tag.id, device)?;
                Ok(TagOutput::Detached(1))
            }

            TagCommand::ListFilesWithTags { filter } => {
                let f = filter.unwrap_or_default();
                // WHY two queries + merge (not a shared tx): the two-SELECT
                // sequence has a benign WAL race — a `file_tags` insert between
                // calls could produce tags for a hash not in the metadata set
                // (harmless — we iterate the metadata list and look up by hash,
                // so extra tags are ignored), and a metadata delete between
                // calls leaves a stale tag entry in the map (also harmless for
                // the same reason). Transient inconsistency is acceptable for
                // UI list refresh. Mirrors `crates/desktop::commands::
                // list_files_with_tags_inner` rationale.
                let rows = self
                    .metadata
                    .list_with_metadata(f.limit as usize, f.volume)?;
                // WHY `flat_map` + `Option::iter`: post-Task-11 `loc.hash` is
                // `Option<BlakeHash>` because pending files (no `full_hash`
                // yet) carry no content address. Tag attachment in v0.6.x is
                // still keyed on `blake3_hash` (the schema pivot to
                // `file_uuid` is a follow-up — #161); rows with `None` hash
                // contribute zero tag-lookup keys and surface with empty
                // tags below. The desktop attach_tag_by_uuid command lets
                // pending files acquire tags by `file_uuid` regardless.
                let hashes: Vec<BlakeHash> = rows.iter().filter_map(|(loc, _)| loc.hash).collect();
                let tag_map = self.tags.tags_for_hashes(&hashes)?;
                let files = rows
                    .into_iter()
                    .map(|(loc, meta)| {
                        let tags = loc.hash.map_or_else(Vec::new, |h| {
                            tag_map.get(&h).cloned().unwrap_or_default()
                        });
                        FileWithTags {
                            location: loc,
                            metadata: meta,
                            tags,
                        }
                    })
                    .collect();
                Ok(TagOutput::FilesWithTags(files))
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.
mod tests {
    use perima_core::{AppEvent, BlakeHash, DeviceId};
    use perima_db::{
        ReadPool, SqliteMetadataRepository, SqliteTagRepository, SqliteWriter, SqliteWriterHandle,
    };
    use tempfile::TempDir;

    use super::*;

    /// No-op event bus for tests that don't care about emissions.
    struct NullBus;
    impl EventBus for NullBus {
        fn emit(&self, _event: &AppEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }

    /// Build a [`TagUseCase`] backed by a real `SQLite` DB in a tempdir.
    ///
    /// WHY single harness: every test uses this helper so setup is
    /// consistent and the `TempDir` lifetime is managed uniformly.
    /// The previous reviewer on `SearchUseCase` flagged inline-setup
    /// inconsistency — we avoid that here.
    ///
    /// WHY the writer handle is returned: tests must keep it alive so
    /// the writer thread outlives the `TagRepository` +
    /// `MetadataRepository` handles (post-Batch-C Tasks 3 + 4 both
    /// adapters hold a sender tied to this writer).
    fn harness() -> (TagUseCase, TempDir, SqliteWriterHandle) {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("perima.db");
        // WHY writer + pool for both repos (Tasks 3 + 4):
        // `SqliteTagRepository` + `SqliteMetadataRepository` now both
        // hold `(flume::Sender<WriteCmd>, ReadPool)`; share the same
        // writer sender + pool.
        let events: Arc<dyn EventBus> = Arc::new(NullBus);
        let writer = SqliteWriter::start(&db_path, Arc::clone(&events)).unwrap();
        let reads = ReadPool::open(&db_path).unwrap();
        let tags: Arc<dyn TagRepository> =
            Arc::new(SqliteTagRepository::new(writer.sender(), reads.clone()));
        let metadata: Arc<dyn MetadataRepository> =
            Arc::new(SqliteMetadataRepository::new(writer.sender(), reads));
        (TagUseCase::new(tags, metadata, events), tmp, writer)
    }

    fn device() -> DeviceId {
        DeviceId::new()
    }

    fn sample_hash() -> BlakeHash {
        BlakeHash::parse_hex(&"a".repeat(64)).unwrap()
    }

    // -----------------------------------------------------------------------
    // List
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn list_returns_tags_present_in_db() {
        let (uc, _tmp, _writer) = harness();
        let dev = device();

        // Seed via Attach command.
        uc.execute(TagCommand::Attach {
            hash: sample_hash(),
            name: "vacation".into(),
            device: dev,
        })
        .await
        .unwrap();

        let out = uc.execute(TagCommand::List).await.unwrap();
        let TagOutput::Tags(tags) = out else {
            panic!("expected TagOutput::Tags");
        };
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "vacation");
    }

    // -----------------------------------------------------------------------
    // Attach
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn attach_creates_link_and_returns_attached_1() {
        let (uc, _tmp, _writer) = harness();
        let dev = device();
        let hash = sample_hash();

        let out = uc
            .execute(TagCommand::Attach {
                hash,
                name: "trip".into(),
                device: dev,
            })
            .await
            .unwrap();

        assert!(
            matches!(out, TagOutput::Attached(1)),
            "expected Attached(1), got {out:?}"
        );

        // Verify the tag now appears in the DB.
        let list_out = uc.execute(TagCommand::List).await.unwrap();
        let TagOutput::Tags(tags) = list_out else {
            panic!("expected TagOutput::Tags");
        };
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "trip");
    }

    // -----------------------------------------------------------------------
    // Detach
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn detach_on_existing_link_returns_detached_1() {
        let (uc, _tmp, _writer) = harness();
        let dev = device();
        let hash = sample_hash();

        // First attach.
        uc.execute(TagCommand::Attach {
            hash,
            name: "beach".into(),
            device: dev,
        })
        .await
        .unwrap();

        let out = uc
            .execute(TagCommand::Detach {
                hash,
                name: "beach".into(),
                device: dev,
            })
            .await
            .unwrap();

        assert!(
            matches!(out, TagOutput::Detached(1)),
            "expected Detached(1), got {out:?}"
        );
    }

    // -----------------------------------------------------------------------
    // ListFilesWithTags
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn list_files_with_tags_none_filter_returns_empty_on_fresh_db() {
        let (uc, _tmp, _writer) = harness();

        let out = uc
            .execute(TagCommand::ListFilesWithTags { filter: None })
            .await
            .unwrap();

        let TagOutput::FilesWithTags(files) = out else {
            panic!("expected TagOutput::FilesWithTags");
        };
        // Fresh DB has no file_metadata rows; list_with_metadata returns [].
        assert!(files.is_empty(), "fresh DB should yield no files");
    }
}
