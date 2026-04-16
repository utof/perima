# Phase 2 — Tauri shell (thin UI)

**Status:** draft awaiting reviewer
**Date:** 2026-04-16
**Parent:** meta-plan phase 2.
**Prior:** `phase-1-complete` tag.

---

## Goal

Wrap the working indexing engine in a Tauri 2 + React desktop window.
Single page: flat table of indexed files (path, size, hash, volume,
status). Scan trigger button + native folder picker. No tags, no
search, no thumbnails — those are phases 4-5. The UI can be ugly;
the data layer proved itself in phase 1.

## Non-goals

- File watching / live updates (phase 3).
- Thumbnails, grid view, media metadata (phase 4).
- Tags, search, FTS5 (phases 5a/5b).
- HTTP API (phase 6).
- Docs site (phase 7).

---

## Architecture

```
apps/desktop/                       # Vite + React + Tailwind (Bun)
├── src/
│   ├── App.tsx                     # root component
│   ├── components/
│   │   ├── FileTable.tsx           # table of FileLocationRecord
│   │   ├── ScanButton.tsx          # folder picker + scan trigger
│   │   └── StatusBar.tsx           # scan progress / last scan summary
│   ├── hooks/
│   │   └── usePerima.ts            # Tauri invoke wrappers
│   ├── types.ts                    # TS types mirroring Rust structs
│   └── main.tsx                    # React entry
├── index.html
├── package.json                    # Bun
├── tailwind.config.js
├── vite.config.ts
├── tsconfig.json
crates/desktop/                     # Tauri backend (thin Rust wrapper)
                                    # NO symlink from apps/desktop/src-tauri.
                                    # Tauri CLI is invoked from workspace root:
                                    # `cargo tauri dev -c crates/desktop/tauri.conf.json`
                                    # This avoids symlink path-resolution issues.
├── Cargo.toml
├── tauri.conf.json
├── src/
│   ├── lib.rs                      # Tauri plugin setup
│   └── commands.rs                 # #[tauri::command] fns
├── icons/                          # app icons (Tauri defaults)
└── capabilities/                   # Tauri v2 permissions
```

### IPC via `tauri-specta`

`tauri-specta` generates TypeScript bindings from `#[tauri::command]`
functions at build time. The TS side calls `invoke("scan", { path })`
and gets typed responses. No manual JSON marshaling.

### Commands exposed to the frontend

```rust
#[tauri::command]
#[specta::specta]
async fn scan(path: String, dry_run: bool) -> Result<ScanResult, String>;

#[tauri::command]
#[specta::specta]
async fn list_files(limit: u32, volume: Option<String>) -> Result<Vec<FileLocationRecord>, String>;

#[tauri::command]
#[specta::specta]
async fn list_volumes() -> Result<Vec<VolumeRecord>, String>;
```

Each command constructs its own DB connection (same `open_and_migrate`
pattern from phase 1b) inside a `tauri::State<AppState>` that holds
the resolved `Config`. The commands are thin wrappers calling the
same core logic the CLI uses.

### `AppState`

```rust
pub struct AppState {
    pub config: Config,
    pub device_id: DeviceId,
}
```

