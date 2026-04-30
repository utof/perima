//! `AppContainer` — the single dependency hub CLI + Desktop + future
//! axum/plugin shells consume. Clone is cheap (all fields are Arc).
//!
//! # Shape
//!
//! - [`AppDeps`] — flat `Arc<dyn Port>` DI struct; shells construct
//!   one directly.
//! - [`AppContainer`] — five `Arc<UseCase>` fields + shared
//!   `Arc<dyn EventBus>` (a [`Bus`] under the hood). `Clone` is cheap;
//!   axum `with_state` and Tauri `manage` both accept it trivially.
//!
//! # Event-bus wiring (Batch E Task 6)
//!
//! [`AppContainer::new`] builds a single [`Bus`] (the canonical
//! single-construction-site invariant from Batch B), assigns
//! `bus.clone()` to `events` (the `Arc<dyn EventBus>` shared with every
//! `UseCase`), and spawns one tokio task per registered
//! [`EventHandler`] running `crate::events::recv_loop`. Each task
//! owns its own broadcast `Receiver` cursor; tasks exit when the bus
//! closes (container drop).

use std::sync::Arc;

use perima_core::{
    FileRepository, HashService, IdentityCacheRepository, MetadataRepository, Scanner,
    SearchRepository, TagRepository, VolumeRepository, events::EventBus,
};
use perima_media::ThumbnailGenerator;

