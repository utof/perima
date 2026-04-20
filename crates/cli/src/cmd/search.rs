//! `perima search` subcommand — full-text search over indexed metadata.

use perima_core::{CoreError, SearchRepository};

/// Arguments for the `perima search` command.
#[derive(clap::Args, Debug)]
pub(crate) struct SearchArgs {
    /// `FTS5` match expression (e.g. `"vacation"`, `"image/jpeg"`, `"Canon*"`).
    ///
    /// Required unless `--rebuild` is specified.
    #[arg(required_unless_present = "rebuild")]
    pub query: Option<String>,

    /// Maximum results to return.
    #[arg(long, default_value = "50")]
    pub limit: u32,

    /// Output results as a JSON array.
    #[arg(long)]
    pub json: bool,

    /// Rebuild the `FTS5` index from the current DB state and exit.
    ///
    /// WHY exposed in CLI: needed after migrations that add new indexed
    /// fields, and as a manual recovery tool if the index drifts from
    /// the DB state (e.g. after a crash mid-trigger).
    #[arg(long)]
    pub rebuild: bool,
}

/// Execute `perima search`.
///
/// # Errors
/// Returns [`CoreError::Internal`] on DB/`FTS5` errors.
/// Returns [`CoreError::Unsupported`] when no query is supplied without `--rebuild`.
pub(crate) fn run<S>(repo: &S, args: &SearchArgs) -> Result<(), CoreError>
where
    S: SearchRepository + ?Sized,
{
    if args.rebuild {
        repo.rebuild()?;
        eprintln!("perima: search index rebuilt");
        return Ok(());
    }

    let query = args
        .query
        .as_deref()
        .filter(|q| !q.trim().is_empty())
        .ok_or_else(|| CoreError::Unsupported("query must be non-empty".into()))?;

    let hits = repo.search(query, args.limit)?;

    if args.json {
        let json = serde_json::to_string(&hits)
            .map_err(|e| CoreError::Internal(format!("json serialize: {e}")))?;
        println!("{json}");
        return Ok(());
    }

    if hits.is_empty() {
        println!("(no results)");
        return Ok(());
    }

    // Plain table output: HASH(8) | PATH | RANK
    println!("{:<8}  {:<60}  RANK", "HASH", "PATH");
    println!("{}", "-".repeat(80));
    for h in &hits {
        println!(
            "{:<8}  {:<60}  {:.4}",
            &h.blake3_hash[..8.min(h.blake3_hash.len())],
            h.relative_path,
            h.rank
        );
    }
    Ok(())
}
