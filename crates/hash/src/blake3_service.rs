//! BLAKE3 content-hashing service.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use perima_core::{BlakeHash, CoreError, DeviceKind, HashService};

use crate::errors::Error;

/// Default chunk size for streaming reads. BLAKE3 is fastest when
/// fed reasonably-large chunks.
const CHUNK_SIZE: usize = 64 * 1024;

/// First-64-KiB cap used by `quick_hash`.
const QUICK_CAP: u64 = 64 * 1024;

/// Prefix + suffix region size for `quick_hash_prefix_suffix`.
/// Each region is 64 KiB; the combined digest covers 128 KiB of content.
const PREFIX_SUFFIX_REGION: usize = 64 * 1024;

/// Files ≤ this size are hashed whole by `quick_hash_prefix_suffix`.
/// WHY 2 × `PREFIX_SUFFIX_REGION`: if the file is ≤128 KiB the prefix and
/// suffix would overlap, producing an ambiguous hash; hashing the whole
/// file is cheaper and unambiguous.
const PREFIX_SUFFIX_THRESHOLD: u64 = 2 * PREFIX_SUFFIX_REGION as u64;

/// Files ≥ this size trigger mmap-based hashing (spec §4.5.1 size thresholds).
const MMAP_MIN_SIZE: u64 = 16 * 1024; // 16 KiB

/// Files ≥ this size on SSD/Unknown trigger rayon-parallel mmap (spec §4.5.3).
const RAYON_MIN_SIZE: u64 = 1024 * 1024; // 1 MiB

/// Files > this size get a post-hash `posix_fadvise(DONTNEED)` hint (Linux).
/// See GH issue for implementation (stubbed in this release).
const FADVISE_THRESHOLD: u64 = 64 * 1024 * 1024; // 64 MiB

/// `HashService` implementation backed by the `blake3` crate.
#[derive(Clone, Copy, Debug, Default)]
pub struct Blake3Service;

impl Blake3Service {
    /// Construct a stateless hasher.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Hash prefix ‖ suffix of `path` for fast candidate-duplicate detection.
    ///
    /// For files larger than 128 KiB, reads the first 64 KiB and the last
    /// 64 KiB and hashes their concatenation. For files ≤ 128 KiB, hashes
    /// the whole file (same result as `full_hash`; no overlap ambiguity).
    ///
    /// WHY not on the `HashService` trait: only `Blake3Service` exposes this
    /// method. The worker calls `Blake3Service` directly (spec §4.5.1).
    ///
    /// # Errors
    /// Returns `CoreError::Io` on read failures.
    pub fn quick_hash_prefix_suffix(
        &self,
        path: &Path,
        size_bytes: u64,
    ) -> Result<BlakeHash, CoreError> {
        if size_bytes <= PREFIX_SUFFIX_THRESHOLD {
            // Whole-file fallback: prefix/suffix would overlap.
            return hash_file(path, None).map_err(Into::into);
        }

        let mut file = std::fs::File::open(path).map_err(Error::Io)?;
        let mut hasher = blake3::Hasher::new();

        // Prefix: first 64 KiB.
        let mut buf = vec![0u8; PREFIX_SUFFIX_REGION].into_boxed_slice();
        let mut remaining = PREFIX_SUFFIX_REGION;
        while remaining > 0 {
            let n = file.read(&mut buf[..remaining]).map_err(Error::Io)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            remaining -= n;
        }

        // Suffix: last 64 KiB. Seek from end.
        // WHY i64 cast: SeekFrom::End takes i64; PREFIX_SUFFIX_REGION is
        // 65536 which fits well within i64::MAX.
        #[allow(clippy::cast_possible_wrap)]
        file.seek(SeekFrom::End(-(PREFIX_SUFFIX_REGION as i64)))
            .map_err(Error::Io)?;
        let mut remaining = PREFIX_SUFFIX_REGION;
        while remaining > 0 {
            let n = file.read(&mut buf[..remaining]).map_err(Error::Io)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            remaining -= n;
        }

        let out = hasher.finalize();
        Ok(BlakeHash::from_bytes(*out.as_bytes()))
    }
}

impl HashService for Blake3Service {
    fn quick_hash(&self, path: &Path) -> Result<BlakeHash, CoreError> {
        hash_file(path, Some(QUICK_CAP)).map_err(Into::into)
    }

    fn full_hash(&self, path: &Path) -> Result<BlakeHash, CoreError> {
        hash_file(path, None).map_err(Into::into)
    }

