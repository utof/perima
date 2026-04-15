//! `perima scan --dry-run` implementation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use perima_core::{CoreError, DiscoveredFile, HashService, Scanner};
use rayon::prelude::*;

use crate::signals::Cancellation;

/// Arguments for the scan command.
#[derive(Debug, Clone)]
pub struct ScanArgs {
    /// Root directory to walk.
    pub root: PathBuf,
    /// When true, hashes and prints but skips all DB writes.
    /// REQUIRED in phase 1a (no DB wired); optional in phase 1b.
    pub dry_run: bool,
    /// Suppress per-file stdout lines; print summary only.
    pub quiet: bool,
}

/// Exit code returned to `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Completed normally.
    Success,
    /// Ctrl-C received; partial scan summarized.
    Interrupted,
}

/// Execute `scan`. In 1a, `args.dry_run` must be `true`; the
/// caller (`main.rs`) enforces this and the scan's own guard
/// doubles as documentation.
///
/// # Errors
/// Returns `CoreError::InvalidPath` if `root` is not a directory;
/// propagates `CoreError` from hashing and walking.
pub fn run<S, H>(
    scanner: &S,
    hasher: &H,
    cancel: &Cancellation,
    args: &ScanArgs,
) -> Result<ExitCode, CoreError>
where
    S: Scanner + ?Sized,
    H: HashService + ?Sized,
{
    // WHY: guard fires BEFORE Config::resolve in main.rs, so the
    // caller already rejected this path. If we still land here (e.g.
    // programmatic call), surface Unsupported so the CLI maps to
    // exit 2 without string-matching prose.
    if !args.dry_run {
        return Err(CoreError::Unsupported(
            "phase 1a ships only 'scan --dry-run'; real scan arrives in 1b".into(),
        ));
    }
    validate_root(&args.root)?;

    let volume_root = canonicalize_for_walk(&args.root)?;
    let mut count: u64 = 0;
    let stdout = std::io::stdout();

    // Collect up-front so rayon can parallelize hashing; the walker
    // iterator itself isn't Send across the par_iter boundary. The
    // inner `take_while` polls between yielded items so a Ctrl-C
    // during walk short-circuits quickly.
    let discovered: Vec<DiscoveredFile> = scanner
        .walk(&args.root, &volume_root)?
        .take_while(|_| !cancel.cancelled())
        .collect();

    // Parallel hash. WHY: we also check cancellation at the top of
    // each map closure so in-flight hashes short-circuit the moment
    // Ctrl-C lands — without this, a large fixture would drain the
    // par_iter to completion even after the flag flips, defeating
    // the "Ctrl-C stops hashing" guarantee in the spec.
    let cancel_flag = cancel.token();
    let results: Vec<Result<(DiscoveredFile, perima_core::BlakeHash), CoreError>> = discovered
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
                count += 1;
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
            }
            Err(e) => {
                tracing::warn!(error = %e, "skipping file: hash failed");
            }
        }
    }
    drop(handle);

    let interrupted = cancel.cancelled();
    let suffix = if interrupted { " (interrupted)" } else { "" };
    eprintln!("scanned {count} files (dry-run; DB not yet wired){suffix}");

    Ok(if interrupted {
        ExitCode::Interrupted
    } else {
        ExitCode::Success
    })
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
