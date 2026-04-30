//! `ScanUseCase` — orchestrates volume scanning across walker, hasher,
//! repositories, and the metadata queue.
//!
//! This is the `crates/app` port of the orchestration that previously
//! lived in `crates/cli/src/cmd/scan.rs::run<S, H, FR, VR>` (≈450 LOC)
//! and the parallel desktop helpers. Zero generics: dependency ports
//! are carried as `Arc<dyn Port>` fields; a single
//! `async fn execute(&self, cmd: ScanCommand) -> Result<ScanReport,
//! CoreError>` exposes the workflow.
//!
//! See [`ScanUseCase::execute`] for the workflow body.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use perima_core::{
    BlakeHash, CacheEntry, CacheKey, CoreError, DeviceId, DiscoveredFile, EventBus, FileRepository,
    FileStat, HashService, HashedFile, IdentityCacheRepository, MediaPath, MetadataExtractor,
    MetadataRepository, Scanner, UpsertOutcome, VolumeId, VolumeRepository,
};
use perima_media::{
    CompositeExtractor, ImageExtractor, MetadataQueue, ThumbnailGenerator, VideoExtractor,
};
use rayon::prelude::*;
use tokio_util::sync::CancellationToken;

/// Maximum time `execute` waits for the metadata worker to drain after
/// the walk loop completes.
///
/// WHY 30 s: long enough for the typical <10-file corpus the integration
/// tests use to complete comfortably, short enough that Ctrl-C remains
/// responsive (the drain also polls cancel). `no_wait_metadata` bypasses
/// this when the caller wants a fast scan exit.
pub const METADATA_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Optional callback invoked after a successful `upsert_location`.
///
/// Signature: `(relative_path, real_volume_id, device_id)`.
///
/// WHY this survives as a command-level hook (vs. a `FileEvent`): the
/// current production caller (CLI + Desktop) wires this to
/// `perima_db::SqliteFileRepository::migrate_sentinel_row` — an
/// adapter-specific method that is NOT on the `FileRepository` trait.
/// Lifting it into a `FileEvent` variant would force every `EventBus`
/// impl to handle a new event shape and breaks the Batch-B "no public
/// API break in crates/core" constraint. When that trade-off becomes
/// worthwhile, add a `FileEvent::LocationUpserted` variant and an
/// `EventHandler` impl in the `db` adapter — the post-Batch-E `Bus`
/// makes that additive and cheap.
///
/// WHY `Arc<dyn Fn + Send + Sync>` (not `&dyn Fn`): `ScanCommand` is
/// `Clone` + passed into tokio tasks in some callers; `&dyn Fn` would
/// pin the command to a single stack frame and trip borrow-checker
/// errors the moment anyone moved it into `tokio::spawn`.
pub type OnPersist = Arc<dyn Fn(&MediaPath, VolumeId, DeviceId) + Send + Sync>;

/// Inputs to [`ScanUseCase::execute`].
#[derive(Clone)]
pub enum ScanCommand {
    /// Full scan of a root path.
    Full(FullScan),
    /// Re-scan — incremental update at the same root.
    ///
    /// WHY delegates to `Full` internally: the current orchestration
    /// body has no dedicated "rescan" branch — idempotence comes from
    /// the `INSERT OR REPLACE` semantics of the `upsert_*` methods.
    /// A user-visible rescan differs from a scan only in defaults:
    /// no metadata, no dry-run, no on-persist sentinel migration
    /// (the sentinel row is already resolved by the first scan).
    /// Collapsing to `Full { with_metadata: false, dry_run: false,
    /// on_persist: None, ... }` preserves behavior until the CRDT
    /// rescan story lands (post-v1).
    Rescan {
        /// Root directory to walk.
        path: PathBuf,
        /// Device performing the scan.
        device_id: DeviceId,
        /// Cancellation token. Test callers pass a fresh `CancellationToken::new()`.
        cancel: CancellationToken,
    },
}

impl std::fmt::Debug for ScanCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full(inner) => f.debug_tuple("Full").field(inner).finish(),
            Self::Rescan {
                path, device_id, ..
            } => f
                .debug_struct("Rescan")
                .field("path", path)
                .field("device_id", device_id)
                .finish_non_exhaustive(),
        }
    }
}

impl ScanCommand {
    /// Short kind name for tracing spans. WHY: enum Debug print is too noisy;
    /// `?cmd` would dump full bodies into spans. (Batch I Task 5.)
    pub(crate) const fn kind_str(&self) -> &'static str {
        match self {
            Self::Full(_) => "full",
            Self::Rescan { .. } => "rescan",
        }
    }

    /// Display the path inside any variant. WHY: `Full(FullScan { path, .. })`
    /// and `Rescan { path, .. }` both have a path; this surfaces it for the span.
    pub(crate) fn path_display(&self) -> impl std::fmt::Display + '_ {
        // PathRef wrapping needed to return a single concrete Display from both arms.
        struct PathRef<'a>(&'a std::path::Path);
        impl std::fmt::Display for PathRef<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.display().fmt(f)
            }
        }
        match self {
            Self::Full(f) => PathRef(&f.path),
            Self::Rescan { path, .. } => PathRef(path.as_path()),
        }
    }
}