    /// MUST-OVERRIDE implementation (spec §4.4) — forwards to the inherent
    /// [`Blake3Service::quick_hash_prefix_suffix`] so trait-object callers
    /// (e.g. `ScanUseCase`'s `Arc<dyn HashService>`) get the prefix-‖-suffix
    /// shape and not the trait's `quick_hash` fallback.
    ///
    /// WHY UFCS form `Self::...`: `self.quick_hash_prefix_suffix(...)` would
    /// resolve to the trait method (the one we're implementing) and
    /// infinite-recurse. `Self::quick_hash_prefix_suffix(self, ...)` pins
    /// the call to the inherent impl above (rustc resolves inherent method
    /// names ahead of trait method names — see Rust ref. §6.10.1).
    fn quick_hash_prefix_suffix(
        &self,
        path: &Path,
        size_bytes: u64,
    ) -> Result<BlakeHash, CoreError> {
        Self::quick_hash_prefix_suffix(self, path, size_bytes)
    }

    /// MUST-OVERRIDE implementation (spec §4.5.1) — dispatch matrix:
    ///
    /// | Size          | HDD             | SSD / Unknown     |
    /// |---------------|-----------------|-------------------|
    /// | < 16 KiB      | `update`        | `update`          |
    /// | 16 KiB–1 MiB  | `update_mmap`   | `update_mmap`     |
    /// | ≥ 1 MiB       | `update_mmap`   | `update_mmap_rayon`|
    ///
    /// WHY explicit if-chain (not pattern guards): combining size + device
    /// into a single `match` arm with pattern guards produces hard-to-read
    /// guards and conflates the two orthogonal checks (spec §4.5.1 + plan
    /// §WHY block).
    ///
    /// WHY HDD always single-thread: rayon spawns N threads each seeking to
    /// independent chunk offsets; on spinning-rust the resulting seek storm
    /// causes severe throughput regression compared to a sequential read.
    ///
    /// # Errors
    /// Returns `CoreError::Io` on read or mmap failures.
    fn full_hash_dispatched(
        &self,
        path: &Path,
        size_bytes: u64,
        device_kind: DeviceKind,
    ) -> Result<BlakeHash, CoreError> {
        let result = dispatch_hash(path, size_bytes, device_kind)?;

        // Linux-only page-cache eviction hint for very large files.
        // WHY stub: `crates/hash/src/lib.rs` has `#![forbid(unsafe_code)]`
        // so we cannot call `libc::posix_fadvise` directly. A safe wrapper
        // via `rustix` is tracked in GH issue #159.
        if size_bytes > FADVISE_THRESHOLD {
            tracing::debug!(
                path = %path.display(),
                size_bytes,
                "fadvise(DONTNEED) stub — real call deferred (GH #159)"
            );
        }

        Ok(result)
    }
}

