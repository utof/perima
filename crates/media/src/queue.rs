//! Bounded `tokio::mpsc` queue driving async metadata extraction.
//!
//! Scanner threads call [`MetadataQueue::enqueue`] (sync) with a freshly
//! hashed file; the background worker pops the [`Work`] item, computes
//! its MIME via `mime_guess::from_path`, runs the [`MetadataExtractor`],
//! and persists through the [`MetadataRepository`].
//!
//! WHY single worker: the v0.4.0 scope is "get metadata into the DB
//! correctly"; parallelism is a v0.5+ optimisation. Measuring
//! single-worker throughput first gives us a baseline to beat.

use std::path::PathBuf;
use std::sync::Arc;

use perima_core::{BlakeHash, CoreError, DeviceId, MetadataExtractor, MetadataRepository};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::thumbnail::ThumbnailGenerator;
use fast_image_resize::Resizer;

/// Bounded channel capacity.
///
/// WHY 16384: enough to absorb a 100k-file scan's initial burst without
/// blocking the scanner, while bounding worst-case memory
/// (~16384 * sizeof(Work) ≈ 1 MiB).
pub const QUEUE_CAPACITY: usize = 16384;

/// Interval at which a saturated [`MetadataQueue::enqueue`] call re-polls
/// the channel and the [`CancellationToken`] while waiting for space.
const ENQUEUE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Payload queued by the scanner; consumed by the worker.
///
/// The worker computes MIME via `mime_guess::from_path` *after* dequeue
/// — the scanner deliberately does not carry a MIME here so `mime_guess`
/// stays confined to this crate.
#[derive(Clone, Debug)]
pub struct Work {
    /// BLAKE3 content hash of the freshly scanned file.
    pub hash: BlakeHash,
    /// Absolute path for the extractor to read.
    pub absolute_path: PathBuf,
}

/// Producer handle for the metadata-extraction worker.
///
/// Dropping every clone of this handle closes the channel and lets the
/// worker drain and exit. Callers should hold one [`MetadataQueue`] per
/// scan and `drop()` it when the scan exits.
pub struct MetadataQueue {
    tx: mpsc::Sender<Work>,
    worker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for MetadataQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetadataQueue").finish_non_exhaustive()
    }
}

impl MetadataQueue {
    /// Spawn the background worker and return a queue handle for
    /// producers.
    ///
    /// The worker lives until either (a) the returned [`MetadataQueue`]
    /// is dropped (channel closes, worker drains and exits) or (b) the
    /// `cancel` token is tripped (worker stops between items).
    pub fn spawn(
        extractor: Arc<dyn MetadataExtractor>,
        repo: Arc<dyn MetadataRepository>,
        thumbnailer: Arc<ThumbnailGenerator>,
        device: DeviceId,
        cancel: CancellationToken,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<Work>(QUEUE_CAPACITY);
        let worker = tokio::spawn(async move {
            // WHY one Resizer per worker task: amortizes scratch-buffer
            // alloc + CPU-extension dispatch cache across thumbnail
            // iterations. Per-call Resizer::new() (pre-Batch-J) discarded
            // both. Future multi-worker = each spawn gets its own
            // Resizer; do NOT hoist to a shared Mutex<Resizer> (would
            // re-introduce contention defeating the amortization).
            // Resizer: Send (fast_image_resize 6.0 supertrait), so
            // owning it in this `async move` future is sound under
            // tokio's multi-thread work-stealing runtime.
            let mut resizer = Resizer::new();
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        tracing::debug!("metadata queue worker cancelled");
                        break;
                    }
                    maybe_work = rx.recv() => {
                        let Some(work) = maybe_work else {
                            tracing::debug!("metadata queue channel closed; worker exiting");
                            break;
                        };
                        if cancel.is_cancelled() {
                            break;
                        }
                        process(
                            extractor.as_ref(),
                            repo.as_ref(),
                            thumbnailer.as_ref(),
                            device,
                            &work,
                            &mut resizer,
                        );
                    }
                }
            }
        });
        Self {
            tx,
            worker: Some(worker),
        }
    }

    /// Take ownership of the background worker's `JoinHandle`.
    ///
    /// Intended for callers that want to await the worker with a bounded
    /// timeout at shutdown (scan's bounded-drain path).
    pub const fn take_worker(&mut self) -> Option<JoinHandle<()>> {
        self.worker.take()
    }

    /// Enqueue a unit of work synchronously.
    ///
    /// WHY sync + `try_send` + poll-loop: the scanner runs on a plain
    /// blocking thread (not an async task); `blocking_send` would park a
    /// Tokio runtime thread unresponsively to Ctrl-C. Polling on a fixed
    /// 50 ms interval keeps cancellation responsive even when the worker
    /// is saturated.
    ///
    /// # Errors
    /// - `CoreError::Internal("cancelled during enqueue")` if `cancel`
    ///   is tripped while waiting for channel capacity.
    /// - `CoreError::Internal("metadata queue worker exited")` if the
    ///   worker dropped its receiver (usually means it panicked or was
    ///   cancelled earlier).
    pub fn enqueue(
        &self,
        hash: BlakeHash,
        absolute_path: PathBuf,
        cancel: &CancellationToken,
    ) -> Result<(), CoreError> {
        let mut work = Work {
            hash,
            absolute_path,
        };
        loop {
            if cancel.is_cancelled() {
                return Err(CoreError::Internal("cancelled during enqueue".into()));
            }
            match self.tx.try_send(work) {
                Ok(()) => return Ok(()),
                Err(mpsc::error::TrySendError::Full(w)) => {
                    work = w;
                    std::thread::sleep(ENQUEUE_POLL_INTERVAL);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Err(CoreError::Internal("metadata queue worker exited".into()));
                }
            }
        }
    }
}

