//! `perima scan` implementation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use perima_core::{
    BlakeHash, CoreError, DeviceId, DiscoveredFile, FileRepository, HashService, Scanner,
    UpsertOutcome, VolumeId,
};
use rayon::prelude::*;

use crate::signals::Cancellation;

/// Arguments for the scan command.
#[derive(Debug, Clone)]
pub struct ScanArgs {
    /// Root directory to walk.
    pub root: PathBuf,
    /// When true, hashes and prints but skips all DB writes.
    pub dry_run: bool,
    /// Suppress per-file stdout lines; print summary only.
    pub quiet: bool,
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

/// Execute `scan`.
///
/// When `dry_run` is false, `repo` must be `Some`; each hashed file is
/// persisted via [`FileRepository::upsert_file`] +
/// [`FileRepository::upsert_location`]. When `dry_run` is true, pass
/// `None` — no DB writes occur.
///
/// # Errors
/// Returns `CoreError::InvalidPath` if `root` is not a directory;
/// propagates `CoreError` from hashing and walking.
pub fn run<S, H, R>(
    scanner: &S,
    hasher: &H,
    mut repo: Option<&mut R>,
    device: DeviceId,
    volume: VolumeId,
    cancel: &Cancellation,
    args: &ScanArgs,
) -> Result<(ExitCode, ScanStats), CoreError>
where
    S: Scanner + ?Sized,
    H: HashService + ?Sized,
    R: FileRepository + ?Sized,
{
    validate_root(&args.root)?;

    // WHY: canonicalize once, then use the canonical form for BOTH
    // the walk root and volume_root. On macOS, tempdir() returns
    // /var/folders/... which is a symlink to /private/var/folders/...;
    // without canonicalizing the walk root, walkdir produces paths
    // under /var/ that fail strip_prefix against /private/var/.
    let canonical_root = canonicalize_for_walk(&args.root)?;
    let volume_root = canonical_root.clone();
    let stdout = std::io::stdout();
    let mut stats = ScanStats::default();

    // Collect up-front so rayon can parallelize hashing; the walker
    // iterator itself isn't Send across the par_iter boundary. The
    // inner `take_while` polls between yielded items so a Ctrl-C
    // during walk short-circuits quickly.
    let discovered: Vec<DiscoveredFile> = scanner
        .walk(&canonical_root, &volume_root)?
        .take_while(|_| !cancel.cancelled())
        .collect();

    // Parallel hash. WHY: we also check cancellation at the top of
    // each map closure so in-flight hashes short-circuit the moment
    // Ctrl-C lands — without this, a large fixture would drain the
    // par_iter to completion even after the flag flips, defeating
    // the "Ctrl-C stops hashing" guarantee in the spec.
    let cancel_flag = cancel.token();
    let results: Vec<Result<(DiscoveredFile, BlakeHash), CoreError>> = discovered
        .into_par_iter()
        .map(|d| {
            if cancel_flag.load(Ordering::SeqCst) {
                return Err(CoreError::Internal("cancelled".into()));
            }
            let h = hasher.full_hash(&d.absolute_path)?;
            Ok((d, h))
        })
        .collect();

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
                if let Some(ref mut r) = repo {
                    match persist_file(*r, &d, &h, device, volume) {
                        Ok(outcome) => match outcome {
                            UpsertOutcome::Inserted => stats.new += 1,
                            UpsertOutcome::Updated | UpsertOutcome::Unchanged => {
                                stats.existing += 1;
                            }
                        },
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

    let interrupted = cancel.cancelled();
    let suffix = if interrupted { " (interrupted)" } else { "" };
    if args.dry_run {
        let total = stats.new + stats.existing + stats.errors;
        eprintln!("scanned {total} files (dry-run; DB not wired){suffix}");
    } else {
        let vol_str = volume.0.to_string();
        let vol_short = &vol_str[..8];
        eprintln!(
            "scanned {} files on volume {vol_short} ({} new, {} existing, {} errors){suffix}",
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