/// Payload for [`ScanCommand::Full`].
///
// WHY allow struct_excessive_bools: each flag corresponds to a distinct
// user-facing CLI flag on `perima scan` (or a Desktop UI toggle). The
// flags are orthogonal (dry-run vs no-wait-metadata vs no-thumbnails)
// and collapsing them into a typed enum would either fuse axes or
// bloat the CLI surface. Matches the `ScanArgs` pattern in
// `crates/cli/src/cmd/scan.rs`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone)]
pub struct FullScan {
    /// Root directory to walk.
    pub path: PathBuf,
    /// Device performing the scan.
    pub device_id: DeviceId,
    /// When true, spawn the metadata queue + extract per file.
    pub with_metadata: bool,
    /// When true, hash + summarize but skip every DB write and volume
    /// detection. `file_repo` and `volume_repo` are not read.
    pub dry_run: bool,
    /// When true, skip the bounded post-walk drain of the metadata queue.
    ///
    /// WHY opt-in: by default `execute` waits up to
    /// [`METADATA_DRAIN_TIMEOUT`] for in-flight metadata extraction
    /// to persist. For very large scans where the caller would rather
    /// return immediately and let the queue die with the runtime,
    /// setting this true bypasses the drain.
    pub no_wait_metadata: bool,
    /// Disable WebP thumbnail generation for image/video files.
    ///
    /// WHY opt-in: thumbnails double the per-file work (decode + encode
    /// vs header-only read). Callers that want metadata-only indexing
    /// set this; rows stay at `thumbnail_status = 'pending'` so a
    /// future retry can generate them later. Regardless of the
    /// `ScanUseCase::thumbnailer` field supplied at construction,
    /// this flag forces `ThumbnailGenerator::disabled()` internally.
    pub no_thumbnails: bool,
    /// Cancellation token. Test callers pass a fresh
    /// `CancellationToken::new()` and never cancel.
    pub cancel: CancellationToken,
    /// Optional callback invoked after each successful location upsert.
    /// See [`OnPersist`] for the intended use (sentinel-row migration)
    /// and the WHY this is a command-level hook rather than a
    /// `FileEvent` variant.
    pub on_persist: Option<OnPersist>,
}

impl std::fmt::Debug for FullScan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FullScan")
            .field("path", &self.path)
            .field("device_id", &self.device_id)
            .field("with_metadata", &self.with_metadata)
            .field("dry_run", &self.dry_run)
            .field("no_wait_metadata", &self.no_wait_metadata)
            .field("no_thumbnails", &self.no_thumbnails)
            .field("on_persist", &self.on_persist.as_ref().map(|_| "<fn>"))
            .finish_non_exhaustive()
    }
}

/// One row of per-file scan output, surfaced to shells that want to
/// print hash/size/path lines (CLI `perima scan` default behaviour).
///
/// WHY exposed on `ScanReport` instead of emitted via `FileEvent`:
/// see the [`OnPersist`] WHY — adding a new `FileEvent` variant in
/// Batch B would cascade through every `EventBus` impl; Batch E's
/// bus-engine swap is the right place for that change. The shell
/// iterates `per_file_entries` post-execute and prints its own lines.
///
/// WHY `Serialize`: the desktop `scan` handler returns `ScanReport`
/// directly across the IPC boundary (Batch D Task 8); per-file entries
/// are skipped at the serde boundary (`#[serde(skip)]`) because the
/// frontend only needs aggregate stats. CLI shells access the field
/// directly without serde.
#[derive(Debug, Clone, Serialize)]
pub struct ScanReportEntry {
    /// Content hash of the file as hashed this run.
    pub hash: BlakeHash,
    /// File size in bytes at walk time.
    pub size: u64,
    /// Relative path within the volume.
    pub relative_path: MediaPath,
}

/// Output of a successful scan.
///
/// WHY `Serialize + specta::Type`: the desktop `scan` handler returns this
/// struct directly across the Tauri IPC boundary (Batch D Task 8).
/// Shell-internal fields (`per_file_entries`, `manifest_files`,
/// `volume_mount`) are marked `#[serde(skip)]` because the frontend
/// only needs aggregate stats; CLI shells access those fields in
/// Rust after `execute` returns.
#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct ScanReport {
    /// Total files walked + attempted to hash.
    pub files_seen: u64,
    /// Files newly inserted into `files` / `file_locations`.
    pub files_new: u64,
    /// Files present on a prior scan (Updated or Unchanged outcome).
    pub files_updated: u64,
    /// Files that errored during hash or persist.
    pub files_errored: u64,
    /// Total bytes hashed.
    pub bytes_hashed: u64,
    /// Wall-clock duration of the scan.
    pub duration_ms: u64,
    /// True if cancellation was signalled during the run.
    pub interrupted: bool,
    /// Volume label (from `VolumeIdentifiers::label`) once detected.
    pub volume_label: Option<String>,
    /// Volume id + mount point assigned this run. `None` in dry-run.
    ///
    /// WHY surfaced: the shell needs this to call
    /// `perima_db::manifest::write_manifest` after `execute`
    /// returns; `crates/app` deliberately does NOT depend on
    /// `perima-db` (spec §2 IN), so the link is plain-text rather
    /// than an intra-doc reference.
    ///
    /// WHY `#[serde(skip)]`: this is a shell-internal routing value
    /// (used by CLI + desktop to call `write_manifest`). The frontend
    /// has no use for a raw `(VolumeId, PathBuf)` tuple. Aggregate
    /// stats (`files_new`, etc.) are what the UI consumes.
    #[serde(skip)]
    pub volume_mount: Option<(VolumeId, PathBuf)>,
    /// Per-file details for shells that print a hash/size/path line.
    /// Empty for callers that only consume aggregate stats.
    ///
    /// WHY `#[serde(skip)]`: shell-internal; the frontend has no use
    /// for a per-file entry list on scan completion. CLI access is
    /// direct (no serde). Future UI needs (Batch H) would define a
    /// dedicated IPC event stream, not a bulk payload.
    #[serde(skip)]
    pub per_file_entries: Vec<ScanReportEntry>,
    /// Hashed files that were successfully persisted this run; passed
    /// by the shell to `perima_db::manifest::write_manifest` to create
    /// `.perima/manifest.db` at the volume root.
    ///
    /// WHY `#[serde(skip)]`: see `volume_mount` — manifest writing is
    /// shell-side plumbing; the frontend never inspects this list.
    #[serde(skip)]
    pub manifest_files: Vec<HashedFile>,
}

