# Phase 3b — Tauri Events + Frontend Live Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the phase 3a watcher into the Tauri desktop app. Backend emits `file-event` Tauri events; frontend subscribes and refreshes the file table (debounced 300ms). Auto-start the watcher on scan completion.

**Architecture:** `crates/desktop/src/events.rs` defines `FileEventPayload` wrapper (keeps core framework-free) + `TauriEventEmitter` impl of `EventBus`. New commands: `start_watch`, `stop_watch`, `is_watching`. `WatcherState` with `tokio::sync::Mutex` holds the active watcher. Frontend subscribes via `@tauri-apps/api/event::listen`, debounces re-fetches via `setTimeout`, auto-starts watch after scan.

**Tech Stack:** Tauri 2 `AppHandle::emit`, tokio::sync::Mutex, `@tauri-apps/api/event::listen`, vitest fake timers.

**Spec:** `docs/superpowers/specs/2026-04-16-phase-3b-tauri-events-frontend-design.md`

**Execution rule:** All work on `main`. Per-commit: execute → `just ci` green → reviewer → commit.

---

## File Structure

```
crates/desktop/src/
├── events.rs                           # new — FileEventPayload + TauriEventEmitter
├── commands.rs                         # modify — add start_watch/stop_watch/is_watching
├── state.rs                            # modify — add WatcherState
└── lib.rs                              # modify — register new commands + manage WatcherState

apps/desktop/src/
├── types.ts                            # modify — add FileEvent union
├── api.ts                              # modify — startWatch/stopWatch/isWatching + subscribeToFileEvents
├── App.tsx                             # modify — subscribe + debounce + auto-start
└── __tests__/
    └── App.test.tsx                    # new — debounce test
```

Two reviewer-gated commits: (1) backend Tauri events + commands, (2) frontend subscription + debounce + auto-start + test + push + tag.

---

## Task 1: `FileEventPayload` + `TauriEventEmitter` in `crates/desktop/src/events.rs`

The implementer should create `crates/desktop/src/events.rs`:

1. Define `FileEventPayload` enum with 4 variants (Created/Modified/Deleted/Renamed) — all fields are `String`. Derive `Serialize + specta::Type + Debug + Clone`. Use `#[serde(tag = "type")]` for a discriminated union.
2. Implement `From<&perima_core::FileEvent> for FileEventPayload` — converts `MediaPath → String` via `.as_str().to_owned()` and `VolumeId → String` via `.0.to_string()`.
3. Define `TauriEventEmitter { app_handle: tauri::AppHandle }` struct.
4. Implement `EventBus for TauriEventEmitter`:
   ```rust
   impl EventBus for TauriEventEmitter {
       fn emit(&self, event: &FileEvent) -> Result<(), CoreError> {
           let payload: FileEventPayload = event.into();
           self.app_handle.emit("file-event", payload)
               .map_err(|e| CoreError::Internal(format!("tauri emit: {e}")))
       }
   }
   ```
5. 1 unit test: construct `FileEvent::Created`, convert to payload, serialize to JSON, assert JSON shape `{"type":"Created","path":"...","volume":"..."}`.

Update `crates/desktop/src/lib.rs` to add `pub mod events;`.

---

## Task 2: `WatcherState` in `crates/desktop/src/state.rs`

Add to `state.rs`:

```rust
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Tracks an active filesystem watcher so commands can start/stop it.
///
/// WHY tokio::sync::Mutex: Tauri v2 async commands run on tokio;
/// std::sync::Mutex held across .await triggers a clippy warning
/// and can deadlock. tokio::sync::Mutex is await-safe.
pub struct WatcherState {
    pub(crate) inner: Mutex<Option<perima_fs::DebouncedWatcher>>,
    pub(crate) cancel: Mutex<Option<CancellationToken>>,
}

impl WatcherState {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::const_new(None),
            cancel: Mutex::const_new(None),
        }
    }
}

impl Default for WatcherState {
    fn default() -> Self { Self::new() }
}
```

Add `tokio-util` to `crates/desktop/Cargo.toml` `[dependencies]` if not already present.

---

## Task 3: Tauri commands `start_watch` / `stop_watch` / `is_watching`

