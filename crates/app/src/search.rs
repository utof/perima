//! `SearchUseCase` — orchestrates FTS5 full-text search and index rebuilds.
//!
//! This is the `crates/app` port of the search orchestration that
//! previously lived in `crates/cli/src/cmd/search.rs::run` (78 LOC)
//! and `crates/desktop/src/commands.rs::{search, search_rebuild}`.
//! Zero generics: dependency ports are carried as `Arc<dyn Port>`
//! fields; a single `async fn execute(&self, cmd: SearchCommand) ->
//! Result<SearchOutput, CoreError>` exposes both operations.
//!
//! See [`SearchUseCase::execute`] for the workflow body.

use std::sync::Arc;
use std::time::Instant;

use perima_core::{CoreError, EventBus, SearchHit, SearchRepository};

use crate::telemetry::truncated;

/// Inputs to [`SearchUseCase::execute`].
#[derive(Debug, Clone)]
pub enum SearchCommand {
    /// Run a FTS5 full-text query and return ranked hits.
    ///
    /// `limit` defaults to 50 when `None` — matches the CLI
    /// `--limit` flag's `default_value = "50"`.
    Query {
        /// Raw FTS5 MATCH expression (e.g. `"vacation"`, `"image/jpeg"`).
        q: String,
        /// Maximum results. `None` → 50.
        limit: Option<u32>,
    },
    /// Wipe and rebuild the entire FTS5 index from the current DB state.
    ///
    /// WHY exposed as a command variant: needed after migrations that add
    /// new indexed fields, and as a manual recovery tool when the index
    /// drifts (e.g. after a crash mid-trigger). Mirrors CLI's
    /// `perima search --rebuild` and Desktop's `search_rebuild` command.
    Rebuild,
}

impl SearchCommand {
    /// Short kind name for tracing spans. WHY: enum Debug print is too noisy;
    /// `?cmd` would dump full bodies into spans. (Batch I Task 5.)
    pub(crate) const fn kind_str(&self) -> &'static str {
        match self {
            Self::Query { .. } => "query",
            Self::Rebuild => "rebuild",
        }
    }

    /// Query string for the span field; empty for non-query variants.
    pub(crate) const fn query_str(&self) -> &str {
        match self {
            Self::Query { q, .. } => q.as_str(),
            Self::Rebuild => "",
        }
    }

    /// Effective limit for the span field; 0 for non-query variants.
    pub(crate) fn limit_val(&self) -> u32 {
        match self {
            Self::Query { limit, .. } => limit.unwrap_or(50),
            Self::Rebuild => 0,
        }
    }
}

/// Output of a successful search or rebuild.
#[derive(Debug, Clone)]
pub struct SearchOutput {
    /// Ranked hits, best match first (BM25 ascending).
    /// Empty for [`SearchCommand::Rebuild`].
    pub hits: Vec<SearchHit>,
    /// Wall-clock time of the repository call in milliseconds.
    pub took_ms: u64,
}

/// Orchestrator: FTS5 query + index rebuild.
///
/// Dependencies are carried as `Arc<dyn Port>` fields; there are zero
/// generic parameters on the struct itself. See [`SearchUseCase::execute`]
/// for the workflow body.
pub struct SearchUseCase {
    search: Arc<dyn SearchRepository>,
    // WHY `events` is held but unused in the orchestration body today:
    // Batch E will emit `FileEvent::SearchIndexRebuilt` (or equivalent)
    // from here once the bus-engine swap lands. Holding the handle at
    // construction makes the Batch-E diff a single-file addition rather
    // than a signature churn across every caller. The field is silenced
    // below with a `_ = &self.events` one-liner rather than `Arc::clone`
    // to avoid an unnecessary refcount increment on every call.
    events: Arc<dyn EventBus>,
}

impl std::fmt::Debug for SearchUseCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchUseCase").finish_non_exhaustive()
    }
}

