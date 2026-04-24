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
//! 3. Shutdown happens when EITHER (a) [`SqliteWriterHandle::join`] /
//!    final-drop sends [`crate::WriteCmd::Shutdown`] which the writer
//!    loop matches and breaks on, OR (b) the last `Sender<WriteCmd>`
//!    drops and the writer observes `recv() == Err(Disconnected)`.
//!    Path (a) is the normal path and means callers DON'T need to
//!    drop every cloned sender (held by repos / handlers) before
//!    teardown — see [`crate::WriteCmd::Shutdown`] doc for the
//!    "magic-drop" antipattern this replaces.
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
use crate::schema::install_fts_triggers;

mod file;
mod metadata;
mod search;
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
    /// Sends [`WriteCmd::Shutdown`] on this handle's sender, then
    /// joins the writer thread. Surviving `Sender<WriteCmd>` clones
    /// held by repos / event handlers do NOT block this call —
    /// they become inert (their next `.send()` returns
    /// `Disconnected`) once the writer processes Shutdown.
    ///
    /// WHY explicit Shutdown vs the prior "drop sender + wait for
    /// channel close" pattern: callers no longer need an N-deep
    /// `drop(repo); drop(repo); ...; writer.join();` ladder matching
    /// how many senders the surrounding code cloned. The "magic-drop"
    /// antipattern (GH #131 root cause for the desktop scan _inner
    /// helpers) is gone — `writer.join()` is sufficient.
    ///
    /// Production note: handles that are merely *dropped* (without
    /// `.join()`) — e.g. `crates/cli/src/main.rs::build_container`
    /// returning while repos hold sender clones — keep the OLD
    /// channel-close behavior. The writer continues running until the
    /// last sender drops at process exit. Drop is intentionally NOT
    /// implemented on this type; doing so would Shutdown-trigger any
    /// time the handle leaves scope, breaking the CLI's
    /// "senders-extend-lifetime" pattern.
    pub fn join(self) {
        let Self { sender, join } = self;
        // Best-effort: try_send on an unbounded channel only fails if
        // the channel has already been disconnected (e.g. writer
        // panicked). In that case, the join below still reaps the
        // thread.
        let _ = sender.try_send(WriteCmd::Shutdown);
        // Drop our local sender now that Shutdown is queued; not
        // strictly required (writer breaks on Shutdown regardless of
        // sender count), but releases the refcount eagerly.
        drop(sender);
        let h = join.lock().ok().and_then(|mut g| g.take());
        if let Some(h) = h {
            // WHY swallow: the writer loop is infallible; a panicked
            // writer would surface as `Err(Any)` here, but the handle
            // is an advisory shutdown primitive — we can't do anything
            // useful with the panic payload at teardown.
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
        // WHY: idempotent post-migration install — keeps FTS trigger bodies in
        // lockstep with `schema::FTS_AGGREGATIONS` + the codegen template,
        // closes the V006→V007→V008 drift bug class. Runs every boot.
        install_fts_triggers(&conn)?;
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
        // WHY: same as `start` — idempotent post-migration install keeps
        // in-memory test DBs converged with the codegen template.
        install_fts_triggers(&conn)?;
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
        if matches!(cmd, WriteCmd::Shutdown) {
            tracing::debug!("sqlite writer actor exiting (Shutdown received)");
            return;
        }
        dispatch(&mut conn, cmd, &bus);
    }
    tracing::debug!("sqlite writer actor exiting (channel disconnected)");
}

#[tracing::instrument(
    name = "write_cmd",
    skip(conn, cmd, bus),
    fields(cmd_kind = cmd.kind_str())
)]
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
        WriteCmd::Metadata(c) => metadata::handle(conn, c, bus),
        WriteCmd::File(c) => handle_file(conn, c, bus),
        WriteCmd::Search(c) => search::handle(conn, c, bus),
        // WHY unreachable: Shutdown is short-circuited in
        // `run_writer_loop` BEFORE this dispatch is invoked. Reaching
        // here means the loop ordering changed without updating
        // dispatch — a programming error.
        WriteCmd::Shutdown => unreachable!("Shutdown is handled in run_writer_loop"),
    }
}

fn handle_file(conn: &mut Connection, cmd: crate::cmd::FileWriteCmd, bus: &Arc<dyn EventBus>) {
    file::handle(conn, cmd, bus);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use perima_core::EventBus;

    use super::SqliteWriter;
    use crate::test_utils::NoopBus;

    #[test]
    fn writer_spawns_and_shuts_down_cleanly() {
        let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
        let handle = SqliteWriter::start_in_memory(bus).expect("spawn writer");
        // Dropping the handle drops its internal `sender`; because no
        // other senders exist (we haven't cloned any), the writer loop
        // observes `recv() == Err(Disconnected)` and returns.
        handle.join();
    }

    /// Regression: shutdown must work even when extra `Sender<WriteCmd>`
    /// clones outlive the handle.
    ///
    /// Pre-`WriteCmd::Shutdown`, this test would hang `pthread_join`
    /// forever — `handle.join()` only returned when the channel closed,
    /// which required ALL sender clones (including `extra_sender` here)
    /// to drop. Repos / handlers commonly held such clones, producing
    /// the GH #131 magic-drop bug class.
    ///
    /// Post-fix, `Drop`/`join` send `WriteCmd::Shutdown` directly, the
    /// writer loop matches and returns, and `extra_sender` becomes a
    /// no-op (its next `try_send` would return `Disconnected`).
    #[test]
    fn writer_shuts_down_with_outstanding_sender_clones() {
        use std::time::{Duration, Instant};

        let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
        let handle = SqliteWriter::start_in_memory(bus).expect("spawn writer");
        // Simulate a repo holding a sender clone that the test code
        // forgets to drop before joining.
        let extra_sender = handle.sender();

        let start = Instant::now();
        handle.join();
        let elapsed = start.elapsed();

        // The writer thread should exit promptly via the Shutdown
        // signal — well under any deadlock-detector threshold.
        assert!(
            elapsed < Duration::from_secs(5),
            "writer.join() took {elapsed:?}; sender-clone-survival regression"
        );

        // Post-shutdown the surviving clone's send returns Disconnected.
        let send_result = extra_sender.try_send(crate::cmd::WriteCmd::Shutdown);
        assert!(
            send_result.is_err(),
            "post-shutdown try_send must fail (got {send_result:?})"
        );
    }

    #[test]
    fn start_in_memory_installs_fts_triggers() {
        let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
        let h = SqliteWriter::start_in_memory(bus).expect("start_in_memory");
        // If install_fts_triggers panicked or returned Err, start_in_memory
        // above would have failed. Reaching here proves the install ran
        // cleanly. join() matches sibling tests' shutdown discipline —
        // SqliteWriterHandle has no Drop impl (see lines 121-124), so a bare
        // drop() leaks the writer thread until process teardown.
        h.join();
    }
}
