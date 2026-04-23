//! `MetadataUseCase` — orchestrates file-location listing and metadata join.
//!
//! This is the `crates/app` port of the file-listing orchestration that
//! previously lived in `crates/cli/src/cmd/ls.rs` (`run`, calling
//! `FileRepository::list_file_locations` + `MetadataRepository::list_with_metadata`)
//! and `crates/desktop/src/commands.rs::{list_files_inner,
//! list_files_with_metadata_inner}`.
//!
//! Zero generics: dependency ports are carried as `Arc<dyn Port>` fields;
//! a single `async fn execute(&self, cmd: MetadataCommand) ->
//! Result<MetadataOutput, CoreError>` exposes both listing operations.
//!
//! # Why two commands (not two use-cases)
//!
//! `ListFiles` (file-locations only) and `ListFilesWithMetadata`
//! (LEFT JOIN with `file_metadata`) share the same `FileRepository` +
//! `MetadataRepository` dependency set. Splitting them into two
//! use-cases would force callers to wire two structs for what is
//! conceptually one "list files" surface. Two commands in one
//! `UseCase` is the right grain — distinct operations, shared deps.
//!
//! # `DeviceId` on commands
//!
//! Both commands carry a `device: DeviceId` field following the
//! `TagCommand::Attach/Detach` + `VolumeCommand::RecordMount` pattern:
//! callers know their device context at call-site, not at construction
//! time. v1 uses the field for future CRDT scoping; it is forwarded to
//! repo calls where applicable.
//!
//! # Pagination defaults
//!
//! When `limit` is `None`, a default of **100** rows is applied — matching
//! the CLI's `--limit` `default_value` of 100 (verified in the `ls` arg
//! parser). `offset` defaults to 0.
//!
//! See [`MetadataUseCase::execute`] for the workflow body.

use std::sync::Arc;

use perima_core::{
    CoreError, DeviceId, EventBus, FileLocationRecord, FileRepository, MediaMetadata,
    MetadataRepository,
};

/// Default page size when `limit` is `None`.
///
/// WHY 100: matches the CLI's `--limit` `default_value` in
/// `crates/cli/src/cmd/ls.rs` (`LsArgs::limit` is usize, CLI sets 100).
pub(crate) const DEFAULT_LIMIT: u32 = 100;

/// Inputs to [`MetadataUseCase::execute`].
#[derive(Debug, Clone)]
pub enum MetadataCommand {
    /// List file-location records for `device`, up to `limit` rows
    /// starting at `offset`.
    ///
    /// WHY per-command device: same rationale as `VolumeCommand::List` —
    /// the caller knows the machine context; the `UseCase` struct is
    /// shared across machines in multi-device CLI scenarios.
    ListFiles {
        /// Max rows to return; `None` applies `DEFAULT_LIMIT` (100).
        limit: Option<u32>,
        /// Row offset for pagination; `None` applies 0.
        offset: Option<u32>,
        /// Device (machine) requesting the listing.
        device: DeviceId,
    },

    /// List file-location records left-joined with `file_metadata`.
    ///
    /// Locations without a metadata row appear with `metadata: None`;
    /// callers should treat that as "pending extraction", not "absent".
    ListFilesWithMetadata {
        /// Max rows to return; `None` applies `DEFAULT_LIMIT` (100).
        limit: Option<u32>,
        /// Row offset for pagination; `None` applies 0.
        offset: Option<u32>,
        /// Device (machine) requesting the listing.
        device: DeviceId,
    },
}

/// Output of a successful metadata operation.
#[derive(Debug, Clone)]
pub enum MetadataOutput {
    /// Response to [`MetadataCommand::ListFiles`] — plain location records.
    Files(Vec<FileLocationRecord>),

    /// Response to [`MetadataCommand::ListFilesWithMetadata`] — each record
    /// paired with its optional metadata.
    ///
    /// `None` metadata means extraction is pending or failed; callers
    /// MUST NOT treat `None` as "file has no metadata" for display
    /// purposes — "pending" is the correct label.
    FilesWithMetadata(Vec<(FileLocationRecord, Option<MediaMetadata>)>),
}

