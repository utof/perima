//! `perima scan` implementation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use perima_core::{
    BlakeHash, CoreError, DeviceId, DiscoveredFile, FileRepository, HashService, HashedFile,
    MediaPath, MetadataExtractor, MetadataRepository, Scanner, UpsertOutcome, VolumeId,
    VolumeRepository,
};
use perima_media::{CompositeExtractor, ImageExtractor, MetadataQueue, VideoExtractor};
use rayon::prelude::*;

use crate::signals::Cancellation;

/// Maximum time `scan` waits for the metadata worker to drain after the
/// walk loop completes.
///
/// WHY 30 s: long enough for the typical <10-file corpus the integration
/// tests use to complete comfortably, short enough that Ctrl-C remains
/// responsive (the drain also polls cancel). `--no-wait-metadata`
/// bypasses this when the user wants fast scan exit.
pub const METADATA_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Arguments for the scan command.
#[derive(Debug, Clone)]
pub struct ScanArgs {
    /// Root directory to walk.
    pub root: PathBuf,
    /// When true, hashes and prints but skips all DB writes.
    pub dry_run: bool,
    /// Suppress per-file stdout lines; print summary only.
    pub quiet: bool,
    /// Skip the bounded post-walk drain of the metadata queue.
    ///
    /// WHY opt-in: by default `scan` waits up to
    /// [`METADATA_DRAIN_TIMEOUT`] for the in-flight metadata
    /// extraction to persist. For very large scans where the user
    /// would rather the CLI return immediately and let the queue die
    /// with the process, `--no-wait-metadata` bypasses the drain.
    pub no_wait_metadata: bool,
}

/// Scan statistics.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanStats {
    /// Files newly indexed.
    pub new: u64,
    /// Files already present (unchanged or updated).
    pub existing: u64,
    /// Files that errored during hash or persist.
    pub errors: u64,
}

/// Exit code returned to `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Completed normally.
    Success,
    /// Ctrl-C received; partial scan summarized.
    Interrupted,
}

/// Callback invoked after each successful file persist:
/// `(relative_path, real_volume_id, device_id)`.
///
/// WHY type alias: the full `Option<&dyn Fn(...)>` signature trips
/// `clippy::type_complexity`; a named alias keeps the `run` signature readable.
pub type OnPersistFn<'a> = Option<&'a dyn Fn(&MediaPath, VolumeId, DeviceId)>;

