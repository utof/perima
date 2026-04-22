//! `perima tag` subcommand — thin delegator to [`perima_app::TagUseCase`].
//!
//! Provides three sub-subcommands:
//! - `tag add <path> <tags…>` — attach one or more labels to a file
//! - `tag rm <path> <tag>` — detach a label from a file
//! - `tag ls [--json]` — list all active tags with attachment counts
//!
//! WHY still opens one DB connection inline: two helpers here
//! (`resolve_hash` and the `tag ls` per-tag count lookup) need ports
//! that `TagUseCase` does not currently expose on its output surface
//! (`FileRepository::list_file_locations` for path→hash resolution +
//! `TagRepository::count_files_for_tag` for attachment counts). A future
//! batch can lift these onto the app layer; Task 8 keeps the shell
//! minimal by re-using a short-lived connection, matching the pattern
//! already in `cmd/metadata.rs`.

use std::io::Write;
use std::path::{Path, PathBuf};

use perima_app::{AppContainer, TagCommand, TagOutput};
use perima_core::{BlakeHash, CoreError, DeviceId, FileRepository, normalize_tag};
use perima_db::{SqliteFileRepository, open_and_migrate};

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
pub(crate) async fn run(
    container: &AppContainer,
    data_dir: &Path,
    device: DeviceId,
    args: &TagArgs,
) -> Result<(), CoreError> {
    match &args.action {
        TagAction::Add { path, tags } => run_add(container, data_dir, device, path, tags).await,
        TagAction::Rm { path, tag } => run_rm(container, data_dir, device, path, tag).await,
        TagAction::Ls { json } => run_ls(container, data_dir, *json).await,
    }
}

/// Resolve a path to a `BlakeHash` by opening a fresh `FileRepository`
/// connection and suffix-matching indexed locations.
fn resolve_hash(data_dir: &Path, path: &Path) -> Result<BlakeHash, CoreError> {
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

    let absolute = perima_fs::platform_path::canonicalize(path).map_err(CoreError::Io)?;
    let absolute_str = absolute
        .to_str()
        .ok_or_else(|| CoreError::InvalidPath(format!("non-UTF8 path: {}", absolute.display())))?;

    let db_path = data_dir.join("perima.db");
    // WHY new_legacy: Task 7 migrates this callsite to use the shared
    // writer+pool via container.files_repo. Legacy constructor is deprecated.
    #[allow(deprecated)]
    let file_repo = SqliteFileRepository::new_legacy(open_and_migrate(&db_path)?);
    // WHY list across ALL volumes (None): the user supplies an absolute
    // path that may live on any known volume. Suffix-matching across the
    // full location set is the only portable approach.
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
async fn run_add(
    container: &AppContainer,
    data_dir: &Path,
    device: DeviceId,
    path: &Path,
    tags: &[String],
) -> Result<(), CoreError> {
    let hash = resolve_hash(data_dir, path)?;

    let mut applied = Vec::with_capacity(tags.len());
    for raw in tags {
        container
            .tag
            .execute(TagCommand::Attach {
                hash,
                name: raw.clone(),
                device,
            })
            .await?;
        // WHY call normalize_tag for the message: the UseCase does the
        // same normalization internally; doing it here keeps the user-
        // visible confirmation line consistent with the actual stored
        // name when the raw input carried whitespace/case variance.
        let normalized = normalize_tag(raw)?;
        applied.push(normalized);
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
async fn run_rm(
    container: &AppContainer,
    data_dir: &Path,
    device: DeviceId,
    path: &Path,
    tag_raw: &str,
) -> Result<(), CoreError> {
    let hash = resolve_hash(data_dir, path)?;

    container
        .tag
        .execute(TagCommand::Detach {
            hash,
            name: tag_raw.to_owned(),
            device,
        })
        .await?;

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let normalized = normalize_tag(tag_raw)?;
    writeln!(handle, "removed {} from {}", normalized, path.display()).map_err(CoreError::Io)
}

/// List all active tags with their per-tag file counts.
async fn run_ls(container: &AppContainer, _data_dir: &Path, json: bool) -> Result<(), CoreError> {
    let out = container.tag.execute(TagCommand::List).await?;
    let TagOutput::Tags(tags) = out else {
        return Err(CoreError::Internal(
            "TagCommand::List returned non-Tags output".into(),
        ));
    };

    // WHY direct TagRepository via container.tags: `count_files_for_tag`
    // is not (yet) exposed through `TagUseCase`. Post-Batch-C Task 3,
    // the container exposes `Arc<dyn TagRepository>` directly so we no
    // longer need a short-lived `SqliteTagRepository::new(...)` open
    // here. A future UseCase iteration can lift this into `TagOutput`.
    let counts: Vec<u64> = tags
        .iter()
        .map(|t| container.tags.count_files_for_tag(t.id))
        .collect::<Result<_, _>>()?;

    if json {
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
