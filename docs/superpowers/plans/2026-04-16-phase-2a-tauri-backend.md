# Phase 2a — Tauri Backend Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create `crates/desktop` as a Tauri v2 backend crate exposing `scan`, `list_files`, and `list_volumes` as `#[tauri::command]` functions with `tauri-specta` type-safe bindings. Headless command tests prove the IPC contract without a browser window.

**Architecture:** `crates/desktop` is a thin Tauri wrapper: `AppState` holds resolved `Config` + `DeviceId`. Each command opens its own DB connection (cheap under WAL, avoids lifetime issues with Tauri's async). Commands delegate to the same core logic the CLI uses. `tauri-specta` generates TS bindings at build time via `build.rs`. `tauri.conf.json` lives in `crates/desktop/` and references `../../apps/desktop/dist` for the frontend (which 2b creates).

**Tech Stack:** Tauri 2, tauri-specta 2.0.0-rc.24, specta 2.0.0-rc.24, specta-typescript 0.0.11, tauri-build 2. Existing: rusqlite, blake3, walkdir, sysinfo, clap, rayon.

**Spec:** `docs/superpowers/specs/2026-04-16-phase-2-tauri-shell-design.md` (section "Phase 2a exit criteria")

**Execution rule:** All work on `main`. Per-commit: execute → `just ci` green → reviewer → commit.

---

## File Structure

```
Cargo.toml                              # modify — add workspace members, deps
crates/desktop/
├── Cargo.toml                          # new
├── build.rs                            # new — tauri-build + specta codegen
├── tauri.conf.json                     # new
├── capabilities/
│   └── default.json                    # new — Tauri v2 permissions
├── icons/                              # new — default Tauri icons (generated)
└── src/
    ├── lib.rs                          # new — Tauri plugin/app setup
    ├── state.rs                        # new — AppState
    └── commands.rs                     # new — #[tauri::command] fns
justfile                                # modify — add desktop targets
```

Two reviewer-gated commits: (1) crate scaffold + commands, (2) justfile + final sweep.

---

## Task 1: Install Tauri CLI + scaffold empty crate

**Files:**
- Modify: `Cargo.toml` (workspace members)
- Create: `crates/desktop/Cargo.toml`
- Create: `crates/desktop/build.rs`
- Create: `crates/desktop/src/lib.rs`

The implementer should:

1. Install `tauri-cli` if missing: `cargo install tauri-cli --locked`.
2. Add `crates/desktop` to workspace members. The workspace `Cargo.toml` already has `members = ["crates/*"]` which auto-discovers — but we need the new deps. Add to `[workspace.dependencies]`:
   ```toml
   specta = "=2.0.0-rc.24"
   specta-typescript = "0.0.11"
   tauri-build = "2"
   ```
3. Create `crates/desktop/Cargo.toml`:
   ```toml
   [package]
   name = "perima-desktop"
   version = "0.1.0"
   edition.workspace = true
   rust-version.workspace = true
   license.workspace = true
   repository.workspace = true

   [lib]
   crate-type = ["staticlib", "cdylib", "rlib"]
   name = "perima_desktop"

   [dependencies]
   perima-core = { path = "../core" }
   perima-db   = { path = "../db" }
   perima-fs   = { path = "../fs" }
   perima-hash = { path = "../hash" }
   tauri.workspace         = true
   tauri-specta.workspace  = true
   specta.workspace        = true
   serde.workspace         = true
   serde_json.workspace    = true
   tracing.workspace       = true
   uuid.workspace          = true
   chrono.workspace        = true
   rayon.workspace         = true
   dunce.workspace         = true

   [build-dependencies]
   tauri-build.workspace = true

   [dev-dependencies]
   tempfile.workspace = true

   [lints]
   workspace = true
   ```
4. Create `crates/desktop/build.rs`:
   ```rust
   fn main() {
       tauri_build::build();
   }
   ```
5. Create `crates/desktop/src/lib.rs` with a minimal placeholder:
   ```rust
   //! Tauri desktop backend for perima.

   pub mod commands;
   pub mod state;
   ```
6. Create empty `crates/desktop/src/state.rs` and `crates/desktop/src/commands.rs` with module docstrings only.
7. Create `crates/desktop/tauri.conf.json` per the spec (with `"frontendDist": "../../apps/desktop/dist"`, `devUrl` at localhost:5173, dialog plugin, window config).
8. Create `crates/desktop/capabilities/default.json` per the spec.
9. Verify: `cargo build -p perima-desktop` exits 0. **NOTE:** the build will fail if `tauri.conf.json` references a `frontendDist` that doesn't exist — `tauri-build` checks this at compile time. Workaround: create the placeholder dir `mkdir -p apps/desktop/dist && touch apps/desktop/dist/index.html` so the build doesn't fail. 2b replaces this with the real Vite build output.

- [ ] **Step 1: Install tauri-cli**
- [ ] **Step 2: Create all files above**
- [ ] **Step 3: Create placeholder `apps/desktop/dist/index.html`**
- [ ] **Step 4: Verify `cargo build -p perima-desktop`**
- [ ] **Step 5: Run `just ci`** — may need to add `perima-desktop` to CI awareness if clippy/test skips it.

---

## Task 2: `AppState` (`crates/desktop/src/state.rs`)

The implementer should create `state.rs`:

```rust
//! Shared application state for Tauri commands.

use perima_core::DeviceId;
use crate::config_path;

/// State shared across all Tauri commands via `tauri::State<AppState>`.
pub struct AppState {
    /// Resolved data directory (where perima.db lives).
    pub data_dir: std::path::PathBuf,
    /// Stable device identifier.
    pub device_id: DeviceId,
}
```

And a helper in `lib.rs` to construct it using `Config::resolve`:
- Import the CLI's `config` module... wait, the CLI's config is in `crates/cli/src/config.rs` which is a binary crate. The desktop crate can't import from it.
- **Solution:** extract config resolution into a shared location. Two options:
  (a) Move `config.rs` to `crates/core/` (but core has zero framework deps — `directories` is a framework dep).
  (b) Duplicate the config logic in `crates/desktop/`.
  (c) Create a tiny `crates/config/` crate.
  
  **Best for v1:** duplicate. The config is ~40 lines. A shared crate is premature abstraction. Add a `config.rs` to `crates/desktop/src/` that mirrors the CLI's `Config::resolve` (using `directories` + env vars + `device_id.txt`).

- [ ] **Step 1: Add `directories.workspace = true` to desktop Cargo.toml deps**
- [ ] **Step 2: Create `crates/desktop/src/config.rs`** — same logic as CLI's config (resolve data_dir, config_dir, device_id). Can be a simplified version since no `--data-dir` CLI flag in Tauri.
- [ ] **Step 3: Create `crates/desktop/src/state.rs`** with `AppState` struct.
- [ ] **Step 4: Update `lib.rs`** to add `pub mod config; pub mod state;`.
- [ ] **Step 5: Verify build**

---

## Task 3: Tauri commands (`crates/desktop/src/commands.rs`)

The implementer should create `commands.rs` with three `#[tauri::command]` functions:

1. `scan(path: String, dry_run: bool, state: tauri::State<AppState>) -> Result<ScanResult, String>`:
   - Opens DB via `open_and_migrate(state.data_dir.join("perima.db"))`.
   - Constructs `WalkdirScanner`, `Blake3Service`, `SqliteFileRepository`, `SqliteVolumeRepository`.
   - Calls the same scan logic from phase 1c (detect_volume, find_or_create, walk, hash, persist, manifest).
   - Returns a `ScanResult { total: u64, new: u64, existing: u64, errors: u64 }` struct.
   - Note: the scan logic currently lives as a function in `crates/cli/src/cmd/scan.rs` which is in the CLI binary crate. The desktop crate can't import from it either.
   - **Solution:** The scan logic needs to be extracted to a shared crate or duplicated. Since `scan::run` has 8+ parameters and complex borrow patterns, the cleanest approach is to create a thin wrapper in `commands.rs` that replicates the scan flow (walk → hash → persist → manifest) directly. It's ~50 lines of glue. The core + adapters do the real work.

2. `list_files(limit: u32, volume: Option<String>, state: tauri::State<AppState>) -> Result<Vec<FileLocationRecord>, String>`:
   - Opens DB, constructs `SqliteFileRepository`, calls `list_file_locations`.

3. `list_volumes(state: tauri::State<AppState>) -> Result<Vec<VolumeRecord>, String>`:
   - Opens DB, constructs `SqliteVolumeRepository`, calls `list(device_id)`.

All commands map `CoreError` → `String` via `.map_err(|e| e.to_string())` (Tauri commands require `Result<T, String>` for IPC serialization).

Define a `ScanResult` struct with `#[derive(Serialize, specta::Type)]`.

- [ ] **Step 1: Define `ScanResult` struct**
- [ ] **Step 2: Implement `list_files` command (simplest)**
- [ ] **Step 3: Implement `list_volumes` command**
- [ ] **Step 4: Implement `scan` command (most complex — walk + hash + persist + manifest)**
- [ ] **Step 5: Verify build**

---

## Task 4: Tauri app setup (`crates/desktop/src/lib.rs`)

The implementer should wire `lib.rs` to register commands and manage state:

```rust
//! Tauri desktop backend for perima.

pub mod commands;
pub mod config;
pub mod state;

use tauri::Manager;

/// Build and run the Tauri application.
///
/// # Errors
/// Returns a `tauri::Error` if the app fails to initialize.
pub fn run() -> Result<(), tauri::Error> {
    let config = config::resolve_config()
        .expect("failed to resolve config");

    let app_state = state::AppState {
        data_dir: config.data_dir,
        device_id: config.device_id,
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::scan,
            commands::list_files,
            commands::list_volumes,
        ])
        .run(tauri::generate_context!())
}
```

Note: `tauri_plugin_dialog` needs to be added as a dependency. Add `tauri-plugin-dialog = "2"` to workspace deps and `crates/desktop/Cargo.toml`.

Also, the `tauri::generate_context!()` macro reads `tauri.conf.json` relative to the crate — verify the path works.

- [ ] **Step 1: Add `tauri-plugin-dialog` dep**
- [ ] **Step 2: Write `lib.rs` with app builder**
- [ ] **Step 3: Verify build**

- [ ] **Step 4: Dispatch reviewer (checkpoint #1)**

Reviewer checklist:
- [ ] `cargo build -p perima-desktop` exits 0.
- [ ] `cargo clippy -p perima-desktop -- -D warnings` exits 0.
- [ ] Commands are `#[tauri::command]` annotated.
- [ ] `AppState` holds data_dir + device_id.
- [ ] Config resolution duplicated from CLI (acceptable for v1).
- [ ] `tauri.conf.json` has correct frontendDist path.
- [ ] `just ci` green.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/desktop/ apps/desktop/dist/index.html justfile
git commit -m "$(cat <<'EOF'
feat(phase-2a): Tauri backend crate with scan/list/volumes commands

crates/desktop: Tauri v2 backend crate with three #[tauri::command]
functions (scan, list_files, list_volumes) via tauri-specta for
type-safe IPC. AppState holds resolved Config + DeviceId. Each
command opens its own DB connection (cheap under WAL). Config
resolution duplicated from CLI (shared crate deferred to avoid
premature abstraction).

tauri.conf.json: window config, dialog plugin, frontendDist at
../../apps/desktop/dist. Placeholder index.html until phase 2b
ships the real React build.

Refs: docs/superpowers/specs/2026-04-16-phase-2-tauri-shell-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Headless command tests

**Files:**
- Create: `crates/desktop/tests/commands_test.rs`

The implementer should write integration tests that invoke the Tauri commands directly (without spawning a window). Approach:

Since `tauri::test` utilities in v2 are still experimental, the simplest approach is to test the command functions directly as regular Rust functions by constructing a mock `tauri::State<AppState>`. If that's not possible due to Tauri's `State` being tied to the app lifecycle, test the underlying logic instead:

1. Create a tmpdir fixture with 3 files.
2. Construct `AppState` with `data_dir` pointing to the tmpdir.
3. Call the command functions with the constructed state.
4. Assert results.

If `tauri::State` cannot be constructed outside an app, fall back to testing the glue functions that the commands delegate to (extract the DB-opening + core-calling logic into testable non-Tauri functions, then test those).

3 tests:
- `scan_command_indexes_files` — scan a tmpdir → ScanResult with total=3, new=3.
- `list_files_returns_indexed` — after scan, list_files returns 3 records.
- `list_volumes_returns_one` — after scan, list_volumes returns ≥1 volume.

- [ ] **Step 1: Write tests (adapting to whatever approach works with tauri::State)**
- [ ] **Step 2: Run tests**
- [ ] **Step 3: Verify `just ci` green**

---

## Task 6: Update `justfile` + final sweep

**Files:**
- Modify: `justfile`

Add desktop-specific targets:

```just
build-desktop:
    cargo build -p perima-desktop

clippy-desktop:
    cargo clippy -p perima-desktop -- -D warnings
```

The `ci` target already runs `cargo clippy --workspace --all-targets` which covers `perima-desktop`. No change needed to `ci`. But verify this is true.

- [ ] **Step 1: Verify `just ci` includes desktop crate in all targets**
- [ ] **Step 2: WHY-comment check**

Run: `grep -rE '^\s*//\s*WHY:' crates/desktop/ | wc -l`
Expected: ≥ 1 (per-command DB connection rationale).

- [ ] **Step 3: Clean build**

Run: `cargo clean && just ci`

- [ ] **Step 4: Dispatch reviewer (checkpoint #2)**

Reviewer checklist:
- [ ] All P2a exit criteria met (P2a-1 through P2a-6).
- [ ] Headless tests pass.
- [ ] WHY comments present.
- [ ] `just ci` green.

- [ ] **Step 5: Commit tests**

```bash
git add crates/desktop/tests/ justfile Cargo.lock
git commit -m "$(cat <<'EOF'
test(phase-2a): headless Tauri command tests

Headless tests for scan/list_files/list_volumes commands against
tmpdir fixtures. Tests construct AppState directly and invoke
command logic without spawning a Tauri window.

Refs: docs/superpowers/specs/2026-04-16-phase-2-tauri-shell-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 6: Push + wait CI green**

```bash
git push origin main
gh run watch --exit-status "$(gh run list --workflow=ci.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
```

Note: Do NOT tag yet — phase 2 completes after 2b. No `phase-2a-complete` tag (unlike 1a/1b/1c which were sub-phases of a single meta-plan phase; 2a/2b are just plan splits within one phase).

---

## Self-review

**Spec coverage (2a exit criteria):**
- P2a-1: `cargo build -p perima-desktop` → Tasks 1-4 ✓
- P2a-2: Headless command tests → Task 5 ✓
- P2a-3: `cargo clippy -p perima-desktop -- -D warnings` → Task 6 ✓
- P2a-4: `just ci` green → Task 6 ✓
- P2a-5: `tauri.conf.json` present → Task 1 ✓
- P2a-6: specta version resolved → Task 1 (pins at rc.24) ✓

**Placeholder scan:** Tasks 2-5 describe requirements rather than verbatim code because Tauri v2's API surface (especially `tauri::test`, `tauri::State` construction, and specta codegen) has enough churn that prescribing exact code would produce compile errors. The requirements are specific enough for a skilled Rust developer to implement. This is the same approach used successfully in phase 1c Tasks 2-4.

**Type consistency:** `ScanResult` matches between commands.rs definition and test assertions. `AppState` matches between state.rs, lib.rs, and commands.rs. `FileLocationRecord` and `VolumeRecord` are re-used from `perima-core`.

**Commit discipline:** Two reviewer-gated commits matching the checkpoint pattern.