/// Orchestrator: walk → hash → persist → (optionally) extract metadata.
///
/// Dependencies are carried as `Arc<dyn Port>` fields; there are zero
/// generic parameters on the struct itself. See [`ScanUseCase::execute`]
/// for the workflow body.
pub struct ScanUseCase {
    files: Arc<dyn FileRepository>,
    volumes: Arc<dyn VolumeRepository>,
    metadata: Arc<dyn MetadataRepository>,
    // WHY `cache` is held alongside the other ports (Task 7): on every
    // file the scan loop consults the Tier-0 identity cache before any
    // hash compute (spec §4.3). Cache hits skip the file read entirely
    // — that's the v0.6.x re-scan perf landing.
    cache: Arc<dyn IdentityCacheRepository>,
    scanner: Arc<dyn Scanner>,
    hasher: Arc<dyn HashService>,
    thumbnailer: Arc<ThumbnailGenerator>,
    // WHY `events` is held but unused in the orchestration body today:
    // Batch E will emit `FileEvent::{Created, Modified}` from here
    // once the bus-engine swap lands. Holding the handle at
    // construction makes the Batch-E diff a single-file addition
    // rather than a signature churn across every caller. The field
    // goes through a `_events` mention below to quiet dead-code lints.
    events: Arc<dyn EventBus>,
}

impl std::fmt::Debug for ScanUseCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanUseCase").finish_non_exhaustive()
    }
}

/// Per-file resolution result from [`ScanUseCase::resolve_with_cache`].
///
/// Triple of `(discovered_file, blake3_hash, quick_hash)`. `quick_hash`
/// is `Some` for cache hits (`entry.quick_hash`) and cache misses (the
/// freshly-computed fingerprint). It is `None` only when a cache lookup
/// error demoted the file to the miss path and hashing was also skipped
/// (cancellation). The scan loop passes this value through to
/// `persist_file` to eagerly populate `files.quick_hash` per spec §4.1.1.
///
/// WHY type alias (not a struct): the 3-tuple is internal to this module;
/// a struct would require field names and `pub` visibility for no gain.
/// The alias suppresses `clippy::type_complexity` at all three use sites.
type ResolveEntry = Result<(DiscoveredFile, BlakeHash, Option<BlakeHash>), CoreError>;

