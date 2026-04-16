# Phase 3 — Watching + incremental updates

**Status:** draft awaiting reviewer
**Date:** 2026-04-16
**Parent:** meta-plan phase 3.
**Prior:** `phase-2-complete` tag.

---

## Goal

Watch managed directories for filesystem changes and update
`file_locations.status` (active/missing/moved) in real time. Ship
`perima watch <path>` in the CLI and emit Tauri events so the
desktop UI reflects changes live.

This phase introduces:
- `tokio` as the async runtime (needed for `notify-debouncer-full`).
- `EventBus` trait in core + concrete adapters.
- `file-id` crate for rename stitching.
- `Stale` variant added to `LocationStatus` enum.
- **`Asset<State>` type-state is NOT introduced** — still no
  consumer that needs compile-time state transitions. The watcher's
  `FileEvent` enum carries all needed information. Revisit when a
  real consumer appears (phase 9 recovery?).

## Non-goals

- Thumbnails, EXIF metadata (phase 4).
- Tags, search, FTS5 (phases 5a/5b).
- HTTP API (phase 6).
- Re-hashing moved/modified files (tracked as "missing" + "active"
  at new path; full re-hash is phase 9 perf work).

---

## Architecture

### New/modified crates

```
crates/core/src/
├── events.rs               # new — EventBus trait + FileEvent enum
├── types.rs                # modify — add Stale to LocationStatus

crates/fs/src/
├── watcher.rs              # new — DebouncedWatcher wrapping notify

crates/db/src/
├── file_repo.rs            # modify — add update_status method

crates/cli/src/
├── cmd/watch.rs            # new — perima watch <path>
├── main.rs                 # modify — add Watch command

crates/desktop/src/
├── commands.rs             # modify — add start_watch/stop_watch
├── events.rs               # new — Tauri event emitter (EventBus impl)
```

### EventBus trait

```rust
// crates/core/src/events.rs

/// A filesystem event that the watcher detected.
#[derive(Clone, Debug, Serialize)]
pub enum FileEvent {
    Created { path: MediaPath, volume: VolumeId },
    Modified { path: MediaPath, volume: VolumeId },
    Deleted { path: MediaPath, volume: VolumeId },
    Renamed { from: MediaPath, to: MediaPath, volume: VolumeId },
}

/// Consumers of filesystem events.
pub trait EventBus: Send + Sync {
    /// Emit an event. Implementations decide how to deliver it
    /// (DB update, Tauri event, log, etc.).
    ///
    /// Returns `Err` if the handler fails (e.g., DB write error).
    /// The composite bus logs errors from individual handlers but
    /// does not abort — remaining handlers still fire.
    fn emit(&self, event: &FileEvent) -> Result<(), CoreError>;
}
```

Multiple `EventBus` implementations can be composed:
- **`DbEventHandler`** — updates `file_locations.status` on each event.
- **`TauriEventEmitter`** — emits Tauri app events for the frontend.
- **`LogEventHandler`** — logs via `tracing::info!` (always on).
- **`CompositeEventBus`** — wraps `Vec<Box<dyn EventBus>>`, fans out.

### Watcher (`crates/fs/src/watcher.rs`)

Uses `notify-debouncer-full` (handles debouncing, renames via
`file-id`, cross-platform quirks). The debouncer emits
`DebouncedEvent`s on a channel; our adapter maps them to
`FileEvent`s and calls `EventBus::emit`.

```rust
pub struct DebouncedWatcher {
    // Holds the debouncer handle; dropping it stops watching.
    _debouncer: Debouncer<RecommendedWatcher, FileIdMap>,
}

impl DebouncedWatcher {
    /// Start watching `paths` recursively. Events are emitted to
    /// `bus` via a background tokio task.
    pub fn start(
        paths: &[PathBuf],
        volume_root: &Path,
        bus: Arc<dyn EventBus>,
        cancel: CancellationToken,
    ) -> Result<Self, CoreError>;
}
```

