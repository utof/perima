//! `perima volumes` implementation.

use std::io::Write;

use perima_core::{CoreError, DeviceId, VolumeRepository};

use super::format::format_size;

/// Execute `volumes`.
///
/// Lists all known volumes for `machine` in a human-readable table,
/// showing the volume ID prefix, label, removable flag, capacity, and
/// any mount paths seen on this machine.
///
/// # Errors
/// Propagates `CoreError` from the repository.
pub(crate) fn run<VR: VolumeRepository>(repo: &VR, machine: DeviceId) -> Result<(), CoreError> {
    let records = repo.list(machine)?;

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    writeln!(
        handle,
        "{:<10} {:<16} {:<9} {:<10} MOUNT PATHS",
        "VOLUME ID", "LABEL", "REMOVABLE", "CAPACITY",
    )
    .map_err(CoreError::Io)?;

    for r in &records {
        let vol_str = r.id.0.to_string();
        let vol_short = format!("{}…", &vol_str[..8]);
        let label = r.label.as_deref().unwrap_or("(none)");
        let removable = if r.is_removable { "yes" } else { "no" };
        let capacity = format_size(r.capacity_bytes);
        let mounts = if r.mounts_on_this_machine.is_empty() {
            "(none)".to_owned()
        } else {
            r.mounts_on_this_machine
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        };
        writeln!(
            handle,
            "{vol_short:<10} {label:<16} {removable:<9} {capacity:<10} {mounts}",
        )
        .map_err(CoreError::Io)?;
    }

    Ok(())
}