/// Core dispatch logic, extracted to keep `full_hash_dispatched` readable.
///
/// WHY separate function: `full_hash_dispatched` has cognitive complexity >10
/// if it contains both the dispatch chain and the fadvise stub inline.
fn dispatch_hash(
    path: &Path,
    size_bytes: u64,
    device_kind: DeviceKind,
) -> Result<BlakeHash, Error> {
    if size_bytes < MMAP_MIN_SIZE {
        // < 16 KiB: streaming `update` (mmap fallback regime).
        // mmap setup overhead dominates at this size; sequential read wins.
        tracing::debug!(target: "hash.dispatch.update", path = %path.display(), size_bytes, "dispatch: update");
        return hash_file(path, None);
    }

    if matches!(device_kind, DeviceKind::Hdd) {
        // HDD at any size ≥ 16 KiB: single-threaded mmap.
        // WHY: rayon spawns N threads seeking to independent chunk offsets;
        // on spinning-rust that seek storm tanks throughput.
        tracing::debug!(target: "hash.dispatch.update_mmap_hdd", path = %path.display(), size_bytes, "dispatch: update_mmap_hdd");
        let mut hasher = blake3::Hasher::new();
        hasher.update_mmap(path).map_err(Error::Io)?;
        let out = hasher.finalize();
        return Ok(BlakeHash::from_bytes(*out.as_bytes()));
    }

    // SSD / Unknown from here.
    if size_bytes < RAYON_MIN_SIZE {
        // 16 KiB – 1 MiB, SSD/Unknown: single-threaded mmap.
        // WHY: rayon overhead (thread wakeup + work-steal cost) exceeds
        // parallelism gains below 1 MiB on SSD.
        tracing::debug!(target: "hash.dispatch.update_mmap", path = %path.display(), size_bytes, "dispatch: update_mmap");
        let mut hasher = blake3::Hasher::new();
        hasher.update_mmap(path).map_err(Error::Io)?;
        let out = hasher.finalize();
        return Ok(BlakeHash::from_bytes(*out.as_bytes()));
    }

    // ≥ 1 MiB, SSD/Unknown: rayon-parallel mmap (best throughput).
    tracing::debug!(target: "hash.dispatch.update_mmap_rayon", path = %path.display(), size_bytes, "dispatch: update_mmap_rayon");
    let mut hasher = blake3::Hasher::new();
    hasher.update_mmap_rayon(path).map_err(Error::Io)?;
    let out = hasher.finalize();
    Ok(BlakeHash::from_bytes(*out.as_bytes()))
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
#[allow(clippy::unwrap_used)] // WHY: test code; unwrap panics signal bugs
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

    // -----------------------------------------------------------------------
    // Task 5 tests (TDD — written before implementation)
    // -----------------------------------------------------------------------

    /// `quick_hash_prefix_suffix` must produce the same hash as manually
    /// concatenating the prefix and suffix regions and hashing them.
    /// File size > 128 KiB so prefix/suffix are distinct regions.
    #[test]
    fn quick_hash_prefix_suffix_matches_concatenation() {
        const REGION: usize = 64 * 1024; // 64 KiB
        const FILE_SIZE: usize = 3 * REGION; // 192 KiB — larger than 128 KiB threshold

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");

        // Write 192 KiB of varied bytes so prefix ≠ suffix.
        let mut payload = vec![0u8; FILE_SIZE];
        for (i, b) in payload.iter_mut().enumerate() {
            // WHY: i & 0xFF always fits in u8; allow truncation explicitly.
            #[allow(clippy::cast_possible_truncation)]
            let byte = (i & 0xFF) as u8;
            *b = byte;
        }
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&payload)
            .unwrap();

        let svc = Blake3Service::new();
        let got = svc
            .quick_hash_prefix_suffix(&path, FILE_SIZE as u64)
            .unwrap();

        // Manual: blake3(prefix ‖ suffix)
        let mut concat = Vec::with_capacity(2 * REGION);
        concat.extend_from_slice(&payload[..REGION]);
        concat.extend_from_slice(&payload[FILE_SIZE - REGION..]);
        let expected = blake3::hash(&concat);

        assert_eq!(got.as_bytes(), expected.as_bytes());
    }

    /// For files ≤ 128 KiB, `quick_hash_prefix_suffix` must hash the
    /// whole file (same result as `full_hash`).
    #[test]
    fn quick_hash_prefix_suffix_small_file_hashes_whole_file() {
        const FILE_SIZE: usize = 64 * 1024; // exactly 64 KiB — well below threshold

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.bin");

        let payload = vec![0xABu8; FILE_SIZE];
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&payload)
            .unwrap();

        let svc = Blake3Service::new();
        let prefix_suffix = svc
            .quick_hash_prefix_suffix(&path, FILE_SIZE as u64)
            .unwrap();
        let full = svc.full_hash(&path).unwrap();

        assert_eq!(
            prefix_suffix.as_bytes(),
            full.as_bytes(),
            "small file: prefix-suffix must equal full hash"
        );
    }

    /// `full_hash_dispatched` MUST use distinct code paths for different
    /// size + device combinations, proven via distinct tracing targets.
    ///
    /// Three cells tested:
    ///  - 8 KiB, any device   → `hash.dispatch.update`
    ///  - 256 KiB, HDD        → `hash.dispatch.update_mmap_hdd`
    ///  - 4 MiB, SSD          → `hash.dispatch.update_mmap_rayon`
    #[tracing_test::traced_test]
    #[test]
    fn full_hash_dispatched_overrides_default() {
        use perima_core::{DeviceKind, HashService as _};

        let dir = tempfile::tempdir().unwrap();
        let svc = Blake3Service::new();

        // Cell 1: 8 KiB — should use `update` path (<16 KiB threshold).
        let path_8k = dir.path().join("8k.bin");
        std::fs::File::create(&path_8k)
            .unwrap()
            .write_all(&vec![0u8; 8 * 1024])
            .unwrap();
        svc.full_hash_dispatched(&path_8k, 8 * 1024, DeviceKind::Ssd)
            .unwrap();
        assert!(
            logs_contain("hash.dispatch.update"),
            "8 KiB file should use update path"
        );

        // Cell 2: 256 KiB, HDD — should use single-thread mmap (HDD path).
        let path_256k = dir.path().join("256k.bin");
        std::fs::File::create(&path_256k)
            .unwrap()
            .write_all(&vec![1u8; 256 * 1024])
            .unwrap();
        svc.full_hash_dispatched(&path_256k, 256 * 1024, DeviceKind::Hdd)
            .unwrap();
        assert!(
            logs_contain("hash.dispatch.update_mmap_hdd"),
            "256 KiB HDD file should use update_mmap_hdd path"
        );

        // Cell 3: 4 MiB, SSD — should use rayon mmap (SSD ≥1 MiB path).
        let path_4m = dir.path().join("4m.bin");
        std::fs::File::create(&path_4m)
            .unwrap()
            .write_all(&vec![2u8; 4 * 1024 * 1024])
            .unwrap();
        svc.full_hash_dispatched(&path_4m, 4 * 1024 * 1024, DeviceKind::Ssd)
            .unwrap();
        assert!(
            logs_contain("hash.dispatch.update_mmap_rayon"),
            "4 MiB SSD file should use update_mmap_rayon path"
        );
    }
}