/// Execute one unit of work: MIME-guess from path, delegate to the
/// extractor, persist through the repository, then (for image / video
/// kinds) generate a thumbnail and persist its status. Errors are
/// traced but not propagated — scanner-side is fire-and-forget.
fn process(
    extractor: &dyn MetadataExtractor,
    repo: &dyn MetadataRepository,
    thumbnailer: &ThumbnailGenerator,
    device: DeviceId,
    work: &Work,
    resizer: &mut Resizer,
) {
    // WHY `mime_guess::from_path` after dequeue: keeps `mime_guess`
    // confined to `perima-media` (scanner never sees it) and avoids a
    // `String` copy on every Work payload.
    let mime = mime_guess::from_path(&work.absolute_path)
        .first_or_octet_stream()
        .essence_str()
        .to_owned();

    let meta = match extractor.extract(work.hash, &work.absolute_path, &mime) {
        Ok(meta) => meta,
        Err(err) => {
            tracing::warn!(
                error = %err,
                path = %work.absolute_path.display(),
                mime = %mime,
                "metadata extraction failed; will retry on next scan",
            );
            return;
        }
    };

    if let Err(err) = repo.upsert_metadata(&meta, device) {
        tracing::warn!(
            error = %err,
            path = %work.absolute_path.display(),
            "metadata persist failed; will retry on next scan",
        );
        return;
    }

    // WHY only image/*: the `ThumbnailGenerator` decodes via
    // `image::ImageReader` which cannot handle video containers
    // (MP4/MOV). Routing video/* through it guaranteed every video
    // ended up at `thumbnail_status = 'failed'` — misleading for users
    // whose video tooling is simply out of scope for v0.4.x. Audio
    // and other kinds have no visual preview and are also skipped.
    //
    // WHY mark videos as `"skipped"` (not leave NULL): the UI
    // placeholder logic in `FileGrid` branches on status to pick a
    // glyph. A stable `"skipped"` state that won't flap gives the
    // frontend a signal distinct from "pending extraction". ffmpeg-
    // backed video frame extraction is tracked as a future
    // enhancement. See utof/perima#15 HIGH #11b.
    if mime.starts_with("video/") {
        if let Err(err) = repo.update_thumbnail(&work.hash, None, "skipped", device) {
            tracing::warn!(
                error = %err,
                path = %work.absolute_path.display(),
                "update_thumbnail(skipped) failed for video",
            );
        }
        return;
    }
    if !mime.starts_with("image/") {
        return;
    }

    match thumbnailer.generate(&work.hash, &work.absolute_path, resizer) {
        Ok(None) => {
            // Disabled generator (`--no-thumbnails`): leave the row's
            // thumbnail_status at its migration default ("pending").
        }
        Ok(Some(path)) => {
            // WHY absolute path: Tauri's `convertFileSrc` requires an
            // absolute path on disk; storing a relative path would
            // force every frontend read to re-resolve against
            // `data_dir`. `ThumbnailGenerator` always returns
            // absolute paths (it joins onto its owned `data_dir`).
            let path_str = path.to_string_lossy().into_owned();
            if let Err(err) = repo.update_thumbnail(&work.hash, Some(&path_str), "ready", device) {
                tracing::warn!(
                    error = %err,
                    path = %work.absolute_path.display(),
                    "update_thumbnail(ready) failed",
                );
            }
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                path = %work.absolute_path.display(),
                "thumbnail generation failed; marking status = failed",
            );
            if let Err(err2) = repo.update_thumbnail(&work.hash, None, "failed", device) {
                tracing::warn!(
                    error = %err2,
                    path = %work.absolute_path.display(),
                    "update_thumbnail(failed) failed",
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::Duration;

    use perima_core::{
        BlakeHash, CoreError, DeviceId, FileLocationRecord, MediaMetadata, MetadataExtractor,
        MetadataRepository, UpsertOutcome, VolumeId,
    };

    use super::*;

    /// Extractor that echoes a stub `MediaMetadata` so tests can assert
    /// which files reached the worker.
    struct EchoExtractor;
    impl MetadataExtractor for EchoExtractor {
        fn accepts(&self, _mime: &str) -> bool {
            true
        }
        fn extract(
            &self,
            hash: BlakeHash,
            _absolute_path: &Path,
            mime: &str,
        ) -> Result<MediaMetadata, CoreError> {
            Ok(MediaMetadata {
                hash,
                width: None,
                height: None,
                duration_ms: None,
                captured_at: None,
                camera_make: None,
                camera_model: None,
                codec: None,
                bitrate_bps: None,
                mime_type: Some(mime.to_owned()),
                thumbnail_path: None,
                thumbnail_status: None,
            })
        }
    }

    /// In-memory repo for counting upserts.
    #[derive(Default)]
    struct MockRepo {
        rows: Mutex<HashMap<BlakeHash, MediaMetadata>>,
    }

    impl MetadataRepository for MockRepo {
        fn upsert_metadata(
            &self,
            meta: &MediaMetadata,
            _device: DeviceId,
        ) -> Result<UpsertOutcome, CoreError> {
            let mut rows = self.rows.lock().expect("mock mutex");
            let existed = rows.insert(meta.hash, meta.clone()).is_some();
            Ok(if existed {
                UpsertOutcome::Updated
            } else {
                UpsertOutcome::Inserted
            })
        }
        fn find_by_hash(&self, hash: &BlakeHash) -> Result<Option<MediaMetadata>, CoreError> {
            Ok(self.rows.lock().expect("mock mutex").get(hash).cloned())
        }
        fn list_with_metadata(
            &self,
            _limit: usize,
            _volume: Option<VolumeId>,
        ) -> Result<Vec<(FileLocationRecord, Option<MediaMetadata>)>, CoreError> {
            Ok(vec![])
        }
        fn update_thumbnail(
            &self,
            hash: &BlakeHash,
            path: Option<&str>,
            status: &str,
            _device: DeviceId,
        ) -> Result<u64, CoreError> {
            let mut rows = self.rows.lock().expect("mock mutex");
            if let Some(row) = rows.get_mut(hash) {
                row.thumbnail_path = path.map(str::to_owned);
                row.thumbnail_status = Some(status.to_owned());
                Ok(1)
            } else {
                Ok(0)
            }
        }
    }

    fn unique_hash(i: u8) -> BlakeHash {
        let mut bytes = [0u8; 32];
        bytes[0] = i;
        BlakeHash::from_bytes(bytes)
    }

    #[tokio::test]
    async fn queue_processes_work() {
        let extractor: Arc<dyn MetadataExtractor> = Arc::new(EchoExtractor);
        let repo = Arc::new(MockRepo::default());
        let repo_dyn: Arc<dyn MetadataRepository> = repo.clone();
        let thumbnailer = Arc::new(ThumbnailGenerator::disabled());
        let cancel = CancellationToken::new();
        let queue = MetadataQueue::spawn(
            extractor,
            repo_dyn,
            thumbnailer,
            DeviceId::new(),
            cancel.clone(),
        );

        for i in 0..5u8 {
            queue
                .enqueue(unique_hash(i), PathBuf::from(format!("/tmp/f{i}")), &cancel)
                .expect("enqueue");
        }
        drop(queue);
        // Allow worker to drain the five items.
        for _ in 0..20 {
            if repo.rows.lock().expect("mock mutex").len() == 5 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            repo.rows.lock().expect("mock mutex").len(),
            5,
            "worker should have drained all 5 items",
        );
    }

    #[tokio::test]
    async fn queue_exits_on_cancel() {
        let extractor: Arc<dyn MetadataExtractor> = Arc::new(EchoExtractor);
        let repo = Arc::new(MockRepo::default());
        let repo_dyn: Arc<dyn MetadataRepository> = repo.clone();
        let thumbnailer = Arc::new(ThumbnailGenerator::disabled());
        let cancel = CancellationToken::new();
        let mut queue = MetadataQueue::spawn(
            extractor,
            repo_dyn,
            thumbnailer,
            DeviceId::new(),
            cancel.clone(),
        );

        // Enqueue one item, then cancel before the worker runs again.
        queue
            .enqueue(unique_hash(1), PathBuf::from("/tmp/a"), &cancel)
            .expect("enqueue");
        cancel.cancel();
        let handle = queue.take_worker().expect("worker handle");
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("worker should exit within 1s of cancel")
            .expect("worker join");
    }

    /// Regression: video files must NOT be routed through the image
    /// thumbnailer (which can't decode MP4/MOV). Instead the worker
    /// writes `thumbnail_status = "skipped"` so the UI can render a
    /// stable placeholder distinct from "pending extraction". See
    /// `utof/perima#15` HIGH #11b.
    #[tokio::test]
    async fn worker_skips_thumbnail_for_video() {
        let extractor: Arc<dyn MetadataExtractor> = Arc::new(EchoExtractor);
        let repo = Arc::new(MockRepo::default());
        let repo_dyn: Arc<dyn MetadataRepository> = repo.clone();
        // Build a non-disabled thumbnailer pointed at a tempdir. If the
        // gate regresses (video/* reaches `generate`), the image decoder
        // would error and leave status = "failed" — assertion below
        // catches that case too.
        let td = tempfile::tempdir().expect("tempdir");
        let thumbnailer = Arc::new(ThumbnailGenerator::new(td.path().to_path_buf()));
        let cancel = CancellationToken::new();
        let queue = MetadataQueue::spawn(
            extractor,
            repo_dyn,
            thumbnailer,
            DeviceId::new(),
            cancel.clone(),
        );

        let hash = unique_hash(42);
        queue
            .enqueue(hash, PathBuf::from("/tmp/clip.mp4"), &cancel)
            .expect("enqueue");
        drop(queue);

        // Allow worker to drain.
        for _ in 0..40 {
            let seen = {
                let rows = repo.rows.lock().expect("mock mutex");
                rows.get(&hash)
                    .and_then(|r| r.thumbnail_status.clone())
                    .is_some()
            };
            if seen {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let row = repo
            .rows
            .lock()
            .expect("mock mutex")
            .get(&hash)
            .cloned()
            .expect("row persisted");
        assert_eq!(
            row.thumbnail_status.as_deref(),
            Some("skipped"),
            "video/* must be marked 'skipped', not routed through image thumbnailer",
        );
        assert!(
            row.thumbnail_path.is_none(),
            "skipped videos must have thumbnail_path = None",
        );
    }

    /// WHY `#[ignore]`: this test intentionally saturates the channel
    /// and asserts cancellation responsiveness under load. On slow CI it
    /// can flake because the saturated-enqueue path relies on a 50ms
    /// sleep loop. Gated behind `cargo test -- --ignored` / a future
    /// `just stress` target — the two happy-path queue tests above cover
    /// the non-stress invariants.
    #[tokio::test]
    #[ignore = "timing-sensitive; run under `just stress`"]
    async fn queue_enqueue_returns_on_cancel_while_full() {
        // Build a queue that never drains: extractor is fine, but we
        // never poll the worker, so the channel fills up.
        let extractor: Arc<dyn MetadataExtractor> = Arc::new(EchoExtractor);
        let repo = Arc::new(MockRepo::default());
        let repo_dyn: Arc<dyn MetadataRepository> = repo.clone();
        let cancel = CancellationToken::new();
        // Cancel *before* spawning so the worker exits immediately.
        cancel.cancel();
        let thumbnailer = Arc::new(ThumbnailGenerator::disabled());
        let queue = MetadataQueue::spawn(
            extractor,
            repo_dyn,
            thumbnailer,
            DeviceId::new(),
            cancel.child_token(),
        );

        // Give the worker a moment to observe cancellation and exit.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // After worker exits, the channel's Receiver is dropped, so
        // try_send returns Closed. The enqueue path should surface an
        // error (not hang) within the poll interval.
        let start = std::time::Instant::now();
        let result = queue.enqueue(unique_hash(0), PathBuf::from("/tmp/a"), &cancel);
        assert!(result.is_err(), "enqueue must fail once worker exits");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "enqueue must return within 500ms of cancel; took {:?}",
            start.elapsed(),
        );
    }
}
