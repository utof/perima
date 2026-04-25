//! Recursive filesystem walker implementing `Scanner`.

use std::path::Path;

use perima_core::{CoreError, DiscoveredFile, FileSize, FileStat, Scanner};

use crate::{errors::Error, paths::relativize};

/// `walkdir`-backed scanner.
#[derive(Clone, Copy, Debug, Default)]
pub struct WalkdirScanner;

impl WalkdirScanner {
    /// Construct a stateless walker.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Scanner for WalkdirScanner {
    fn walk<'a>(
        &'a self,
        root: &Path,
        volume_root: &Path,
    ) -> Result<Box<dyn Iterator<Item = DiscoveredFile> + Send + 'a>, CoreError> {
        // Smoke-check the root exists before we return the iterator
        // so callers get a terminal error immediately rather than
        // an empty stream.
        if !root.exists() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("root does not exist: {}", root.display()),
            ))
            .into());
        }

        let owned_volume_root = volume_root.to_path_buf();
        let iter = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(move |entry| match entry {
                Ok(e) => {
                    if !e.file_type().is_file() {
                        return None;
                    }
                    let metadata = match e.metadata() {
                        Ok(m) => m,
                        Err(err) => {
                            tracing::warn!(
                                path = %e.path().display(),
                                error = %err,
                                "skipping entry: cannot read metadata"
                            );
                            return None;
                        }
                    };
                    let rel = match relativize(e.path(), &owned_volume_root) {
                        Ok(r) => r,
                        Err(err) => {
                            tracing::warn!(
                                path = %e.path().display(),
                                error = %err,
                                "skipping entry: cannot relativize"
                            );
                            return None;
                        }
                    };
                    Some(DiscoveredFile {
                        absolute_path: e.path().to_path_buf(),
                        relative_path: rel,
                        size: FileSize(metadata.len()),
                    })
                }
                Err(err) => {
                    tracing::warn!(error = %err, "skipping entry: walk error");
                    None
                }
            });
        Ok(Box::new(iter))
    }

    fn stat_with_id(&self, path: &Path) -> Result<FileStat, CoreError> {
        // WHY std::fs::metadata + file_id::get_file_id (not a single syscall):
        // `std::fs::metadata` already provides size + mtime cross-platform;
        // `file_id::get_file_id` (a workspace dep used elsewhere by the
        // watcher) handles the OS-specific inode/file-id extraction. Two
        // calls each go to a cached stat on most filesystems — adding a
        // direct `statx` binding would buy a tiny perf win at the cost of
        // an unsafe FFI dep and is deferred (spec §4.3).
        let meta = std::fs::metadata(path).map_err(Error::Io)?;
        let size_bytes = meta.len();
        let mtime_ns = mtime_to_nanos(&meta)?;
        let fs_file_id = file_id_to_i64(path)?;
        Ok(FileStat {
            size_bytes,
            mtime_ns,
            fs_file_id,
        })
    }
}

/// Convert `Metadata::modified()` to nanoseconds since the Unix epoch.
///
/// Pre-1970 timestamps return a negative `i64`; far-future timestamps
/// (≥ year 2262) saturate at `i64::MAX`. WHY saturating: the cache key is
/// only used for equality (cache hit/miss); a saturated mtime would never
/// produce a stale cache hit because real-world mtimes never reach that
/// value.
fn mtime_to_nanos(meta: &std::fs::Metadata) -> Result<i64, CoreError> {
    let mtime = meta.modified().map_err(Error::Io)?;
    let nanos = match mtime.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i128::from(d.as_secs())
            .saturating_mul(1_000_000_000)
            .saturating_add(i128::from(d.subsec_nanos())),
        Err(e) => {
            // Pre-epoch: produce a negative i128.
            let d = e.duration();
            -(i128::from(d.as_secs())
                .saturating_mul(1_000_000_000)
                .saturating_add(i128::from(d.subsec_nanos())))
        }
    };
    // Saturate i128 → i64. Saturation here is observable only on far-
    // future / far-past timestamps that no real filesystem produces.
    Ok(i64::try_from(nanos).unwrap_or(if nanos < 0 { i64::MIN } else { i64::MAX }))
}

/// Convert `file_id::get_file_id` output to a `i64` for the cache lookup.
///
/// Linux/macOS: `Inode { inode_number: u64 }` — bit-faithful `as i64`.
/// Windows: `LowRes { file_index: u64 }` — same cast. `HighRes`'s `u128`
/// is truncated to its low 64 bits then cast (matches spec §4.3 table —
/// "use the low 64 bits per spec").
fn file_id_to_i64(path: &Path) -> Result<i64, CoreError> {
    let id = file_id::get_file_id(path).map_err(Error::Io)?;
    // WHY `as` casts allowed here: spec §4.3 accepts bit-faithful
    // wrap-on-overflow. Equality semantics are preserved as long as
    // every reader uses the same cast (CacheKey::fs_file_id: i64).
    #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
    let v = match id {
        file_id::FileId::Inode { inode_number, .. } => inode_number as i64,
        file_id::FileId::LowRes { file_index, .. } => file_index as i64,
        file_id::FileId::HighRes { file_id, .. } => file_id as i64,
    };
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn mk_file(dir: &Path, name: &str, bytes: &[u8]) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create dirs");
        }
        std::fs::File::create(&path)
            .expect("create file")
            .write_all(bytes)
            .expect("write");
    }

    #[test]
    fn walks_three_files() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path();
        mk_file(root, "a.txt", b"alpha");
        mk_file(root, "sub/b.txt", b"beta");
        mk_file(root, "sub/c.bin", b"gamma");

        let scanner = WalkdirScanner::new();
        let mut names: Vec<String> = scanner
            .walk(root, root)
            .expect("walk")
            .map(|f| f.relative_path.as_str().to_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["a.txt", "sub/b.txt", "sub/c.bin"]);
    }

    #[test]
    fn missing_root_is_err() {
        let scanner = WalkdirScanner::new();
        let bogus = std::path::Path::new("/definitely/does/not/exist/perima-test");
        assert!(scanner.walk(bogus, bogus).is_err());
    }

    #[test]
    fn sizes_are_populated() {
        let td = tempfile::tempdir().expect("tempdir");
        mk_file(td.path(), "f.bin", &vec![0x00; 1024]);
        let scanner = WalkdirScanner::new();
        let files: Vec<_> = scanner.walk(td.path(), td.path()).expect("walk").collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].size.0, 1024);
    }
}
