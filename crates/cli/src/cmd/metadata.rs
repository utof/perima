//! `perima metadata <path>` — re-extract metadata for a specific file.
//!
//! Opens the DB, resolves the supplied path to its `(volume, relative_path)`
//! pair, locates the file by scanning the already-indexed locations, enqueues
//! a re-extraction through a freshly spawned [`MetadataQueue`], polls the
//! metadata repo for up to [`POLL_TIMEOUT`], and prints the resulting
//! [`MediaMetadata`] as either a human-readable table (default) or JSON.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use perima_core::{
    CoreError, DeviceId, FileLocationRecord, FileRepository, MediaMetadata, MetadataExtractor,
    MetadataRepository, VolumeRepository,
};
use perima_db::{
    SqliteFileRepository, SqliteMetadataRepository, SqliteVolumeRepository, open_and_migrate,
};
use perima_media::{CompositeExtractor, ImageExtractor, MetadataQueue, VideoExtractor};
use tokio_util::sync::CancellationToken;

/// Maximum wait after enqueue for the worker to persist a row.
///
/// WHY 3 s: typical image/video extraction completes in well under 500 ms;
/// 3 s is >5× headroom for slow disks / large files. Longer waits make
/// `perima metadata` feel unresponsive; shorter waits flake on CI.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(3);

/// Per-iteration backoff while polling for the metadata row.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Arguments for the metadata command.
#[derive(Debug, Clone)]
pub struct MetadataArgs {
    /// Path to the file whose metadata should be re-extracted.
    pub path: PathBuf,
    /// When `true`, emit JSON instead of a human-readable table.
    pub json: bool,
}