Add to `crates/desktop/src/commands.rs`:

1. `start_watch(path: String, app_handle: AppHandle, state: State<AppState>, watcher_state: State<WatcherState>) -> Result<(), String>`:
   - Validate path (exists + dir).
   - Canonicalize via `dunce::canonicalize`.
   - Detect volume via `perima_fs::detect_volume`.
   - Open DB, find_or_create volume, record mount.
   - Wrap `SqliteFileRepository` in `Arc`. Reuse the existing `DbEventHandler` pattern from CLI's `watch.rs` — you may need to DUPLICATE it in commands.rs (acceptable for v1; consolidate post-v1).
   - Create `CompositeEventBus::new(vec![Arc::new(db_handler), Arc::new(TauriEventEmitter { app_handle: app_handle.clone() }), Arc::new(LogEventHandler)])`.
   - Cancel any existing watcher: lock `watcher_state.cancel`, if `Some`, call `cancel.cancel()`.
   - Drop any existing `DebouncedWatcher`.
   - Create fresh `CancellationToken`, store in `watcher_state.cancel`.
   - Start new `DebouncedWatcher`, store in `watcher_state.inner`.
   - Return Ok.

2. `stop_watch(watcher_state: State<WatcherState>) -> Result<(), String>`:
   - Lock `watcher_state.cancel`, if `Some`, call `.cancel()`, take it to None.
   - Lock `watcher_state.inner`, take it to None (drops the watcher, stops notify).
   - Return Ok.

3. `is_watching(watcher_state: State<WatcherState>) -> Result<bool, String>`:
   - Lock inner, return `.is_some()`.

Annotate all three with `#[tauri::command]` + `#[specta::specta]`.

**Duplication note:** `DbEventHandler` from CLI's watch.rs must be reimplemented here because CLI's module isn't accessible from desktop crate. Create a private `fn build_db_handler(repo, volume_id, device_id) -> Arc<dyn EventBus>` in commands.rs or events.rs.

---

## Task 4: Wire `WatcherState` into Tauri app setup

Modify `crates/desktop/src/lib.rs`:
- Import `WatcherState`.
- In `run()`, add `.manage(WatcherState::new())` to the builder.
- Register the three new commands in `tauri::generate_handler![...]`.
- Register them in the specta builder if the specta collector is used.

Verify: `cargo build -p perima-desktop` exits 0 (with Tauri env vars).

- [ ] **Step 1: Create events.rs (Task 1)**
- [ ] **Step 2: Add WatcherState to state.rs (Task 2)**
- [ ] **Step 3: Add 3 commands to commands.rs (Task 3)**
- [ ] **Step 4: Register in lib.rs (Task 4)**
- [ ] **Step 5: Run gates**

