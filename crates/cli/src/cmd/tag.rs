//! `perima tag` subcommand — add, remove, and list tags.
//!
//! Provides three sub-subcommands:
//! - `tag add <path> <tags…>` — attach one or more labels to a file
//! - `tag rm <path> <tag>` — detach a label from a file
//! - `tag ls [--json]` — list all active tags with attachment counts

use std::io::Write;
use std::path::PathBuf;

use perima_core::{BlakeHash, CoreError, DeviceId, FileRepository, TagRepository, normalize_tag};

use super::metadata::find_by_absolute_suffix;

/// Arguments for the `perima tag` command.
#[derive(clap::Args, Debug)]
pub(crate) struct TagArgs {
    /// The tag sub-action to perform.
    #[command(subcommand)]
    pub action: TagAction,
}

/// Individual actions available under `perima tag`.
#[derive(clap::Subcommand, Debug)]
pub(crate) enum TagAction {
    /// Attach one or more labels to a file.
    Add {
        /// Path to the file to tag.
        path: PathBuf,
        /// One or more tag names to attach.
        ///
        /// WHY required + 1..: clap's default for Vec<T> is "zero or
        /// more positionals" — `perima tag add foo.jpg` would silently
        /// no-op. Force at least one tag argument so the error is a
        /// clear "missing required tag" message instead of success-
        /// that-did-nothing.
        #[arg(required = true, num_args = 1..)]
        tags: Vec<String>,
    },
    /// Remove a tag from a file.
    Rm {
        /// Path to the file.
        path: PathBuf,
        /// Tag name to remove.
        tag: String,
    },
    /// List all active tags with attachment counts.
    Ls {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Execute `perima tag <action>`.
///
/// # Errors
/// Returns [`CoreError::InvalidPath`] when the file does not exist or
/// is not yet indexed (run `perima scan` first); propagates
/// [`CoreError`] from tag normalization, DB access, and I/O.
pub(crate) fn run<T, F>(
    tag_repo: &T,
    file_repo: &F,
    device: DeviceId,
    args: &TagArgs,
) -> Result<(), CoreError>
where
    T: TagRepository + ?Sized,
    F: FileRepository + ?Sized,
{
    match &args.action {
        TagAction::Add { path, tags } => run_add(tag_repo, file_repo, device, path, tags),
        TagAction::Rm { path, tag } => run_rm(tag_repo, file_repo, device, path, tag),
        TagAction::Ls { json } => run_ls(tag_repo, *json),
    }
}

/// Resolve a path to a `BlakeHash` by suffix-matching indexed locations.
///
/// WHY separate helper: both `run_add` and `run_rm` need the same
/// canonicalization + suffix-match logic. Extracting it avoids
/// duplicating the error messages and the `list_file_locations` call.
fn resolve_hash<F>(file_repo: &F, path: &PathBuf) -> Result<BlakeHash, CoreError>
where
    F: FileRepository + ?Sized,
{
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

    let absolute = dunce::canonicalize(path).map_err(CoreError::Io)?;
    let absolute_str = absolute
        .to_str()
        .ok_or_else(|| CoreError::InvalidPath(format!("non-UTF8 path: {}", absolute.display())))?;

    // WHY list across ALL volumes (None): the user supplies an absolute
    // path that may live on any known volume. We don't know which scan
    // root produced the relative-path record, so suffix-matching against
    // the full location set is the only portable approach.
    let records = file_repo.list_file_locations(usize::MAX, None)?;
    let record = find_by_absolute_suffix(&records, absolute_str).ok_or_else(|| {
        CoreError::InvalidPath(format!(
            "not indexed: {} (run `perima scan` first)",
            absolute.display(),
        ))
    })?;

    Ok(record.hash)
}

/// Attach one or more tags to a file.
fn run_add<T, F>(
    tag_repo: &T,
    file_repo: &F,
    device: DeviceId,
    path: &PathBuf,
    tags: &[String],
) -> Result<(), CoreError>
where
    T: TagRepository + ?Sized,
    F: FileRepository + ?Sized,
{
    let hash = resolve_hash(file_repo, path)?;

    let mut applied = Vec::with_capacity(tags.len());
    for raw in tags {
        let tag = tag_repo.upsert_tag(raw, device)?;
        tag_repo.attach(&hash, tag.id, device)?;
        applied.push(tag.name);
    }

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    writeln!(
        handle,
        "tagged {} with {}",
        path.display(),
        applied.join(", ")
    )
    .map_err(CoreError::Io)
}

/// Remove a tag from a file.
fn run_rm<T, F>(
    tag_repo: &T,
    file_repo: &F,
    device: DeviceId,
    path: &PathBuf,
    tag_raw: &str,
) -> Result<(), CoreError>
where
    T: TagRepository + ?Sized,
    F: FileRepository + ?Sized,
{
    let hash = resolve_hash(file_repo, path)?;

    // WHY upsert_tag for rm: we need the tag's UUID to call detach.
    // upsert_tag is idempotent — if the tag doesn't exist we create it
    // (harmless), then detach finds no active row (no-op soft-delete).
    let tag = tag_repo.upsert_tag(tag_raw, device)?;
    tag_repo.detach(&hash, tag.id, device)?;

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let normalized = normalize_tag(tag_raw)?;
    writeln!(handle, "removed {} from {}", normalized, path.display()).map_err(CoreError::Io)
}

/// List all active tags with their per-tag file counts.
fn run_ls<T>(tag_repo: &T, json: bool) -> Result<(), CoreError>
where
    T: TagRepository + ?Sized,
{
    let tags = tag_repo.list_tags()?;

    // WHY pre-compute counts: propagate DB errors via `?` instead of
    // swallowing them with `unwrap_or(0)` — a mutex-poison or SQLite
    // failure should surface, not produce silent zero-counts.
    let counts: Vec<u64> = tags
        .iter()
        .map(|t| tag_repo.count_files_for_tag(t.id))
        .collect::<Result<_, _>>()?;

    if json {
        // WHY manual construction instead of deriving Serialize on Tag:
        // Tag already derives Serialize but we want a flat `{name, count, id}`
        // shape rather than Tag's `{id, name, first_seen}` shape.
        let rows: Vec<serde_json::Value> = tags
            .iter()
            .zip(&counts)
            .map(|(t, &count)| {
                serde_json::json!({
                    "name":  t.name,
                    "count": count,
                    "id":    t.id.to_string(),
                })
            })
            .collect();

        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        serde_json::to_writer_pretty(&mut handle, &rows)
            .map_err(|e| CoreError::Internal(format!("json: {e}")))?;
        writeln!(handle).map_err(CoreError::Io)?;
    } else {
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();
        writeln!(handle, "{:<32} {:>6}  ID", "NAME", "COUNT").map_err(CoreError::Io)?;
        for (t, &count) in tags.iter().zip(&counts) {
            writeln!(handle, "{:<32} {:>6}  {}", t.name, count, t.id).map_err(CoreError::Io)?;
        }
    }

    Ok(())
}
