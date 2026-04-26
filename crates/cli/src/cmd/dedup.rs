//! `perima dedup` subcommand — CLI counterpart of the `/dedup` desktop route.
//!
//! Three modes:
//! - `perima dedup check` — list candidate groups (files sharing `quick_hash`).
//! - `perima dedup verify` — list candidates + compute full hash on each group.
//! - `perima dedup mark-distinct <uuid>...` — mark files as verified-distinct.

use std::path::Path;

use perima_app::AppContainer;
use perima_core::{CoreError, DeviceId, FileUuid};

/// Arguments for the `perima dedup` command.
#[derive(clap::Args, Debug)]
pub(crate) struct DedupArgs {
    /// The dedup action to perform.
    #[command(subcommand)]
    pub action: DedupAction,
}

/// Individual actions available under `perima dedup`.
#[derive(clap::Subcommand, Debug)]
pub(crate) enum DedupAction {
    /// Print all candidate duplicate groups (files sharing `quick_hash`).
    ///
    /// Each group is printed as a section with the shared `quick_hash` fingerprint
    /// followed by one line per file showing path and size.
    Check,

    /// Print candidate groups AND compute full hash on every file in each group
    /// to confirm or deny true duplication.
    ///
    /// Files that fail hashing (not mounted, I/O error) are reported as errors
    /// but do not abort the run. After verification, groups are labelled
    /// "TRUE DUPLICATE" or "FALSE POSITIVE" based on full-hash equality.
    Verify,

    /// Memorise that the listed file UUIDs share a `quick_hash` but are verified
    /// distinct (full hashes differ). They will be excluded from future
    /// `check` and `verify` output.
    #[command(name = "mark-distinct")]
    MarkDistinct {
        /// UUIDs of files to mark as verified-distinct.
        /// Must include every file in the group to suppress the group.
        #[arg(required = true, num_args = 1..)]
        file_uuids: Vec<String>,
    },
}

/// Execute `perima dedup <action>`.
///
/// # Errors
/// Propagates [`CoreError`] from the underlying use-case ports.
pub(crate) async fn run(
    container: &AppContainer,
    _data_dir: &Path,
    device: DeviceId,
    args: &DedupArgs,
) -> Result<(), CoreError> {
    match &args.action {
        DedupAction::Check => run_check(container),
        DedupAction::Verify => run_verify(container).await,
        DedupAction::MarkDistinct { file_uuids } => {
            run_mark_distinct(container, device, file_uuids)
        }
    }
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

/// `perima dedup check` — print all candidate duplicate groups.
fn run_check(container: &AppContainer) -> Result<(), CoreError> {
    let groups = container.dedup.list_collisions()?;

    if groups.is_empty() {
        println!("no duplicate candidates found");
        return Ok(());
    }

    println!("{} candidate group(s):", groups.len());

    for (i, group) in groups.iter().enumerate() {
        println!();
        println!(
            "--- group {} of {} ---  quick_hash: {}  state: {:?}",
            i + 1,
            groups.len(),
            group.quick_hash.to_hex(),
            group.verified_state
        );

        for file in &group.files {
            let uuid = file.file_uuid.0;
            let path = file.relative_path.as_str();
            let size = file.size.0;
            let hash_str = file
                .hash
                .as_ref()
                .map_or_else(|| "(pending)".to_owned(), perima_core::BlakeHash::to_hex);
            println!("  [{uuid}]  {path}  {size} bytes  full_hash: {hash_str}");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// verify
// ---------------------------------------------------------------------------

/// `perima dedup verify` — list candidates + compute full hash on each.
///
/// WHY sequential per-file hashing: avoids parallel disk thrash on HDDs
/// (spec §4.5). Groups are processed one at a time; within each group files
/// are hashed sequentially.
async fn run_verify(container: &AppContainer) -> Result<(), CoreError> {
    let groups = container.dedup.list_collisions()?;

    if groups.is_empty() {
        println!("no duplicate candidates found");
        return Ok(());
    }

    println!(
        "{} candidate group(s) — verifying by full hash:",
        groups.len()
    );

    for (i, group) in groups.iter().enumerate() {
        println!(
            "\n--- group {} of {} ---  quick_hash: {}",
            i + 1,
            groups.len(),
            group.quick_hash.to_hex()
        );

        // Collect the full hashes for each file in the group.
        let mut full_hashes: Vec<Option<String>> = Vec::with_capacity(group.files.len());

        for file in &group.files {
            match container
                .compute_full_hash
                .execute_single(file.file_uuid)
                .await
            {
                Ok(hash) => {
                    let hash_hex = hash.to_hex();
                    println!(
                        "  [{}]  {}  full_hash: {}",
                        file.file_uuid.0,
                        file.relative_path.as_str(),
                        hash_hex
                    );
                    full_hashes.push(Some(hash_hex));
                }
                Err(e) => {
                    eprintln!(
                        "  [{}]  {}  ERROR: {e}",
                        file.file_uuid.0,
                        file.relative_path.as_str()
                    );
                    full_hashes.push(None);
                }
            }
        }

        // Determine verification outcome for this group.
        let computed: Vec<&str> = full_hashes.iter().filter_map(|h| h.as_deref()).collect();

        let verdict = if computed.is_empty() {
            "UNVERIFIABLE (all files failed to hash)"
        } else {
            let first = computed[0];
            let all_match = computed.iter().all(|h| *h == first);
            if all_match {
                "TRUE DUPLICATE"
            } else {
                "FALSE POSITIVE (full hashes differ)"
            }
        };

        println!("  => {verdict}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// mark-distinct
// ---------------------------------------------------------------------------

/// `perima dedup mark-distinct <uuid>...` — mark files as verified-distinct.
fn run_mark_distinct(
    container: &AppContainer,
    device: DeviceId,
    raw_uuids: &[String],
) -> Result<(), CoreError> {
    let file_uuids: Result<Vec<FileUuid>, CoreError> = raw_uuids
        .iter()
        .map(|s| {
            uuid::Uuid::parse_str(s)
                .map(FileUuid)
                .map_err(|e| CoreError::InvalidPath(format!("bad file UUID '{s}': {e}")))
        })
        .collect();
    let file_uuids = file_uuids?;
    let count = file_uuids.len();

    container.dedup.mark_verified_distinct(file_uuids, device)?;

    println!("marked {count} file(s) as verified-distinct");
    Ok(())
}