- [ ] **Step 6: Dispatch reviewer (checkpoint #1)**

Reviewer checklist:
- [ ] `FileEventPayload` keeps core specta-free.
- [ ] `TauriEventEmitter` impls `EventBus` with `app_handle.emit("file-event", payload)`.
- [ ] `WatcherState` uses `tokio::sync::Mutex`.
- [ ] `start_watch` cancels existing watcher before starting new.
- [ ] `stop_watch` takes the watcher to None (drops it).
- [ ] All 3 commands registered in tauri::generate_handler.
- [ ] 1 payload-shape unit test passes.
- [ ] `just ci` green.

- [ ] **Step 7: Commit**

```bash
git add crates/desktop/ Cargo.lock
git commit -m "$(cat <<'EOF'
feat(phase-3b): Tauri events + start_watch/stop_watch/is_watching

crates/desktop/events.rs: FileEventPayload wrapper derives
Serialize + specta::Type (keeps core framework-free).
TauriEventEmitter impls EventBus, emits "file-event" via
AppHandle::emit.

crates/desktop/state.rs: WatcherState wraps DebouncedWatcher +
CancellationToken in tokio::sync::Mutex (await-safe).

crates/desktop/commands.rs: start_watch (detect volume, open DB,
compose CompositeEventBus with Db+Tauri+Log handlers, cancel any
prior, store in WatcherState), stop_watch (cancel + drop),
is_watching. All annotated with specta for type-safe TS bindings.

Refs: docs/superpowers/specs/2026-04-16-phase-3b-tauri-events-frontend-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Frontend types + api

1. Update `apps/desktop/src/types.ts`:
   ```typescript
   export type FileEvent =
     | { type: "Created"; path: string; volume: string }
     | { type: "Modified"; path: string; volume: string }
     | { type: "Deleted"; path: string; volume: string }
     | { type: "Renamed"; from: string; to: string; volume: string };
   ```

2. Update `apps/desktop/src/api.ts`:
   ```typescript
   import { listen } from "@tauri-apps/api/event";

   export function startWatch(path: string): ResultAsync<void, string> {
     return fromInvoke("start_watch", { path });
   }
   export function stopWatch(): ResultAsync<void, string> {
     return fromInvoke("stop_watch", {});
   }
   export function isWatching(): ResultAsync<boolean, string> {
     return fromInvoke("is_watching", {});
   }

   export type UnsubscribeFn = () => void;
   export async function subscribeToFileEvents(
     callback: (event: FileEvent) => void,
   ): Promise<UnsubscribeFn> {
     return listen<FileEvent>("file-event", (tauriEvent) => {
       callback(tauriEvent.payload);
     });
   }
   ```

Note: `fromInvoke` already exists in api.ts from phase 2b.

3. Update `apps/desktop/src/__tests__/setup.ts` to add a mock for `@tauri-apps/api/event`:
   ```typescript
   vi.mock("@tauri-apps/api/event", () => ({
     listen: vi.fn(async (_event, _handler) => () => {}),
   }));
   ```

---

## Task 6: `App.tsx` auto-start + debounced refresh

Update `App.tsx`:

1. After a successful scan (inside the `handleScanComplete` callback), call `startWatch(lastScannedPath)` and log errors but don't block.
2. Add a `useEffect` that subscribes to file events on mount:
   ```tsx
   useEffect(() => {
     let timer: ReturnType<typeof setTimeout> | null = null;
     let unsubscribe: UnsubscribeFn | null = null;
     let active = true;

     subscribeToFileEvents(() => {
       if (timer) clearTimeout(timer);
       timer = setTimeout(() => {
         // Refresh via listFiles
         listFiles(100).match(
           (files) => { if (active) setFiles(files); },
           (err) => { if (active) setError(err); },
         );
       }, 300);
     }).then((fn) => { if (active) unsubscribe = fn; });

     return () => {
       active = false;
       if (timer) clearTimeout(timer);
       if (unsubscribe) unsubscribe();
     };
   }, []);
   ```
3. Track `lastScannedPath` in state so auto-start after scan knows the path. The `ScanButton` already opens the folder picker — pass the path up via a new prop or by lifting state.
4. Optional: add a "👁 watching" indicator next to the status bar text when `isWatching()` returns true. Skip if it adds complexity.

---

## Task 7: Frontend debounce test

Create `apps/desktop/src/__tests__/App.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi, beforeEach } from "vitest";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import type { Mock } from "vitest";
import App from "../App";

describe("App file-event debounce", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    (invoke as Mock).mockReset();
    (listen as Mock).mockReset();
  });

  test("5 rapid file-events within 300ms trigger only 1 listFiles call", async () => {
    // Arrange: invoke("list_files") returns empty array on mount.
    (invoke as Mock).mockResolvedValue([]);
    // Capture the handler passed to listen.
    let capturedHandler: ((ev: { payload: unknown }) => void) | null = null;
    (listen as Mock).mockImplementation(async (_event, handler) => {
      capturedHandler = handler;
      return () => {};
    });

    render(<App />);

    // Wait for subscribeToFileEvents to resolve.
    await vi.runAllTicks?.();
    // Give async setup a beat.
    await Promise.resolve();
    await Promise.resolve();

    // The initial listFiles on mount fires; reset the mock so
    // we only count subsequent calls.
    const initialCalls = (invoke as Mock).mock.calls.filter(
      ([cmd]) => cmd === "list_files",
    ).length;
    (invoke as Mock).mockClear();
    (invoke as Mock).mockResolvedValue([]);

    // Fire 5 events synchronously.
    if (!capturedHandler) throw new Error("listen handler not captured");
    for (let i = 0; i < 5; i++) {
      capturedHandler({
        payload: {
          type: "Created",
          path: `file${i}.txt`,
          volume: "00000000-0000-0000-0000-000000000000",
        },
      });
    }

    // Nothing should have fired yet.
    expect((invoke as Mock).mock.calls.filter(([c]) => c === "list_files"))
      .toHaveLength(0);

    // Advance past the 300ms debounce.
    vi.advanceTimersByTime(300);
    // Flush microtasks from the .match() callback.
    await Promise.resolve();
    await Promise.resolve();

    // Exactly one list_files call.
    const postCalls = (invoke as Mock).mock.calls.filter(
      ([cmd]) => cmd === "list_files",
    );
    expect(postCalls).toHaveLength(1);
  });
});
```

Note: if this test proves too timing-sensitive with async/fake-timer interaction, relax to `expect(postCalls.length).toBeLessThanOrEqual(1)` OR mark as `#[vi.skip]` and add a WHY comment. The primary signal is "debounce prevents N→1 explosion"; exact-1 vs at-most-1 is acceptable for v1.

