//! `AppContainer` — the single dependency hub CLI + Desktop + future
//! axum/plugin shells consume. Clone is cheap (all fields are Arc).
//!
//! Also owns [`CompositeEventBus`] — moved here from duplicated shell
//! copies (#69 consolidation). Stays out of `crates/core` because
//! `tracing::warn!` usage isn't allowed there.
//!
//! # Shape
//!
//! - [`AppDeps`] — flat `Arc<dyn Port>` DI struct; shells construct
//!   one directly.
//! - [`CompositeEventBus`] — fan-out `EventBus` impl; forwards each
//!   event to every wrapped handler; logs + continues on per-handler
//!   failure.
//! - [`AppContainer`] — five `Arc<UseCase>` fields + shared
//!   `Arc<dyn EventBus>`. `Clone` is cheap; axum `with_state` and
//!   Tauri `manage` both accept it trivially.

use std::sync::Arc;

use perima_core::{
    AppEvent, FileRepository, HashService, MetadataRepository, Scanner, SearchRepository,
    TagRepository, VolumeRepository, events::EventBus,
};
use perima_media::ThumbnailGenerator;

use crate::{MetadataUseCase, ScanUseCase, SearchUseCase, TagUseCase, VolumeUseCase};

// ---------------------------------------------------------------------------
// CompositeEventBus
// ---------------------------------------------------------------------------

/// Fans out events to multiple [`EventBus`] implementations.
///
/// Individual handler errors are logged but do not abort the fan-out —
/// all registered handlers always fire regardless of prior failures.
///
/// # Why this lives in `crates/app`, not `crates/core`
///
/// `CompositeEventBus` uses `tracing::warn!` which requires the
/// `tracing` crate. `crates/core` deliberately has zero framework
/// dependencies, so the composite lives in the application-service
/// layer where `tracing` is already a direct dependency. Historical
/// copies in `crates/cli/src/cmd/watch.rs` and
/// `crates/desktop/src/commands.rs` are deleted in Tasks 8 + 9 of the
/// Batch B plan (#69 consolidation).
pub struct CompositeEventBus {
    handlers: Vec<Arc<dyn EventBus>>,
}

impl std::fmt::Debug for CompositeEventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // WHY: `dyn EventBus` has no `Debug` bound; print only a
        // handler count rather than widening the trait just for logs.
        f.debug_struct("CompositeEventBus")
            .field("handlers", &self.handlers.len())
            .finish()
    }
}

impl CompositeEventBus {
    /// Construct from a list of handlers.
    #[must_use]
    pub fn new(handlers: Vec<Arc<dyn EventBus>>) -> Self {
        Self { handlers }
    }
}

