# Phase 3b — Tauri events + frontend live refresh

**Status:** draft awaiting reviewer
**Date:** 2026-04-16
**Parent:** phase 3 spec (split 3a/3b).
**Prior:** phase 3a committed (watcher + CLI watch + CancellationToken + DB status methods).

---

## Goal

Wire the watcher into the Tauri desktop app so the frontend file
table updates live when files change in a watched directory. Emit
Tauri events from the backend; subscribe from the React frontend;
debounce frontend re-fetches.

## Non-goals

- Starting/stopping the watcher via UI buttons (auto-start on scan
  completion for v1).
- Incremental list patching (full re-fetch with 300ms debounce is
  fine for v1).
- Pausing events / filtering by volume (all events fire; frontend
  decides what to do).

---

## Architecture

### New module: `crates/desktop/src/events.rs`

Implements a Tauri-specific `EventBus` that emits events to the
frontend via `app_handle.emit`:

```rust
pub struct TauriEventEmitter {
    app_handle: AppHandle,
}

impl TauriEventEmitter {
    pub fn new(app_handle: AppHandle) -> Self { ... }
}

impl EventBus for TauriEventEmitter {
    fn emit(&self, event: &FileEvent) -> Result<(), CoreError> {
        // emit on the "file-event" channel with a specta-typed payload.
        self.app_handle.emit("file-event", event)
            .map_err(|e| CoreError::Internal(format!("tauri emit: {e}")))
    }
}
```

`FileEvent` already derives `Serialize`. **Decision:** keep
`perima-core` framework-free — do NOT add specta to core. Instead,
use a desktop-crate-local wrapper `FileEventPayload` that derives
`specta::Type` + `Serialize` (see "Dependencies" section below for
the wrapper definition).

### New Tauri commands

```rust
#[tauri::command]
#[specta::specta]
async fn start_watch(
    path: String,
    app_handle: AppHandle,
    state: tauri::State<'_, AppState>,
    watcher_state: tauri::State<'_, WatcherState>,
) -> Result<(), String>;

#[tauri::command]
#[specta::specta]
async fn stop_watch(
    watcher_state: tauri::State<'_, WatcherState>,
) -> Result<(), String>;

#[tauri::command]
#[specta::specta]
async fn is_watching(
    watcher_state: tauri::State<'_, WatcherState>,
) -> Result<bool, String>;
```

### `WatcherState`

Holds the active watcher (if any):

```rust
pub struct WatcherState {
    // WHY tokio::sync::Mutex: Tauri v2 async commands run on tokio;
    // std::sync::Mutex held across .await is a clippy warning and a
    // real footgun. tokio::sync::Mutex is await-safe.
    inner: tokio::sync::Mutex<Option<DebouncedWatcher>>,
    cancel: tokio::sync::Mutex<Option<CancellationToken>>,
}
```

When `start_watch` is called: cancel any existing watcher, create a
new `CancellationToken`, create `CompositeEventBus(DbEventHandler +
TauriEventEmitter + LogEventHandler)`, start `DebouncedWatcher`,
store it in `inner`.

When `stop_watch` is called: `cancel.cancel()` + drop the watcher.

### Frontend subscription

`apps/desktop/src/api.ts` gains:

```typescript
export function startWatch(path: string): ResultAsync<void, string> { ... }
export function stopWatch(): ResultAsync<void, string> { ... }
export function isWatching(): ResultAsync<boolean, string> { ... }

export function subscribeToFileEvents(
  callback: (event: FileEvent) => void,
): UnsubscribeFn {
  // wraps listen("file-event", callback) from @tauri-apps/api/event
}
```

### `App.tsx` changes

- After a successful scan, automatically call `startWatch(path)`.
- Subscribe to `file-event` on mount. On each event:
  - Debounce via a 300ms timer (clear + reset).
  - When the timer fires, call `listFiles(100)` and update the
    table.
- Add a visual indicator in the status bar: "👁 watching <path>"
  when `isWatching()` is true.
- On unmount: unsubscribe + `stopWatch()`.

---

## Dependencies