/// Orchestrator: file-location listing and metadata join.
///
/// Dependencies are carried as `Arc<dyn Port>` fields; there are zero
/// generic parameters on the struct itself. See
/// [`MetadataUseCase::execute`] for the workflow body.
pub struct MetadataUseCase {
    files: Arc<dyn FileRepository>,
    metadata: Arc<dyn MetadataRepository>,
    // WHY `events` is held but unused in the orchestration body today:
    // Batch E will emit `FileEvent::Listed` (or similar) once the
    // async-broadcast bus lands. Holding the handle at construction makes
    // the Batch-E diff a single-file addition rather than a signature
    // churn across every caller. The field is silenced with a
    // `_ = &self.events` one-liner (preferred zero-cost form).
    events: Arc<dyn EventBus>,
}

impl std::fmt::Debug for MetadataUseCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetadataUseCase").finish_non_exhaustive()
    }
}

impl MetadataUseCase {
    /// Construct a `MetadataUseCase` with the given dependency ports.
    ///
    /// The container (Task 7) calls this once and shares the resulting
    /// `Arc<MetadataUseCase>` across surfaces.
    #[must_use]
    pub fn new(
        files: Arc<dyn FileRepository>,
        metadata: Arc<dyn MetadataRepository>,
        events: Arc<dyn EventBus>,
    ) -> Self {
        Self {
            files,
            metadata,
            events,
        }
    }

