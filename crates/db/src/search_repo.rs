//! `SearchRepository` implementation backed by rusqlite FTS5.
//!
//! Post-Batch-C Task 7. The struct holds two cheap-to-clone handles:
//! a [`flume::Sender<WriteCmd>`] connected to the single writer actor
//! (spec §3.1) and a [`ReadPool`] of read-only `r2d2_sqlite`
//! connections (spec §3.4). Writes build a [`SearchWriteCmd`] variant
//! with a `flume::bounded(1)` reply channel and block on the reply.
//! Reads run SQL directly against a pooled connection.
//!
//! No `Mutex<Connection>`. Every caller now supplies
//! `(writer_sender, read_pool)` via `SqliteSearchRepository::new`.

use flume::Sender;
use perima_core::{CoreError, FileUuid, SearchHit, SearchRepository};
use rusqlite::Connection;
use uuid::Uuid;

use crate::cmd::{SearchWriteCmd, WriteCmd};
use crate::errors::Error;
use crate::pool::ReadPool;

/// Writer-actor + read-pool backed full-text search repository.
///
/// Cheap to [`Clone`]: both fields (`flume::Sender`, `ReadPool`) are
/// internally refcounted.
#[derive(Clone)]
pub struct SqliteSearchRepository {
    writer: Sender<WriteCmd>,
    reads: ReadPool,
}

impl std::fmt::Debug for SqliteSearchRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSearchRepository")
            .finish_non_exhaustive()
    }
}

impl SqliteSearchRepository {
    /// Construct an adapter from a writer-command sender + a read pool.
    ///
    /// WHY no migration run here: migrations happen exactly once inside
    /// [`crate::SqliteWriter::start`] BEFORE the writer thread spawns
    /// (spec §3.6). The read pool is opened after migrations complete.
    #[must_use]
    pub const fn new(writer: Sender<WriteCmd>, reads: ReadPool) -> Self {
        Self { writer, reads }
    }
}

// ---------------------------------------------------------------------------
// SearchRepository trait impl
// ---------------------------------------------------------------------------

impl SearchRepository for SqliteSearchRepository {
    fn search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>, CoreError> {
        let conn = self.reads.get()?;
        search_impl(&conn, query, limit)
    }

    fn rebuild(&self) -> Result<(), CoreError> {
        let (tx, rx) = flume::bounded::<Result<(), CoreError>>(1);
        self.writer
            .send(WriteCmd::Search(SearchWriteCmd::Rebuild { reply: tx }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        rx.recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
    }
}

// ---------------------------------------------------------------------------
// Read-path helpers (pool variant)
// ---------------------------------------------------------------------------

/// SELECT body for [`SearchRepository::search`].
///
/// V007: `search_content` has `(rowid, blake3_hash, relative_path, ...)`
/// but [`perima_core::SearchHit`] requires `volume_id` which lives only
/// on `file_locations`. Pick the first-seen active location per hash to
/// populate `volume_id`. The subquery ordering (`first_seen ASC, id ASC`)
/// mirrors the trigger representative-selection rule, so the `volume_id`
/// returned here agrees with the path indexed in `search_content`.
fn search_impl(conn: &Connection, query: &str, limit: u32) -> Result<Vec<SearchHit>, CoreError> {
    // WHY `sc.file_uuid` first column + `sc.blake3_hash` typed as
    // `Option<String>`: Task 11 makes `file_uuid` the stable surrogate on the
    // IPC boundary; `blake3_hash` becomes nullable for rows whose `full_hash`
    // has not yet been computed (spec §4.8). `search_content.file_uuid` was
    // populated in V011 + has UNIQUE-NOT-NULL semantics post-Task-3.
    let mut stmt = conn
        .prepare(
            "SELECT sc.file_uuid,
                    sc.blake3_hash,
                    COALESCE((
                        SELECT fl.volume_id FROM file_locations fl
                        WHERE fl.blake3_hash = sc.blake3_hash
                          AND fl.deleted_at IS NULL
                        ORDER BY fl.first_seen ASC, fl.id ASC
                        LIMIT 1
                    ), ''),
                    sc.relative_path,
                    search_index.rank
             FROM search_index
             JOIN search_content sc ON sc.rowid = search_index.rowid
             WHERE search_index MATCH ?1
             ORDER BY search_index.rank
             LIMIT ?2",
        )
        .map_err(Error::from)?;

    let rows = stmt
        .query_map(rusqlite::params![query, limit], |row| {
            let file_uuid_str: String = row.get(0)?;
            let blake3_hash: Option<String> = row.get(1)?;
            let volume_id: String = row.get(2)?;
            let relative_path: String = row.get(3)?;
            let rank: f64 = row.get(4)?;
            Ok((file_uuid_str, blake3_hash, volume_id, relative_path, rank))
        })
        .map_err(Error::from)?;

    let mut hits = Vec::new();
    for r in rows {
        let (file_uuid_str, blake3_hash, volume_id, relative_path, rank) =
            r.map_err(Error::from)?;
        let file_uuid = FileUuid(
            Uuid::parse_str(&file_uuid_str)
                .map_err(|e| CoreError::Internal(format!("bad file_uuid: {e}")))?,
        );
        hits.push(SearchHit {
            file_uuid,
            blake3_hash,
            volume_id,
            relative_path,
            rank,
        });
    }
    Ok(hits)
}