use crate::{
    Bus, ComputeFullHashUseCase, DedupUseCase, MetadataUseCase, ScanUseCase, SearchUseCase,
    TagUseCase, VolumeUseCase, events::EventHandler,
};

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
    /// Tier-0 identity-cache repository port (device-local; stores
    /// `quick_hash` and optional `full_hash` keyed on the tuple
    /// `(device, volume, fs_file_id, size, mtime_ns)`). Wired into
    /// `ScanUseCase` so re-scans skip rehashing unchanged files
    /// (spec §4.3 — the v0.6.x perf landing).
    pub identity_cache: Arc<dyn IdentityCacheRepository>,
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
/// # Why `Arc<dyn EventBus>` and not `Arc<Bus>`
///
/// Callers (and tests) may occasionally want to swap in a non-`Bus`
/// implementor (e.g., a stub for unit tests). Exposing the trait
/// object keeps the container type stable across those configurations.
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
    /// [`ComputeFullHashUseCase`] — on-demand full-hash compute (single + batch).
    pub compute_full_hash: Arc<ComputeFullHashUseCase>,
    /// [`DedupUseCase`] — quick-hash collision listing + verified-distinct flips.
    pub dedup: Arc<DedupUseCase>,
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
    /// Direct handle to the content-hash service port.
    ///
    /// WHY exposed (Task 8 backfill): the backfill worker needs the same
    /// `Arc<dyn HashService>` that `ScanUseCase` holds internally so it
    /// can compute `quick_hash_prefix_suffix` without re-constructing a
    /// `Blake3Service`. The field is a refcount clone — zero allocation.
    pub hasher: Arc<dyn HashService>,
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
    /// Wiring (Batch E Task 6):
    ///
    /// 1. Constructs a single [`Bus`] — the canonical
    ///    single-construction-site invariant carried over from Batch B's
    ///    `CompositeEventBus`.
    /// 2. Sets `events = bus.clone()` (coerces `Arc<Bus>` to
    ///    `Arc<dyn EventBus>` because `Bus: EventBus`).
    /// 3. For each handler: subscribes a fresh `Receiver` and
    ///    `tokio::spawn`s `crate::events::recv_loop` for it. Each task
    ///    runs until the bus closes (container drop).
    ///
    /// Pass `vec![]` to skip listeners (unit tests, dry runs, or shells
    /// that don't need any event reaction).
    ///
    /// # Panics
    ///
    /// Must be called from within a tokio runtime context — `tokio::spawn`
    /// requires it. Both shells (CLI `#[tokio::main]`, Desktop via
    /// Tauri's runtime) satisfy this.
    #[must_use]
    // WHY: `deps` is consumed conceptually — the shell hands its DI
    // bundle to the container at startup and the outer `AppDeps` is
    // dropped after this call. Taking `&AppDeps` would force callers
    // to keep the bundle alive pointlessly. Every field is an `Arc`,
    // so by-value move here is cheap.
    #[allow(clippy::needless_pass_by_value)]
    pub fn new(deps: AppDeps, handlers: Vec<Box<dyn EventHandler>>) -> Arc<Self> {
        // Single Bus construction site — spec §2.1 + Batch B/C invariant.
        let bus: Arc<Bus> = Bus::new();
        let events: Arc<dyn EventBus> = bus.clone();

        // Spawn one tokio task per handler. Each task owns its own
        // Receiver and runs the shared recv_loop until the bus closes
        // (container drop, when all Sender clones release).
        for handler in handlers {
            let name = handler.name();
            let recv = bus.subscribe();
            tokio::spawn(crate::events::recv_loop(name, handler, recv));
        }

        let scan = Arc::new(ScanUseCase::new(
            Arc::clone(&deps.files),
            Arc::clone(&deps.volumes),
            Arc::clone(&deps.metadata),
            Arc::clone(&deps.identity_cache),
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
        let compute_full_hash = Arc::new(ComputeFullHashUseCase::new(
            Arc::clone(&deps.hasher),
            Arc::clone(&deps.files),
            Arc::clone(&events),
        ));
        let dedup = Arc::new(DedupUseCase::new(
            Arc::clone(&deps.files),
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
        // WHY same treatment for hasher: the backfill worker (Task 8)
        // needs `quick_hash_prefix_suffix` without re-constructing
        // a Blake3Service. Arc::clone is refcount-only.
        let hasher = Arc::clone(&deps.hasher);

        Arc::new(Self {
            scan,
            search,
            tag,
            volume,
            metadata,
            compute_full_hash,
            dedup,
            events,
            volumes,
            tags,
            metadata_repo,
            files_repo,
            hasher,
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
    use std::time::Duration;

    use perima_core::{AppEvent, FileEvent, MediaPath, VolumeId};
    use perima_db::{
        ReadPool, SqliteFileRepository, SqliteIdentityCacheRepository, SqliteMetadataRepository,
        SqliteSearchRepository, SqliteTagRepository, SqliteVolumeRepository, SqliteWriter,
        SqliteWriterHandle,
    };
    use perima_fs::WalkdirScanner;
    use perima_hash::Blake3Service;
    use tempfile::TempDir;

    use super::*;

    /// Records every event it receives. Used to assert fan-out via the
    /// new `Bus` + `EventHandler` wiring. Async `handle` matches the
    /// post-Task-6 trait shape (Batch E §2.2).
    struct RecordingHandler {
        received: Arc<Mutex<Vec<AppEvent>>>,
    }

    #[async_trait::async_trait]
    impl EventHandler for RecordingHandler {
        fn name(&self) -> &'static str {
            "recording_handler"
        }

        async fn handle(&mut self, event: AppEvent) {
            self.received.lock().unwrap().push(event);
        }
    }

    /// Build an `AppEvent::File(FileEvent::Created)` for tests.
    fn event() -> AppEvent {
        AppEvent::File(FileEvent::Created {
            path: MediaPath::new("test-fanout.bin"),
            volume: VolumeId::new(),
            file_uuid: None,
        })
    }

    /// Build `AppDeps` backed by real `SQLite` adapters on a fresh
    /// temp DB. Matches the `harness()` pattern in `scan.rs` tests.
    ///
    /// WHY the writer handle is returned: tests must keep it alive so
    /// the writer thread outlives the container's repository handles
    /// (post-Batch-C Task 2 the volume adapter holds a sender tied to
    /// this writer).
    ///
    /// WHY a fresh `Bus` is passed to `SqliteWriter::start`: the writer
    /// needs an `Arc<dyn EventBus>` to publish post-COMMIT events. In
    /// tests we don't observe those events through this bus — the
    /// container builds its own `Bus` internally — but the writer still
    /// requires a live sink. A bare `Bus` with no subscribers acts as a
    /// no-op (events are queued in the ring buffer but never consumed).
    fn deps_harness() -> (TempDir, AppDeps, SqliteWriterHandle) {
        let db_tmp = tempfile::tempdir().unwrap();
        let db_path = db_tmp.path().join("perima.db");

        let writer_bus: Arc<dyn EventBus> = Bus::new();
        let writer = SqliteWriter::start(&db_path, writer_bus).unwrap();
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
            Arc::new(SqliteSearchRepository::new(writer.sender(), reads.clone()));
        let identity_cache: Arc<dyn perima_core::IdentityCacheRepository> =
            Arc::new(SqliteIdentityCacheRepository::new(writer.sender(), reads));
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
                identity_cache,
                hasher,
                scanner,
                thumbnailer,
            },
            writer,
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn app_container_new_builds_successfully_with_real_adapters() {
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

    #[tokio::test(flavor = "multi_thread")]
    async fn app_container_shares_events_across_use_cases() {
        // The container's `events` field is the single shared `Bus`;
        // each UseCase receives an `Arc::clone` of it. After construction,
        // the strong count on `container.events` reflects the shared
        // ownership: 1 (container) + 7 (one per UseCase: scan, search, tag,
        // volume, metadata, compute_full_hash, dedup) = 8.
        let (_db_tmp, deps, _writer) = deps_harness();

        // Pass a recording handler so we can observe fan-out from the
        // container's single shared bus when a UseCase emits.
        let received = Arc::new(Mutex::new(Vec::<AppEvent>::new()));
        let handler: Box<dyn EventHandler> = Box::new(RecordingHandler {
            received: Arc::clone(&received),
        });
        let container = AppContainer::new(deps, vec![handler]);

        let events_strong = Arc::strong_count(&container.events);
        assert_eq!(
            events_strong, 8,
            "container.events should be Arc-cloned once per UseCase plus the container field"
        );

        // Direct emit through the container's bus must reach every
        // spawned handler task. The recv_loop is async, so yield long
        // enough for the spawned task to drain the broadcast queue.
        container.events.emit(&event()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(received.lock().unwrap().len(), 1);
    }
}
