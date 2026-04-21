//! `perima scan` implementation — thin CLI delegator to
//! [`perima_app::ScanUseCase`].
//!
//! The orchestration body (walk → hash → persist → metadata queue) lives in
//! `crates/app/src/scan.rs` (Task 2 landed). This module keeps only:
//! - [`ScanArgs`] — CLI flag bag parsed by clap.
//! - [`ScanStats`] — aggregate counts returned for main.rs's exit-code mapping.
//! - [`ExitCode`] — local enum the CLI dispatcher maps to a POSIX code.
//! - [`OnPersistFactory`] — shell-owned sentinel-migration closure builder.
//! - [`run`] — single ≈40-line delegator.
//!
//! Bodies: imports + clap args + thin dispatcher. No rayon, no `MetadataQueue`,
//! no volume detection, no manifest write — all moved into `ScanUseCase`.

use std::path::PathBuf;

use perima_app::{AppContainer, FullScan, OnPersist, ScanCommand, ScanReport};
use perima_core::{CoreError, DeviceId};

use crate::signals::Cancellation;

/// Arguments for the scan command.
// WHY allow(struct_excessive_bools): each flag corresponds to a
// distinct user-facing `--flag` on `perima scan`; converting them into
// a typed enum would either collapse independent axes (dry-run vs
// quiet vs no-wait-metadata are orthogonal) or bloat the CLI surface.
// Per plan the flags stay individually named.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub(crate) struct ScanArgs {
    /// Root directory to walk.
    pub root: PathBuf,
    /// When true, hashes and prints but skips all DB writes.
    pub dry_run: bool,
    /// Suppress per-file stdout lines; print summary only.
    pub quiet: bool,
    /// Skip the bounded post-walk drain of the metadata queue.
    pub no_wait_metadata: bool,
    /// Disable WebP thumbnail generation for image/video files.
    pub no_thumbnails: bool,
}

/// Scan statistics (CLI summary surface).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ScanStats {
    /// Files newly indexed.
    pub new: u64,
    /// Files already present (unchanged or updated).
    pub existing: u64,
    /// Files that errored during hash or persist.
    pub errors: u64,
}

impl From<&ScanReport> for ScanStats {
    fn from(r: &ScanReport) -> Self {
        Self {
            new: r.files_new,
            existing: r.files_updated,
            errors: r.files_errored,
        }
    }
}

/// Exit code returned to `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitCode {
    /// Completed normally.
    Success,
    /// Ctrl-C received; partial scan summarized.
    Interrupted,
}

/// Shell-owned factory for the per-file `on_persist` sentinel migration
/// closure. The production dispatcher in `main.rs` opens its own DB
/// connection and constructs a closure that calls
/// `perima_db::SqliteFileRepository::migrate_sentinel_row`; tests pass
/// `None` to bypass sentinel migration entirely.
///
/// WHY a type alias rather than an inline signature: the full
/// `Option<OnPersist>` expression triggers `clippy::type_complexity` once
/// it's threaded through a function signature; this alias keeps call sites
/// readable.
pub(crate) type OnPersistFactory = Option<OnPersist>;

/// Execute `perima scan` by delegating to [`perima_app::ScanUseCase`].
///
/// Per-file output lines remain a CLI concern — the `UseCase` surfaces each
/// file on `ScanReport::per_file_entries` so shells can print whatever
/// columns they like. The aggregate summary line + exit-code mapping stay
/// in the CLI.
///
/// # Errors
/// Propagates [`CoreError`] from the `UseCase` (invalid path, hash failure,
/// persist failure, etc.). `main.rs` maps specific variants to exit codes.
pub(crate) async fn run(
    container: &AppContainer,
    device: DeviceId,
    cancel: &Cancellation,
    on_persist: OnPersistFactory,
    args: &ScanArgs,
) -> Result<(ExitCode, ScanStats), CoreError> {
    let cmd = ScanCommand::Full(FullScan {
        path: args.root.clone(),
        device_id: device,
        // WHY `with_metadata = !dry_run`: the CLI historically spawned the
        // metadata queue on every non-dry scan. Preserving that default here
        // keeps the CI fixtures (`scan_with_metadata_test.rs`) green.
        with_metadata: !args.dry_run,
        dry_run: args.dry_run,
        no_wait_metadata: args.no_wait_metadata,
        no_thumbnails: args.no_thumbnails,
        cancel: cancel.token(),
        on_persist,
    });

    let report = container.scan.execute(cmd).await?;

    // WHY: per-file output stays in the CLI — it's a shell presentation
    // concern, not a UseCase output. `ScanReport::per_file_entries` is
    // populated for exactly this purpose.
    if !args.quiet {
        use std::io::Write;
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        for entry in &report.per_file_entries {
            writeln!(
                handle,
                "{}  {}  {}",
                entry.hash.to_hex(),
                entry.size,
                entry.relative_path.as_str()
            )
            .map_err(CoreError::Io)?;
        }
    }

    // WHY manifest write stays in CLI: the UseCase deliberately does NOT
    // depend on `perima-db` (spec §2 IN). The report exposes the volume
    // mount + manifest files so shells can call `manifest::write_manifest`.
    if let Some((vol_id, mount)) = report.volume_mount.as_ref() {
        perima_db::manifest::write_manifest(mount, *vol_id, &report.manifest_files)?;
    }

    let stats = ScanStats::from(&report);
    let suffix = if report.interrupted {
        " (interrupted)"
    } else {
        ""
    };
    if args.dry_run {
        let total = stats.new + stats.existing + stats.errors;
        eprintln!("scanned {total} files (dry-run; DB not wired){suffix}");
    } else {
        let label_or_id = report.volume_mount.as_ref().map_or_else(
            || "?".to_owned(),
            |(vol_id, _)| {
                let label = report.volume_label.as_deref().unwrap_or("unknown");
                if label == "unknown" || label.is_empty() {
                    let s = vol_id.0.to_string();
                    s[..8].to_owned()
                } else {
                    label.to_owned()
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
        if report.interrupted {
            ExitCode::Interrupted
        } else {
            ExitCode::Success
        },
        stats,
    ))
}