impl SearchUseCase {
    /// Construct a `SearchUseCase` with the given dependency ports.
    ///
    /// The container (Task 7) calls this once and shares the resulting
    /// `Arc<SearchUseCase>` across surfaces.
    #[must_use]
    pub fn new(search: Arc<dyn SearchRepository>, events: Arc<dyn EventBus>) -> Self {
        Self { search, events }
    }

    /// Execute the search command.
    ///
    /// # Errors
    /// - [`CoreError::Unsupported`] if [`SearchCommand::Query`] is given an
    ///   empty or whitespace-only query string.
    /// - [`CoreError::Internal`] on `SQLite` / `FTS5` errors from the
    ///   repository.
    // WHY allow unused_async: `SearchRepository` methods are synchronous today;
    // the `async fn` signature is mandated by the UseCase contract so the
    // Batch-C connection-actor swap (async write channel) can evolve the
    // impl without touching callers. Removing `async` now would force a
    // caller-side churn when the trait gains async variants.
    #[allow(clippy::unused_async)]
    #[tracing::instrument(
        name = "search",
        skip(self, cmd),
        fields(search_kind = cmd.kind_str(), query = %truncated(cmd.query_str(), 64), limit = cmd.limit_val()),
        err(level = "warn", Display)
    )]
    pub async fn execute(&self, cmd: SearchCommand) -> Result<SearchOutput, CoreError> {
        // WHY touch self.events: held for the Batch-E event-emit path;
        // reference the field so `unused` lints don't fire before
        // Batch E wires the emissions.
        let _ = &self.events;

        match cmd {
            SearchCommand::Query { q, limit } => {
                let query = q.trim();
                if query.is_empty() {
                    return Err(CoreError::Unsupported(
                        "search query must be non-empty".into(),
                    ));
                }
                let effective_limit = limit.unwrap_or(50);
                let start = Instant::now();
                let hits = self.search.search(query, effective_limit)?;
                let took_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                Ok(SearchOutput { hits, took_ms })
            }
            SearchCommand::Rebuild => {
                let start = Instant::now();
                self.search.rebuild()?;
                let took_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                Ok(SearchOutput {
                    hits: vec![],
                    took_ms,
                })
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.
mod tests {
    use perima_core::AppEvent;
    use perima_db::{ReadPool, SqliteSearchRepository, SqliteWriter};
    use tempfile::TempDir;

    use super::*;

    /// No-op event bus for tests that don't care about emissions.
    struct NullBus;
    impl EventBus for NullBus {
        fn emit(&self, _event: &AppEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }

    /// Build a [`SearchUseCase`] backed by a real `SQLite` DB in a tempdir.
    fn harness() -> (SearchUseCase, TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("perima.db");
        let writer = SqliteWriter::start(&db_path, Arc::new(NullBus) as Arc<dyn EventBus>).unwrap();
        let reads = ReadPool::open(&db_path).unwrap();
        let repo: Arc<dyn SearchRepository> =
            Arc::new(SqliteSearchRepository::new(writer.sender(), reads));
        // WHY drop handle: sender inside repo keeps thread alive.
        drop(writer);
        let events: Arc<dyn EventBus> = Arc::new(NullBus);
        (SearchUseCase::new(repo, events), tmp)
    }

    /// Seed a `search_content` row by opening a second connection.
    ///
    /// WHY insert into `search_content` directly: `SearchRepository::search`
    /// queries `search_index` (the `FTS5` virtual table) which is populated
    /// from `search_content` via triggers. Inserting into `search_content`
    /// is the minimal path that avoids wiring a full scan pipeline just to
    /// exercise search semantics. Using a second connection is fine under
    /// `WAL` mode.
    fn seed_via_conn(db_path: &std::path::Path, hash: &str, path: &str, mime: &str) {
        use rusqlite::Connection;
        // WHY #[allow]: opens a second writable Connection alongside the
        // SqliteWriter actor to seed `search_content` directly. Post-GH #131
        // (SQLite 3.51.3) this no longer hits the lock-order-inversion close race that
        // afflicted 3.51.0-3.51.1. The pattern remains fragile to future
        // SQLite regressions; see clippy.toml header. Migrating this seed
        // to a writer-routed test helper is tracked separately and is the
        // canonical structural fix.
        #[allow(clippy::disallowed_methods)]
        let conn = Connection::open(db_path).unwrap();
        // WHY explicit column list: matches V011 `search_content` schema
        // (file_uuid, blake3_hash, filename, relative_path, mime_type,
        //  camera_model, captured_at, tags). `file_uuid` is `NOT NULL UNIQUE`
        //  post-V011; we synthesise a fresh UUIDv7 per seed call so each row
        //  satisfies the constraint.
        conn.execute(
            "INSERT INTO search_content \
             (file_uuid, blake3_hash, filename, relative_path, mime_type, camera_model, captured_at, tags) \
             VALUES (?1, ?2, '', ?3, ?4, '', '', '')",
            rusqlite::params![uuid::Uuid::now_v7().to_string(), hash, path, mime],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn query_returns_matching_hits() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("perima.db");
        let writer = SqliteWriter::start(&db_path, Arc::new(NullBus) as Arc<dyn EventBus>).unwrap();
        let reads = ReadPool::open(&db_path).unwrap();
        let repo: Arc<dyn SearchRepository> =
            Arc::new(SqliteSearchRepository::new(writer.sender(), reads));
        drop(writer);
        let events: Arc<dyn EventBus> = Arc::new(NullBus);
        let uc = SearchUseCase::new(repo, events);

        seed_via_conn(&db_path, "aabbcc", "photos/vacation.jpg", "image/jpeg");

        let out = uc
            .execute(SearchCommand::Query {
                q: "vacation".into(),
                limit: None,
            })
            .await
            .unwrap();

        assert_eq!(out.hits.len(), 1, "one seeded row matches 'vacation'");
        assert_eq!(out.hits[0].relative_path, "photos/vacation.jpg");
        assert_eq!(out.hits[0].blake3_hash.as_deref(), Some("aabbcc"));
    }

    #[tokio::test]
    async fn query_limit_is_respected() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("perima.db");
        let writer = SqliteWriter::start(&db_path, Arc::new(NullBus) as Arc<dyn EventBus>).unwrap();
        let reads = ReadPool::open(&db_path).unwrap();
        let repo: Arc<dyn SearchRepository> =
            Arc::new(SqliteSearchRepository::new(writer.sender(), reads));
        drop(writer);
        let events: Arc<dyn EventBus> = Arc::new(NullBus);
        let uc = SearchUseCase::new(repo, events);

        // Seed two rows matching "beach"
        seed_via_conn(&db_path, "hash1", "beach/a.jpg", "image/jpeg");
        seed_via_conn(&db_path, "hash2", "beach/b.jpg", "image/jpeg");

        let out = uc
            .execute(SearchCommand::Query {
                q: "beach".into(),
                limit: Some(1),
            })
            .await
            .unwrap();

        assert_eq!(out.hits.len(), 1, "limit=1 must cap results at 1");
    }

    #[tokio::test]
    async fn rebuild_succeeds_and_returns_empty_hits() {
        let (uc, _tmp) = harness();
        let out = uc.execute(SearchCommand::Rebuild).await.unwrap();
        assert!(
            out.hits.is_empty(),
            "Rebuild must return empty hits vec (no search performed)",
        );
        // took_ms is timing-dependent; just verify it's a plausible value.
        assert!(
            out.took_ms < 60_000,
            "took_ms should be sub-minute for rebuild"
        );
    }

    #[tokio::test]
    async fn empty_query_returns_unsupported_error() {
        let (uc, _tmp) = harness();
        let err = uc
            .execute(SearchCommand::Query {
                q: "   ".into(),
                limit: None,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, CoreError::Unsupported(_)),
            "whitespace-only query must return CoreError::Unsupported, got: {err:?}",
        );
    }
}
