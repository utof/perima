# Phase 3a — EventBus + Watcher + CLI Watch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add filesystem watching to perima: `EventBus` trait in core, `DebouncedWatcher` in fs crate via `notify-debouncer-full`, DB status updates (`Stale`/`Missing`/`Moved`), `perima watch <path>` CLI command, and migration from `AtomicBool` cancellation to `tokio-util::CancellationToken`. Tokio runtime introduced for the CLI.

**Architecture:** `crates/core` gains `events.rs` (FileEvent enum + EventBus trait) and a `Stale` variant on `LocationStatus`. `crates/fs` gains `watcher.rs` wrapping `notify-debouncer-full` with a tokio background task that maps debounced events to `FileEvent`s and calls `EventBus::emit`. `crates/db` gains `update_location_status` + `update_location_path` methods on `SqliteFileRepository`. `crates/cli` gains `cmd/watch.rs` and switches `main` to `#[tokio::main]`.

**Tech Stack:** tokio (already in workspace), tokio-util (new — `CancellationToken`), notify-debouncer-full 0.7, file-id 0.2 (both already pinned). Existing: rusqlite, blake3, walkdir, sysinfo, clap, rayon, ctrlc.

**Spec:** `docs/superpowers/specs/2026-04-16-phase-3-watching-design.md`

**Execution rule:** All work on `main`. Per-commit: execute → `just ci` green → reviewer → commit.

---

## File Structure

```
Cargo.toml                              # modify — add tokio-util workspace dep
crates/core/src/
├── types.rs                            # modify — add Stale to LocationStatus
├── events.rs                           # new — FileEvent + EventBus trait
└── lib.rs                              # modify — add events module + re-exports

crates/fs/
├── Cargo.toml                          # modify — add notify-debouncer-full, file-id, tokio, tokio-util
└── src/
    ├── watcher.rs                      # new — DebouncedWatcher
    └── lib.rs                          # modify — add watcher module

crates/db/src/
├── file_repo.rs                        # modify — add update_location_status, update_location_path

crates/cli/
├── Cargo.toml                          # modify — add tokio, tokio-util
└── src/
    ├── signals.rs                      # modify — migrate to CancellationToken
    ├── cmd/
    │   ├── mod.rs                      # modify — add watch module
    │   ├── scan.rs                     # modify — use CancellationToken
    │   └── watch.rs                    # new — perima watch command
    └── main.rs                         # modify — add Watch command, #[tokio::main]
```

Three reviewer-gated commits: (1) core events + Stale + DB methods, (2) watcher + cancellation migration, (3) CLI watch + tests.

---

## Task 1: Add `Stale` to `LocationStatus` + workspace deps

**Files:**
- Modify: `crates/core/src/types.rs`
- Modify: `Cargo.toml` (workspace deps)

The implementer should:

1. Add `Stale` variant to `LocationStatus` in `crates/core/src/types.rs`:
   ```rust
   pub enum LocationStatus {
       Active,
       Missing,
       Moved,
       /// Hash is outdated — file was modified in place. Next scan
       /// will re-hash and restore to Active.
       Stale,
   }
   ```

2. Update the DB deserializer in `crates/db/src/file_repo.rs` — the `list_file_locations` method has a `match status_str.as_str()` block. Add `"stale" => LocationStatus::Stale`.

3. Add `tokio-util` to workspace `[workspace.dependencies]`:
   ```toml
   tokio-util = { version = "0.7", features = ["rt"] }
   ```

4. Verify: `just ci` green.

---

## Task 2: `FileEvent` enum + `EventBus` trait

**Files:**
- Create: `crates/core/src/events.rs`
- Modify: `crates/core/src/lib.rs`

The implementer should create `events.rs`:

```rust
//! Filesystem event types and the `EventBus` trait.

use serde::Serialize;

use crate::{CoreError, MediaPath, VolumeId};

/// A filesystem event detected by the watcher.
#[derive(Clone, Debug, Serialize)]
pub enum FileEvent {
    /// A new file appeared at this path.
    Created {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
    /// An existing file's content was modified.
    Modified {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
    /// A file was deleted from this path.
    Deleted {
        /// Relative path within the volume.
        path: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
    /// A file was renamed/moved within the same volume.
    Renamed {
        /// Previous relative path.
        from: MediaPath,
        /// New relative path.
        to: MediaPath,
        /// Volume the file lives on.
        volume: VolumeId,
    },
}

/// Consumer of filesystem events.
///
/// Multiple implementations can be composed via a fan-out adapter
/// (e.g., `CompositeEventBus`). The composite logs errors from
/// individual handlers but does not abort — remaining handlers
/// still fire.
pub trait EventBus: Send + Sync {
    /// Process an event.
    ///
    /// # Errors
    /// Returns `CoreError` if the handler fails (e.g., DB write).
    fn emit(&self, event: &FileEvent) -> Result<(), CoreError>;
}
```

