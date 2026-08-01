//! `perima prune` — thin delegator to [`perima_app::PruneUseCase`].

use std::io::Write;

use perima_app::{AppContainer, PruneCommand, PruneReport};
use perima_core::{CoreError, DeviceId};

/// Execute `prune`.
///
/// Retires catalogue entries for files a previous `perima verify` found
/// missing. Requires `--yes` for a live run — see [`run`]'s WHY below.
///
/// # Errors
/// Propagates `CoreError` from the `UseCase` / repository.
pub(crate) fn run(
    container: &AppContainer,
    machine: DeviceId,
    dry_run: bool,
    yes: bool,
) -> Result<PruneReport, CoreError> {
    let missing = container.prune.count_missing()?;

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    // WHY a confirmation gate on the live path (and not on `verify`):
    // prune is the only destructive command in this pair, and the rows
    // it removes are exactly the rows the user can no longer see on
    // disk to sanity-check against. Requiring an explicit `--yes` means
    // a mistyped command cannot silently retire a catalogue. `--dry-run`
    // is the discoverable way to see the number first.
    if !dry_run && !yes && missing > 0 {
        writeln!(
            handle,
            "{missing} location(s) are marked missing.\n\
             Re-run with --yes to remove them, or --dry-run to inspect first.\n\
             Tip: run `perima verify` first so the missing set reflects the \
             filesystem as it is now.",
        )
        .map_err(CoreError::from)?;
        return Ok(PruneReport {
            missing_found: missing,
            rows_pruned: 0,
        });
    }

    let report = container.prune.execute(&PruneCommand {
        device_id: machine,
        dry_run,
    })?;
    render(&mut handle, &report, dry_run).map_err(CoreError::from)?;
    Ok(report)
}

/// Render a [`PruneReport`] as human-readable lines.
fn render<W: Write>(w: &mut W, r: &PruneReport, dry_run: bool) -> std::io::Result<()> {
    if r.missing_found == 0 {
        writeln!(w, "nothing to prune — no locations are marked missing")?;
        return Ok(());
    }
    if dry_run {
        writeln!(
            w,
            "would remove {} location(s) marked missing (dry run — nothing was written)",
            r.missing_found,
        )?;
    } else {
        writeln!(w, "removed {} location(s) marked missing", r.rows_pruned)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.
mod tests {
    use super::*;

    fn s(r: &PruneReport, dry_run: bool) -> String {
        let mut buf = Vec::new();
        render(&mut buf, r, dry_run).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn empty_catalogue_says_nothing_to_prune() {
        let out = s(&PruneReport::default(), false);
        assert!(out.contains("nothing to prune"));
    }

    #[test]
    fn dry_run_is_conditional_and_labelled() {
        let out = s(
            &PruneReport {
                missing_found: 7,
                rows_pruned: 0,
            },
            true,
        );
        assert!(out.contains("would remove 7"));
        assert!(out.contains("dry run"));
    }

    #[test]
    fn live_run_reports_rows_actually_removed() {
        let out = s(
            &PruneReport {
                missing_found: 7,
                rows_pruned: 7,
            },
            false,
        );
        assert!(out.contains("removed 7"));
        assert!(!out.contains("dry run"));
    }
}