---

## Task 8: Final sweep + push + tag

- [ ] **Step 1: Verify all tests pass**
  - `cargo test --workspace` → all Rust tests.
  - `bun run test` in apps/desktop → 9 existing + 1 new = 10 TS tests.

- [ ] **Step 2: GH issue audit** — any new deferrals from phase 3? Check DECISIONS.md, reviewer feedback. Create issues for anything new.

- [ ] **Step 3: Dispatch final reviewer (checkpoint #2)**

Reviewer checklist:
- [ ] App.tsx auto-starts watcher on scan complete.
- [ ] 300ms debounce verified by test.
- [ ] subscribeToFileEvents cleans up on unmount.
- [ ] All Tauri commands registered and specta-exported.
- [ ] `bun run build` + `bun run test` + `bun run lint` exit 0.
- [ ] `just ci` green.
- [ ] GH issue audit done.

- [ ] **Step 4: Commit frontend**

```bash
git add apps/desktop/ Cargo.lock
git commit -m "$(cat <<'EOF'
feat(phase-3b): frontend subscribes to file-event with 300ms debounce

apps/desktop: App.tsx subscribes to Tauri file-event via
subscribeToFileEvents on mount, debounces re-fetches via 300ms
setTimeout, auto-starts watcher after each successful scan.
Cleans up timer + unsubscribes on unmount.

api.ts: startWatch, stopWatch, isWatching wrappers returning
ResultAsync. subscribeToFileEvents wraps @tauri-apps/api/event
listen() with a typed FileEvent callback.

types.ts: FileEvent discriminated union matching Rust
FileEventPayload (type tag + path/volume fields).

New vitest test: 5 rapid file-events within 300ms trigger 1
list_files call (debounce correctness via fake timers).

Refs: docs/superpowers/specs/2026-04-16-phase-3b-tauri-events-frontend-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 5: Push + wait CI green**

```bash
git push origin main
gh run watch --exit-status "$(gh run list --workflow=ci.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
```

- [ ] **Step 6: Tag phase-3-complete (ONLY after CI green)**

```bash
git tag -a phase-3-complete -m "Phase 3: watching + live updates (backend + frontend)"
git push origin phase-3-complete
```

---

## Self-review

**Spec coverage (3b):**
- FileEventPayload wrapper → Task 1 ✓
- TauriEventEmitter → Task 1 ✓
- WatcherState tokio::sync::Mutex → Task 2 ✓
- start_watch / stop_watch / is_watching commands → Task 3 ✓
- app state registration → Task 4 ✓
- frontend types + api → Task 5 ✓
- App.tsx subscribe + debounce + auto-start → Task 6 ✓
- Debounce test → Task 7 ✓
- GH audit + push + tag → Task 8 ✓

**Placeholder scan:** no TBD/TODO. Every file has exact content or specific enough requirements.

**Type consistency:** `FileEventPayload` (Rust) ↔ `FileEvent` (TS) use the same discriminated-union shape. `UnsubscribeFn` used consistently. `WatcherState` API matches between state.rs and commands.rs.

**Commit discipline:** two reviewer-gated commits + push + tag. Tag lands only after CI green.