Update `lib.rs` to add `pub mod events;` and re-export `FileEvent` + `EventBus`.

Verify: `just ci` green.

---

## Task 3: DB status update methods (TDD)

**Files:**
- Modify: `crates/db/src/file_repo.rs`

The implementer should add two new methods to `SqliteFileRepository` (NOT on the `FileRepository` trait — these are impl-specific like `migrate_sentinel_row`):

1. `update_location_status(&self, volume: VolumeId, path: &MediaPath, status: LocationStatus, device: DeviceId) -> Result<u64, CoreError>` — updates the status of the matching active (non-deleted) file_location row. Returns rows affected.

2. `update_location_path(&self, volume: VolumeId, old_path: &MediaPath, new_path: &MediaPath, device: DeviceId) -> Result<u64, CoreError>` — updates `relative_path` and sets `status = 'active'` for the matching row. For renames.

Write 4 tests (TDD):
- `update_status_to_missing` — set a location to Missing.
- `update_status_to_stale` — set a location to Stale.
- `update_location_path_renames` — rename a location, verify new path + Active status.
- `update_location_path_nonexistent` — rename a path that doesn't exist → 0 rows affected.

- [ ] **Step 1: Write tests first**
- [ ] **Step 2: Implement methods**
- [ ] **Step 3: Run gates** — `cargo test -p perima-db` (26+ tests).

