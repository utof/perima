//! `perima volumes` — thin delegator to [`perima_app::VolumeUseCase`].

use std::io::Write;

use perima_app::{AppContainer, VolumeCommand, VolumeOutput};
use perima_core::{CoreError, DeviceId};

use super::format::format_size;

/// Execute `volumes`.
///
/// Lists all known volumes for `machine` in a human-readable table,
/// showing the volume ID prefix, label, removable flag, capacity, and
/// any mount paths seen on this machine.
///
/// # Errors
/// Propagates `CoreError` from the `UseCase` / repository.
pub(crate) async fn run(container: &AppContainer, machine: DeviceId) -> Result<(), CoreError> {
    let out = container
        .volume
        .execute(VolumeCommand::List { device: machine })
        .await?;

    let VolumeOutput::Volumes(records) = out else {
        return Err(CoreError::Internal(
            "VolumeCommand::List returned non-Volumes output".into(),
        ));
    };

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    writeln!(
        handle,
        "{:<10} {:<16} {:<9} {:<10} MOUNT PATHS",
        "VOLUME ID", "LABEL", "REMOVABLE", "CAPACITY",
    )
    .map_err(CoreError::from)?;

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
        .map_err(CoreError::from)?;
    }

    Ok(())
}
