//! `perima ls` implementation.

use std::collections::HashSet;
use std::io::Write;

use perima_core::{
    BlakeHash, CoreError, FileLocationRecord, FileRepository, MediaMetadata, MetadataRepository,
    TagRepository, VolumeId, normalize_tag,
};

/// Arguments for the ls command.
#[derive(Debug, Clone)]
pub struct LsArgs {
    /// Filter to a specific volume.
    pub volume: Option<VolumeId>,
    /// Maximum number of rows to return.
    pub limit: usize,
    /// Output as JSON instead of a human-readable table.
    pub json: bool,
    /// Include media metadata columns (`captured_at`, dimensions, `camera_model`).
    ///
    /// WHY opt-in flag: the `--with-metadata` path uses a LEFT JOIN
    /// against `file_metadata`, which is slightly more expensive than
    /// the base `list_file_locations` query and returns extra columns
    /// that older scripts do not expect. Keeping the default narrow
    /// preserves v0.3.x output stability.
    pub with_metadata: bool,
    /// Filter to files carrying this tag (normalized before lookup).
    pub tag: Option<String>,
}

/// Execute `ls`.
///
/// Reads all file location records from `repo` (up to `args.limit`)
/// and prints them either as a human-readable table or as JSON. When
/// `args.with_metadata` is `true`, the listing routes through
/// `metadata_repo` so each row is joined with its (optional)
/// `file_metadata`. When `args.tag` is `Some`, only files whose
/// content hash carries the named tag are shown.
///
/// # Errors
/// Propagates `CoreError` from the repository.
pub fn run<R, M, T>(
    repo: &R,
    metadata_repo: &M,
    tag_repo: &T,
    args: &LsArgs,
) -> Result<(), CoreError>
where
    R: FileRepository + ?Sized,
    M: MetadataRepository + ?Sized,
    T: TagRepository + ?Sized,
{
    // Build the optional tag-filter hash set before any repo queries.
    // WHY eager: errors on bad tag names should surface before any output
    // is written to stdout, so the user gets a clean error message.
    let tag_filter: Option<HashSet<BlakeHash>> = args
        .tag
        .as_deref()
        .map(|raw| build_tag_filter(tag_repo, raw))
        .transpose()?;

    if args.with_metadata {
        let rows = metadata_repo.list_with_metadata(args.limit, args.volume)?;
        let rows = apply_metadata_tag_filter(rows, tag_filter.as_ref());
        if args.json {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            serde_json::to_writer_pretty(&mut handle, &rows)
                .map_err(|e| CoreError::Internal(format!("json: {e}")))?;
            writeln!(handle).map_err(CoreError::Io)?;
        } else {
            print_table_with_metadata(&rows)?;
        }
        return Ok(());
    }

    let records = repo.list_file_locations(args.limit, args.volume)?;
    let records = apply_tag_filter(records, tag_filter.as_ref());
    if args.json {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        serde_json::to_writer_pretty(&mut handle, &records)
            .map_err(|e| CoreError::Internal(format!("json: {e}")))?;
        writeln!(handle).map_err(CoreError::Io)?;
    } else {
        print_table(&records)?;
    }
    Ok(())
}

/// Look up a tag by name and collect all hashes that carry it.
///
/// WHY returns `HashSet`: the caller performs membership tests once per
/// location record. A `Vec` lookup would be O(n·m) for large libraries;
/// a `HashSet` makes each test O(1).
fn build_tag_filter<T>(tag_repo: &T, raw: &str) -> Result<HashSet<BlakeHash>, CoreError>
where
    T: TagRepository + ?Sized,
{
    let normalized = normalize_tag(raw)?;
    let all_tags = tag_repo.list_tags()?;
    let tag = all_tags
        .into_iter()
        .find(|t| t.name == normalized)
        .ok_or_else(|| CoreError::NotFound(format!("tag not found: {normalized}")))?;
    let hashes = tag_repo.files_with_tag(tag.id)?;
    Ok(hashes.into_iter().collect())
}

/// Retain only records whose hash is in `filter`.
///
/// WHY consumes the vec: filtering in-place avoids an extra allocation
/// and the caller no longer needs the original unfiltered list.
fn apply_tag_filter(
    records: Vec<FileLocationRecord>,
    filter: Option<&HashSet<BlakeHash>>,
) -> Vec<FileLocationRecord> {
    match filter {
        None => records,
        Some(set) => records
            .into_iter()
            .filter(|r| set.contains(&r.hash))
            .collect(),
    }
}

/// Retain only `(location, meta)` pairs whose hash is in `filter`.
fn apply_metadata_tag_filter(
    rows: Vec<(FileLocationRecord, Option<MediaMetadata>)>,
    filter: Option<&HashSet<BlakeHash>>,
) -> Vec<(FileLocationRecord, Option<MediaMetadata>)> {
    match filter {
        None => rows,
        Some(set) => rows
            .into_iter()
            .filter(|(r, _)| set.contains(&r.hash))
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
    .map_err(CoreError::Io)?;
    for r in records {
        let hash_hex = r.hash.to_hex();
        let hash_short = &hash_hex[..8];
        let vol_str = r.volume_id.0.to_string();
        let vol_short = &vol_str[..8];
        let size = super::format::format_size(r.size.0);
        writeln!(
            handle,
            "{hash_short}…  {size:<10} {vol_short}…  {}",
            r.relative_path.as_str()
        )
        .map_err(CoreError::Io)?;
    }
    Ok(())
}

/// Render `ls --with-metadata` as a human-readable table.
///
/// WHY separate helper (not a branch inside [`print_table`]): the two
/// tables have different column counts and different `writeln!` format
/// strings; sharing the body would mean nullable placeholders for the
/// metadata columns on plain `ls`, which is more confusing than a
/// parallel function.
fn print_table_with_metadata(
    rows: &[(FileLocationRecord, Option<MediaMetadata>)],
) -> Result<(), CoreError> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    writeln!(
        handle,
        "{:<10} {:<10} {:<10} {:<20} {:<10} {:<20} PATH",
        "HASH", "SIZE", "VOLUME", "CAPTURED_AT", "DIMS", "CAMERA",
    )
    .map_err(CoreError::Io)?;
    for (r, meta) in rows {
        let hash_hex = r.hash.to_hex();
        let hash_short = &hash_hex[..8];
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
        .map_err(CoreError::Io)?;
    }
    Ok(())
}
