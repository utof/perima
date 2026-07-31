//! Wire-types for commands that join multiple domain records.
//!
//! WHY separate module: `crates/desktop/src/commands.rs` previously hosted
//! `FileEntry` and `VolumeEntry`, which mapped 1:1 to single `perima-core`
//! records. Those were deleted in Batch D Task 8 — the core types now derive
//! `specta::Type` and are used directly in handler signatures. This module
//! retains only the composite payloads that flatten a `(record,
//! optional_record)` pair with no clean 1:1 core analogue.
//!
//! WHY `TagPayload` and `SearchHitPayload` were deleted (Batch D Task 8):
//! `perima_core::Tag` and `perima_core::SearchHit` now derive
//! `specta::Type`; no shell-side mirror is needed. Handlers return the
//! core types directly.
//!
//! WHY `FileWithMetadataPayload` is retained (spec §8 #6): it flattens a
//! `(FileLocationRecord, Option<MediaMetadata>)` pair into a single-level
//! object that the UI grid can bind without traversing optional sub-objects.
//! There is no equivalent flat type in `perima-core`. The same rationale
//! applies to `FileWithTagsPayload`.

use perima_core::{FileLocationRecord, FileUuid, MediaMetadata, Tag};
use serde::Serialize;

// ---------------------------------------------------------------------------
// Transcription wire-types (T7)
//
// WHY a wire-type mirror for `TranscribeStartedPayload` (not specta-derive on
// `perima_app::TranscribeOutput`):
//
// 1. `TranscribeOutput` is `Debug + Clone` only — no `Serialize`. Adding
//    `Serialize` + `specta::Type` to the entire enum would also expose the
//    `Cancelled` variant across the IPC boundary, which the `cancel_transcription`
//    command does not need (it returns `Result<(), CoreError>`). The
//    transcription Tauri commands surface a tightly-scoped IPC API; the wire
//    type captures only the fields the frontend actually needs.
// 2. `TranscribeCommand::Start { source: PathBuf, ... }` does not cross the IPC
//    boundary at all — the Tauri handler accepts `source: String` (Tauri
//    serializes paths as strings) and constructs the `PathBuf` shell-side
//    before calling the use-case. So no specta-derive on the command enum is
//    needed.
//
// `ListProvidersPayload` + `ProviderListEntry` are shell-private composites:
// they query the OS keyring at construction time to set the `has_key` flag,
// which is a Tauri-shell concern (CLI's `auth list` does the same lookup
// shell-side). No clean core/app analogue exists.
// ---------------------------------------------------------------------------

/// Returned by the `transcribe` Tauri command — mirror of
/// [`perima_app::TranscribeOutput::Started`].
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct TranscribeStartedPayload {
    /// Per-request `UUIDv7` (lowercase-hex simple form). Pairs with every
    /// `AppEvent::Transcription*` variant for this job.
    pub request_uuid: String,
    /// 1-based position in the queue at the moment of enqueue.
    pub queue_position: u32,
}

/// Returned by the `list_providers` Tauri command. Combines the TOML config
/// view with a per-provider keyring lookup.
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct ListProvidersPayload {
    /// Active provider name from `[transcription].active_provider`, or `None`
    /// when the user has not selected one yet.
    pub active: Option<String>,
    /// One entry per provider in `[transcription.providers.*]`.
    pub providers: Vec<ProviderListEntry>,
}

/// One row in [`ListProvidersPayload::providers`].
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct ProviderListEntry {
    /// Provider name (the `[transcription.providers.<name>]` key).
    pub name: String,
    /// Preset name (`groq`, `openai`, `custom`, ...).
    pub preset: String,
    /// Optional model override; `None` falls back to the preset's default.
    pub model: Option<String>,
    /// Whether a keyring entry exists for this provider on this device.
    /// Computed via [`keyring::Entry::get_password`] at command-handler time.
    pub has_key: bool,
}