The watcher runs on a tokio background task. Phase 3 **migrates the
CLI's cancellation from `AtomicBool` (`signals.rs`) to
`CancellationToken` from `tokio-util`**. The `ctrlc` handler now
calls `token.cancel()` instead of flipping an `AtomicBool`. Both
`scan` and `watch` share the same `CancellationToken`. The desktop
backend also migrates its cancellation stub to use the same token.
This unifies the two cancellation mechanisms that would otherwise
coexist and cause bugs when scan + watch need to share a shutdown.

### Tokio introduction

Phase 3 is the first phase that needs async:
- `notify-debouncer-full`'s channel is `std::sync::mpsc`, but we
  need a tokio task to poll it without blocking the main thread.
- Tauri v2 commands already run on tokio under the hood.
- The CLI's `perima watch` needs to run indefinitely until Ctrl-C.

**Approach:** introduce `tokio` as the runtime in both CLI and
desktop. The CLI's `main()` becomes `#[tokio::main]`. Existing
sync commands (`scan`, `ls`, `volumes`) remain synchronous —
`tokio::task::spawn_blocking` wraps them if needed, but since
they're already sync and called from the main task, they just run
directly.

### Status transitions

When an event arrives:

| Event | DB action |
|-------|-----------|
| Created | Hash the new file → `upsert_file` + `upsert_location` (status = active) |
| Modified | `UPDATE file_locations SET status = 'stale'` — hash is outdated, re-scan re-hashes |
| Deleted | `UPDATE file_locations SET status = 'missing'` WHERE path matches |
| Renamed | `UPDATE file_locations SET relative_path = <new>, status = 'active'` WHERE old path matches |

**WHY Modified → stale (not missing):** a modified file's BLAKE3
hash is now outdated, but the file is still present at the same
path. `Missing` means "file not found at this path" — using it for
modified files would corrupt the semantic meaning. `Stale` means
"hash outdated, re-scan needed." Rather than re-hashing
synchronously in the event handler (slow for large files, blocks
the watcher), we set `stale` so the next `perima scan` picks it up
and re-hashes. Trade-off: accuracy deferred by one scan cycle in
exchange for watcher responsiveness.

`LocationStatus` enum gains a `Stale` variant. The `status` column
in `file_locations` is `TEXT`, so adding `"stale"` as a value is a
data-level change (no DDL), respecting the schema freeze.

### Frontend live updates

The Tauri backend starts watching when a scan completes (or when
the user explicitly clicks "Watch"). Events are emitted to the
frontend via `app_handle.emit("file-event", &event)`. The React
frontend subscribes via `listen("file-event", callback)` and
updates the file table reactively.

Simple approach for v1: on each event, re-fetch the entire file
list via `listFiles()`. This is a full refresh, not an incremental
patch — acceptable for up to ~10k files. Incremental patching is
phase 9 perf work.

**Frontend debounce:** batch events within a 300ms window before
calling `listFiles()`. Without debouncing, rapid filesystem
operations (extracting an archive) would fire dozens of full
SELECT+JOIN queries per second.

---

## CLI: `perima watch <path>`

```
perima watch <path>
```

Options:
- `--data-dir <path>` — override DB location.
- `-v / -vv` — tracing verbosity.

Behavior:
1. Resolve config, open DB, detect volume.
2. Start `DebouncedWatcher` on `<path>`.
3. Log events via `tracing::info!`.
4. On each event, update DB status.
5. Run until Ctrl-C (exit 130).
6. On Ctrl-C: drop the watcher (stops notify), print summary
   `"watched <path>: <N> events processed"`.

---

## Testing strategy

### Unit tests (`crates/fs/src/watcher.rs`)

- `watcher_detects_create` — create a file in a watched tmpdir →
  assert `FileEvent::Created` emitted to a mock `EventBus`.
