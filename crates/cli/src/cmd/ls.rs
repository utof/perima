//! `perima ls` — thin delegator to [`perima_app::MetadataUseCase`] with a
//! small CLI-side post-filter for `--volume` + `--tag` args that the
//! `UseCase` does not yet surface through its command enum.
//!
//! WHY use `container.tags` for `--tag`: the
//! `TagRepository::files_with_tag` port is not exposed through
//! `TagUseCase::List` output. Post-Batch-C Task 3, `AppContainer`
//! exposes `Arc<dyn TagRepository>` directly so the filter path no
//! longer needs a short-lived `SqliteTagRepository::new(...)` open.
//! A future `UseCase` extension can lift this into `MetadataCommand`.

use std::collections::HashSet;
use std::io::Write;

use perima_app::{AppContainer, MetadataCommand, MetadataOutput};
use perima_core::{BlakeHash, CoreError, DeviceId, FileLocationRecord, VolumeId, normalize_tag};

/// Arguments for the ls command.
#[derive(Debug, Clone)]
pub(crate) struct LsArgs {
    /// Filter to a specific volume.
    pub volume: Option<VolumeId>,
    /// Maximum number of rows to return.
    pub limit: usize,
    /// Output as JSON instead of a human-readable table.
    pub json: bool,
    /// Include media metadata columns (`captured_at`, dimensions, `camera_model`).
    pub with_metadata: bool,
    /// Filter to files carrying this tag (normalized before lookup).
    pub tag: Option<String>,
}

/// Execute `ls`.
///
/// Reads file-location records (optionally joined with metadata) through
/// [`perima_app::MetadataUseCase`], then post-filters by `--volume` +
/// `--tag` in memory. Prints either a human-readable table or JSON.
///
/// # Errors
/// Propagates `CoreError` from the `UseCase` / repositories.
pub(crate) async fn run(
    container: &AppContainer,
    _data_dir: &std::path::Path,
    device: DeviceId,
    args: &LsArgs,
) -> Result<(), CoreError> {
    // Build the optional tag-filter hash set before any repo queries so
    // a bad tag name errors cleanly before output.
    let tag_filter: Option<HashSet<BlakeHash>> = args
        .tag
        .as_deref()
        .map(|raw| build_tag_filter(container, raw))
        .transpose()?;

    // WHY limit widening: downstream post-filters by volume/tag reduce the
    // pool. If the caller passed `--limit 100 --volume X` and we only fetch
    // 100 pre-filter, we may lose rows on volume X. For shell use with a
    // small index this is fine; the fully-correct fix is a
    // `MetadataCommand` with volume + tag filters baked in (follow-up).
    let effective_limit: u32 = u32::try_from(args.limit).unwrap_or(u32::MAX);

    if args.with_metadata {
        let out = container
            .metadata
            .execute(MetadataCommand::ListFilesWithMetadata {
                limit: Some(effective_limit),
                offset: None,
                device,
            })
            .await?;
        let MetadataOutput::FilesWithMetadata(rows) = out else {
            return Err(CoreError::Internal(
                "ListFilesWithMetadata returned non-FilesWithMetadata output".into(),
            ));
        };
        let rows = apply_metadata_volume_filter(rows, args.volume);
        let rows = apply_metadata_tag_filter(rows, tag_filter.as_ref());
        if args.json {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            serde_json::to_writer_pretty(&mut handle, &rows)
                .map_err(|e| CoreError::Internal(format!("json: {e}")))?;
            writeln!(handle).map_err(CoreError::from)?;
        } else {
            print_table_with_metadata(&rows)?;
        }
        return Ok(());
    }

    let out = container
        .metadata
        .execute(MetadataCommand::ListFiles {
            limit: Some(effective_limit),
            offset: None,
            device,
        })
        .await?;
    let MetadataOutput::Files(records) = out else {
        return Err(CoreError::Internal(
            "ListFiles returned non-Files output".into(),
        ));
    };
    let records = apply_volume_filter(records, args.volume);
    let records = apply_tag_filter(records, tag_filter.as_ref());
    if args.json {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        serde_json::to_writer_pretty(&mut handle, &records)
            .map_err(|e| CoreError::Internal(format!("json: {e}")))?;
        writeln!(handle).map_err(CoreError::from)?;
    } else {
        print_table(&records)?;
    }
    Ok(())
}

/// Look up a tag by name via the container's tag port and collect all
/// hashes that carry it.
fn build_tag_filter(container: &AppContainer, raw: &str) -> Result<HashSet<BlakeHash>, CoreError> {
    let normalized = normalize_tag(raw)?;
    let all_tags = container.tags.list_tags()?;
    let tag = all_tags
        .into_iter()
        .find(|t| t.name == normalized)
        .ok_or_else(|| CoreError::NotFound(format!("tag not found: {normalized}")))?;
    let hashes = container.tags.files_with_tag(tag.id)?;
    Ok(hashes.into_iter().collect())
}