Constructed once during Tauri setup from the same `Config::resolve`
the CLI uses. DB connections are opened per-command (cheap under WAL;
avoids lifetime/Send issues with Tauri's async command system).

---

## Frontend

### Tech stack

- **Bun** for install/run/build (per CLAUDE.md).
- **Vite** for dev server + HMR.
- **React 19** (latest stable).
- **Tailwind CSS 4** for styling.
- **TypeScript** strict mode.
- `eslint-plugin-tsdoc` for TSDoc validation.

### UI layout (single page)

```
┌──────────────────────────────────────────┐
│  perima                    [Scan Folder]  │
├──────────────────────────────────────────┤
│  HASH      SIZE    VOLUME   PATH         │
│  a1b2c3…   1.2MB   f0e9…    photos/a.jpg │
│  d4e5f6…   3.4MB   f0e9…    photos/b.jpg │
│  ...                                      │
├──────────────────────────────────────────┤
│  Status: scanned 42 files (3 new)         │
└──────────────────────────────────────────┘
```

- **ScanButton**: opens native folder picker via Tauri's `dialog`
  plugin, then invokes `scan(path, false)`. While scanning, the
  button is disabled and the StatusBar shows progress.
- **FileTable**: calls `list_files(100, null)` on mount and after
  each scan. Renders a `<table>` with sortable columns (client-side
  sort only — no server-side pagination in phase 2).
- **StatusBar**: shows the result of the last scan or "No scans yet."

### Type safety

`tauri-specta` generates `bindings.ts` with typed `invoke` wrappers.
The frontend imports these instead of calling raw `invoke`.

---

## Tauri configuration

### `tauri.conf.json` key settings

```json
{
  "productName": "perima",
  "identifier": "dev.perima.desktop",
  "build": {
    "devUrl": "http://localhost:5173",
    "frontendDist": "../../apps/desktop/dist"
  },
  "app": {
    "windows": [{
      "title": "perima",
      "width": 1024,
      "height": 768
    }]
  },
  "plugins": {
    "dialog": { "open": true }
  }
}
```

### Capabilities

Tauri v2 uses a capability system. Create
`crates/desktop/capabilities/default.json`:

```json
{
  "identifier": "default",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "dialog:default"
  ]
}
```

---

## Testing strategy

### Headless Tauri test (Rust side)

`tauri::test` utilities allow invoking commands without a window.
Test in `crates/desktop/tests/`:

- `commands_test.rs`: invoke `scan` with a tmpdir fixture → assert
  `ScanResult` contains expected counts. Invoke `list_files` → assert
  3 records. Invoke `list_volumes` → assert ≥ 1 volume.

### React component tests (TS side)

`vitest` + `jsdom` for component rendering:

- `FileTable.test.tsx`: render with mock data → assert 3 rows.
- `ScanButton.test.tsx`: render → assert button present; mock
  invoke → assert calls scan command.
- `StatusBar.test.tsx`: render with "scanned 3 files" → assert text.

### Integration

No e2e (Playwright) in phase 2 — the meta-plan defers visual
verification. Headless command tests + component snapshot tests
cover the IPC contract.

---

## Dependencies

### Rust (workspace)

Already pinned in phase 0: `tauri = "2"`, `tauri-specta = "=2.0.0-rc.24"`.

Add to `crates/desktop/Cargo.toml`:

```toml
[dependencies]
perima-core = { path = "../core" }
perima-db   = { path = "../db" }
perima-fs   = { path = "../fs" }
perima-hash = { path = "../hash" }
tauri.workspace       = true
tauri-specta.workspace = true
specta = "=2.0.0-rc.24"          # matches tauri-specta 2.0.0-rc.24
specta-typescript = "0.0.11"     # latest as of 2026-04-16
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true

[build-dependencies]
tauri-build = "2"
specta-typescript = "0.0.9"
```

### Frontend (Bun)

```json
{
  "dependencies": {
    "react": "^19",
    "react-dom": "^19",
    "@tauri-apps/api": "^2",
    "@tauri-apps/plugin-dialog": "^2"
  },
  "devDependencies": {
    "vite": "^6",
    "@vitejs/plugin-react": "^4",
    "typescript": "^5",
    "tailwindcss": "^4",
    "@tailwindcss/vite": "^4",
    "vitest": "^3",
    "jsdom": "^26",
    "@testing-library/react": "^16",
    "eslint": "^9",
    "eslint-plugin-tsdoc": "^0.4"
  }
}
```

---

## Sub-phase split

Phase 2 is split into 2a (Rust Tauri crate + headless tests) and 2b
(React frontend + component tests). Each gets its own plan.

### Phase 2a exit criteria

P2a-1. `cargo build -p perima-desktop` exits 0.
P2a-2. Headless Tauri command tests pass: scan, list_files, list_volumes
        return expected data against a tmpdir fixture.
P2a-3. `cargo clippy -p perima-desktop -- -D warnings` exits 0.
P2a-4. `just ci` green (existing 63 tests + new desktop command tests).
P2a-5. `tauri.conf.json` present with correct paths.
P2a-6. `specta` version resolved and pinned correctly (matches
        `tauri-specta` transitive dep).

### Phase 2b exit criteria

P2b-1. `bun install && bun run build` in `apps/desktop/` exits 0.
P2b-2. `vitest run` in `apps/desktop/` exits 0 — component tests pass.
P2b-3. TSDoc validation: `bun run lint` exits 0.
P2b-4. `just ci` green (including frontend build + test).
P2b-5. `phase-2-complete` tag pushed after CI green.

---

## Risks

- **Tauri v2 + specta RC churn.** `tauri-specta = "=2.0.0-rc.24"` is
  an exact pin for this reason. If it breaks on a new Tauri patch,
  we hold the pin until specta ships stable.
- **Bun + Vite + Tauri dev-server.** Tauri expects `devUrl` at
  localhost:5173; Bun + Vite serve there by default. If port
  conflicts occur, Vite auto-increments; Tauri won't find it.
  Mitigation: pin port in `vite.config.ts`.
- **UI correctness unverifiable autonomously.** Per the meta-plan:
  "UI phases ship with headless IPC tests + component snapshot tests
  only. Visual polish is explicit non-goal for v1."
- **`tauri::test` maturity.** Tauri v2's test utilities are newer than
  v1's. If headless command invocation doesn't work, fall back to
  integration tests that launch the binary and communicate via
  stdio/IPC.
- **Node.js 20 deprecation.** GitHub Actions warns about this for
  `actions/checkout@v4`. Tracked in DECISIONS.md; not blocking.