/// Execute `perima metadata <path>`.
///
/// # Errors
/// Returns [`CoreError::InvalidPath`] when the file does not exist or
/// is not yet indexed (run `perima scan` first); propagates
/// [`CoreError`] from volume detection, DB access, and the extractor.
pub async fn run(data_dir: &Path, device: DeviceId, args: &MetadataArgs) -> Result<(), CoreError> {
    validate_file(&args.path)?;
    let absolute_path = dunce::canonicalize(&args.path).map_err(CoreError::Io)?;

    // Resolve volume from the containing directory. WHY parent(): volume
    // detection inspects the mount point; a file path's parent is the
    // nearest directory, which shares the same volume.
    let parent = absolute_path
        .parent()
        .ok_or_else(|| CoreError::InvalidPath(format!("no parent: {}", absolute_path.display())))?;
    let detected = perima_fs::detect_volume(parent)?;

    let db_path = data_dir.join("perima.db");

    // WHY three connections: each repo owns its `Mutex<Connection>`, and
    // under WAL mode a fresh open is ~microseconds. Sharing a single
    // connection across repos would require wrapping it in another layer
    // of `Mutex`, which none of the repos' `new(...)` constructors accept.
    let mut vol_repo = SqliteVolumeRepository::new(open_and_migrate(&db_path)?);
    let volume_id = vol_repo.find_or_create(&detected.identifiers, device)?;
    drop(vol_repo);

    let file_repo = SqliteFileRepository::new(open_and_migrate(&db_path)?);
    let metadata_repo = Arc::new(SqliteMetadataRepository::new(open_and_migrate(&db_path)?));

    // WHY suffix match on the absolute path: `scan` walker stores
    // paths relative to the *scan root* (see
    // `WalkdirScanner::walk` — second arg is `volume_root` but the
    // production call passes the scan root itself). We do not know
    // which scan root produced this record, so we match "absolute
    // path ends with stored relative path" against the volume's
    // locations. Collisions are possible but vanishingly rare for
    // anything other than duplicate-named files under the same scan.
    let absolute_str = absolute_path.to_str().ok_or_else(|| {
        CoreError::InvalidPath(format!("non-UTF8 path: {}", absolute_path.display()))
    })?;
    let records = file_repo.list_file_locations(usize::MAX, Some(volume_id))?;
    let Some(record) = find_by_absolute_suffix(&records, absolute_str) else {
        return Err(CoreError::InvalidPath(format!(
            "not indexed: {} (run `perima scan` first)",
            absolute_path.display(),
        )));
    };
    let hash = record.hash;

    // Spawn queue + enqueue. A fresh `CancellationToken` is fine here:
    // `perima metadata` is interactive and short-lived; Ctrl-C during
    // the 3-s poll is rare and accepted as-is (the process exits).
    let cancel = CancellationToken::new();
    let extractor: Arc<dyn MetadataExtractor> = Arc::new(CompositeExtractor::new(vec![
        Arc::new(ImageExtractor::new()) as Arc<dyn MetadataExtractor>,
        Arc::new(VideoExtractor::new()) as Arc<dyn MetadataExtractor>,
    ]));
    let repo_dyn: Arc<dyn MetadataRepository> = Arc::clone(&metadata_repo) as _;
    let mut queue = MetadataQueue::spawn(extractor, repo_dyn, device, cancel.clone());
    queue.enqueue(hash, absolute_path.clone(), &cancel)?;

    // Record the current metadata's `updated_at` (if any) so we know
    // when the row has been freshly rewritten. WHY: if the row already
    // exists, polling for "row present" would return immediately with
    // stale data. Polling for "row present AND different from before"
    // is also fragile (false-Unchanged). The simplest correct signal:
    // drop the queue (closes channel → worker drains this one item and
    // exits), then poll for the worker's JoinHandle to finish.
    let worker = queue
        .take_worker()
        .ok_or_else(|| CoreError::Internal("queue missing worker handle".into()))?;
    drop(queue);
    // WHY timeout: the worker's drain is bounded by extraction time;
    // 3 s is the same budget the integration tests use for a two-file
    // scan, so one file is comfortable.
    let joined = tokio::time::timeout(POLL_TIMEOUT, worker).await;
    if let Err(_elapsed) = joined {
        tracing::warn!("metadata worker did not drain within {POLL_TIMEOUT:?}");
    }

    // Fetch the final row.
    let started = Instant::now();
    let meta = loop {
        if let Some(m) = metadata_repo.find_by_hash(&hash)? {
            break m;
        }
        if started.elapsed() >= POLL_TIMEOUT {
            return Err(CoreError::Internal(
                "metadata worker did not persist a row within poll window".into(),
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    if args.json {
        print_json(&meta)?;
    } else {
        print_table(&meta, record)?;
    }
    Ok(())
}

fn validate_file(path: &Path) -> Result<(), CoreError> {
    if !path.exists() {
        return Err(CoreError::InvalidPath(format!(
            "does not exist: {}",
            path.display()
        )));
    }
    if !path.is_file() {
        return Err(CoreError::InvalidPath(format!(
            "not a file: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Linear scan for a record whose stored `relative_path` is the
/// filename-component suffix of `absolute`.
///
/// WHY suffix match: the scanner stores paths relative to the
/// *scan root*, not the volume mount. Without knowing the original
/// scan root, we can only assert that the absolute path ends with
/// the stored relative path. To avoid `/foo.txt` matching `/prefix/foo.txt`
/// spuriously we require the match to be on a path-component
/// boundary (i.e. preceded by `/`).
///
/// WHY linear: a volume's location count is bounded in practice; an
/// O(n) scan once per `perima metadata` invocation is negligible.
fn find_by_absolute_suffix<'a>(
    records: &'a [FileLocationRecord],
    absolute: &str,
) -> Option<&'a FileLocationRecord> {
    // Normalise `\\` → `/` so Windows backslashes match the stored
    // slash-normalised form (MediaPath stores `/` on every platform).
    let abs = absolute.replace('\\', "/");
    records.iter().find(|r| {
        let rel = r.relative_path.as_str();
        if !abs.ends_with(rel) {
            return false;
        }
        // Check boundary: either the absolute path is exactly the
        // relative path (unusual — scan-root == file-parent), or the
        // character immediately before the suffix is `/`.
        let prefix_len = abs.len() - rel.len();
        prefix_len == 0 || abs.as_bytes().get(prefix_len.saturating_sub(1)) == Some(&b'/')
    })
}

fn print_json(meta: &MediaMetadata) -> Result<(), CoreError> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer_pretty(&mut handle, meta)
        .map_err(|e| CoreError::Internal(format!("json: {e}")))?;
    writeln!(handle).map_err(CoreError::Io)
}

fn print_table(meta: &MediaMetadata, record: &FileLocationRecord) -> Result<(), CoreError> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let hash_hex = meta.hash.to_hex();
    writeln!(handle, "hash:         {hash_hex}").map_err(CoreError::Io)?;
    writeln!(handle, "path:         {}", record.relative_path.as_str()).map_err(CoreError::Io)?;
    if let Some(m) = &meta.mime_type {
        writeln!(handle, "mime:         {m}").map_err(CoreError::Io)?;
    }
    if let (Some(w), Some(h)) = (meta.width, meta.height) {
        writeln!(handle, "dimensions:   {w}x{h}").map_err(CoreError::Io)?;
    }
    if let Some(d) = meta.duration_ms {
        writeln!(handle, "duration_ms:  {d}").map_err(CoreError::Io)?;
    }
    if let Some(c) = &meta.captured_at {
        writeln!(handle, "captured_at:  {c}").map_err(CoreError::Io)?;
    }
    if let Some(m) = &meta.camera_make {
        writeln!(handle, "camera_make:  {m}").map_err(CoreError::Io)?;
    }
    if let Some(m) = &meta.camera_model {
        writeln!(handle, "camera_model: {m}").map_err(CoreError::Io)?;
    }
    if let Some(c) = &meta.codec {
        writeln!(handle, "codec:        {c}").map_err(CoreError::Io)?;
    }
    if let Some(b) = meta.bitrate_bps {
        writeln!(handle, "bitrate_bps:  {b}").map_err(CoreError::Io)?;
    }
    Ok(())
}

// Suppress unused-import warning when Arc<dyn …> coercion happens only
// through a `_` placeholder in the test-free production file — the
// `BlakeHash` import is used via method calls on `hash` above, so no
// `#[allow]` is needed here. The `Arc`/cloning patterns match scan.rs.
#[cfg(test)]
mod tests {
    use super::*;
    use perima_core::{BlakeHash, FileSize, LocationStatus, MediaPath, VolumeId};

    fn rec(p: &str) -> FileLocationRecord {
        FileLocationRecord {
            hash: BlakeHash::from_bytes([0u8; 32]),
            size: FileSize(0),
            volume_id: VolumeId(uuid::Uuid::nil()),
            relative_path: MediaPath::new(p),
            status: LocationStatus::Active,
            first_seen: "2024-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn find_matches_absolute_suffix() {
        let rs = vec![rec("a/b.txt"), rec("c.txt")];
        assert!(find_by_absolute_suffix(&rs, "/tmp/scan/c.txt").is_some());
    }

    #[test]
    fn find_respects_component_boundary() {
        // Stored rel is "foo.txt"; absolute "/x/prefix-foo.txt" should NOT match.
        let rs = vec![rec("foo.txt")];
        assert!(find_by_absolute_suffix(&rs, "/x/prefix-foo.txt").is_none());
    }

    #[test]
    fn find_normalises_backslashes() {
        let rs = vec![rec("sub/x.jpg")];
        assert!(find_by_absolute_suffix(&rs, "C:\\users\\me\\sub\\x.jpg").is_some());
    }

    #[test]
    fn find_returns_none_on_miss() {
        let rs = vec![rec("a.txt")];
        assert!(find_by_absolute_suffix(&rs, "/tmp/b.txt").is_none());
    }
}