fn apply_volume_filter(
    records: Vec<FileLocationRecord>,
    volume: Option<VolumeId>,
) -> Vec<FileLocationRecord> {
    match volume {
        None => records,
        Some(v) => records.into_iter().filter(|r| r.volume_id == v).collect(),
    }
}

fn apply_metadata_volume_filter(
    rows: Vec<perima_core::FileWithMetadataRow>,
    volume: Option<VolumeId>,
) -> Vec<perima_core::FileWithMetadataRow> {
    match volume {
        None => rows,
        Some(v) => rows
            .into_iter()
            .filter(|(r, _, _, _)| r.volume_id == v)
            .collect(),
    }
}

fn apply_tag_filter(
    records: Vec<FileLocationRecord>,
    filter: Option<&HashSet<BlakeHash>>,
) -> Vec<FileLocationRecord> {
    match filter {
        None => records,
        // WHY `r.hash.is_some_and(...)`: post-Task-11 `FileLocationRecord.hash`
        // is `Option<BlakeHash>`. Pending files (no full_hash) cannot match a
        // tag filter keyed on content hash; they're silently excluded.
        Some(set) => records
            .into_iter()
            .filter(|r| r.hash.is_some_and(|h| set.contains(&h)))
            .collect(),
    }
}

fn apply_metadata_tag_filter(
    rows: Vec<perima_core::FileWithMetadataRow>,
    filter: Option<&HashSet<BlakeHash>>,
) -> Vec<perima_core::FileWithMetadataRow> {
    match filter {
        None => rows,
        Some(set) => rows
            .into_iter()
            .filter(|(r, _, _, _)| r.hash.is_some_and(|h| set.contains(&h)))
            .collect(),
    }
}

fn print_table(records: &[FileLocationRecord]) -> Result<(), CoreError> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    writeln!(
        handle,
        "{:<10} {:<10} {:<10} PATH",
        "HASH", "SIZE", "VOLUME",
    )
    .map_err(CoreError::from)?;
    for r in records {
        // WHY `pending` placeholder when hash is None: post-Task-11 a row may
        // have no `full_hash` yet (pending dedup). Render an 8-char "pending"
        // sigil so columns align without forcing the operator to scroll.
        let hash_hex = r.hash.map(|h| h.to_hex());
        let hash_short = hash_hex.as_deref().map_or("pending ", |h| &h[..8]);
        let vol_str = r.volume_id.0.to_string();
        let vol_short = &vol_str[..8];
        let size = super::format::format_size(r.size.0);
        writeln!(
            handle,
            "{hash_short}…  {size:<10} {vol_short}…  {}",
            r.relative_path.as_str()
        )
        .map_err(CoreError::from)?;
    }
    Ok(())
}

/// Render `ls --with-metadata` as a human-readable table.
fn print_table_with_metadata(rows: &[perima_core::FileWithMetadataRow]) -> Result<(), CoreError> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    writeln!(
        handle,
        "{:<10} {:<10} {:<10} {:<20} {:<10} {:<20} PATH",
        "HASH", "SIZE", "VOLUME", "CAPTURED_AT", "DIMS", "CAMERA",
    )
    .map_err(CoreError::from)?;
    for (r, meta, _quick_hash, _mount_path) in rows {
        let hash_hex = r.hash.map(|h| h.to_hex());
        let hash_short = hash_hex.as_deref().map_or("pending ", |h| &h[..8]);
        let vol_str = r.volume_id.0.to_string();
        let vol_short = &vol_str[..8];
        let size = super::format::format_size(r.size.0);
        let captured_at = meta
            .as_ref()
            .and_then(|m| m.captured_at.clone())
            .unwrap_or_else(|| "-".to_owned());
        let dims = meta
            .as_ref()
            .and_then(|m| match (m.width, m.height) {
                (Some(w), Some(h)) => Some(format!("{w}x{h}")),
                _ => None,
            })
            .unwrap_or_else(|| "-".to_owned());
        let camera = meta
            .as_ref()
            .and_then(|m| m.camera_model.clone())
            .unwrap_or_else(|| "-".to_owned());
        writeln!(
            handle,
            "{hash_short}…  {size:<10} {vol_short}…  {captured_at:<20} {dims:<10} {camera:<20} {}",
            r.relative_path.as_str()
        )
        .map_err(CoreError::from)?;
    }
    Ok(())
}
