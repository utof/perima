//! Single-writer `SQLite` actor. Owns the sole writable [`Connection`]
//! for its lifetime on a dedicated OS thread.
//!
//! Writer lifecycle:
//! 1. [`SqliteWriter::start`] opens a [`Connection`], runs Refinery
//!    migrations synchronously, then spawns the writer thread moving
//!    the connection in. Migrations happen exactly once, before the
//!    thread loops — spec §3.6 invariant.
//! 2. Adapters send [`crate::WriteCmd`] variants over the
//!    [`flume::Sender`] returned in [`SqliteWriterHandle::sender`].
//!    Each handler pattern (spec §3.3):
//!
//!    ```text
//!    match handler_impl(conn, sub_cmd) {
//!        Ok((out, events)) => {
//!            for ev in &events {
//!                if let Err(e) = bus.emit(ev) {
//!                    tracing::warn!(?e, ?ev, "post-commit emit failed");
//!                }
//!            }
//!            if reply.send(Ok(out)).is_err() {
//!                tracing::debug!("reply channel closed before send");
//!            }
//!        }
//!        Err(e) => {
//!            if reply.send(Err(e)).is_err() {
//!                tracing::debug!("reply channel closed before send (error path)");
//!            }
//!        }
//!    }
//!    ```
//! 3. Dropping the last [`flume::Sender<WriteCmd>`] closes the channel;
//!    the writer observes `recv() == Err(Disconnected)` and returns.
//!
//! See `docs/superpowers/specs/2026-04-21-arch-audit-batch-C-connection-model-design.md`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use flume::{Receiver, Sender};
use perima_core::{CoreError, EventBus};
use rusqlite::Connection;

use crate::cmd::WriteCmd;
use crate::connection::open_and_migrate;

mod tag;
mod volume;

/// Handle returned by [`SqliteWriter::start`].
///
/// Clone [`Self::sender`] to inject into repo-adapter constructors.
/// The underlying [`JoinHandle`] is single-consumer, wrapped in
/// `Arc<Mutex<Option<_>>>` so the handle itself can [`Clone`] without
/// duplicating join rights. Only the explicit [`Self::join`] call (or
/// the final drop of the last writer-owned handle) reaps the thread.
#[derive(Clone)]
pub struct SqliteWriterHandle {
    sender: Sender<WriteCmd>,
    // WHY `Option` + `Arc<Mutex<_>>`: the `JoinHandle<()>` is
    // single-consumer. `Arc` lets `SqliteWriterHandle` be `Clone`;
    // `Mutex` serializes any race between a `.join()` caller and a
    // final-drop caller. `.take()` moves the handle out on first
    // consumer; subsequent callers see `None` and silently noop.
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl std::fmt::Debug for SqliteWriterHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // WHY custom Debug: `JoinHandle<()>` is not `Debug`; we print
        // a placeholder tag + the current sender count for observability.
        let joined = self
            .join
            .lock()
            .ok()
            .map_or("poisoned", |g| if g.is_some() { "live" } else { "reaped" });
        f.debug_struct("SqliteWriterHandle")
            .field("sender_senders", &self.sender.sender_count())
            .field("join", &joined)
            .finish()
    }
}

impl SqliteWriterHandle {
    /// Clone the write-command sender for injection into repo adapters.
    #[must_use]
    pub fn sender(&self) -> Sender<WriteCmd> {
        self.sender.clone()
    }

    /// Wait for the writer thread to finish.
    ///
    /// Consumes the handle, dropping *this* handle's sender BEFORE
    /// joining. If no other clones of the handle still exist, the
    /// channel closes and the writer loop exits; if clones remain,
    /// this call blocks until the last clone drops its sender.
    ///
    /// WHY consuming self: a `&self` variant would deadlock — the
    /// handle itself holds the sender, and `JoinHandle::join` can't
    /// return until the writer loop exits, which requires the sender
    /// refcount to hit zero. Taking `self` lets us `drop(self.sender)`
    /// inside the function body before joining.
    pub fn join(self) {
        // Destructure to drop the sender BEFORE joining the thread.
        let Self { sender, join } = self;
        drop(sender);
        let h = join.lock().ok().and_then(|mut g| g.take());
        if let Some(h) = h {
            // WHY swallow: the writer loop is infallible; a panicked
            // writer thread would surface as `Err(Any)` here, but the
            // handle is an advisory shutdown primitive — we can't do
            // anything useful with the panic payload at teardown.
            let _ = h.join();
        }
    }
}

/// `SQLite` writer actor. Owns the sole writable [`Connection`].
#[derive(Debug)]
pub struct SqliteWriter;

impl SqliteWriter {
    /// Open `db_path`, run migrations synchronously on the connection
    /// that becomes the writer, then spawn the writer thread and return
    /// the handle.
    ///
    /// `bus` is the [`EventBus`] the writer will `emit` domain events
    /// through AFTER each successful `COMMIT`. Per Batch B standing
    /// constraint the writer does NOT construct a
    /// `CompositeEventBus` — it consumes one passed from
    /// `AppContainer::new`.
    ///
    /// # Errors
    /// Returns [`CoreError::Internal`] on migration failure or thread
    /// spawn failure.
    pub fn start(db_path: &Path, bus: Arc<dyn EventBus>) -> Result<SqliteWriterHandle, CoreError> {
        // WHY: migrations run on the connection that becomes the writer,
        // synchronously, BEFORE the thread spawns. Read pool opens
        // afterwards against a fully-migrated schema (spec §3.6).
        let conn =
            open_and_migrate(db_path).map_err(|e| CoreError::Internal(format!("migrate: {e}")))?;
        spawn_writer(conn, bus)
    }