- `watcher_detects_delete` — delete a file → `FileEvent::Deleted`.
- `watcher_detects_rename` — rename a file → `FileEvent::Renamed`
  with correct `from`/`to` paths.

These tests use real filesystem operations on `tempfile::tempdir()`
with a short debounce timeout (100ms). They may be flaky on very
slow CI — gate behind a retry count or `#[ignore]` + manual run.

### Unit tests (`crates/db/src/file_repo.rs`)

- `update_status_to_missing` — set a location status to "missing".
- `update_status_to_moved` — update relative_path + status.

### Integration test (`crates/cli/tests/watch_integration.rs`)

- Start `perima watch <tmpdir>` as a child process.
- Create a file in the tmpdir.
- Wait 2 seconds.
- Kill the child (SIGTERM).
- Assert stderr mentions the created file event.

This test is `#[cfg(unix)]` only (signal handling differs on Windows).

### Desktop test

- Add a headless test that invokes `start_watch`, creates a file,
  waits, invokes `stop_watch`, then `list_files` → assert the new
  file appears.

---

## Dependencies to add

```toml
# workspace Cargo.toml
tokio-util = { version = "0.7", features = ["rt"] }
notify-debouncer-full = "0.7"  # already in workspace
file-id = "0.2"                # already in workspace
```

`tokio` is already in the workspace. `notify` and
`notify-debouncer-full` are already pinned. `file-id` is already
pinned.

Add `tokio-util` to workspace deps (for `CancellationToken`).

---

## Sub-phase split

Phase 3 is one-plan-sized IF the scope is:
- watcher module + EventBus trait + DB status updates + CLI watch
  command + 1 integration test.
- Desktop Tauri events + frontend live refresh are a thin layer
  on top.

**Split confirmed by reviewer:** 3a (EventBus + watcher + DB status
+ CLI watch + tokio + cancellation migration) and 3b (Tauri events
+ frontend live refresh with debounced re-fetch). Each gets its own
plan. `phase-3-complete` tag lands at the end of 3b.

---

## Exit criteria (autonomously verifiable)

P3-1. `perima watch <tmpdir>` runs, detects file creation, prints
      event to stderr, updates DB status. Verified via integration
      test.
P3-2. `perima watch` exits 130 on Ctrl-C with a summary line.
P3-3. After deleting a file while watching, `perima ls` shows
      `status = missing` for that file.
P3-4. After renaming a file while watching, `perima ls` shows the
      new path with `status = active`.
P3-5. Tauri frontend receives `file-event` events and refreshes
      the file table (tested via headless command test).
P3-6. Integration test with 10+ mutations all reflected in DB
      within 2 seconds (p95).
P3-7. All 76 phase-2 tests still pass.
P3-8. `just ci` green.
P3-9. `phase-3-complete` tag pushed after CI green.

---

## Risks

- **Watcher test flakiness.** Filesystem events are inherently
  timing-dependent. Mitigation: generous timeouts (2s debounce
  window), retry logic in tests, `#[cfg(unix)]` gate for signal
  tests.
- **`notify` cross-platform quirks.** macOS uses `FSEvents` (batch
  events), Linux uses `inotify` (per-file events), Windows uses
  `ReadDirectoryChangesW`. The debouncer normalizes these, but
  edge cases (rapid renames, very deep trees) may differ.
  Mitigation: test on all 3 CI platforms; accept minor behavioral
  differences.
- **Tokio in CLI.** Switching `main()` from sync to
  `#[tokio::main]` could break the existing sync scan flow if any
  code accidentally holds a `MutexGuard` across an `.await`.
  Mitigation: existing scan remains fully synchronous; only `watch`
  uses async. The `#[tokio::main]` macro creates a multi-thread
  runtime, but sync commands complete on the main task without
  yielding.
- **Modified → stale deferred re-hash.** Users see "stale" status
  for files they've edited until the next scan. Acceptable for v1;
  document in `perima watch --help`.
