//! Volume detection via `sysinfo`.
//!
//! WHY: v1 matching is label+capacity only; sysinfo 0.38 does not expose GPT
//! GUID or `fs_uuid` on any platform. The priority-chain structure supports
//! plugging in richer identifiers later.

use std::path::{Path, PathBuf};

use perima_core::{CoreError, VolumeIdentifiers};
use sysinfo::Disks;

/// A volume detected on the local machine, bound to the path used for
/// the query.
#[derive(Clone, Debug)]
pub struct DetectedVolume {
    /// Observed identifiers for priority-chain matching.
    pub identifiers: VolumeIdentifiers,
    /// The mount point of this volume.
    pub mount_point: PathBuf,
}

/// Detect the volume that contains `path` using a longest mount-prefix match.
///
/// Canonicalizes `path` via [`crate::platform_path::canonicalize`]
/// (dunce on Windows, `std::fs::canonicalize` elsewhere), then enumerates all
/// disks with [`sysinfo::Disks::new_with_refreshed_list`] and finds the
/// disk whose mount point is the longest prefix of the canonicalized path.
///
/// # Errors
///
/// - `CoreError::Io` if `path` cannot be canonicalized (e.g. does not exist).
/// - `CoreError::Internal` if no mounted disk covers the path.
pub fn detect_volume(path: &Path) -> Result<DetectedVolume, CoreError> {
    let canonical = crate::platform_path::canonicalize(path)?;
    let disks = Disks::new_with_refreshed_list();

    // WHY: longest-prefix wins so that nested mounts (e.g. /mnt/data and
    // /mnt/data/sub) resolve to the innermost, most-specific mount point.
    let best = disks
        .list()
        .iter()
        .filter(|disk| {
            let mp = disk.mount_point();
            canonical.starts_with(mp)
        })
        .max_by_key(|disk| disk.mount_point().as_os_str().len());

    best.map_or_else(
        || {
            Err(CoreError::Internal(format!(
                "no volume found for path: {}",
                canonical.display()
            )))
        },
        |disk| {
            // WHY: `gpt_partition_guid` and `fs_uuid` are not exposed by sysinfo
            // 0.38 on any platform. We record None honestly so that the
            // priority chain in `SqliteVolumeRepository` falls through to
            // label+capacity without special-casing.
            let identifiers = VolumeIdentifiers {
                gpt_partition_guid: None,
                fs_uuid: None,
                label: Some(disk.name().to_string_lossy().into_owned()),
                capacity_bytes: disk.total_space(),
                is_removable: disk.is_removable(),
            };
            Ok(DetectedVolume {
                identifiers,
                mount_point: disk.mount_point().to_path_buf(),
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_volume_on_cwd() {
        let cwd = std::env::current_dir().expect("current_dir");
        let vol = detect_volume(&cwd).expect("detect_volume on cwd");
        assert!(
            vol.identifiers.capacity_bytes > 0,
            "capacity must be non-zero"
        );
        assert!(
            cwd.starts_with(&vol.mount_point),
            "cwd must be under the detected mount point"
        );
    }

    #[test]
    fn detect_volume_nonexistent_path() {
        let bogus = Path::new("/definitely/does/not/exist");
        let result = detect_volume(bogus);
        assert!(result.is_err(), "expected error for nonexistent path");
    }
}
