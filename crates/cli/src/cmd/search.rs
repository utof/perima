//! `perima search` subcommand — thin delegator to [`perima_app::SearchUseCase`].

use perima_app::{AppContainer, SearchCommand};
use perima_core::CoreError;

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
pub(crate) async fn run(container: &AppContainer, args: &SearchArgs) -> Result<(), CoreError> {
    if args.rebuild {
        container.search.execute(SearchCommand::Rebuild).await?;
        eprintln!("perima: search index rebuilt");
        return Ok(());
    }

    // WHY the empty-query check + unwrap stays here: clap's
    // `required_unless_present = "rebuild"` already guarantees `query` is
    // `Some(...)` once we're past the rebuild branch. The UseCase ALSO
    // rejects empty/whitespace-only queries with `CoreError::Unsupported`,
    // so we just delegate.
    let q = args
        .query
        .as_deref()
        .ok_or_else(|| CoreError::Unsupported("query must be non-empty".into()))?
        .to_owned();

    let out = container
        .search
        .execute(SearchCommand::Query {
            q,
            limit: Some(args.limit),
        })
        .await?;

    let hits = out.hits;
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
    // WHY map+default: post-Task-11 `SearchHit.blake3_hash` is `Option<String>`
    // because pending files (no full_hash) can still hit the FTS index.
    // Render an 8-char "pending" sigil so columns align.
    println!("{:<8}  {:<60}  RANK", "HASH", "PATH");
    println!("{}", "-".repeat(80));
    for h in &hits {
        let hash_short: String = h
            .blake3_hash
            .as_deref()
            .map_or_else(|| "pending ".to_owned(), |s| s[..8.min(s.len())].to_owned());
        println!("{:<8}  {:<60}  {:.4}", hash_short, h.relative_path, h.rank);
    }
    Ok(())
}