    /// Execute the metadata command.
    ///
    /// # Errors
    /// - [`CoreError::Internal`] on `SQLite` failures from the repository.
    // WHY allow unused_async: `FileRepository` + `MetadataRepository`
    // methods are synchronous today; the `async fn` signature is
    // mandated by the UseCase contract so the Batch-C connection-actor
    // swap (async write channel) can evolve the impl without touching
    // callers. Removing `async` now would force caller-side churn when
    // the trait gains async variants.
    #[allow(clippy::unused_async)]
    pub async fn execute(&self, cmd: MetadataCommand) -> Result<MetadataOutput, CoreError> {
        // WHY touch self.events: held for the Batch-E event-emit path;
        // reference the field so `unused` lints don't fire before
        // Batch E wires the emissions.
        let _ = &self.events;

        match cmd {
            MetadataCommand::ListFiles {
                limit,
                offset: _,
                device: _,
            } => {
                // WHY offset ignored today: `FileRepository::list_file_locations`
                // takes only `(limit, volume)`. Offset-based pagination is a
                // Batch C / post-actor concern (see GH issue for cursor
                // pagination). The parameter is accepted in the command so the
                // API surface is stable before the underlying repo gains cursor
                // support — callers won't need a signature churn.
                let effective_limit = limit.unwrap_or(DEFAULT_LIMIT) as usize;
                let records = self.files.list_file_locations(effective_limit, None)?;
                Ok(MetadataOutput::Files(records))
            }

            MetadataCommand::ListFilesWithMetadata {
                limit,
                offset: _,
                device: _,
            } => {
                // WHY offset ignored: same as ListFiles arm above.
                let effective_limit = limit.unwrap_or(DEFAULT_LIMIT) as usize;
                let rows = self.metadata.list_with_metadata(effective_limit, None)?;
                Ok(MetadataOutput::FilesWithMetadata(rows))
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use perima_core::{
        AppEvent, BlakeHash, CoreError, DeviceId, DiscoveredFile, EventBus, FileSize, HashedFile,
        MediaMetadata, MediaPath, VolumeId, VolumeIdentifiers, VolumeRepository,
    };
    use perima_db::{
        ReadPool, SqliteFileRepository, SqliteMetadataRepository, SqliteVolumeRepository,
        SqliteWriter, SqliteWriterHandle,
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

    /// Build a [`MetadataUseCase`] backed by a real `SQLite` DB in a tempdir.
    ///
    /// WHY single harness: every test uses this helper so setup is
    /// consistent and the `TempDir` lifetime is managed uniformly.
    /// Returns the use-case, the file repo (for seeding), the metadata
    /// repo (for seeding), the tempdir guard, and the writer handle
    /// (tests keep it alive so the writer thread outlives the adapter
    /// senders — post-Batch-C Task 4 the metadata adapter holds a
    /// sender tied to this writer).
    fn harness() -> (
        MetadataUseCase,
        Arc<SqliteFileRepository>,
        Arc<SqliteMetadataRepository>,
        TempDir,
        SqliteWriterHandle,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("perima.db");
        let events: Arc<dyn EventBus> = Arc::new(NullBus);
        let writer = SqliteWriter::start(&db_path, Arc::clone(&events)).unwrap();
        let reads = ReadPool::open(&db_path).unwrap();
        let files: Arc<SqliteFileRepository> =
            Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
        let metadata: Arc<SqliteMetadataRepository> =
            Arc::new(SqliteMetadataRepository::new(writer.sender(), reads));
        let uc = MetadataUseCase::new(
            Arc::clone(&files) as Arc<dyn FileRepository>,
            Arc::clone(&metadata) as Arc<dyn MetadataRepository>,
            events,
        );
        (uc, files, metadata, tmp, writer)
    }

    fn device() -> DeviceId {
        DeviceId::new()
    }

    /// Seed a single file + location into the file repository.
    ///
    /// WHY helper: repeated boilerplate for constructing a `HashedFile` +
    /// `upsert_file` + `upsert_location` across every test is noise that
    /// obscures the assertion intent.
    ///
    /// WHY two distinct hash bytes per call site: `BlakeHash::from_bytes`
    /// accepts any 32-byte array; using a counter-derived constant ensures
    /// two files in the same test don't collide on the content-addressed
    /// `files` table. The caller passes a discriminant byte.
    fn seed_file(
        repo: &SqliteFileRepository,
        dev: DeviceId,
        volume_id: VolumeId,
        rel_path: &str,
        hash_byte: u8,
    ) -> BlakeHash {
        use perima_core::{FileRepository, UpsertOutcome};

        let hash = BlakeHash::from_bytes([hash_byte; 32]);
        let file = HashedFile {
            discovered: DiscoveredFile {
                absolute_path: PathBuf::from(format!("/tmp/fake/{rel_path}")),
                relative_path: MediaPath::new(rel_path),
                size: FileSize(1024),
            },
            hash,
        };
        let outcome = repo.upsert_file(&file, dev).unwrap();
        assert!(
            matches!(outcome, UpsertOutcome::Inserted | UpsertOutcome::Updated),
            "upsert_file should succeed"
        );
        repo.upsert_location(&hash, volume_id, &MediaPath::new(rel_path), dev)
            .unwrap();
        hash
    }

    /// Create a volume id for testing (using volume repo via writer+pool).
    ///
    /// WHY a self-contained writer here: these metadata tests don't
    /// otherwise need a volume adapter in their shared harness, and the
    /// seed path is a one-shot. The writer thread is dropped when
    /// `handle.join()` runs at scope end — tempfile-backed DB stays
    /// alive via the caller's `TempDir`.
    fn seed_volume(db_path: &std::path::Path, dev: DeviceId) -> VolumeId {
        struct NoopBus;
        impl EventBus for NoopBus {
            fn emit(&self, _: &AppEvent) -> Result<(), CoreError> {
                Ok(())
            }
        }
        let writer = SqliteWriter::start(db_path, Arc::new(NoopBus)).unwrap();
        let reads = ReadPool::open(db_path).unwrap();
        let vol_repo = SqliteVolumeRepository::new(writer.sender(), reads);
        let ident = VolumeIdentifiers {
            gpt_partition_guid: None,
            fs_uuid: Some("test-uuid-meta".to_owned()),
            label: Some("MetaTestVol".to_owned()),
            capacity_bytes: 1_000_000,
            is_removable: false,
        };
        let out = vol_repo.find_or_create(&ident, dev).unwrap();
        drop(vol_repo);
        writer.join();
        out
    }

    // -----------------------------------------------------------------------
    // ListFiles
    // -----------------------------------------------------------------------

    /// `ListFiles` returns seeded file-location records.
    #[tokio::test]
    async fn list_files_returns_seeded_records() {
        let (uc, file_repo, _meta_repo, tmp, _writer) = harness();
        let dev = device();
        let vol_id = seed_volume(tmp.path().join("perima.db").as_path(), dev);

        seed_file(&file_repo, dev, vol_id, "photos/a.jpg", 0xab);

        let out = uc
            .execute(MetadataCommand::ListFiles {
                limit: None,
                offset: None,
                device: dev,
            })
            .await
            .unwrap();

        let MetadataOutput::Files(records) = out else {
            panic!("expected MetadataOutput::Files");
        };
        assert!(
            !records.is_empty(),
            "expected at least one file after seeding"
        );
        assert_eq!(
            records[0].relative_path.as_str(),
            "photos/a.jpg",
            "seeded path should appear in listing"
        );
    }

    // -----------------------------------------------------------------------
    // ListFilesWithMetadata
    // -----------------------------------------------------------------------

    /// `ListFilesWithMetadata` left-joins correctly:
    /// - Files with a seeded metadata row appear as `(record, Some(meta))`.
    /// - Files without a metadata row appear as `(record, None)`.
    #[tokio::test]
    async fn list_files_with_metadata_left_joins() {
        use perima_core::MetadataRepository;

        let (uc, file_repo, meta_repo, tmp, _writer) = harness();
        let dev = device();
        let vol_id = seed_volume(tmp.path().join("perima.db").as_path(), dev);

        // Seed two files with distinct hash bytes so content-addressed rows don't collide.
        let hash_with_meta = seed_file(&file_repo, dev, vol_id, "media/has_meta.jpg", 0xaa);
        let _hash_no_meta = seed_file(&file_repo, dev, vol_id, "media/no_meta.jpg", 0xbb);

        // Only seed metadata for the first file.
        let meta_row = MediaMetadata {
            hash: hash_with_meta,
            mime_type: Some("image/jpeg".to_owned()),
            width: Some(1920),
            height: Some(1080),
            duration_ms: None,
            captured_at: Some("2024-01-15T10:00:00Z".to_owned()),
            camera_make: Some("Canon".to_owned()),
            camera_model: Some("EOS R5".to_owned()),
            codec: None,
            bitrate_bps: None,
            thumbnail_path: None,
            thumbnail_status: None,
        };
        meta_repo.upsert_metadata(&meta_row, dev).unwrap();

        let out = uc
            .execute(MetadataCommand::ListFilesWithMetadata {
                limit: None,
                offset: None,
                device: dev,
            })
            .await
            .unwrap();

        let MetadataOutput::FilesWithMetadata(rows) = out else {
            panic!("expected MetadataOutput::FilesWithMetadata");
        };

        assert_eq!(rows.len(), 2, "both files should appear in the listing");

        // Find each row by path.
        let with_meta = rows
            .iter()
            .find(|(r, _)| r.relative_path.as_str() == "media/has_meta.jpg")
            .expect("file with metadata should appear");
        let without_meta = rows
            .iter()
            .find(|(r, _)| r.relative_path.as_str() == "media/no_meta.jpg")
            .expect("file without metadata should appear");

        assert!(
            with_meta.1.is_some(),
            "file with seeded metadata should have Some(meta)"
        );
        assert_eq!(
            with_meta.1.as_ref().unwrap().camera_model.as_deref(),
            Some("EOS R5"),
            "camera_model should match seeded value"
        );
        assert!(
            without_meta.1.is_none(),
            "file without seeded metadata should have None"
        );
    }
}