/// Execute `scan`.
///
/// When `dry_run` is false, `file_repo` and `volume_repo` must be `Some`;
/// volume detection is performed via [`perima_fs::detect_volume`], the volume
/// is resolved (or created) via [`VolumeRepository::find_or_create`], and each
/// hashed file is persisted via [`FileRepository::upsert_file`] +
/// [`FileRepository::upsert_location`]. After the persist loop,
/// `.perima/manifest.db` is written at the volume root.
///
/// `on_persist` is an optional callback invoked after each successful location
/// upsert with `(relative_path, real_volume_id, device_id)`. The production
/// caller passes a closure that calls
/// [`perima_db::SqliteFileRepository::migrate_sentinel_row`]; test callers
/// may pass `None`.
///
/// When `dry_run` is true, pass `None` for all three optional arguments —
/// no DB writes or volume detection occur.
///
/// If `metadata_repo` is `Some`, successful upserts (`Inserted` or `Updated`)
/// enqueue `(hash, absolute_path)` into a freshly spawned [`MetadataQueue`].
/// At exit the queue is drained up to [`METADATA_DRAIN_TIMEOUT`] unless
/// `args.no_wait_metadata` is set. Pass `None` (together with `None` for the
/// other repos) in dry-run mode.
///
/// # Errors
/// Returns `CoreError::InvalidPath` if `root` is not a directory;
/// propagates `CoreError` from hashing, walking, and volume detection.
//
// WHY `#[allow(clippy::future_not_send)]`: `on_persist` is typed as
// `&dyn Fn(..)` without `Sync`, so the returned future is not `Send`.
// In practice `scan::run` is only ever awaited from the CLI's main
// task — it is never moved across worker threads — so the Send bound
// is an abstract concern, not a real one. Tightening `on_persist` to
// `Sync` would break the existing callers that close over a
// `SqliteFileRepository` (Mutex<Connection> is Sync, but the closure
// itself captures an immutable borrow that doesn't add work).
//
// WHY `#[allow(clippy::cognitive_complexity)]`: the persist-loop body
// grew a single additional branch for `enqueue` per the plan. Splitting
// it into a helper would require threading half a dozen borrowed
// locals through a signature — worse readability for a lint that
// flags a one-extra-branch nested match.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
#[allow(clippy::future_not_send)]
#[allow(clippy::cognitive_complexity)]
pub async fn run<S, H, FR, VR>(
    scanner: &S,
    hasher: &H,
    mut file_repo: Option<&mut FR>,
    mut volume_repo: Option<&mut VR>,
    metadata_repo: Option<Arc<dyn MetadataRepository>>,
    on_persist: OnPersistFn<'_>,
    device: DeviceId,
    cancel: &Cancellation,
    args: &ScanArgs,
) -> Result<(ExitCode, ScanStats), CoreError>
where
    S: Scanner + ?Sized,
    H: HashService + ?Sized,
    FR: FileRepository + ?Sized,
    VR: VolumeRepository + ?Sized,
{
    validate_root(&args.root)?;

    // WHY: canonicalize once, then use the canonical form for BOTH
    // the walk root and volume_root. On macOS, tempdir() returns
    // /var/folders/... which is a symlink to /private/var/folders/...;
    // without canonicalizing the walk root, walkdir produces paths
    // under /var/ that fail strip_prefix against /private/var/.
    let canonical_root = canonicalize_for_walk(&args.root)?;
    let stdout = std::io::stdout();
    let mut stats = ScanStats::default();

    // Spawn the metadata queue up front (non-dry-run only). WHY at the
    // top: the worker should be alive before the first `upsert_file`
    // so the very first enqueue never races the `tokio::spawn`.
    //
    // WHY `Option<MetadataQueue>`: dry-run passes `None` for
    // `metadata_repo` and this stays `None` — no worker, no drain.
    let mut queue: Option<MetadataQueue> = metadata_repo.as_ref().map(|repo| {
        let extractor: Arc<dyn MetadataExtractor> = Arc::new(CompositeExtractor::new(vec![
            Arc::new(ImageExtractor::new()) as Arc<dyn MetadataExtractor>,
            Arc::new(VideoExtractor::new()) as Arc<dyn MetadataExtractor>,
        ]));
        MetadataQueue::spawn(extractor, Arc::clone(repo), device, cancel.token())
    });

    // Resolve volume once before the scan loop (no-op in dry-run).
    // WHY: detect+find_or_create happen here, outside the per-file loop, so
    // the volume repo connection is not held across rayon's parallel hash phase.
    let volume_info: Option<(VolumeId, String, PathBuf)> = if args.dry_run {
        None
    } else {
        let detected = perima_fs::detect_volume(&canonical_root)?;
        let label = detected
            .identifiers
            .label
            .clone()
            .unwrap_or_else(|| "unknown".to_owned());
        let vol_id = volume_repo
            .as_mut()
            .ok_or_else(|| CoreError::Internal("volume_repo is None in live scan".into()))?
            .find_or_create(&detected.identifiers, device)?;
        volume_repo
            .as_mut()
            .expect("volume_repo checked above")
            .record_mount(vol_id, device, &detected.mount_point)?;
        Some((vol_id, label, detected.mount_point))
    };

    // Collect up-front so rayon can parallelize hashing; the walker
    // iterator itself isn't Send across the par_iter boundary. The
    // inner `take_while` polls between yielded items so a Ctrl-C
    // during walk short-circuits quickly.
    let discovered: Vec<DiscoveredFile> = scanner
        .walk(&canonical_root, &canonical_root)?
        .take_while(|_| !cancel.cancelled())
        .collect();

    // Parallel hash. WHY: we also check cancellation at the top of
    // each map closure so in-flight hashes short-circuit the moment
    // Ctrl-C lands — without this, a large fixture would drain the
    // par_iter to completion even after the flag flips, defeating
    // the "Ctrl-C stops hashing" guarantee in the spec.
    //
    // WHY clone: CancellationToken is Arc-backed; clone is O(1) and
    // shares state with the original. The clone is moved into the
    // rayon closure which requires 'static + Send.
    let cancel_token = cancel.token();
    let results: Vec<Result<(DiscoveredFile, BlakeHash), CoreError>> = discovered
        .into_par_iter()
        .map(|d| {
            if cancel_token.is_cancelled() {
                return Err(CoreError::Internal("cancelled".into()));
            }
            let h = hasher.full_hash(&d.absolute_path)?;
            Ok((d, h))
        })
        .collect();

    // Collect successfully persisted files for the manifest write after the loop.
    let mut manifest_files: Vec<HashedFile> = Vec::new();

    let mut handle = stdout.lock();
    for res in results {
        match res {
            Ok((d, h)) => {
                if !args.quiet {
                    writeln!(
                        handle,
                        "{}  {}  {}",
                        h.to_hex(),
                        d.size.0,
                        d.relative_path.as_str()
                    )
                    .map_err(CoreError::Io)?;
                }
                if let Some(ref mut fr) = file_repo {
                    let volume = volume_info
                        .as_ref()
                        .map_or_else(|| VolumeId(uuid::Uuid::nil()), |(v, _, _)| *v);
                    match persist_file(*fr, &d, &h, device, volume) {
                        Ok(outcome) => {
                            // WHY: sentinel migration runs per-file, scoped to
                            // (relative_path, sentinel volume_id, deleted_at IS NULL).
                            // Running it right after a successful upsert confirms
                            // the file still exists on disk before we reattribute
                            // its old row to the real volume.
                            if let Some(cb) = on_persist {
                                cb(&d.relative_path, volume, device);
                            }
                            // WHY enqueue only on Inserted|Updated (not
                            // Unchanged): Unchanged means the scanner has
                            // already persisted this hash with identical
                            // metadata on a prior scan — re-extracting
                            // would do identical work. If the user wants
                            // a forced re-extract they can call
                            // `perima metadata <path>`.
                            if matches!(outcome, UpsertOutcome::Inserted | UpsertOutcome::Updated) {
                                if let Some(q) = queue.as_ref() {
                                    if let Err(e) =
                                        q.enqueue(h, d.absolute_path.clone(), &cancel.token())
                                    {
                                        // WHY log + continue: the plan is
                                        // explicit that a metadata-queue
                                        // failure must not abort the scan.
                                        // The user can always re-run or
                                        // `perima metadata` for stragglers.
                                        tracing::warn!(
                                            error = %e,
                                            path = %d.absolute_path.display(),
                                            "metadata enqueue failed; continuing scan",
                                        );
                                    }
                                }
                            }
                            manifest_files.push(HashedFile {
                                discovered: d,
                                hash: h,
                            });
                            match outcome {
                                UpsertOutcome::Inserted => stats.new += 1,
                                UpsertOutcome::Updated | UpsertOutcome::Unchanged => {
                                    stats.existing += 1;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "persist failed");
                            stats.errors += 1;
                        }
                    }
                } else {
                    // Dry-run: count every successfully hashed file as new
                    // so the summary total is accurate.
                    stats.new += 1;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "skipping file: hash failed");
                stats.errors += 1;
            }
        }
    }
    drop(handle);

    // Write manifest after the persist loop (non-dry-run only).
    if let Some((vol_id, _, ref mount_point)) = volume_info {
        perima_db::manifest::write_manifest(mount_point, vol_id, &manifest_files)?;
    }

    // Bounded drain of the metadata queue.
    //
    // WHY drop-then-await: dropping the `MetadataQueue` closes the
    // `Sender` half of the channel; the worker's `rx.recv()` returns
    // `None` once the buffer is empty and the worker exits cleanly.
    // Awaiting the `JoinHandle` with a timeout bounds the wait so a
    // stuck extractor cannot hang the CLI.
    //
    // WHY `--no-wait-metadata` bypasses by dropping the queue without
    // awaiting: users who scripted `perima scan` around v0.3's
    // fire-and-forget exit semantics can opt out; stragglers fall off
    // the tokio runtime when `main` returns.
    if let Some(mut q) = queue.take() {
        if args.no_wait_metadata {
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

    let interrupted = cancel.cancelled();
    let suffix = if interrupted { " (interrupted)" } else { "" };
    if args.dry_run {
        let total = stats.new + stats.existing + stats.errors;
        eprintln!("scanned {total} files (dry-run; DB not wired){suffix}");
    } else {
        let label_or_id = volume_info.as_ref().map_or_else(
            || "?".to_owned(),
            |(vol_id, label, _)| {
                if label == "unknown" || label.is_empty() {
                    let s = vol_id.0.to_string();
                    s[..8].to_owned()
                } else {
                    label.clone()
                }
            },
        );
        eprintln!(
            "scanned {} files on volume {label_or_id} ({} new, {} existing, {} errors){suffix}",
            stats.new + stats.existing + stats.errors,
            stats.new,
            stats.existing,
            stats.errors
        );
    }

    Ok((
        if interrupted {
            ExitCode::Interrupted
        } else {
            ExitCode::Success
        },
        stats,
    ))
}

/// Persist a single hashed file: upsert the content record, then the
/// location record. Returns the location outcome so the caller can
/// classify the result as new/existing.
fn persist_file<R: FileRepository + ?Sized>(
    repo: &mut R,
    d: &DiscoveredFile,
    h: &BlakeHash,
    device: DeviceId,
    volume: VolumeId,
) -> Result<UpsertOutcome, CoreError> {
    let hf = perima_core::HashedFile {
        discovered: d.clone(),
        hash: *h,
    };
    repo.upsert_file(&hf, device)?;
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
    // dunce::canonicalize avoids UNC prefixes on Windows.
    dunce::canonicalize(root).map_err(CoreError::Io)
}
