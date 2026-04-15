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
/// # Errors
/// Returns `Error::NotUnderVolume` if `absolute` is not prefixed by
/// `volume_root`.
pub fn relativize(absolute: &Path, volume_root: &Path) -> Result<MediaPath, Error> {
    let abs = dunce::simplified(absolute);
    let root = dunce::simplified(volume_root);
    let rel = abs
        .strip_prefix(root)
        .map_err(|_| Error::NotUnderVolume(absolute.to_path_buf()))?;
    // WHY: convert to forward-slash explicitly before handing to
    // MediaPath so our Windows test cases normalize consistently
    // on non-Windows hosts.
    let as_str = path_slash::PathExt::to_slash_lossy(rel);
    Ok(MediaPath::new(as_str.as_ref()))
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
}