impl EventBus for CompositeEventBus {
    fn emit(&self, event: &AppEvent) -> Result<(), perima_core::CoreError> {
        for h in &self.handlers {
            if let Err(e) = h.emit(event) {
                tracing::warn!(error = %e, "event handler failed");
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AppDeps
// ---------------------------------------------------------------------------

/// Flat dependency-injection struct. Shells build one, hand it to
/// [`AppContainer::new`].
///
/// # Field count
///
/// Eight fields (seven repository/service ports + the concrete
/// [`ThumbnailGenerator`]). `ScanUseCase` requires the thumbnailer for
/// post-hash thumbnail generation; since it's a concrete `Arc<T>` and
/// not a `dyn Trait` port, it rides alongside the trait-object ports
/// in this DI struct rather than being stubbed behind a port.
#[derive(Clone)]
pub struct AppDeps {
    /// File + location repository port.
    pub files: Arc<dyn FileRepository>,
    /// Volume repository port.
    pub volumes: Arc<dyn VolumeRepository>,
    /// Tag repository port.
    pub tags: Arc<dyn TagRepository>,
    /// Metadata repository port (media metadata, not filesystem meta).
    pub metadata: Arc<dyn MetadataRepository>,
    /// Search repository port (FTS5-backed in the live adapter).
    pub search: Arc<dyn SearchRepository>,
    /// Content-hash service port.
    pub hasher: Arc<dyn HashService>,
    /// Filesystem walker port.
    pub scanner: Arc<dyn Scanner>,
    /// Concrete thumbnail generator (not a port — no abstraction yet).
    pub thumbnailer: Arc<ThumbnailGenerator>,
}

impl std::fmt::Debug for AppDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // WHY: every field is a trait object / concrete adapter
        // without `Debug`; printing just the type name keeps traces
        // useful without widening any port trait.
        f.write_str("AppDeps { .. }")
    }
}

// ---------------------------------------------------------------------------
// AppContainer
// ---------------------------------------------------------------------------

/// Application-service root. axum `with_state` + Tauri `manage` both
/// accept this trivially thanks to `Clone` + `Arc` fields.
///
/// # Why `Arc<dyn EventBus>` and not `Arc<CompositeEventBus>`
///
/// Callers (and tests) may occasionally want to swap in a non-composite
/// bus (e.g., `NullBus` in unit tests, a single-handler shell if no
/// DB-side listener exists). Exposing the trait object keeps the
/// container type stable across those configurations.
#[derive(Clone)]
pub struct AppContainer {
    /// [`ScanUseCase`] — full + incremental scan orchestration.
    pub scan: Arc<ScanUseCase>,
    /// [`SearchUseCase`] — FTS5-backed full-text search.
    pub search: Arc<SearchUseCase>,
    /// [`TagUseCase`] — attach/detach/list/list-files-with-tags.
    pub tag: Arc<TagUseCase>,
    /// [`VolumeUseCase`] — create/list/delete/find-by-id/path.
    pub volume: Arc<VolumeUseCase>,
    /// [`MetadataUseCase`] — list files + attached metadata.
    pub metadata: Arc<MetadataUseCase>,
    /// Shared event bus — same `Arc` used inside every `UseCase`.
    pub events: Arc<dyn EventBus>,
    /// Direct handle to the volume repository port.
    ///
    /// WHY exposed (post-Batch-C Task 2): shell sites that need
    /// `find_or_create` (scan / watch startup) operate outside the
    /// [`VolumeUseCase`] surface — the `UseCase` deliberately only exposes
    /// `List` + `RecordMount`. Before Batch C, those sites opened a
    /// short-lived `SqliteVolumeRepository::new(conn)` with their own
    /// `rusqlite::Connection`; with the writer actor in place there is
    /// exactly one writer per process, so every shell site shares the
    /// same adapter handle via this field. The `Arc<dyn VolumeRepository>`
    /// type keeps the container decoupled from the concrete adapter
    /// (same pattern as `AppDeps::volumes`).
    pub volumes: Arc<dyn VolumeRepository>,
    /// Direct handle to the tag repository port.
    ///
    /// WHY exposed (post-Batch-C Task 3): shell sites use
    /// `TagRepository::count_files_for_tag` and `files_with_tag`
    /// directly (CLI `tag ls` counts, CLI `ls --tag` filter); those
    /// methods are not exposed through [`TagUseCase`] today. Before
    /// Batch C, each of those sites opened a short-lived
    /// `SqliteTagRepository::new(conn)`. With the writer actor owning
    /// the sole writable connection, every shell site shares one
    /// adapter handle via this field. Same pattern / rationale as
    /// `volumes` above.
    pub tags: Arc<dyn TagRepository>,
    /// Direct handle to the metadata repository port.
    ///
    /// WHY exposed (post-Batch-C Task 4): `perima metadata <path>`
    /// spawns a [`perima_media::MetadataQueue`] and clones the
    /// `Arc<dyn MetadataRepository>` into the background worker. The
    /// `MetadataUseCase` deliberately exposes only list-style commands
    /// (`ListFiles` / `ListFilesWithMetadata`) — the re-extraction flow
    /// is interactive and stays outside the use-case surface. Before
    /// Batch C, the CLI opened a short-lived
    /// `SqliteMetadataRepository::new(conn)` for the worker; with the
    /// writer actor owning the sole writable connection, every shell
    /// site shares one adapter handle via this field. Same pattern /
    /// rationale as `volumes` + `tags` above.
    pub metadata_repo: Arc<dyn MetadataRepository>,
    /// Direct handle to the file repository port.
    ///
    /// WHY exposed (post-Batch-C Task 7): CLI `tag add/rm` and
    /// `metadata <path>` resolve a filesystem path to a `BlakeHash` by
    /// calling `FileRepository::list_file_locations`. Those paths operate
    /// outside the `UseCase` surface. Each such callsite used to open a
    /// short-lived `Mutex<Connection>`-backed adapter; the writer actor
    /// now owns the sole writable connection, so every shell site shares
    /// this single adapter handle. Same pattern as `volumes`, `tags`,
    /// `metadata_repo` above.
    pub files_repo: Arc<dyn FileRepository>,
}

impl std::fmt::Debug for AppContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // WHY: UseCase structs + `dyn EventBus` lack `Debug`; keep
        // the type name so `#[tracing::instrument]` spans with
        // `state = ?container` render something useful.
        f.write_str("AppContainer { .. }")
    }
}