    /// Test-only in-memory writer.
    ///
    /// Runs migrations on a fresh `:memory:` connection, then spawns the
    /// writer thread. Used by `#[cfg(test)]` fixtures throughout the
    /// workspace.
    ///
    /// # Errors
    /// Returns [`CoreError::Internal`] on migration failure or thread
    /// spawn failure.
    #[cfg(test)]
    pub(crate) fn start_in_memory(bus: Arc<dyn EventBus>) -> Result<SqliteWriterHandle, CoreError> {
        mod embedded {
            use refinery::embed_migrations;
            embed_migrations!("migrations");
        }

        let mut conn = Connection::open_in_memory()
            .map_err(|e| CoreError::Internal(format!("open_in_memory: {e}")))?;
        embedded::migrations::runner()
            .run(&mut conn)
            .map_err(|e| CoreError::Internal(format!("migrate in-memory: {e}")))?;
        spawn_writer(conn, bus)
    }
}

/// Spawn the writer thread, consuming `conn`. Shared between
/// [`SqliteWriter::start`] and the test-only in-memory variant.
fn spawn_writer(conn: Connection, bus: Arc<dyn EventBus>) -> Result<SqliteWriterHandle, CoreError> {
    let (sender, receiver) = flume::unbounded::<WriteCmd>();
    let handle = thread::Builder::new()
        .name("perima-sqlite-writer".into())
        .spawn(move || run_writer_loop(conn, receiver, bus))
        .map_err(|e| CoreError::Internal(format!("writer thread spawn: {e}")))?;

    Ok(SqliteWriterHandle {
        sender,
        join: Arc::new(Mutex::new(Some(handle))),
    })
}

// WHY allow needless_pass_by_value: `bus` and `receiver` are moved into
// the writer thread and live for its lifetime; passing by reference would
// force a shorter lifetime bound than `'static`, which `thread::spawn`
// requires. Same applies to `spawn_writer` below.
#[allow(clippy::needless_pass_by_value)]
fn run_writer_loop(mut conn: Connection, receiver: Receiver<WriteCmd>, bus: Arc<dyn EventBus>) {
    tracing::debug!("sqlite writer actor started");
    while let Ok(cmd) = receiver.recv() {
        dispatch(&mut conn, cmd, &bus);
    }
    tracing::debug!("sqlite writer actor exiting (channel disconnected)");
}

fn dispatch(conn: &mut Connection, cmd: WriteCmd, bus: &Arc<dyn EventBus>) {
    // WHY the match-level dispatch: each per-repo handler owns its own
    // commit + event-emit pattern. The shared shape each handler must
    // follow (spec §3.3) is:
    //
    //   match handler_impl(conn, sub_cmd) {
    //       Ok((out, events)) => {
    //           for ev in &events {
    //               if let Err(e) = bus.emit(ev) {
    //                   tracing::warn!(?e, ?ev, "post-commit emit failed");
    //               }
    //           }
    //           if reply.send(Ok(out)).is_err() {
    //               tracing::debug!("reply closed");
    //           }
    //       }
    //       Err(e) => {
    //           if reply.send(Err(e)).is_err() {
    //               tracing::debug!("reply closed (error path)");
    //           }
    //       }
    //   }
    //
    // Tasks 2-6 populate each sub-enum's handler following this shape.
    match cmd {
        WriteCmd::Volume(c) => volume::handle(conn, c, bus),
        WriteCmd::Tag(c) => tag::handle(conn, c, bus),
        WriteCmd::Metadata(c) => handle_metadata(conn, c, bus),
        WriteCmd::File(c) => handle_file(conn, c, bus),
        WriteCmd::Search(c) => handle_search(conn, c, bus),
    }
}

// WHY allow needless_pass_by_value: `cmd` is moved into `match cmd {}`,
// which is the canonical exhaustive-match on an uninhabited enum. Clippy
// can't tell the empty match consumes `cmd`; Tasks 4-6 populate the
// sub-enums and the move becomes load-bearing.
#[allow(clippy::needless_pass_by_value)]
fn handle_metadata(
    _conn: &mut Connection,
    cmd: crate::cmd::MetadataWriteCmd,
    _bus: &Arc<dyn EventBus>,
) {
    match cmd {}
}

#[allow(clippy::needless_pass_by_value)]
fn handle_file(_conn: &mut Connection, cmd: crate::cmd::FileWriteCmd, _bus: &Arc<dyn EventBus>) {
    match cmd {}
}

#[allow(clippy::needless_pass_by_value)]
fn handle_search(
    _conn: &mut Connection,
    cmd: crate::cmd::SearchWriteCmd,
    _bus: &Arc<dyn EventBus>,
) {
    match cmd {}
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use perima_core::{CoreError, EventBus, FileEvent};

    use super::SqliteWriter;

    struct NoopBus;

    impl EventBus for NoopBus {
        fn emit(&self, _: &FileEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[test]
    fn writer_spawns_and_shuts_down_cleanly() {
        let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
        let handle = SqliteWriter::start_in_memory(bus).expect("spawn writer");
        // Dropping the handle drops its internal `sender`; because no
        // other senders exist (we haven't cloned any), the writer loop
        // observes `recv() == Err(Disconnected)` and returns.
        handle.join();
    }
}
