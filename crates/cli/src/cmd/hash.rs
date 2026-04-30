//! `perima hash` subcommand — on-demand full-hash computation for indexed files.
//!
//! Three modes:
//! - `perima hash <path>` — compute + print full BLAKE3 hash for one file.
//! - `perima hash --all` — compute full hash for every indexed file (opt-in slow).
//! - `perima hash --pending` — compute full hash only for files whose
//!   `blake3_hash` is `NULL` (not yet computed, per V011 nullable column).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use perima_app::AppContainer;
use perima_core::{CoreError, DeviceId, DeviceKind, FileUuid, FullHashUnavailableReason};

use super::metadata::find_by_absolute_suffix;

/// Arguments for the `perima hash` command.
#[derive(clap::Args, Debug)]
pub(crate) struct HashArgs {
    /// Path of a single file to hash. Exclusive with --all and --pending.
    pub path: Option<PathBuf>,

    /// Compute full hash for every indexed file. Sequential; may be slow on
    /// large libraries. Exclusive with --pending and a positional path.
    #[arg(long, conflicts_with_all = ["path", "pending"])]
    pub all: bool,

    /// Compute full hash only for files whose `full_hash` is not yet set.
    /// Uses the pending-full-hash list to find candidates.
    /// Exclusive with --all and a positional path.
    #[arg(long, conflicts_with_all = ["path", "all"])]
    pub pending: bool,
}