impl ScanUseCase {
    /// Construct a `ScanUseCase` with the given dependency ports.
    ///
    /// The container (`AppContainer::new`) calls this once and shares the
    /// resulting `Arc<ScanUseCase>` across surfaces.
    ///
    /// WHY 8 ports (clippy `too_many_arguments` accepted): every field is
    /// an `Arc<dyn _>` port the scan use case must own; collapsing into a
    /// builder struct adds boilerplate without changing call sites
    /// (the container is the only production caller).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        files: Arc<dyn FileRepository>,
        volumes: Arc<dyn VolumeRepository>,
        metadata: Arc<dyn MetadataRepository>,
        cache: Arc<dyn IdentityCacheRepository>,
        scanner: Arc<dyn Scanner>,
        hasher: Arc<dyn HashService>,
        thumbnailer: Arc<ThumbnailGenerator>,
        events: Arc<dyn EventBus>,
    ) -> Self {
        Self {
            files,
            volumes,
            metadata,
            cache,
            scanner,
            hasher,
            thumbnailer,
            events,
        }
    }

    /// Execute the scan command.
    ///
    /// # Errors
    /// - `CoreError::InvalidPath` if the root does not exist or is not
    ///   a directory.
    /// - `CoreError::Io` from the canonicalization + walk path.
    /// - Propagates `CoreError` from the scanner, hasher, volume
    ///   detection, and repository adapters.
    #[tracing::instrument(
        name = "scan",
        skip(self, cmd),
        fields(scan_kind = cmd.kind_str(), path = %cmd.path_display()),
        err(level = "warn", Display)
    )]
    pub async fn execute(&self, cmd: ScanCommand) -> Result<ScanReport, CoreError> {
        match cmd {
            ScanCommand::Full(full) => self.execute_full(full).await,
            ScanCommand::Rescan {
                path,
                device_id,
                cancel,
            } => {
                // Rescan delegates to Full with defaults that match a
                // non-dry, metadata-free, sentinel-migration-free
                // re-walk. See `ScanCommand::Rescan` doc for WHY.
                self.execute_full(FullScan {
                    path,
                    device_id,
                    with_metadata: false,
                    dry_run: false,
                    no_wait_metadata: true,
                    no_thumbnails: true,
                    cancel,
                    on_persist: None,
                })
                .await
            }
        }
    }

    // WHY `#[allow(clippy::too_many_lines)]` + `cognitive_complexity`:
    // this is a faithful port of the pre-Batch-B
    // `crates/cli/src/cmd/scan.rs::run` body. Splitting the loop into
    // helpers would require threading half a dozen borrowed locals
    // (stats, queue, volume_info, manifest_files, cancel_token, entries)
    // through helper signatures — worse readability for a lint that
    // flags a body this project has shipped and hardened for several
    // releases. The extraction into `crates/app` is itself the
    // refactoring goal of Batch B; further sub-extraction is a
    // Batch-I observability concern (tracing spans per phase).
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::cognitive_complexity)]
    async fn execute_full(&self, full: FullScan) -> Result<ScanReport, CoreError> {
        let start = Instant::now();
        validate_root(&full.path)?;

        // WHY canonicalize once: on macOS, tempdir() returns
        // /var/folders/... which is a symlink to /private/var/...;
        // without canonicalizing, walkdir produces paths under /var/
        // that fail strip_prefix against /private/var/.
        let canonical_root = canonicalize_for_walk(&full.path)?;

        let FullScan {
            device_id,
            with_metadata,
            dry_run,
            no_wait_metadata,
            no_thumbnails,
            cancel,
            on_persist,
            ..
        } = full;

        // Effective thumbnailer: `no_thumbnails` forces `disabled()`
        // regardless of the field the container wired in.
        let effective_thumbnailer: Arc<ThumbnailGenerator> = if no_thumbnails {
            Arc::new(ThumbnailGenerator::disabled())
        } else {
            Arc::clone(&self.thumbnailer)
        };

        // Spawn the metadata queue up front (non-dry-run, with-metadata
        // only). WHY at the top: the worker should be alive before the
        // first `upsert_file` so the very first enqueue never races
        // `tokio::spawn`.
        let mut queue: Option<MetadataQueue> = if with_metadata && !dry_run {
            let extractor: Arc<dyn MetadataExtractor> = Arc::new(CompositeExtractor::new(vec![
                Arc::new(ImageExtractor::new()) as Arc<dyn MetadataExtractor>,
                Arc::new(VideoExtractor::new()) as Arc<dyn MetadataExtractor>,
            ]));
            Some(MetadataQueue::spawn(
                extractor,
                Arc::clone(&self.metadata),
                Arc::clone(&effective_thumbnailer),
                device_id,
                cancel.clone(),
            ))
        } else {
            None
        };

        // Resolve volume once before the scan loop (no-op in dry-run).
        // WHY outside the per-file loop: the volume-repo lock is not
        // held across rayon's parallel hash phase.
        let volume_info: Option<(VolumeId, String, PathBuf)> = if dry_run {
            None
        } else {
            let detected = perima_fs::detect_volume(&canonical_root)?;
            let label = detected
                .identifiers
                .label
                .clone()
                .unwrap_or_else(|| "unknown".to_owned());
            let vol_id = self
                .volumes
                .find_or_create(&detected.identifiers, device_id)?;
            self.volumes
                .record_mount(vol_id, device_id, &detected.mount_point)?;
            Some((vol_id, label, detected.mount_point))
        };

        // Collect up-front so rayon can parallelize hashing; the walker
        // iterator itself isn't Send across the par_iter boundary. The
        // inner `take_while` polls between yielded items so a Ctrl-C
        // during walk short-circuits quickly.
        let discovered: Vec<DiscoveredFile> = self
            .scanner
            .walk(&canonical_root, &canonical_root)?
            .take_while(|_| !cancel.is_cancelled())
            .collect();

        // Resolve hashing: Tier-0 cache first, then quick_hash on miss.
        //
        // WHY two phases (sequential cache lookup, then parallel quick_hash):
        // the cache lookup is a synchronous SELECT through the read pool;
        // serialising the lookups keeps the read-pool contention low and
        // batches all the misses into a single rayon par_iter that pays
        // its overhead once. Cache hits skip every file read by design
        // (spec §4.3) — that's the v0.6.x re-scan perf landing.
        //
        // WHY dry-run still uses `full_hash`: dry-run scans skip volume
        // detection (volume_info is None), so the cache key (which needs
        // a real volume_id) cannot be constructed. Dry-run is for
        // standalone hash printing; a future minor can add a CLI flag
        // wiring quick_hash there too. Out of scope for v0.6.x.
        let cancel_token = cancel.clone();
        // WHY 3-tuple (d, h, quick_hash): dry-run carries None for quick_hash
        // (no volume_id available so the cache key cannot be built; full_hash
        // is used instead). The cache-aware path carries Some(quick_hash) from
        // resolve_with_cache so persist_file can eagerly populate files.quick_hash
        // per spec §4.1.1 — see WHY block in resolve_with_cache.
        let results: Vec<ResolveEntry> = if dry_run {
            // Legacy dry-run path: parallel full_hash, no cache. Preserves
            // pre-Task-7 behaviour byte-for-byte.
            let hasher = Arc::clone(&self.hasher);
            discovered
                .into_par_iter()
                .map(|d| {
                    if cancel_token.is_cancelled() {
                        return Err(CoreError::Internal("cancelled".into()));
                    }
                    let h = hasher.full_hash(&d.absolute_path)?;
                    Ok((d, h, None))
                })
                .collect()
        } else {
            // Cache-aware path. `volume_info` is Some in the non-dry-run
            // arm (built unconditionally above when `!dry_run`).
            let vol_id = volume_info.as_ref().map_or_else(
                || {
                    // Defensive: should be unreachable since !dry_run
                    // implies volume_info was populated. Fall back to nil
                    // so we degrade gracefully rather than panic.
                    tracing::warn!("non-dry-run scan with no resolved volume; using nil fallback",);
                    VolumeId(uuid::Uuid::nil())
                },
                |(v, _, _)| *v,
            );
            self.resolve_with_cache(discovered, &cancel_token, device_id, vol_id)
        };

        let mut report = ScanReport {
            volume_label: volume_info.as_ref().map(|(_, label, _)| label.clone()),
            volume_mount: volume_info
                .as_ref()
                .map(|(v, _, mount)| (*v, mount.clone())),
            ..Default::default()
        };

        for res in results {
            report.files_seen += 1;
            match res {
                Ok((d, h, quick_hash)) => {
                    report.bytes_hashed += d.size.0;
                    report.per_file_entries.push(ScanReportEntry {
                        hash: h,
                        size: d.size.0,
                        relative_path: d.relative_path.clone(),
                    });
                    if dry_run {
                        // Dry-run: count every successfully hashed file
                        // as new so the summary total is accurate.
                        report.files_new += 1;
                        continue;
                    }
                    let volume = volume_info
                        .as_ref()
                        .map_or_else(|| VolumeId(uuid::Uuid::nil()), |(v, _, _)| *v);
                    match persist_file(&*self.files, &d, &h, quick_hash, device_id, volume) {
                        Ok(outcome) => {
                            // WHY: sentinel migration runs per-file,
                            // scoped to (relative_path, sentinel
                            // volume_id, deleted_at IS NULL). Running
                            // right after a successful upsert confirms
                            // the file still exists on disk before we
                            // reattribute its old row to the real
                            // volume. See `OnPersist` for the scope
                            // rationale (Batch B keeps this as a
                            // command hook; Batch E moves it behind
                            // a `FileEvent` variant).
                            if let Some(cb) = on_persist.as_ref() {
                                cb(&d.relative_path, volume, device_id);
                            }
                            // WHY enqueue only on Inserted|Updated
                            // (not Unchanged): Unchanged means the
                            // scanner already persisted this hash with
                            // identical metadata on a prior scan —
                            // re-extracting would do identical work.
                            if matches!(outcome, UpsertOutcome::Inserted | UpsertOutcome::Updated)
                                && let Some(q) = queue.as_ref()
                                && let Err(e) = q.enqueue(h, d.absolute_path.clone(), &cancel)
                            {
                                // WHY log + continue: a metadata-queue
                                // failure must not abort the scan. The
                                // caller can re-run or invoke
                                // `perima metadata` for stragglers.
                                tracing::warn!(
                                    error = %e,
                                    path = %d.absolute_path.display(),
                                    "metadata enqueue failed; continuing scan",
                                );
                            }
                            report.manifest_files.push(HashedFile {
                                discovered: d,
                                hash: h,
                            });
                            match outcome {
                                UpsertOutcome::Inserted => report.files_new += 1,
                                UpsertOutcome::Updated | UpsertOutcome::Unchanged => {
                                    report.files_updated += 1;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "persist failed");
                            report.files_errored += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "skipping file: hash failed");
                    report.files_errored += 1;
                }
            }
        }

        // Bounded drain of the metadata queue.
        //
        // WHY drop-then-await: dropping the `MetadataQueue` closes the
        // `Sender` half of the channel; the worker's `rx.recv()`
        // returns `None` once the buffer is empty and the worker exits
        // cleanly. Awaiting the `JoinHandle` with a timeout bounds the
        // wait so a stuck extractor cannot hang the caller.
        //
        // WHY `no_wait_metadata` bypasses by dropping the queue without
        // awaiting: callers that want the old fire-and-forget exit
        // semantics can opt out; stragglers fall off the runtime when
        // the caller returns.
        if let Some(mut q) = queue.take() {
            if no_wait_metadata {
                drop(q);
            } else {
                let worker = q.take_worker();
                drop(q);
                if let Some(handle) = worker {
                    match tokio::time::timeout(METADATA_DRAIN_TIMEOUT, handle).await {
                        Ok(Ok(())) => {
                            tracing::debug!("metadata queue drained cleanly");
                        }
                        Ok(Err(e)) => {
                            tracing::warn!(error = %e, "metadata worker join failed");
                        }
                        Err(_) => {
                            tracing::warn!(
                                "metadata queue did not drain within {METADATA_DRAIN_TIMEOUT:?}; \
                                 re-run `perima scan` or `perima metadata <path>` for stragglers",
                            );
                        }
                    }
                }
            }
        }

        report.interrupted = cancel.is_cancelled();
        report.duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        // Emit ScanCompleted only on successful, non-interrupted scans so the
        // frontend doesn't trigger a stale refetch when the run was aborted.
        // WHY Ok-only: a failed scan shouldn't trigger a frontend refetch.
        // WHY None-volume guard: dry-run produces no VolumeId; skip the emit
        // rather than fabricating a nil UUID for an event the frontend would
        // misinterpret as a real volume.
        if !report.interrupted
            && let Some((vol_id, _)) = report.volume_mount
        {
            let event = perima_core::AppEvent::ScanCompleted {
                volume: vol_id,
                files_seen: report.files_seen,
                files_new: report.files_new,
                duration_ms: report.duration_ms,
            };
            // WHY warn + non-fatal: bus failure (e.g. all receivers
            // dropped at shutdown) must not abort the scan completion
            // path. The scan result is already committed to SQLite.
            if let Err(e) = self.events.emit(&event) {
                tracing::warn!(error = %e, "failed to emit ScanCompleted; non-fatal");
            }
        }

        Ok(report)
    }

    /// Resolve `(DiscoveredFile, BlakeHash)` for every entry, consulting
    /// the Tier-0 identity cache before any hash compute (spec §4.3).
    ///
    /// Phase 1: stat each file + look up the cache row sequentially. Hits
    /// produce a hash directly from the cached entry; misses are pushed
    /// to a separate vec for parallel hashing in phase 2.
    ///
    /// Phase 2: rayon `par_iter` `quick_hash_prefix_suffix` on the misses.
    /// After hashing, the cache row is inserted (best-effort — logged on
    /// failure but not propagated, since the cache is a perf optimisation
    /// not a correctness gate).
    ///
    /// WHY two phases (not single `par_iter`): the cache lookup is a
    /// synchronous read-pool query; serialising lookups keeps the pool
    /// wait-times bounded and means hits never pay the rayon overhead.
    /// The misses still parallelise in phase 2 — preserves the pre-Task-7
    /// throughput on cold caches.
    ///
    /// WHY cancellation checks at every yield + at the top of each rayon
    /// closure: matches the pre-Task-7 contract — Ctrl-C stops the scan
    /// promptly, even mid-cache-resolution.
    ///
    /// WHY `blake3_hash = quick_hash` placeholder for new rows: V001
    /// declares `files.blake3_hash TEXT PRIMARY KEY NOT NULL`. Until a
    /// future V012 relaxes this to nullable (tracked GH issue post-Task-7),
    /// new rows ride a placeholder content-hash equal to the `quick_hash`
    /// fingerprint. Task 9's `compute_full_hash` later promotes the real
    /// `full_hash` (`UPDATE`s both the placeholder AND the new `full_hash`
    /// column atomically). Cache hits with `full_hash` already populated
    /// use the canonical `full_hash` directly — no placeholder needed.
    /// Each success entry is `(discovered_file, blake3_hash, quick_hash)`.
    ///
    /// `quick_hash` is the cheap prefix+suffix fingerprint used to populate
    /// `files.quick_hash` on INSERT (spec §4.1.1):
    /// - Cache hit: `Some(entry.quick_hash)` — available without re-hashing.
    /// - Cache miss: `Some(h)` — `h` is the freshly-computed `quick_hash`.
    ///
    /// Both branches are `Some`; `None` only arises on cache lookup failure
    /// where the file was demoted to the miss path and hashed anyway.
    fn resolve_with_cache(
        &self,
        discovered: Vec<DiscoveredFile>,
        cancel: &CancellationToken,
        device: DeviceId,
        volume: VolumeId,
    ) -> Vec<ResolveEntry> {
        // Phase 1: per-file stat + cache lookup. `hits` carry a (DiscoveredFile,
        // BlakeHash, quick_hash) ready to push into results; `misses` carry
        // the file + its CacheKey so phase 2 can insert the fresh cache row
        // after hashing.
        let mut results: Vec<ResolveEntry> = Vec::with_capacity(discovered.len());
        let mut misses: Vec<(DiscoveredFile, CacheKey)> = Vec::new();
        for d in discovered {
            if cancel.is_cancelled() {
                results.push(Err(CoreError::Internal("cancelled".into())));
                continue;
            }
            let stat = match self.scanner.stat_with_id(&d.absolute_path) {
                Ok(s) => s,
                Err(e) => {
                    // WHY push as Err: matches the pre-Task-7 shape — a
                    // failed hash also yielded Err in the old par_iter.
                    // The outer loop classifies these as files_errored.
                    results.push(Err(e));
                    continue;
                }
            };
            let key = make_cache_key(device, volume, stat);
            match self.cache.lookup(&key) {
                Ok(Some(entry)) => {
                    let h = cached_hash(&entry);
                    // WHY carry entry.quick_hash separately: `h` may be
                    // `full_hash` when the cache row has been promoted,
                    // but `files.quick_hash` must always store the cheap
                    // prefix+suffix fingerprint, not the full hash.
                    results.push(Ok((d, h, Some(entry.quick_hash))));
                }
                Ok(None) => misses.push((d, key)),
                Err(e) => {
                    // Lookup failure: degrade to a miss path so the file is
                    // still hashed + persisted. WHY warn (not propagate):
                    // the cache is a perf optimisation; a transient read-
                    // pool error must not abort the scan.
                    tracing::warn!(error = %e, path = %d.absolute_path.display(),
                        "Tier-0 cache lookup failed; falling back to miss path");
                    misses.push((d, key));
                }
            }
        }

        // Phase 2: parallel quick_hash_prefix_suffix on misses.
        let cancel_for_par = cancel.clone();
        let hasher = Arc::clone(&self.hasher);
        let miss_results: Vec<Result<(DiscoveredFile, BlakeHash, CacheKey), CoreError>> = misses
            .into_par_iter()
            .map(|(d, key)| {
                if cancel_for_par.is_cancelled() {
                    return Err(CoreError::Internal("cancelled".into()));
                }
                let h = hasher.quick_hash_prefix_suffix(&d.absolute_path, d.size.0)?;
                Ok((d, h, key))
            })
            .collect();

        // Phase 3: insert cache rows for the freshly-hashed misses, then
        // accumulate into results. Cache upsert errors are logged (not
        // propagated) — the file write path is the source of truth.
        for r in miss_results {
            match r {
                Ok((d, h, key)) => {
                    let entry = CacheEntry {
                        quick_hash: h,
                        full_hash: None,
                    };
                    if let Err(e) = self.cache.upsert(&key, &entry) {
                        tracing::warn!(
                            error = %e,
                            path = %d.absolute_path.display(),
                            "Tier-0 cache upsert failed; continuing scan",
                        );
                    }
                    // WHY Some(h) for miss path: h IS the quick_hash
                    // (quick_hash_prefix_suffix result). Carry it so the
                    // persist step can populate files.quick_hash eagerly.
                    results.push(Ok((d, h, Some(h))));
                }
                Err(e) => results.push(Err(e)),
            }
        }
        results
    }
}

/// Build the Tier-0 cache lookup key from the per-file stat triple.
///
/// WHY a free function (not an inherent on `CacheKey`): `CacheKey` lives
/// in `crates/core` and `FileStat` lives there too, but the conversion
/// also assigns the (device, volume) coordinates which only the use case
/// knows. Keeping the constructor here keeps `crates/core` framework-free.
const fn make_cache_key(device: DeviceId, volume: VolumeId, stat: FileStat) -> CacheKey {
    CacheKey {
        device_id: device,
        volume_id: volume,
        fs_file_id: stat.fs_file_id,
        size_bytes: stat.size_bytes,
        mtime_ns: stat.mtime_ns,
    }
}

/// Project a cached entry to the `BlakeHash` `ScanUseCase` will write.
///
/// Prefers `full_hash` when present (canonical content address); falls
/// back to `quick_hash` as a placeholder when only the cheap fingerprint
/// has been computed (the v0.6.x lazy-full-hash workflow).
const fn cached_hash(entry: &CacheEntry) -> BlakeHash {
    match entry.full_hash {
        Some(h) => h,
        None => entry.quick_hash,
    }
}

/// Persist a single hashed file: upsert the content record, then the
/// location record. Returns the location outcome so the caller can
/// classify the result as new/existing.
///
/// `quick_hash` is the cheap BLAKE3 prefix+suffix fingerprint. When
/// `Some`, the adapter populates `files.quick_hash` on INSERT per spec
/// §4.1.1, enabling `list_quick_hash_collisions` (Task 9) to return
/// candidates immediately for files scanned post-Task-7-fix.
fn persist_file(
    repo: &dyn FileRepository,
    d: &DiscoveredFile,
    h: &BlakeHash,
    quick_hash: Option<BlakeHash>,
    device: DeviceId,
    volume: VolumeId,
) -> Result<UpsertOutcome, CoreError> {
    let hf = HashedFile {
        discovered: d.clone(),
        hash: *h,
    };
    repo.upsert_file_with_quick_hash(&hf, device, quick_hash)?;
    repo.upsert_location(h, volume, &d.relative_path, device)
}

fn validate_root(root: &Path) -> Result<(), CoreError> {
    if !root.exists() {
        return Err(CoreError::InvalidPath(format!(
            "does not exist: {}",
            root.display()
        )));
    }
    if !root.is_dir() {
        return Err(CoreError::InvalidPath(format!(
            "not a directory: {}",
            root.display()
        )));
    }
    Ok(())
}

fn canonicalize_for_walk(root: &Path) -> Result<PathBuf, CoreError> {
    // WHY: routes through `perima_fs::platform_path::canonicalize` —
    // the single source of truth for the `#[cfg(windows)]` dunce /
    // std fallback.
    // WHY CoreError::from not CoreError::Io: Io is now a struct variant
    // (Batch D Task 2) so it cannot be used as a function pointer.
    perima_fs::platform_path::canonicalize(root).map_err(CoreError::from)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.
mod tests {
    use std::io::Write;
    use std::sync::Mutex;

    use perima_core::{AppEvent, MediaMetadata};
    use perima_db::{
        ReadPool, SqliteFileRepository, SqliteIdentityCacheRepository, SqliteMetadataRepository,
        SqliteVolumeRepository, SqliteWriter, SqliteWriterHandle,
    };
    use perima_fs::WalkdirScanner;
    use perima_hash::Blake3Service;
    use tempfile::TempDir;

    use super::*;

    /// No-op event bus for tests that don't care about emissions.
    struct NullBus;
    impl EventBus for NullBus {
        fn emit(&self, _event: &AppEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }

    /// In-memory metadata repo that records upserts + thumbnail calls.
    ///
    /// WHY: `SqliteMetadataRepository` requires a separate connection;
    /// a trivial mock keeps the test surface focused on whether the
    /// queue spawns, not on metadata semantics (which have their own
    /// coverage in `crates/media`).
    #[derive(Default)]
    struct RecordingMetadata {
        upserts: Mutex<Vec<BlakeHash>>,
    }
    impl MetadataRepository for RecordingMetadata {
        fn upsert_metadata(
            &self,
            meta: &MediaMetadata,
            _device: DeviceId,
        ) -> Result<UpsertOutcome, CoreError> {
            self.upserts.lock().unwrap().push(meta.hash);
            Ok(UpsertOutcome::Inserted)
        }
        fn find_by_hash(&self, _hash: &BlakeHash) -> Result<Option<MediaMetadata>, CoreError> {
            Ok(None)
        }
        fn list_with_metadata(
            &self,
            _limit: usize,
            _volume: Option<VolumeId>,
        ) -> Result<Vec<perima_core::FileWithMetadataRow>, CoreError> {
            Ok(vec![])
        }
        fn update_thumbnail(
            &self,
            _hash: &BlakeHash,
            _path: Option<&str>,
            _status: &str,
            _device: DeviceId,
        ) -> Result<u64, CoreError> {
            Ok(1)
        }
    }

    fn mk_fixture(dir: &Path) {
        for (name, content) in [
            ("alpha.txt", b"alpha" as &[u8]),
            ("sub/beta.txt", b"beta"),
            ("sub/gamma.bin", b"\x00\x01\x02\x03"),
        ] {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::File::create(&path)
                .unwrap()
                .write_all(content)
                .unwrap();
        }
    }

    struct Harness {
        // `_db_tmp` + `fixture` keep their TempDirs alive for the
        // duration of the test; dropping them would delete the DB +
        // fixture files underneath the running scan.
        _db_tmp: TempDir,
        fixture: TempDir,
        uc: ScanUseCase,
        recording_metadata: Arc<RecordingMetadata>,
        // WHY hold the writer handle: the volume adapter is wired to
        // this writer actor (post-Batch-C Task 2). Dropping the handle
        // early would close the channel before the test completes.
        _writer: SqliteWriterHandle,
    }

    fn harness() -> Harness {
        let db_tmp = tempfile::tempdir().unwrap();
        let fixture = tempfile::tempdir().unwrap();
        mk_fixture(fixture.path());

        let db_path = db_tmp.path().join("perima.db");
        let writer = SqliteWriter::start(&db_path, Arc::new(NullBus)).unwrap();
        let reads = ReadPool::open(&db_path).unwrap();

        let files: Arc<dyn FileRepository> =
            Arc::new(SqliteFileRepository::new(writer.sender(), reads.clone()));
        let volumes: Arc<dyn VolumeRepository> =
            Arc::new(SqliteVolumeRepository::new(writer.sender(), reads.clone()));
        // Use the real metadata repo for the DB so persistence tests
        // see a consistent view. A second recording mock is wired via
        // Arc dyn but held out-of-band for tests that want it.
        let _sqlite_meta: Arc<dyn MetadataRepository> = Arc::new(SqliteMetadataRepository::new(
            writer.sender(),
            reads.clone(),
        ));
        let cache: Arc<dyn perima_core::IdentityCacheRepository> =
            Arc::new(SqliteIdentityCacheRepository::new(writer.sender(), reads));
        let recording = Arc::new(RecordingMetadata::default());
        let metadata: Arc<dyn MetadataRepository> = recording.clone();

        let scanner: Arc<dyn Scanner> = Arc::new(WalkdirScanner::new());
        let hasher: Arc<dyn HashService> = Arc::new(Blake3Service::new());
        let thumbnailer = Arc::new(ThumbnailGenerator::disabled());
        let events: Arc<dyn EventBus> = Arc::new(NullBus);

        let uc = ScanUseCase::new(
            files,
            volumes,
            metadata,
            cache,
            scanner,
            hasher,
            thumbnailer,
            events,
        );
        Harness {
            _db_tmp: db_tmp,
            fixture,
            uc,
            recording_metadata: recording,
            _writer: writer,
        }
    }

    #[tokio::test]
    async fn dry_run_hashes_but_does_not_persist() {
        let h = harness();
        let cmd = ScanCommand::Full(FullScan {
            path: h.fixture.path().to_path_buf(),
            device_id: DeviceId::new(),
            with_metadata: false,
            dry_run: true,
            no_wait_metadata: true,
            no_thumbnails: true,
            cancel: CancellationToken::new(),
            on_persist: None,
        });
        let report = h.uc.execute(cmd).await.unwrap();
        assert_eq!(report.files_seen, 3, "all three fixture files walked");
        assert_eq!(report.files_new, 3, "dry-run counts hashed files as new");
        assert_eq!(report.files_errored, 0);
        assert!(
            report.bytes_hashed > 0,
            "dry-run still hashes; bytes_hashed must be non-zero",
        );
        assert!(
            report.manifest_files.is_empty(),
            "dry-run must not persist; manifest_files empty",
        );
        assert!(
            report.volume_mount.is_none(),
            "dry-run skips volume detection",
        );
        assert_eq!(
            report.per_file_entries.len(),
            3,
            "per_file_entries surfaced for every hashed file",
        );
    }

    #[tokio::test]
    async fn full_scan_persists_without_metadata() {
        let h = harness();
        let cmd = ScanCommand::Full(FullScan {
            path: h.fixture.path().to_path_buf(),
            device_id: DeviceId::new(),
            with_metadata: false, // queue must NOT spawn
            dry_run: false,
            no_wait_metadata: true,
            no_thumbnails: true,
            cancel: CancellationToken::new(),
            on_persist: None,
        });
        let report = h.uc.execute(cmd).await.unwrap();
        assert_eq!(report.files_seen, 3);
        assert_eq!(report.files_new, 3, "first scan inserts all three");
        assert_eq!(report.files_errored, 0);
        assert_eq!(
            report.manifest_files.len(),
            3,
            "each persisted file surfaces in manifest_files",
        );
        assert!(
            report.volume_mount.is_some(),
            "non-dry-run records a volume mount",
        );
        // Metadata queue did not spawn -> recording mock untouched.
        assert!(
            h.recording_metadata.upserts.lock().unwrap().is_empty(),
            "with_metadata=false must not invoke the metadata repo",
        );
    }

    #[tokio::test]
    async fn rescan_is_idempotent() {
        let h = harness();
        let device = DeviceId::new();
        let first = ScanCommand::Full(FullScan {
            path: h.fixture.path().to_path_buf(),
            device_id: device,
            with_metadata: false,
            dry_run: false,
            no_wait_metadata: true,
            no_thumbnails: true,
            cancel: CancellationToken::new(),
            on_persist: None,
        });
        let first_report = h.uc.execute(first).await.unwrap();
        assert_eq!(first_report.files_new, 3);

        let second = ScanCommand::Rescan {
            path: h.fixture.path().to_path_buf(),
            device_id: device,
            cancel: CancellationToken::new(),
        };
        let second_report = h.uc.execute(second).await.unwrap();
        assert_eq!(second_report.files_seen, 3);
        assert_eq!(
            second_report.files_new, 0,
            "Rescan over unchanged fixture must produce zero new rows",
        );
        assert_eq!(
            second_report.files_updated, 3,
            "pre-existing rows land in files_updated (Unchanged outcome)",
        );
    }

    #[tokio::test]
    async fn cancellation_short_circuits_before_persist() {
        let h = harness();
        let cancel = CancellationToken::new();
        // Cancel before execute to exercise the top-of-walk short-circuit.
        cancel.cancel();
        let cmd = ScanCommand::Full(FullScan {
            path: h.fixture.path().to_path_buf(),
            device_id: DeviceId::new(),
            with_metadata: false,
            dry_run: true,
            no_wait_metadata: true,
            no_thumbnails: true,
            cancel,
            on_persist: None,
        });
        let report = h.uc.execute(cmd).await.unwrap();
        assert!(
            report.interrupted,
            "cancel-before-execute must surface interrupted=true",
        );
        // The walker `take_while(!cancelled)` yields zero items; nothing
        // hashed -> bytes_hashed == 0.
        assert_eq!(report.bytes_hashed, 0, "pre-cancelled walk hashes nothing");
    }
}
