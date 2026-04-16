//! `perima ls` implementation.

use std::io::Write;

use perima_core::{
    CoreError, FileLocationRecord, FileRepository, MediaMetadata, MetadataRepository, VolumeId,
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
}

/// Execute `ls`.
///
/// Reads all file location records from `repo` (up to `args.limit`)
/// and prints them either as a human-readable table or as JSON. When
/// `args.with_metadata` is `true`, the listing routes through
/// `metadata_repo` so each row is joined with its (optional)
/// `file_metadata`.
///
/// # Errors
/// Propagates `CoreError` from the repository.
pub fn run<R, M>(repo: &R, metadata_repo: &M, args: &LsArgs) -> Result<(), CoreError>
where
    R: FileRepository + ?Sized,
    M: MetadataRepository + ?Sized,
{
    if args.with_metadata {
        let rows = metadata_repo.list_with_metadata(args.limit, args.volume)?;
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