/// Execute `perima hash <args>`.
///
/// # Errors
/// Returns [`CoreError::InvalidPath`] when the file does not exist or is not
/// indexed; propagates [`CoreError`] from DB and hash operations.
pub(crate) async fn run(
    container: &AppContainer,
    _data_dir: &Path,
    _device: DeviceId,
    args: &HashArgs,
) -> Result<(), CoreError> {
    if let Some(path) = &args.path {
        run_single(container, path)
    } else if args.all {
        run_all(container).await
    } else if args.pending {
        run_pending(container).await
    } else {
        Err(CoreError::InvalidPath(
            "perima hash: specify a <path>, --all, or --pending".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Single file
// ---------------------------------------------------------------------------

/// Resolve a path to a [`FileUuid`] by scanning all indexed locations and
/// suffix-matching against the canonicalized absolute path.
///
/// WHY suffix match: same reasoning as `cmd::metadata::find_by_absolute_suffix`
/// — the scanner stores paths relative to the scan root, so matching on the
/// suffix of the absolute path is the only portable resolution strategy without
/// knowing the original scan root.
fn resolve_file_uuid(container: &AppContainer, path: &Path) -> Result<FileUuid, CoreError> {
    if !path.exists() {
        return Err(CoreError::InvalidPath(format!(
            "does not exist: {}",
            path.display()
        )));
    }
    if !path.is_file() {
        return Err(CoreError::InvalidPath(format!(
            "not a file: {}",
            path.display()
        )));
    }

    let absolute = perima_fs::platform_path::canonicalize(path).map_err(CoreError::from)?;
    let absolute_str = absolute
        .to_str()
        .ok_or_else(|| CoreError::InvalidPath(format!("non-UTF8 path: {}", absolute.display())))?;

    // WHY list across ALL volumes (None): the user supplies an absolute path
    // that may live on any known volume; suffix-matching across the full
    // location set is the only portable approach.
    let records = container.files_repo.list_file_locations(usize::MAX, None)?;
    let record = find_by_absolute_suffix(&records, absolute_str).ok_or_else(|| {
        CoreError::InvalidPath(format!(
            "not indexed: {} (run `perima scan` first)",
            absolute.display(),
        ))
    })?;

    Ok(record.file_uuid)
}

/// `perima hash <path>` — compute + print full hash for a single file.
///
/// WHY compute directly here instead of via `execute_single`: `execute_single`
/// looks up the on-disk path through `volume_mounts` so it can work without a
/// user-supplied path. For the CLI case the user has already given us the path,
/// so we skip the mount-point reconstruction (which requires a correctly seeded
/// `volume_mounts` row) and hash the supplied path directly. We then call
/// `update_full_hash` to persist the result, mirroring what `execute_single`
/// does internally. This avoids "no such file" errors caused by
/// `mount_point + relative_path` reconstruction when the scan root ≠ mount.
fn run_single(container: &AppContainer, path: &Path) -> Result<(), CoreError> {
    let absolute = perima_fs::platform_path::canonicalize(path).map_err(CoreError::from)?;

    // Get the file size for the dispatch decision.
    let meta = std::fs::metadata(&absolute).map_err(CoreError::from)?;
    let size_bytes = meta.len();

    // Resolve the file_uuid so we can persist the result.
    let file_uuid = resolve_file_uuid(container, path)?;

    // Hash the file directly using the container's hasher.
    // WHY DeviceKind::Unknown: same rationale as `ComputeFullHashUseCase::compute_one`.
    let hash = container
        .hasher
        .full_hash_dispatched(&absolute, size_bytes, DeviceKind::Unknown)
        .map_err(|e| perima_core::CoreError::FullHashUnavailable {
            reason: FullHashUnavailableReason::IoError {
                message: e.to_string(),
            },
        })?;

    // Persist the full hash.
    container.files_repo.update_full_hash(file_uuid, hash)?;

    println!("{}", hash.to_hex());
    Ok(())
}

// ---------------------------------------------------------------------------
// --all
// ---------------------------------------------------------------------------

/// `perima hash --all` — iterate every indexed file and compute its full hash.
///
/// WHY sequential (not parallel): avoids parallel disk thrash on HDDs.
/// Spec §4.5 mandates sequential full-hash computation per batch. Power
/// users who want parallelism can use `compute_full_hash_batch` via the
/// desktop route or build a shell pipe over `perima hash <path>`.
async fn run_all(container: &AppContainer) -> Result<(), CoreError> {
    // WHY list_file_locations(MAX, None): the simplest way to get all
    // file_uuids without introducing a dedicated "list all uuids" port
    // method. The result may contain duplicate file_uuids when a file has
    // multiple active locations; we dedup below.
    let records = container.files_repo.list_file_locations(usize::MAX, None)?;

    // Dedup: keep unique file_uuids only.
    let mut seen = HashSet::new();
    let uuids: Vec<FileUuid> = records
        .into_iter()
        .filter_map(|r| {
            if seen.insert(r.file_uuid) {
                Some(r.file_uuid)
            } else {
                None
            }
        })
        .collect();

    let total = uuids.len();
    if total == 0 {
        println!("no indexed files found; run `perima scan` first");
        return Ok(());
    }

    println!("computing full hash for {total} files (sequential)...");

    let mut ok_count: usize = 0;
    let mut err_count: usize = 0;

    for (i, file_uuid) in uuids.iter().enumerate() {
        let n = i + 1;
        match container.compute_full_hash.execute_single(*file_uuid).await {
            Ok(hash) => {
                println!("{n}/{total}  {}  OK", hash.to_hex());
                ok_count += 1;
            }
            Err(e) => {
                eprintln!("{n}/{total}  (error: {e})");
                err_count += 1;
            }
        }
    }

    println!("done: {ok_count} computed, {err_count} errors");
    Ok(())
}

// ---------------------------------------------------------------------------
// --pending
// ---------------------------------------------------------------------------

/// `perima hash --pending` — compute full hash for every file whose
/// `blake3_hash` is `NULL` (pending full-hash computation).
///
/// WHY use `list_files_pending_full_hash`: the backfill worker populates
/// `quick_hash` for old rows, but `full_hash` / `blake3_hash` is only
/// computed on demand. This subcommand is the CLI escape hatch for power
/// users who want to front-load all full-hash computation before the
/// desktop dedup route surfaces collisions.
async fn run_pending(container: &AppContainer) -> Result<(), CoreError> {
    // WHY large limit: we want all pending rows; the DB cursor keeps
    // memory bounded via the pool connection.
    let uuids = container
        .files_repo
        .list_files_pending_full_hash(usize::MAX)?;

    let total = uuids.len();
    if total == 0 {
        println!("no files pending full-hash computation");
        return Ok(());
    }

    println!("computing full hash for {total} pending files (sequential)...");

    let mut ok_count: usize = 0;
    let mut err_count: usize = 0;

    for (i, file_uuid) in uuids.iter().enumerate() {
        let n = i + 1;
        match container.compute_full_hash.execute_single(*file_uuid).await {
            Ok(hash) => {
                println!("{n}/{total}  {}  OK", hash.to_hex());
                ok_count += 1;
            }
            Err(e) => {
                eprintln!("{n}/{total}  (error: {e})");
                err_count += 1;
            }
        }
    }

    println!("done: {ok_count} computed, {err_count} errors");
    Ok(())
}