No new deps. `specta::Type` already available on core (added
transitively through desktop's needs in phase 2a).

Wait — core currently does NOT depend on specta. Adding `specta` to
core breaks the "zero framework deps" rule.

**Revised decision:** wrap `FileEvent` in a desktop-crate-local
wrapper type that derives `specta::Type`:

```rust
// crates/desktop/src/events.rs
#[derive(Serialize, specta::Type)]
#[serde(tag = "type")]
pub enum FileEventPayload {
    Created { path: String, volume: String },
    Modified { path: String, volume: String },
    Deleted { path: String, volume: String },
    Renamed { from: String, to: String, volume: String },
}

impl From<&FileEvent> for FileEventPayload {
    fn from(e: &FileEvent) -> Self { ... }
}
```

This keeps `perima-core` framework-free while letting Tauri emit a
specta-typed payload.

---

## Frontend

### `apps/desktop/src/types.ts`

Add `FileEvent`:

```typescript
export type FileEvent =
  | { type: "Created"; path: string; volume: string }
  | { type: "Modified"; path: string; volume: string }
  | { type: "Deleted"; path: string; volume: string }
  | { type: "Renamed"; from: string; to: string; volume: string };
```

### `apps/desktop/src/api.ts`

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
  const unlisten = await listen<FileEvent>("file-event", (tauriEvent) => {
    callback(tauriEvent.payload);
  });
  return unlisten;
}
```

### `App.tsx` debounced refresh

```typescript
useEffect(() => {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let unsubscribe: UnsubscribeFn | null = null;

  subscribeToFileEvents(() => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      // re-fetch via api.listFiles
      refreshFileList();
    }, 300);
  }).then((fn) => { unsubscribe = fn; });

  return () => {
    if (timer) clearTimeout(timer);
    if (unsubscribe) unsubscribe();
  };
}, []);
```

---

## Testing

### Backend (Rust)

- Unit test in `crates/desktop/src/events.rs`: construct a
  `FileEventPayload` from each `FileEvent` variant, serialize to
  JSON, assert the expected shape.
- Integration test: `start_watch_inner` + `stop_watch_inner` helper
  functions (same pattern as scan/list_files). Mock `AppHandle`
  with `tauri::test::MockRuntime`. Skip if Tauri test utilities
  are too painful — accept unit-only coverage.

### Frontend (TypeScript)

- `App.test.tsx`: new test verifying that multiple `file-event`s
  within 300ms trigger only ONE `listFiles` call (debounce
  behavior). Pattern:
  ```typescript
  vi.useFakeTimers();
  vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
  // capture the handler passed to listen
  const handler = (listen as Mock).mock.calls[0][1];
  // fire 5 events synchronously
  for (let i = 0; i < 5; i++) handler({ payload: {...} });
  vi.advanceTimersByTime(300);
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(invoke).toHaveBeenCalledWith("list_files", ...);
  ```

---

## Exit criteria (autonomously verifiable)

P3b-1. `start_watch` command creates a watcher that emits Tauri
       events.
P3b-2. Frontend subscribes via `subscribeToFileEvents` and the
       callback fires when the backend emits.
P3b-3. 300ms debounce: 5 rapid events within 300ms trigger 1
       `listFiles` call.
P3b-4. `cargo build -p perima-desktop` exits 0.
P3b-5. `bun run test` in `apps/desktop` exits 0 (existing 9 + new
       debounce test).
P3b-6. `just ci` green.
P3b-7. `phase-3-complete` tag pushed after CI green (this
       completes all of phase 3).

---

## Risks

- **`AppHandle` thread-safety.** Tauri's `AppHandle` is `Clone +
  Send + Sync`, so holding it in `TauriEventEmitter: Send + Sync`
  is fine.
- **Watcher state across command calls.** The `WatcherState`'s
  `Mutex<Option<DebouncedWatcher>>` can deadlock if `emit` holds a
  lock that `stop_watch` also wants. Mitigation: `TauriEventEmitter`
  doesn't touch `WatcherState` — it only holds `AppHandle`.
- **Frontend debounce timing.** 300ms is a trade-off. Too short:
  wastes DB queries. Too long: UI feels laggy. Acceptable for v1.
- **macOS watcher quirks** (issue #5) still apply — live refresh
  works on Linux, partially on macOS (creates land, deletes/renames
  depend on notify coalescing). Frontend doesn't care about the
  semantic mismatch — any event triggers a re-fetch.