- [ ] **Step 4: Dispatch reviewer (checkpoint #1)**

Reviewer checklist:
- [ ] `Stale` variant in `LocationStatus`.
- [ ] DB deserializer handles `"stale"`.
- [ ] `FileEvent` enum + `EventBus` trait in core.
- [ ] `update_location_status` + `update_location_path` on `SqliteFileRepository`.
- [ ] 4 new tests pass. `just ci` green.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/core/ crates/db/src/file_repo.rs
git commit -m "$(cat <<'EOF'
feat(phase-3a): FileEvent + EventBus trait, Stale status, DB status methods

crates/core: FileEvent enum (Created/Modified/Deleted/Renamed) +
EventBus trait with fallible emit(). LocationStatus gains Stale
variant (hash outdated, next scan re-hashes).

crates/db: SqliteFileRepository gains update_location_status and
update_location_path for watcher-driven status transitions.
DB deserializer handles "stale" string.

tokio-util added to workspace deps (CancellationToken for phase 3a
watcher + cancellation migration).

Refs: docs/superpowers/specs/2026-04-16-phase-3-watching-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `DebouncedWatcher` (`crates/fs/src/watcher.rs`)

**Files:**
- Modify: `crates/fs/Cargo.toml` — add `notify-debouncer-full`, `file-id`, `tokio`, `tokio-util`
- Create: `crates/fs/src/watcher.rs`
- Modify: `crates/fs/src/lib.rs`

The implementer should:

1. Add deps to `crates/fs/Cargo.toml`:
   ```toml
   notify-debouncer-full.workspace = true
   file-id.workspace              = true
   tokio.workspace                = true
   tokio-util.workspace           = true
   ```

2. Create `watcher.rs` with:
   - `pub struct DebouncedWatcher` holding the debouncer handle.
   - `pub fn start(paths: &[PathBuf], volume_root: &Path, volume_id: VolumeId, bus: Arc<dyn EventBus>, cancel: CancellationToken) -> Result<Self, CoreError>`.
   - Inside `start`: create a `notify_debouncer_full::new_debouncer` with a 1-second timeout. The debouncer's event handler receives `Result<Vec<DebouncedEvent>>`. Spawn a tokio task that:
     - Receives events from the debouncer's channel.
     - Maps each `DebouncedEvent` to a `FileEvent` using the event's `kind` + `paths`.
     - For renames: `notify` emits `EventKind::Modify(ModifyKind::Name(RenameMode::Both))` with `paths[0]` = old, `paths[1]` = new.
     - Calls `bus.emit(&file_event)` for each.
     - Checks `cancel.is_cancelled()` between events.
   - Dropping `DebouncedWatcher` stops the debouncer.

3. Write 3 tests (timing-sensitive; use 100ms debounce for tests):
   - `watcher_detects_create` — create a file → assert `FileEvent::Created` emitted.
   - `watcher_detects_delete` — delete a file → `FileEvent::Deleted`.
   - `watcher_detects_rename` — rename a file → `FileEvent::Renamed`.

   Tests use a `MockEventBus` that collects events into `Arc<Mutex<Vec<FileEvent>>>`. Wait up to 3 seconds for the expected event count.

   **Important:** these tests require a tokio runtime. Use `#[tokio::test]`.

4. Update `crates/fs/src/lib.rs` — add `pub mod watcher;` + `pub use watcher::DebouncedWatcher;`.

- [ ] **Step 1: Add deps**
- [ ] **Step 2: Write `watcher.rs` with tests**
- [ ] **Step 3: Run gates** — may be flaky; retry once if timing-related.

---

## Task 5: Migrate cancellation to `CancellationToken`

**Files:**
- Modify: `crates/cli/Cargo.toml` — add `tokio.workspace = true`, `tokio-util.workspace = true`
- Modify: `crates/cli/src/signals.rs`
- Modify: `crates/cli/src/cmd/scan.rs`
- Modify: `crates/cli/src/main.rs`

The implementer should:

1. Rewrite `signals.rs`: replace `AtomicBool` with `tokio_util::sync::CancellationToken`.
   ```rust
   pub struct Cancellation {
       token: CancellationToken,
   }
   impl Cancellation {
       pub fn cancelled(&self) -> bool { self.token.is_cancelled() }
       pub fn token(&self) -> CancellationToken { self.token.clone() }
   }
   pub fn install() -> Result<Cancellation, CoreError> {
       let token = CancellationToken::new();
       let cloned = token.clone();
       ctrlc::set_handler(move || { cloned.cancel(); })
           .map_err(|e| CoreError::Internal(format!("ctrlc: {e}")))?;
       Ok(Cancellation { token })
   }
   ```

2. Update `scan.rs`: the `cancel.token()` calls now return `CancellationToken` instead of `Arc<AtomicBool>`. Inside the rayon map closure, use `cancel_token.is_cancelled()` instead of `cancel_flag.load(Ordering::SeqCst)`. The `take_while` in the walk uses `cancel.cancelled()` which still works.

3. Update `main.rs`: add `#[tokio::main]` and make `main` async. Existing sync commands run directly (no `spawn_blocking` needed). Add `tokio` + `tokio-util` to cli Cargo.toml deps.

4. Verify: all existing tests pass. The cancellation unit test in `signals.rs` updates to use `CancellationToken` semantics.

- [ ] **Step 1: Rewrite signals.rs**
- [ ] **Step 2: Update scan.rs**
- [ ] **Step 3: Update main.rs to `#[tokio::main] async fn main()`**
- [ ] **Step 4: Run gates**

- [ ] **Step 5: Dispatch reviewer (checkpoint #2)**

Reviewer checklist:
- [ ] `CancellationToken` replaces `AtomicBool` everywhere.
- [ ] `ctrlc` handler calls `token.cancel()`.
- [ ] `scan.rs` rayon closure uses `is_cancelled()`.
- [ ] `main.rs` is `#[tokio::main] async fn main()`.
- [ ] All existing tests pass (76 Rust + 9 TS).
- [ ] `just ci` green.

- [ ] **Step 6: Commit**

```bash
git add crates/fs/ crates/cli/ Cargo.lock
git commit -m "$(cat <<'EOF'
feat(phase-3a): DebouncedWatcher + CancellationToken migration

crates/fs/watcher.rs: DebouncedWatcher wraps notify-debouncer-full
with a tokio background task mapping DebouncedEvents to FileEvents
and emitting via EventBus. 3 watcher tests (create/delete/rename).

crates/cli: signals.rs migrated from AtomicBool to tokio-util
CancellationToken. ctrlc handler calls token.cancel(). scan.rs
updated to use is_cancelled(). main.rs now #[tokio::main] async.
All existing sync commands unchanged — they complete on the main
task without yielding.

Refs: docs/superpowers/specs/2026-04-16-phase-3-watching-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `perima watch <path>` CLI command

**Files:**
- Create: `crates/cli/src/cmd/watch.rs`
- Modify: `crates/cli/src/cmd/mod.rs`
- Modify: `crates/cli/src/main.rs`

The implementer should:

1. Create `watch.rs`:
   ```rust
   pub async fn run(
       data_dir: &Path,
       device_id: DeviceId,
       path: &Path,
       cancel: &Cancellation,
   ) -> Result<(), CoreError>
   ```
   - Validate path (exists, is dir).
   - Canonicalize via dunce.
   - Detect volume.
   - Open DB, find_or_create volume, record mount.
   - Create a `DbEventHandler` that implements `EventBus` by calling `update_location_status` / `update_location_path` on the file repo.
   - Create a `LogEventHandler` that logs each event.
   - Compose into a `CompositeEventBus`.
   - Start `DebouncedWatcher` with the composed bus.
   - `cancel.token().cancelled().await` — blocks until Ctrl-C.
   - Print summary to stderr.

2. The `DbEventHandler` needs access to `SqliteFileRepository`. Since it implements `EventBus: Send + Sync` and `SqliteFileRepository` uses `Mutex<Connection>`, it can hold an `Arc<SqliteFileRepository>`.

3. The `CompositeEventBus`:
   ```rust
   pub struct CompositeEventBus {
       handlers: Vec<Arc<dyn EventBus>>,
   }
   impl EventBus for CompositeEventBus {
       fn emit(&self, event: &FileEvent) -> Result<(), CoreError> {
           for h in &self.handlers {
               if let Err(e) = h.emit(event) {
                   tracing::warn!(error = %e, "event handler failed");
               }
           }
           Ok(())
       }
   }
   ```
   This can live in `crates/core/src/events.rs` or in `watch.rs`.

4. Update `cmd/mod.rs` — add `pub mod watch;`.
5. Update `main.rs` — add `Watch { root: PathBuf }` variant. Dispatch to `cmd::watch::run`.

- [ ] **Step 1: Create watch.rs + event handlers**
- [ ] **Step 2: Update mod.rs + main.rs**
- [ ] **Step 3: Run gates** — `cargo run -p perima -- watch /tmp` should start watching and log events until Ctrl-C.

---

## Task 7: Integration test + final sweep

**Files:**
- Create: `crates/cli/tests/watch_integration.rs`

The implementer should:

1. Create a `#[cfg(unix)]` integration test:
   - Start `perima watch <tmpdir>` as a child process via `Command::new(bin())`.
   - Wait 1 second for the watcher to initialize.
   - Create a new file in the tmpdir.
   - Wait 2 seconds for event processing.
   - Send SIGTERM to the child (`nix::sys::signal::kill` or `libc::kill`).
   - Capture stderr.
   - Assert stderr mentions the watch summary or event count.

   **Alternative if signal handling is too complex:** just test that `perima watch <tmpdir>` starts and exits 130 when killed.

2. GH issue audit: review DECISIONS.md + phase 3 spec for deferrals.

3. WHY-comment check: `grep -rE '^\s*//\s*WHY:' crates/` ≥ previous count + new.

4. `cargo clean && just ci` green.

- [ ] **Step 1: Write integration test**
- [ ] **Step 2: GH issue audit**
- [ ] **Step 3: Final sweep**

- [ ] **Step 4: Dispatch reviewer (checkpoint #3)**

Reviewer checklist:
- [ ] `perima watch <tmpdir>` runs, detects creates, logs events.
- [ ] Ctrl-C exits 130 with summary.
- [ ] `update_location_status` called on Delete (→ Missing).
- [ ] `update_location_status` called on Modified (→ Stale).
- [ ] `update_location_path` called on Rename.
- [ ] Integration test passes on Linux/macOS.
- [ ] All 76+ previous tests pass.
- [ ] `just ci` green.
- [ ] GH issues created for deferrals.

- [ ] **Step 5: Commit**

```bash
git add crates/cli/ Cargo.lock
git commit -m "$(cat <<'EOF'
feat(phase-3a): perima watch command + integration test

crates/cli/cmd/watch.rs: async watch command that starts
DebouncedWatcher, composes DbEventHandler + LogEventHandler via
CompositeEventBus, runs until Ctrl-C (CancellationToken), prints
summary.

DbEventHandler: Created → hash + upsert, Modified → status=stale,
Deleted → status=missing, Renamed → update_location_path.
CompositeEventBus fans out to all handlers, logs individual errors.

Integration test (unix only): starts watch as child, creates a file,
kills child, asserts exit 130.

Refs: docs/superpowers/specs/2026-04-16-phase-3-watching-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 6: Push + wait CI green**

```bash
git push origin main
gh run watch --exit-status "$(gh run list --workflow=ci.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
```

Note: Do NOT tag yet — phase 3 completes after 3b (Tauri events + frontend).

---

## Self-review

**Spec coverage (3a scope):**
- FileEvent enum → Task 2 ✓
- EventBus trait with fallible emit → Task 2 ✓
- Stale LocationStatus variant → Task 1 ✓
- DB status update methods → Task 3 ✓
- DebouncedWatcher (notify-debouncer-full + tokio) → Task 4 ✓
- CancellationToken migration → Task 5 ✓
- `perima watch <path>` CLI → Task 6 ✓
- CompositeEventBus → Task 6 ✓
- DbEventHandler + LogEventHandler → Task 6 ✓
- tokio runtime in CLI → Task 5 ✓
- Integration test → Task 7 ✓

**NOT in 3a scope (deferred to 3b):**
- Tauri event emission
- Frontend live refresh with debounce

**Placeholder scan:** Tasks describe requirements with enough specificity. No "implement later."

**Type consistency:** `FileEvent` matches between core/events.rs, watcher.rs, and watch.rs. `CancellationToken` used consistently after migration. `LocationStatus::Stale` matches between types.rs, file_repo.rs deserializer, and watch.rs handler.

**Commit discipline:** three reviewer-gated commits matching the checkpoint pattern.
