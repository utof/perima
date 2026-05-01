//! `DatabaseAdmin` adapter — sends `WriteCmd::Backup` to the writer actor
//! and awaits the reply via `flume::bounded(1)`.

use std::path::Path;

use flume::Sender;
use perima_core::{CoreError, errors::BackupFailureReason, ports::DatabaseAdmin};

use crate::cmd::{BackupWriteCmd, WriteCmd};

/// SQLite-backed `DatabaseAdmin` adapter.
///
/// Holds a `Sender<WriteCmd>` to the single writer actor; backup
/// requests serialize naturally with other writes.
#[derive(Clone, Debug)]
pub struct SqliteDatabaseAdmin {
    writer: Sender<WriteCmd>,
}

impl SqliteDatabaseAdmin {
    /// Construct from the writer command sender (typically obtained via
    /// `SqliteWriter::start(...)?.sender()`).
    #[must_use]
    pub const fn new(writer: Sender<WriteCmd>) -> Self {
        Self { writer }
    }
}

impl DatabaseAdmin for SqliteDatabaseAdmin {
    fn backup(&self, target: &Path) -> Result<u64, CoreError> {
        // WHY flume::bounded(1) reply (not unbounded): one-shot reply
        // pattern across the codebase (per Batch C). bounded(1) means
        // the writer can't pile up replies if the use case dies.
        let (reply_tx, reply_rx) = flume::bounded(1);

        // WHY map send/recv errors to BackupFailed { reason: Internal(...) }
        // rather than bare CoreError::Internal: keeps the typed-error
        // contract intact; frontend pattern-matches on err.kind ===
        // "BackupFailed" for ALL backup failure paths.
        self.writer
            .send(WriteCmd::Backup(BackupWriteCmd {
                target: target.to_path_buf(),
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::BackupFailed {
                reason: BackupFailureReason::Internal(format!("writer send: {e}")),
            })?;

        reply_rx.recv().map_err(|e| CoreError::BackupFailed {
            reason: BackupFailureReason::Internal(format!("writer reply: {e}")),
        })?
    }
}