impl AppContainer {
    /// Build the container from flat deps + the shell's chosen
    /// event handlers.
    ///
    /// The shell injects `handlers` (e.g., a `DbEventHandler` + a
    /// `LogEventHandler` in the CLI `watch` command) and this
    /// constructor wires them into a [`CompositeEventBus`] — the
    /// single bus-construction site in the codebase after Tasks 8 + 9
    /// delete the shell-local copies (resolves #69).
    ///
    /// Pass `vec![]` to skip the DB-side listener (unit tests, dry
    /// runs, or shells that don't need volume event reaction).
    #[must_use]
    // WHY: `deps` is consumed conceptually — the shell hands its DI
    // bundle to the container at startup and the outer `AppDeps` is
    // dropped after this call. Taking `&AppDeps` would force callers
    // to keep the bundle alive pointlessly. Every field is an `Arc`,
    // so by-value move here is cheap.
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(deps: AppDeps, handlers: Vec<Arc<dyn EventBus>>) -> Arc<Self> {
        let events: Arc<dyn EventBus> = Arc::new(CompositeEventBus::new(handlers));

        let scan = Arc::new(ScanUseCase::new(
            Arc::clone(&deps.files),
            Arc::clone(&deps.volumes),
            Arc::clone(&deps.metadata),
            Arc::clone(&deps.scanner),
            Arc::clone(&deps.hasher),
            Arc::clone(&deps.thumbnailer),
            Arc::clone(&events),
        ));
        let search = Arc::new(SearchUseCase::new(
            Arc::clone(&deps.search),
            Arc::clone(&events),
        ));
        let tag = Arc::new(TagUseCase::new(
            Arc::clone(&deps.tags),
            Arc::clone(&deps.metadata),
            Arc::clone(&events),
        ));
        let volume = Arc::new(VolumeUseCase::new(
            Arc::clone(&deps.volumes),
            Arc::clone(&events),
        ));
        let metadata = Arc::new(MetadataUseCase::new(
            Arc::clone(&deps.files),
            Arc::clone(&deps.metadata),
            Arc::clone(&events),
        ));

        // WHY clone: the same `Arc<dyn VolumeRepository>` lives inside
        // `VolumeUseCase` (above) AND on the container field so shell
        // sites that need `find_or_create` can reach it without a
        // second open. Arc::clone is refcount-only; no allocation.
        let volumes = Arc::clone(&deps.volumes);
        // WHY same treatment for tags: CLI `tag ls` + `ls --tag` call
        // `count_files_for_tag` / `files_with_tag` directly — not
        // exposed by `TagUseCase`. Arc::clone is refcount-only.
        let tags = Arc::clone(&deps.tags);
        // WHY same treatment for metadata_repo: CLI `perima metadata
        // <path>` clones the adapter into a background `MetadataQueue`
        // worker; re-extraction is not exposed by `MetadataUseCase`.
        // Arc::clone is refcount-only.
        let metadata_repo = Arc::clone(&deps.metadata);
        // WHY same treatment for files_repo: CLI `tag add/rm` +
        // `metadata <path>` resolve a filesystem path → BlakeHash via
        // `FileRepository::list_file_locations`. Not exposed by any
        // UseCase. Arc::clone is refcount-only.
        let files_repo = Arc::clone(&deps.files);

        Arc::new(Self {
            scan,
            search,
            tag,
            volume,
            metadata,
            events,
            volumes,
            tags,
            metadata_repo,
            files_repo,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.
mod tests {
    use std::sync::Mutex;

    use perima_core::{AppEvent, CoreError, FileEvent, MediaPath, VolumeId};
    use perima_db::{
        ReadPool, SqliteFileRepository, SqliteMetadataRepository, SqliteSearchRepository,
        SqliteTagRepository, SqliteVolumeRepository, SqliteWriter, SqliteWriterHandle,
    };
    use perima_fs::WalkdirScanner;
    use perima_hash::Blake3Service;
    use tempfile::TempDir;

    use super::*;

    /// Records every event it receives. Used to assert fan-out.
    #[derive(Default)]
    struct RecordingBus {
        received: Mutex<Vec<AppEvent>>,
    }
    impl EventBus for RecordingBus {
        fn emit(&self, event: &AppEvent) -> Result<(), CoreError> {
            self.received.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    /// Always errors. Used to verify failure isolation.
    struct FailingBus;
    impl EventBus for FailingBus {
        fn emit(&self, _event: &AppEvent) -> Result<(), CoreError> {
            Err(CoreError::Internal("synthetic handler failure".into()))
        }
    }

    /// Build an `AppEvent::File(FileEvent::Created)` for tests.
    fn event() -> AppEvent {
        AppEvent::File(FileEvent::Created {
            path: MediaPath::new("test-fanout.bin"),
            volume: VolumeId::new(),
        })
    }

    #[test]
    fn composite_event_bus_fans_out_to_all_handlers() {
        let a = Arc::new(RecordingBus::default());
        let b = Arc::new(RecordingBus::default());
        let bus = CompositeEventBus::new(vec![
            Arc::clone(&a) as Arc<dyn EventBus>,
            Arc::clone(&b) as Arc<dyn EventBus>,
        ]);

        bus.emit(&event()).unwrap();

        assert_eq!(a.received.lock().unwrap().len(), 1);
        assert_eq!(b.received.lock().unwrap().len(), 1);
    }

    #[test]
    fn composite_event_bus_continues_after_handler_failure() {
        // WHY: even when an earlier handler errors, later handlers
        // must still fire. The composite logs via `tracing::warn!` and
        // returns `Ok(())` — we assert the recording handler ran.
        let recording = Arc::new(RecordingBus::default());
        let bus = CompositeEventBus::new(vec![
            Arc::new(FailingBus) as Arc<dyn EventBus>,
            Arc::clone(&recording) as Arc<dyn EventBus>,
        ]);

        let res = bus.emit(&event());

        assert!(res.is_ok(), "composite must swallow per-handler errors");
        assert_eq!(
            recording.received.lock().unwrap().len(),
            1,
            "recording handler must fire even after an earlier failure"
        );
    }

    #[test]
    fn composite_event_bus_with_empty_handlers_is_noop() {
        let bus = CompositeEventBus::new(vec![]);
        assert!(bus.emit(&event()).is_ok());
    }

    /// `NoopBus` used for the writer during test harness setup. The
    /// volume adapter emits no events (Task 2 hybrid state), so this
    /// handler never fires — but `SqliteWriter::start` requires an
    /// `Arc<dyn EventBus>` parameter.
    struct TestNoopBus;
    impl EventBus for TestNoopBus {
        fn emit(&self, _: &AppEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }

    /// Build `AppDeps` backed by real `SQLite` adapters on a fresh
    /// temp DB. Matches the `harness()` pattern in `scan.rs` tests.
    ///
    /// WHY the writer handle is returned: tests must keep it alive so
    /// the writer thread outlives the container's repository handles
    /// (post-Batch-C Task 2 the volume adapter holds a sender tied to
    /// this writer).
    fn deps_harness() -> (TempDir, AppDeps, SqliteWriterHandle) {
        let db_tmp = tempfile::tempdir().unwrap();
        let db_path = db_tmp.path().join("perima.db");

        let writer = SqliteWriter::start(&db_path, Arc::new(TestNoopBus)).unwrap();
        let reads = ReadPool::open(&db_path).unwrap();

        let files: Arc<dyn FileRepository> =
            Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
        let volumes: Arc<dyn VolumeRepository> =
            Arc::new(SqliteVolumeRepository::new(writer.sender(), reads.clone()));
        let tags: Arc<dyn TagRepository> =
            Arc::new(SqliteTagRepository::new(writer.sender(), reads.clone()));
        let metadata: Arc<dyn MetadataRepository> = Arc::new(SqliteMetadataRepository::new(
            writer.sender(),
            reads.clone(),
        ));
        let search: Arc<dyn SearchRepository> =
            Arc::new(SqliteSearchRepository::new(writer.sender(), reads));
        let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
        let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());
        let thumbnailer: Arc<ThumbnailGenerator> = Arc::new(ThumbnailGenerator::disabled());

        (
            db_tmp,
            AppDeps {
                files,
                volumes,
                tags,
                metadata,
                search,
                hasher,
                scanner,
                thumbnailer,
            },
            writer,
        )
    }

    #[test]
    fn app_container_new_builds_successfully_with_real_adapters() {
        let (_db_tmp, deps, _writer) = deps_harness();
        let container = AppContainer::new(deps, vec![]);

        // Arc clone must be cheap; the inner struct is shared.
        let c2 = Arc::clone(&container);
        assert!(Arc::ptr_eq(&container, &c2));

        // All five UseCases must be populated.
        assert_eq!(Arc::strong_count(&container.scan), 1);
        assert_eq!(Arc::strong_count(&container.search), 1);
        assert_eq!(Arc::strong_count(&container.tag), 1);
        assert_eq!(Arc::strong_count(&container.volume), 1);
        assert_eq!(Arc::strong_count(&container.metadata), 1);
    }

    #[test]
    fn app_container_shares_events_across_use_cases() {
        // The container's `events` field is the composite bus; each
        // UseCase receives an `Arc::clone` of it. After construction,
        // the strong count on `container.events` reflects the shared
        // ownership: 1 (container) + 5 (one per UseCase) = 6.
        let (_db_tmp, deps, _writer) = deps_harness();

        // Pass a recording handler so we can observe fan-out from the
        // container's single shared bus, if a UseCase were to emit.
        let recording = Arc::new(RecordingBus::default());
        let handlers: Vec<Arc<dyn EventBus>> = vec![Arc::clone(&recording) as Arc<dyn EventBus>];

        let container = AppContainer::new(deps, handlers);

        let events_strong = Arc::strong_count(&container.events);
        assert_eq!(
            events_strong, 6,
            "container.events should be Arc-cloned once per UseCase plus the container field"
        );

        // Direct emit through the container's bus must fan out to
        // every wrapped handler (just the one here).
        container.events.emit(&event()).unwrap();
        assert_eq!(recording.received.lock().unwrap().len(), 1);
    }
}
