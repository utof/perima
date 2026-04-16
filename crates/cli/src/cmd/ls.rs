//! `perima ls` implementation.

use std::io::Write;

use perima_core::{CoreError, FileLocationRecord, FileRepository, VolumeId};

/// Arguments for the ls command.
#[derive(Debug, Clone)]
pub struct LsArgs {
    /// Filter to a specific volume.
    pub volume: Option<VolumeId>,
    /// Maximum number of rows to return.
    pub limit: usize,
    /// Output as JSON instead of a human-readable table.
    pub json: bool,
}

/// Execute `ls`.
///
/// Reads all file location records from `repo` (up to `args.limit`)
/// and prints them either as a human-readable table or as JSON.
///
/// # Errors
/// Propagates `CoreError` from the repository.
pub fn run<R: FileRepository + ?Sized>(repo: &R, args: &LsArgs) -> Result<(), CoreError> {
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
