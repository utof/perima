//! Recursive filesystem walker implementing `Scanner`.

use std::path::Path;

use perima_core::{CoreError, DiscoveredFile, FileSize, Scanner};

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