/// Flattened `(FileLocationRecord, Option<MediaMetadata>)` pair for the
/// frontend.
///
/// WHY flat (not nested `{ location: …, metadata: … }`): the UI grid
/// binds one row per location and needs every column addressable with a
/// single key. Nesting would force every cell to traverse an optional
/// subobject just to discover it is absent. Flat fields with `None`
/// columns match SQL's native shape and the existing encoding.
///
/// WHY `file_uuid` non-nullable + `hash` nullable (Task 11, spec §4.8):
/// `file_uuid` is the stable surrogate present on every `files` row from V011
/// on, so React keys + IPC lookups use it. `hash` (full BLAKE3) is `None` for
/// pending files (no `full_hash` computed yet).
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct FileWithMetadataPayload {
    // File-location fields (mirrors `FileLocationRecord`).
    /// Stable surrogate identifier for the file row (`UUIDv7`).
    pub file_uuid: FileUuid,
    /// BLAKE3-256 content hash as lowercase hex; `None` until `full_hash`
    /// computes for this file.
    pub hash: Option<String>,
    /// Quick-hash value as lowercase hex, or `None` if the backfill worker
    /// has not yet run for this row.
    ///
    /// WHY exposed here: pre-V012 `blake3_hash` is `NOT NULL` — the scan
    /// path stores `quick_hash` there as a placeholder until
    /// `compute_full_hash` promotes the real hash. So `hash == null` never
    /// fires for any file today. The frontend detects placeholder rows via
    /// `hash === quick_hash` (same value = placeholder, different = promoted).
    pub quick_hash: Option<String>,
    /// File size in bytes.
    pub size: u64,
    /// Volume UUID.
    pub volume_id: String,
    /// Relative path within the volume.
    pub relative_path: String,
    /// Location status string (`"active"`, `"missing"`, `"moved"`, `"stale"`).
    pub status: String,
    /// ISO 8601 UTC timestamp of first sighting.
    pub first_seen: String,

    // Media-metadata fields — all optional; absent when no
    // `file_metadata` row exists or a field was not extractable.
    /// Pixel width (images / video).
    pub width: Option<u32>,
    /// Pixel height (images / video).
    pub height: Option<u32>,
    /// Duration in milliseconds (video / audio).
    pub duration_ms: Option<u64>,
    /// ISO 8601 UTC capture timestamp.
    pub captured_at: Option<String>,
    /// Camera manufacturer (EXIF `Make`).
    pub camera_make: Option<String>,
    /// Camera model (EXIF `Model`).
    pub camera_model: Option<String>,
    /// Codec identifier (e.g. `"avc1"`, `"hevc"`).
    pub codec: Option<String>,
    /// Overall bitrate in bits per second.
    pub bitrate_bps: Option<u32>,
    /// MIME type as detected at extraction time.
    pub mime_type: Option<String>,
    /// Absolute on-disk path to the generated WebP thumbnail; `None`
    /// until the `MetadataQueue` worker writes the thumbnail.
    pub thumbnail_path: Option<String>,
    /// Thumbnail lifecycle: `"pending"`, `"ready"`, `"failed"`, or
    /// `None` if the metadata row predates v0.4.1.
    pub thumbnail_status: Option<String>,
    /// Absolute on-disk path joining the volume's current mount root
    /// with [`Self::relative_path`]. `None` when the volume is not
    /// currently mounted on this machine.
    ///
    /// WHY exposed here (T9 review fix): the desktop transcribe
    /// command (and any future open-file / preview / export action)
    /// must hand ffmpeg / the OS an absolute path — relative paths
    /// resolve against the desktop process cwd, which is rarely the
    /// volume root, producing silent ENOENT. Pre-fix the frontend was
    /// passing `relative_path` to `transcribe`, which forwards to
    /// ffmpeg unchanged.
    ///
    /// WHY `Option<String>`: a volume that is currently unmounted has
    /// no `volume_mounts` row → SQL produces NULL → frontend disables
    /// the dependent action with a "Volume not mounted" tooltip.
    pub absolute_path: Option<String>,
}

/// File-with-metadata plus its attached tags.
///
/// WHY compose (not extend `FileWithMetadataPayload`): keeps each
/// payload focused. The `tags` field is a `Vec`, not a flat field set,
/// so it doesn't fit the "one-column-per-SQL-field" flat pattern of
/// the metadata payload.
///
/// WHY `tags: Vec<Tag>` (not `Vec<TagPayload>`): `perima_core::Tag`
/// now derives `specta::Type`; no shell-side mirror is needed
/// (Batch D Task 8).
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct FileWithTagsPayload {
    /// All file + metadata fields (flat).
    #[serde(flatten)]
    pub file: FileWithMetadataPayload,
    /// Tags attached to this content hash.
    pub tags: Vec<Tag>,
}

impl
    From<(
        FileLocationRecord,
        Option<MediaMetadata>,
        Option<String>,
        Option<String>,
    )> for FileWithMetadataPayload
{
    fn from(
        (loc, meta, quick_hash, mount_path): (
            FileLocationRecord,
            Option<MediaMetadata>,
            Option<String>,
            Option<String>,
        ),
    ) -> Self {
        // WHY unzip via `meta.map(...)`: the nine optional metadata
        // fields each need independent `None` defaults when the
        // metadata row is absent. Chained `.map(|m| m.field.clone())`
        // through `Option::and_then` would still require one call per
        // field; an explicit destructure is equally verbose but
        // strictly more readable.
        let (
            width,
            height,
            duration_ms,
            captured_at,
            camera_make,
            camera_model,
            codec,
            bitrate_bps,
            mime_type,
            thumbnail_path,
            thumbnail_status,
        ) = match meta {
            Some(m) => (
                m.width,
                m.height,
                m.duration_ms,
                m.captured_at,
                m.camera_make,
                m.camera_model,
                m.codec,
                m.bitrate_bps,
                m.mime_type,
                m.thumbnail_path,
                m.thumbnail_status,
            ),
            None => (
                None, None, None, None, None, None, None, None, None, None, None,
            ),
        };
        // WHY compute absolute_path here (not in SQL): the join already
        // produces `mount_path`; combining with `relative_path` is a
        // platform-sensitive `PathBuf::push` that belongs in Rust (SQL
        // string concatenation would produce a forward-slash string on
        // Windows, breaking ffmpeg + the OS open-file APIs). When
        // `mount_path` is None the volume is unmounted — the frontend
        // disables the dependent UI control.
        let absolute_path = mount_path.as_ref().map(|mp| {
            let mut p = std::path::PathBuf::from(mp);
            p.push(loc.relative_path.as_str());
            p.to_string_lossy().into_owned()
        });
        Self {
            file_uuid: loc.file_uuid,
            hash: loc.hash.map(|h| h.to_hex()),
            quick_hash,
            size: loc.size.0,
            volume_id: loc.volume_id.0.to_string(),
            relative_path: loc.relative_path.as_str().to_owned(),
            status: format!("{:?}", loc.status),
            first_seen: loc.first_seen,
            width,
            height,
            duration_ms,
            captured_at,
            camera_make,
            camera_model,
            codec,
            bitrate_bps,
            mime_type,
            thumbnail_path,
            thumbnail_status,
            absolute_path,
        }
    }
}
