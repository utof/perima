# perima meta-plan — phase roadmap toward v1

**Status:** revised after reviewer pass #1; amended 2026-04-17
**Author:** Claude Opus 4.6 (autonomous mode)
**Date:** 2026-04-15
**Scope:** Phase-level sequencing for perima v1. Each phase below gets
its own brainstorm → spec → plan → execute cycle when it becomes the
current phase. This document is the *skeleton*, not the plan for
every phase.

---

## Progress snapshot (as of 2026-04-17)

| Phase | Status | Tags |
|---|---|---|
| 0 — Scaffold & gates | ✅ done | `phase-0-complete` |
| 1a/1b/1c — Indexing core + CLI | ✅ done | `phase-1a-complete`, `phase-1b-complete`, `phase-1-complete` |
| 2 — Tauri shell | ✅ done | `phase-2-complete` |
| 3 — Watching + incremental updates | ✅ done | `v0.3.0` through `v0.3.2` |
| 4 — Thumbnails + media metadata | ✅ done | `v0.4.0` through `v0.4.3` |
| 5a — Tags | ✅ done | `v0.5.0`, `v0.5.1` |
| 5b — Search + filter | ✅ done | `v0.6.0`, `v0.6.1` |
| 5b follow-ups (v0.6.2/v0.6.3) | in progress | (post-review cleanup; issue #25 live faceted search + issue #22 FTS5 stale-rename) |
| 6 — Local HTTP API | pending | — |
| 7 — Docs site (Starlight) | pending | — |
| 8 — Plugin API | pending | — |
| 9 — v1 hardening | pending | — |

Update this table when a phase ships. Source of truth for detail is
CHANGELOG.md + the per-phase specs under `docs/superpowers/specs/`.

---

## MVP target (v1)

From `2026-04-09-multiplatform-rust-perima.md`: scan + BLAKE3 hashing,
SQLite storage with CRDT-ready schema, cross-volume path tracking,
thumbnails, tagging, search, file watching, local HTTP API. Desktop
only for v1. No CRDT sync, no mobile, no WASM plugins, no AI tagging,
no Obsidian plugin in v1.

User-set constraint: v1 begins with "indexing engine + a UI"; therefore
phase 1 (CLI core) and phase 2 (Tauri shell) together form the earliest
demoable form. Subsequent phases add capability on the same base.

---

## Ordering principle

1. **Foundation before features.** Workspace, lints, CI gates (phase 0)
   before any domain code — so every subsequent phase ships against
   the same quality bar.
2. **Prove the core in isolation.** CLI (phase 1) verifies indexing
   correctness with zero UI ambiguity.
3. **Shell over known-good core.** Tauri (phase 2) wraps an already-
   working engine; UI can be ugly, the data layer cannot.
4. **Incremental capability.** Each subsequent phase adds one
   independently testable concern.
5. **Hardening last.** Perf, recovery, and drive-loss edge cases
   (phase 9) come after functional completeness.
6. **Core schema freezes at end of phase 1.** Phases 4+ may add new
   tables (tags, media metadata, etc.) but must not alter `files`,
   `volumes`, `file_locations`, or `volume_mounts` shape. This keeps
   phase 6 (HTTP API) movable and prevents late schema churn.

---

## Phase list

### Phase 0 — Scaffold & gates

- Virtual workspace at root. Empty `crates/{core,db,fs,hash,cli}`.
- Workspace `Cargo.toml` with pinned deps: `rusqlite` (bundled),
  `blake3`, `walkdir`, `notify`, `notify-debouncer-full`, `uuid` (v7),
  `tracing`, `tracing-subscriber`, `thiserror`, `anyhow`, `serde`,
  `sysinfo`, `directories`, `unicode-normalization`, `path-slash`,
  `dunce`, `file-id`, `refinery`. Pin `tauri-specta` + `tauri` at
  workspace level even though they're used in phase 2.
- **Workspace lints (exact flags, enforced in CI):**
  - `#![deny(rustdoc::broken_intra_doc_links)]`
  - `#![deny(rustdoc::private_intra_doc_links)]`
  - `#![warn(missing_docs)]`
  - `cargo clippy --workspace --all-targets -- -D warnings
    -W clippy::pedantic -W clippy::cognitive_complexity
    -W clippy::too_many_lines -W clippy::excessive_nesting`
  - Thresholds: cyclomatic <10, cognitive <15.
- `justfile` targets: `test`, `clippy`, `doctest`, `mdbook-test`
  (Rust doctests on core), `verify` (kani on-demand), `ci`
  (clippy + test + mdbook-test + docs-coverage), `docs-coverage`.
- **Pre-commit hook (local):** `just ci` (clippy + test + mdbook-test
  + docs-coverage). `kani` never pre-commit.
- **CI:** mirror of pre-commit on every push. **Nightly job:** kani.
- `// WHY:` comment convention stated in `CLAUDE.md` (already present);
  `clippy` not blocked by their absence — they are reviewer-enforced.
- `.gitignore` preserved (`**/*.md` with narrow whitelist: `CHANGELOG.md`,
  `CLAUDE.md`, `docs/superpowers/**/*.md`, `docs/routines/**/*.md`,
  `.claude/**/*.md`).
- ~~`DECISIONS.md` stub (gitignored).~~ **2026-04-17 revision:** a flat
  DECISIONS.md was created at phase 0 but became stale after 12 entries.
  Per deep-research verdict, we rely on CLAUDE.md (living rules), phase
  specs (intent snapshots), and commit WHY blocks (ground truth) instead.
  No separate decisions log.
- **Exit (autonomously verifiable):** `just ci` green on empty crates;
  CI pipeline green; `cargo clippy` produces zero warnings; doc-coverage
  script exits 0.

### Phase 1 — Indexing core + foundation concerns (CLI-visible)

Internally split into three plans (1a/1b/1c) per 2026-04-16 spec
reviewer — scope of phase 1 exceeds one sensible implementation plan.
Each sub-phase owns a spec + plan + reviewer pass + commits.

**Tagging note (2026-04-17 revision):** phases 0-2 used
`phase-N-complete` milestone tags as a historical convention. From v0.3.0
onward the project adopted **semver tags** (`v0.N.x`) with release-plz
auto-tagging on `chore(release):` commits. The `phase-N-complete` tags
remain in history as historical markers only; do not create new ones.

- **1a** — core types, trait ports (hash + scanner + file + volume
  repositories), BLAKE3 adapter, filesystem walker, path
  normalization, CLI scaffold with a DB-less `scan --dry-run` that
  only walks + hashes + prints. Config + logging + Ctrl-C handler +
  panic hook.
- **1b** — rusqlite adapter, refinery migrations, WAL + `synchronous
  = NORMAL` pragmas, `FileRepository` + `VolumeRepository` real
  implementations, `scan` persists, `perima ls` reads.
- **1c** — volume detection, `volume_mounts`, per-drive
  `.perima/manifest.db` creation, `perima volumes` command,
  integration tests, property tests. (Shipped pre-semver as the
  historical `phase-1-complete` tag; semver equivalent is v0.2.x.)

- **Domain types** in `crates/core`: `BlakeHash`, `FileSize`,
  `MediaPath`, `VolumeId`, `DeviceId`, `DiscoveredFile`, `HashedFile`.
  **`Asset<State>` type-state is deferred to phase 3** (no state
  transitions exist until file-watching introduces them;
  designing the state machine without a consumer is blind).
- **Trait ports** in `core`: `HashService`, `Scanner`,
  `FileRepository`, `VolumeRepository`. **`EventBus` is deferred to
  phase 3** (only consumer is the watcher; YAGNI until then).
- **`crates/hash`:** BLAKE3 via `blake3` crate; two-phase strategy
  (first-64KB + full).
- **`crates/fs`:** `walkdir` scanner; NFC via `unicode-normalization`;
  forward-slash via `path-slash`; UNC stripping via `dunce`; volume
  detection via `sysinfo`; drive-identifier priority chain
  (GPT GUID → fs UUID → label).
- **`crates/db`:** rusqlite adapter; **CRDT-compliant schema** per
  `CLAUDE.md` rules — UUIDv7 PKs, `updated_at` + `device_id` on every
  mutable row, soft deletes, **no UNIQUE on mutable columns** (the
  research doc's `UNIQUE(volume_id, relative_path)` is rejected;
  uniqueness enforced in application code via index lookup before
  insert), **no FK cascades** (referential integrity in app layer);
  migrations via `refinery`. Schema covers: `files`, `volumes`,
  `file_locations`, `volume_mounts`. **This schema is frozen at
  phase 1 exit.**
- **`crates/cli`:** `perima scan <path>`, `perima ls`, `perima volumes`.
- **Per-drive manifest write:** `.perima/manifest.db` at each volume
  root is created and populated during scan. Recovery logic deferred
  to phase 9; creation path is phase 1.
- **Cross-cutting concerns landed here, not deferred:**
  - **Config:** `directories` crate for platform paths (XDG, AppData,
    Application Support). Main DB at the platform data dir.
  - **Logging:** `tracing` + `tracing-subscriber` JSON output to
    stderr; `RUST_LOG` env respected.
  - **Error taxonomy:** `CoreError` enum in `core` with
    `NotFound`, `Duplicate`, `IoError`, `HashMismatch`, `DbError`
    variants via `thiserror`. `cli` converts to `anyhow` + user
    messages.
  - **Secrets:** none in phase 1 (API key lives in phase 6 with
    OS-keychain integration specified there).
- **Tests:** unit per crate; integration hitting real SQLite temp DBs;
  `proptest` for hash determinism + path round-trip + NFC
  idempotence; `insta` snapshots for CLI output; manifest round-trip
  test.
- **Exit (autonomously verifiable):**
  - `perima scan <tmpfixture>` populates main DB with N expected rows.
  - `perima ls` stdout matches insta snapshot.
  - `.perima/manifest.db` exists at fixture root with matching rows.
  - `just ci` green. Proptest runs pass at default cases count.
  - Zero `clippy` warnings. `missing_docs` satisfied on all public items.

### Phase 2 — Tauri shell (thin UI)

- `apps/desktop` (Vite + React + Tailwind + Bun).
- `crates/desktop` (Tauri backend) wiring core traits.
- `tauri-specta` for type-safe IPC (version pinned in phase 0).
- UI: single page, flat table of files (path, size, hash, volume,
  last_seen). Scan trigger + folder picker.
- **Tests:** `vitest` + jsdom for React components; **headless Tauri
  integration test** driving IPC via `tauri::test` — asserts that
  invoking `scan` updates the exposed store and that `list_files`
  returns the expected snapshot. No human-in-loop verification.
- **Exit (autonomously verifiable):** headless test scans a fixture,
  asserts IPC response payload matches insta snapshot; React
  component snapshot test renders table without crashing; zero
  clippy/lint warnings; TSDoc on every exported symbol.

### Phase 3 — Watching + incremental updates

- `crates/fs` extended with `notify` + `notify-debouncer-full`.
- Rename stitching via `file-id`.
- Core reacts to events via `EventBus` trait (defined phase 1),
  updates `file_locations.status` (active/missing/moved).
- CLI gains `perima watch <path>` (simpler surface, tested first).
- UI (Tauri) subscribes to Tauri events emitted from the same
  `EventBus` adapter; reflects changes live.
- **Exit (autonomously verifiable):** integration test with a tmpfs
  fixture performs 50 mutations (create/rename/delete); p95
  event-to-DB latency under 2s; status transitions match expected
  table; UI headless test observes Tauri events fired with matching
  payloads.

### Phase 4 — Thumbnails + media metadata (additive schema only)

- New `crates/media` with `image`, `kamadak-exif`, `nom-exif`.
- Thumbnail generation: on-demand + background queue (`tokio`
  task via core trait).
- EXIF/video metadata stored in a **new** table `file_metadata`
  (additive; does not alter `files`).
- UI: grid view with thumbnails (toggle between table/grid).
- **Exit (autonomously verifiable):** scan over a fixture produces
  thumbnail bytes that decode to expected dimensions via `image`;
  `file_metadata` rows present with expected EXIF values; component
  snapshot for grid view; schema diff confirms `files`/`volumes`/
  `file_locations`/`volume_mounts` unchanged from phase 1.

### Phase 5a — Tags

- New schema tables: `tags`, `file_tags` (CRDT-ready: UUIDv7,
  soft deletes, `device_id`, `updated_at`). Additive only.
- Core: tag service (add, remove, list, by-file, by-tag).
- UI: tag add/remove per file, tag list in sidebar.
- **Exit:** integration tests for tag CRUD; component snapshot for
  sidebar; schema diff confirms core tables unchanged.

### Phase 5b — Search + filter

- SQLite FTS5 virtual table over `files.path + file_metadata + tags`.
- Core: search service with query DSL (simple: `tag:foo kind:image
  free-text`).
- UI: search bar + tag filter sidebar.
- **Exit:** search integration tests with expected result sets;
  component snapshot for search UI; FTS5 table rebuild test.

### Phase 6 — Local HTTP API

- `crates/api` with axum bound to 127.0.0.1 on configurable port.
- **API-key auth:** generated first-run, stored in OS keychain via
  `keyring` crate (cross-platform: Secret Service/Keychain/Credential
  Manager); fallback to config dir with `0600` perms if keychain
  unavailable. **No plaintext logs.**
- CORS for `app://obsidian.md` + `http://localhost:*`.
- Endpoints: `/api/assets`, `/api/search`, `/api/tags`,
  `/api/volumes`, `/api/graph`.
- WebSocket for live updates (driven by same `EventBus`).
- OpenAPI doc emission via `utoipa`.
- **Exit:** integration tests hitting the server with `reqwest`;
  auth-enforcement tests; WS subscription test asserts events push;
  OpenAPI spec generated.

### Phase 7 — Docs site (Astro Starlight)

- `docs/` Astro Starlight project (Bun-managed).
- Diátaxis structure: tutorials, how-to, reference, explanation.
- `cargo doc` output mounted under `/api/rust/`.
- TypeDoc output mounted under `/api/ts/`.
- Starlight link-check in CI.
- Note: `mdbook test` on core doctests already runs from phase 0;
  this phase only ships the site.
- **Exit:** Starlight build green in CI; link-check passes;
  rustdoc + TypeDoc output present under `/api/*`.

### Phase 8 — Plugin API (Rust traits + Extism/WASM)

- `crates/plugin-api` with `AssetProcessor`, `MetadataExtractor`
  traits.
- Extism/wasmtime loader in `crates/core`.
- Reference WASM plugin (e.g., batch-rename) as smoke test.
- **Exit:** loading a sample `.wasm` runs correctly against real
  assets; sandbox enforcement test.

### Phase 9 — v1 hardening

- Per-drive manifest **recovery** (creation already in phase 1):
  OS-reinstall simulation, multi-drive match by GUID priority.
- Volume-loss / volume-reinsert edge cases.
- Performance: 100k-file scan benchmark. **Placeholder target
  (revisable at phase 9 brainstorm): cold ≤ 5 min, warm ≤ 30 s on
  SSD reference hardware.**
- `tracing` end-to-end audit (structured spans across crate
  boundaries).
- Error-message quality pass.
- **Exit:** perf target met; recovery integration test passes;
  clippy + tests + docs-coverage + Starlight link-check all green;
  v1 tag cut.

### Post-v1 (out of scope for this meta-plan)

CRDT sync, mobile (UniFFI + Expo), Obsidian TS plugin, AI tagging,
graph visualization. These get their own meta-plan after v1 ships.

---

## Cross-phase dependencies

- Phase 0 gates every later phase.
- Phase 2 depends on phase 1 (core stable before UI).
- Phase 3 depends on phase 1 (needs scanner + `EventBus` trait) and
  phase 2 (live UI). Watching CLI (`perima watch`) could ship in
  phase 1 if bandwidth allows — deferred only to keep phase 1 bounded.
- Phase 4 depends on phase 2 (UI surface).
- Phases 5a/5b depend on phase 4 (tags render on grid; search
  indexes metadata).
- Phase 6 depends on phase 5b (stable search surface in API).
  **Because the core schema froze at phase 1, phase 6 is not blocked
  by 4/5 schema drift.**
- Phase 7 depends on phase 1 (real code to document); best signal
  after phase 6.
- Phase 8 depends on phase 1 (trait ports stable).
- Phase 9 depends on every prior phase being complete.

## Per-phase execution loop (recap)

Per `CLAUDE.md`:
`brainstorming → spec → reviewer → plan → reviewer → execute via
subagents (each task: execute → tests → reviewer → commit) →
verification-before-completion → tag phase.`

## Risks flagged

- **UI correctness is unverifiable without visual review.**
  Mitigation (honest, consistent with autonomous-mode rule): UI phases
  (2, 4, 5a, 5b) ship with **headless IPC tests + component snapshot
  tests only**. Visual polish is an explicit **non-goal for v1**;
  tracked post-v1 via [issue #27 (three-pane layout)](https://github.com/utof/perima/issues/27)
  and its sub-issues (#28-#31, #33-#35). We do not stop for human
  review, and we do not pretend tests cover aesthetics.
- **Scope creep mid-phase.** Reviewer subagent checks each task
  against the phase spec, not the meta-plan.
- **Review-gate skipping.** Observed in v0.6.x: the cloud trigger
  shipped 4 feat commits with zero `fix(...)` review-fix commits, vs
  v0.5.1's 1:1 ratio. Mitigated 2026-04-17 via tightened rules in
  `.claude/commands/autonomous-continue.md` (Rule 3: every feat
  commit must be followed by two-stage review; "zero fix commits
  across a phase = red flag requiring justification in release body").
  Tracked as [issue #26](https://github.com/utof/perima/issues/26).
- **Phase-plan rot.** Re-read + revise this meta-plan before starting
  each phase. Originally called for full regeneration after phase 5b;
  2026-04-17 decision is to amend in place (progress snapshot,
  decision-log walkback, risk updates). Regenerate only if post-v1
  scope clarifies substantially.
- **kani cost.** On-demand only, never per-commit.
- **cr-sqlite maintenance risk.** Research doc flags last release
  2025-01. v1 does not depend on cr-sqlite. Post-v1 sync phase ships
  with a fallback: a hand-rolled LWW merge over `updated_at` + HLC if
  cr-sqlite is dead by then. The CRDT-ready schema supports either
  path; this is captured here so the meta-plan author doesn't default
  to cr-sqlite.
- **tauri-specta / tauri API churn.** Versions pinned at workspace
  level from phase 0; upgrades are explicit tasks, never drifted.
