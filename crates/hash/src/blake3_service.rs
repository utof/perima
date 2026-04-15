//! BLAKE3 content-hashing service.

use std::io::Read;
use std::path::Path;

use perima_core::{BlakeHash, CoreError, HashService};

use crate::errors::Error;

/// Default chunk size for streaming reads. BLAKE3 is fastest when
/// fed reasonably-large chunks.
const CHUNK_SIZE: usize = 64 * 1024;

/// First-64-KiB cap used by `quick_hash`.
const QUICK_CAP: u64 = 64 * 1024;

/// `HashService` implementation backed by the `blake3` crate.
#[derive(Clone, Copy, Debug, Default)]
pub struct Blake3Service;

impl Blake3Service {
    /// Construct a stateless hasher.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl HashService for Blake3Service {
    fn quick_hash(&self, path: &Path) -> Result<BlakeHash, CoreError> {
        hash_file(path, Some(QUICK_CAP)).map_err(Into::into)
    }

    fn full_hash(&self, path: &Path) -> Result<BlakeHash, CoreError> {
        hash_file(path, None).map_err(Into::into)
    }
}

fn hash_file(path: &Path, cap: Option<u64>) -> Result<BlakeHash, Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    // WHY: heap allocation avoids large-stack-array lint; CHUNK_SIZE (64 KiB)
    // exceeds the 16 KiB clippy threshold for stack allocations.
    let mut buf = vec![0u8; CHUNK_SIZE].into_boxed_slice();
    let mut remaining = cap.unwrap_or(u64::MAX);
    while remaining > 0 {
        // WHY: buf.len() <= CHUNK_SIZE <= u64::MAX, so the cast is safe; we
        // clamp to `remaining` first which is also a u64, making the final
        // as-usize safe on any realistic platform (usize >= u32 everywhere
        // we support). allow rather than try_from to keep the loop readable.
        #[allow(clippy::cast_possible_truncation)]
        let want = std::cmp::min(buf.len() as u64, remaining) as usize;
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        remaining = remaining.saturating_sub(n as u64);
    }
    let out = hasher.finalize();
    Ok(BlakeHash::from_bytes(*out.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn deterministic_full_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.bin");
        std::fs::File::create(&path)
            .expect("create")
            .write_all(b"hello world")
            .expect("write");
        let svc = Blake3Service::new();
        let a = svc.full_hash(&path).expect("hash1");
        let b = svc.full_hash(&path).expect("hash2");
        assert_eq!(a, b);
    }

    #[test]
    fn full_hash_matches_blake3_reference() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.bin");
        std::fs::File::create(&path)
            .expect("create")
            .write_all(b"hello world")
            .expect("write");
        let svc = Blake3Service::new();
        let got = svc.full_hash(&path).expect("hash");
        let expected = blake3::hash(b"hello world");
        assert_eq!(got.as_bytes(), expected.as_bytes());
    }

    #[test]
    fn quick_hash_caps_at_64kib() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.bin");
        let payload = vec![0x42u8; 128 * 1024];
        std::fs::File::create(&path)
            .expect("create")
            .write_all(&payload)
            .expect("write");
        let svc = Blake3Service::new();
        let q = svc.quick_hash(&path).expect("quick");
        let expected = blake3::hash(&payload[..64 * 1024]);
        assert_eq!(q.as_bytes(), expected.as_bytes());
    }
}
