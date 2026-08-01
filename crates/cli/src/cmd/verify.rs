//! `perima verify` — thin delegator to [`perima_app::VerifyUseCase`].

use std::io::Write;

use perima_app::{AppContainer, VerifyCommand, VerifyReport};
use perima_core::{CoreError, DeviceId};
use tokio_util::sync::CancellationToken;

/// Execute `verify`.
///
/// Walks every catalogued location on a mounted volume, checks whether
/// the file is still there, and updates statuses that changed.
///
/// # Errors
/// Propagates `CoreError` from the `UseCase` / repository.
pub(crate) fn run(
    container: &AppContainer,
    machine: DeviceId,
    dry_run: bool,
    cancel: &CancellationToken,
) -> Result<VerifyReport, CoreError> {
    let report = container.verify.execute(&VerifyCommand {
        device_id: machine,
        dry_run,
        cancel: cancel.clone(),
    })?;

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    render(&mut handle, &report, dry_run).map_err(CoreError::from)?;
    Ok(report)
}

/// Render a [`VerifyReport`] as human-readable lines.
///
/// WHY the skipped-volume line is unconditional when non-zero rather
/// than tucked behind `--verbose`: it is the difference between "your
/// library is clean" and "I could not look at 400 of your files". A
/// user who prunes on the strength of a partial sweep deletes rows whose
/// files are intact on a drive that simply was not plugged in.
fn render<W: Write>(w: &mut W, r: &VerifyReport, dry_run: bool) -> std::io::Result<()> {
    let verb = if dry_run { "would mark" } else { "marked" };
    writeln!(w, "checked {} location(s)", r.checked)?;
    if r.newly_missing > 0 {
        writeln!(w, "{verb} {} missing", r.newly_missing)?;
    }
    if r.recovered > 0 {
        writeln!(w, "{verb} {} recovered (missing -> active)", r.recovered)?;
    }
    if r.newly_missing == 0 && r.recovered == 0 {
        writeln!(w, "no changes — catalogue matches the filesystem")?;
    }
    if r.skipped_unmounted > 0 {
        writeln!(
            w,
            "skipped {} location(s) on unmounted volume(s) — NOT checked, \
             status left unchanged",
            r.skipped_unmounted,
        )?;
    }
    if !r.completed {
        writeln!(w, "cancelled before completion; no changes were written")?;
    }
    if dry_run && (r.newly_missing > 0 || r.recovered > 0) {
        writeln!(w, "(dry run — nothing was written)")?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.
mod tests {
    use super::*;

    fn render_to_string(r: &VerifyReport, dry_run: bool) -> String {
        let mut buf = Vec::new();
        render(&mut buf, r, dry_run).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn clean_sweep_says_so() {
        let out = render_to_string(
            &VerifyReport {
                checked: 12,
                completed: true,
                ..Default::default()
            },
            false,
        );
        assert!(out.contains("checked 12"));
        assert!(out.contains("no changes"));
        assert!(
            !out.contains("skipped"),
            "no skip line when nothing was skipped",
        );
    }

    /// A partial sweep must say so in its own right — the counts alone
    /// read as a clean bill of health.
    #[test]
    fn skipped_unmounted_is_always_surfaced() {
        let out = render_to_string(
            &VerifyReport {
                checked: 78,
                skipped_unmounted: 417,
                completed: true,
                ..Default::default()
            },
            false,
        );
        assert!(out.contains("417"), "skipped count must be printed");
        assert!(out.contains("NOT checked"));
    }

    #[test]
    fn dry_run_uses_conditional_wording_and_flags_itself() {
        let out = render_to_string(
            &VerifyReport {
                checked: 3,
                newly_missing: 2,
                completed: true,
                ..Default::default()
            },
            true,
        );
        assert!(out.contains("would mark 2 missing"));
        assert!(out.contains("dry run"));
    }

    #[test]
    fn live_run_uses_past_tense() {
        let out = render_to_string(
            &VerifyReport {
                checked: 3,
                newly_missing: 2,
                rows_written: 2,
                completed: true,
                ..Default::default()
            },
            false,
        );
        assert!(out.contains("marked 2 missing"));
        assert!(!out.contains("dry run"));
    }

    #[test]
    fn cancellation_is_reported() {
        let out = render_to_string(
            &VerifyReport {
                checked: 5,
                completed: false,
                ..Default::default()
            },
            false,
        );
        assert!(out.contains("cancelled"));
        assert!(out.contains("no changes were written"));
    }
}
