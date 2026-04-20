//! Filesystem-level path helpers — resolve absolute paths to
//! volume-relative `MediaPath`s.

use std::path::Path;

use perima_core::MediaPath;

use crate::errors::Error;

/// Convert `absolute` into a `MediaPath` relative to `volume_root`.
///
/// `absolute` must be under `volume_root`; callers should pre-
/// canonicalize both sides to avoid symlink surprises.
///
/// Only `Path::Component::Normal` segments are preserved; `..`
/// (`ParentDir`) and `.` (`CurDir`) are silently dropped during the
/// components-iterator join. Callers must pre-resolve such components
/// if semantic preservation is required — all current callers feed
/// canonicalized OS paths where `..`/`.` cannot survive.
///
/// # Errors
/// Returns `Error::NotUnderVolume` if `absolute` is not prefixed by
/// `volume_root`.
pub fn relativize(absolute: &Path, volume_root: &Path) -> Result<MediaPath, Error> {
    use std::path::Component;

    let abs = crate::platform_path::simplified(absolute);
    let root = crate::platform_path::simplified(volume_root);
    let rel = abs
        .strip_prefix(root)
        .map_err(|_| Error::NotUnderVolume(absolute.to_path_buf()))?;

    // WHY: iterate Normal components and join with '/'. This handles
    // Windows (backslash separator), Unix (forward slash), AND Windows
    // UNC/drive-letter prefixes correctly — Prefix/RootDir/CurDir/
    // ParentDir components are skipped by the filter, so there's no
    // leading `//` to fight MediaPath::new's trim_start_matches('/').
    // Equivalent on Unix to the prior path_slash behavior
    // (to_slash_lossy).
    let as_str = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    Ok(MediaPath::new(&as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relativize_simple() {
        let root = std::path::Path::new("/tmp/perima");
        let abs = std::path::Path::new("/tmp/perima/photos/a.jpg");
        let r = relativize(abs, root).expect("relativize");
        assert_eq!(r.as_str(), "photos/a.jpg");
    }

    #[test]
    fn relativize_rejects_outside_root() {
        let root = std::path::Path::new("/tmp/perima");
        let abs = std::path::Path::new("/tmp/other/a.jpg");
        assert!(relativize(abs, root).is_err());
    }

    #[test]
    fn relativize_identity_root() {
        let root = std::path::Path::new("/tmp/perima");
        let r = relativize(root, root).expect("relativize root");
        assert_eq!(r.as_str(), "");
    }

    #[test]
    fn relativize_joins_with_forward_slash_regardless_of_platform() {
        let root = std::path::Path::new("/tmp/perima");
        let abs = std::path::Path::new("/tmp/perima/sub/dir/a.jpg");
        let r = relativize(abs, root).expect("relativize");
        assert_eq!(r.as_str(), "sub/dir/a.jpg");
    }

    #[test]
    #[cfg(windows)]
    fn relativize_windows_unc_path_produces_forward_slash_path() {
        // Real UNC input — would be mangled by replace('\\',"/") →
        // //server/share/... then trim_start_matches('/') collapses
        // leading slashes. Components-iterator approach sidesteps this.
        let root = std::path::Path::new(r"\\server\share");
        let abs = std::path::Path::new(r"\\server\share\photos\a.jpg");
        let r = relativize(abs, root).expect("relativize");
        assert_eq!(r.as_str(), "photos/a.jpg");
    }

    #[test]
    #[cfg(windows)]
    fn relativize_windows_drive_letter_path_produces_forward_slash_path() {
        let root = std::path::Path::new(r"C:\data");
        let abs = std::path::Path::new(r"C:\data\photos\a.jpg");
        let r = relativize(abs, root).expect("relativize");
        assert_eq!(r.as_str(), "photos/a.jpg");
    }
}
