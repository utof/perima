# perima — cross-platform media asset manager

Rust hexagonal core + Tauri 2 / React desktop. Mobile (Expo + UniFFI) and
Obsidian (axum HTTP API) are downstream shells. Full rationale in
`2026-04-09-multiplatform-rust-perima.md` and
`2026-04-09-documentation-research.md` — read those before decisions.

## Autonomous mode

Developed without human confirmation. Never stop to ask; human speech is
steering, not interruption. Chip one task at a time.

## Phase loop (repeat per phase/step)

1. `superpowers:brainstorming` — intent, requirements, design.
2. Spec: write in working tree → reviewer subagent → revise → repeat
   until clean → **one** `docs(specs):` commit.
3. `superpowers:writing-plans` — same working-tree loop → **one**
   `docs(plans):` commit.
4. `superpowers:executing-plans` + `superpowers:subagent-driven-development`.
5. Per task: reviewer subagent (`superpowers:requesting-code-review`)
   checks code vs spec + plan.
6. `superpowers:verification-before-completion` before claiming done.
7. `superpowers:test-driven-development` throughout.

If you have just been compacted, load the skills appropriate for the current task. The most common and important ones: mid-execution - subagent-dr-dev. Pre: brainstorm.

Correctness rides on tests (no human e2e feedback): unit + integration
always, e2e where feasible. Spec/plan drafts stay uncommitted during
review — collapses N revision commits into one clean commit and keeps
main from snapshotting half-reviewed intent as "current state".

## Workspace layout (target)

Flat `crates/` layout, virtual workspace manifest at root. `crates/core`
= domain + trait ports, zero framework deps. `crates/{db,fs,hash}` =
adapters. `crates/{desktop,api,cli,ffi}` wire adapters to core.
`apps/desktop` = React frontend (Vite + Tailwind). Build automation in
`justfile`; reintroduce `crates/xtask` only when shell scripts stop scaling.

## Tooling
- JS/TS: **always `bun`** unless incompatible.
- Rust: `cargo`, `cargo clippy`, `cargo test`, `cargo doc`. `rusqlite`
  (bundled), not sqlx. `thiserror` in libs, `anyhow` in apps.
- `justfile` at root (Rust `xtask` deferred).
- **`codebase-memory-mcp`** — perima is indexed (project name
  `media-vboxuser-G-samsung1-0utoffiles-code-perima`). Tools:
  `mcp__codebase-memory-mcp__{query_graph, trace_call_path, search_code,
  search_graph, get_architecture, index_status}`. Reach for it on
  "who calls X?", "what breaks if I rename Y?", "crates depending on
  Z?", "find all Tauri command handlers". Skip for raw text in
  docs/comments or known file paths. **Dispatch subagents with the MCP
  note** — they inherit the tools; include "`mcp__codebase-memory-mcp__*`
  available; prefer over grep for structural queries" in their prompt.
- LSP rust-analyzer — `ToolSearch select:LSP`.
  Rust-only semantic symbol queries; prefer over grep for Rust names.
  Grep last resort
- In doubt of api and implementations, use context7, deepwiki, and web search, to find latest docs. Things include: crates, ts/react libraries, best practices, test libraries, e2e, DBs, react practices and anti-patterns.

## Test stack (pinned, don't re-litigate)
- Rust: **`cargo nextest run`** + `insta` (snapshots) + `proptest` (hash
  determinism, path round-trip). Integration tests hit real SQLite.
  Doctests still go through `cargo test --doc` (nextest doesn't run them).
- **NEVER use `cargo test` for non-doc tests** (banned repo-wide via
  `scripts/no-cargo-test.sh`, enforced in `just ci` + lefthook pre-push).
  WHY: SQLite has a lock-order inversion in `unixClose` (GLOBAL→PER-INODE)
  vs `unixLock` from `sqlite3WalClose` (PER-INODE→GLOBAL) — concurrent
  Connection drops on the same DB file deadlock. `cargo test` reproduces
  it ~20% of runs (intermittent silent hang); `cargo nextest run`'s
  process-per-test isolation eliminates it. See `RESEARCH-sqlite-deadlock.md`
  + GH #121. Reproduced 2026-04-23 with two gdb backtraces.
- TS: `bun test` units; `vitest` + jsdom for React; Playwright only when
  a phase ships UI worth e2e-ing.

## Test timing baselines (cold compile, `-j 2`, as of 532ac2a / 2026-04-22)
Set subagent `timeout NNN` wrappers at **~2× expected**, not blanket 600s.
A too-loose timeout wastes 5-10 min per failed attempt.

- `cargo nextest run -p perima-db --test-threads 2`: **~15s** (slowest single
  test `fts_consistent_under_tag_churn` ~14s — proptest)
- `cargo nextest run -p perima-app --test-threads 2`: **~5-10s** (23 tests)
- `cargo nextest run -p perima-cli --test-threads 2`: **<10s**
- `cargo nextest run --workspace --exclude perima-desktop -j 2`: **<60s** clean
- `cargo nextest run -p perima-desktop`: **120-300s** (Tauri compile dominates)
- `cargo clippy --workspace --exclude perima-desktop -j 2 -- -D warnings`: **30-90s**

Nextest 0.9.133 (latest as of 2026-04-22) lacks a `--slow-timeout` CLI
flag, but honors the equivalent config key. `.config/nextest.toml`
committed at repo root with `[profile.default] slow-timeout = { period
= "60s", terminate-after = 2 }` — any test past 120s self-aborts so
outer `timeout NNN` wrappers don't kill the whole suite on one hang.

## Long-running commands
- Wrap workspace-wide `cargo {nextest,build,run}` in `timeout 600`. Trip = deadlock signal, NOT retry trigger.
- **`cargo nextest run` is mandatory** for non-doc tests (see Test stack above + `scripts/no-cargo-test.sh`). Hung test self-terminates via `.config/nextest.toml` slow-timeout; suite continues. Doctests still need `cargo test --doc`.
- Bash output silent >3min + `ps` shows 0% CPU with all threads on `futex_do_wait`/`ep_poll` = deadlock. Kill, `gdb -p <pid> -batch -ex "thread apply all bt 20"` for backtrace, fix root cause.
- Include this rule verbatim in every subagent dispatch that runs tests — they read CLAUDE.md but forget rules under load.

## Doc discipline (strict, enforced in CI)
- Rust: `#![deny(rustdoc::broken_intra_doc_links)]` +
  `#![warn(missing_docs)]` workspace-wide.
- Clippy: `-D warnings` + `clippy::cognitive_complexity`,
  `clippy::too_many_lines`, `clippy::excessive_nesting`. Cyclomatic <10,
  cognitive <15.
- `kani` on curated `#[kani::proof]` invariants (hashing, path norm);
  `just verify` + nightly CI only, never per-commit.
- TS: `eslint-plugin-tsdoc`, TypeDoc `--validation.notDocumented`.
- Every non-obvious decision gets a `// WHY:` comment.
- Architectural decisions: CLAUDE.md (rules) + phase specs (intent) +
  commit WHY blocks (ground truth). No separate DECISIONS.md.
- Unified doc site: Astro Starlight (Tauri-aligned, MDX polyglot).
  Rust doctests via `mdbook test`. `cargo doc` + TypeDoc feed Starlight
  in a later phase.

## Schema rules (CRDT-ready from day one)
UUIDv7 PKs, `updated_at` + `device_id` on every mutable row, soft
deletes, no UNIQUE on mutable columns, no FK cascades. Every FTS /
search aggregation MUST filter `deleted_at IS NULL` on every joined
table (link + entity, e.g. both `file_tags` AND `tags`); every
trigger touching a soft-deletable table MUST handle both transition
directions (soft-delete AND restore). V007→V008 bug class.

## Git
- All work on `main`. No branches, no worktrees.
- **Releases = semver tags** (`v0.N.x`). No fixed v1.0.0. "Phase" is
  internal vocabulary only — never in tags, commits, or CHANGELOG.
- **Conventional Commits** with component scopes (`core`, `db`, `fs`,
  `hash`, `cli`, `desktop`, `ci`, `deps`, `docs`, `release`).
  `release-plz` handles bumps + CHANGELOG from v0.4.0 on.
- Commit order: execute → tests green → reviewer green → commit.
- `**/*.md` gitignored except `CHANGELOG.md` + `docs/superpowers/**/*.md`
  (tracked so cloud agents can continue across sessions).
- New commits only; no amend, no `--no-verify`.

## Model defaults
Opus 4.6 for planning/review & complex coding. Sonnet for bulk/simple execution; Haiku 4.5 for trivial lookups. **Never include `Co-Authored-By:` in commits.**

## Architecture conventions (evolving — see arch-audit umbrella spec)

These sections are incrementally populated as Batches B-J land. Cross-ref: AGENTS.md for library/framework pins (different scope).

### Crate topology (target post-Batch-B)

- `crates/core` — domain types + trait ports, zero framework deps.
- `crates/app` — UseCase structs with `Arc<dyn Port>` fields (Batch B).
- `crates/{db,fs,hash,media}` — adapters.
- `crates/{cli,desktop}` + future `api` + `ffi` — shell binaries, wire-up only.

### Port traits

- Repositories take `&self`, not `&mut self` (A2, landed `081c694` + `333c4b6`).
- Interior mutability lives in the adapter.
- No generic parameters on service structs; use `Arc<dyn Port>` fields.

### UseCase pattern (Batch B landed 2026-04-21)

- Concrete `struct XxUseCase` with `Arc<dyn Port>` fields; zero generics.
- Single `pub async fn execute(&self, cmd: XxCommand) -> Result<XxOutput, CoreError>`.
- One UseCase per user-visible workflow (not per entity). Current set: `Scan`, `Search`, `Tag`, `Volume`, `Metadata`.
- `AppContainer` is `#[derive(Clone)]`; fields are `Arc<XxUseCase>` + `Arc<dyn EventBus>` (named `events`).
- `AppContainer::new(deps: AppDeps, handlers: Vec<Arc<dyn EventBus>>)` is the ONLY `CompositeEventBus::new` construction site in production code. Adding a second is a regression.
- Shells build one container per process: desktop via Tauri `.setup(...)`, CLI via `build_container(db_path, extra_handlers)`. Shell-specific event handlers (Tauri emitter, db writeback) enter as `extra_handlers`.
- `LogEventHandler` lives in `perima_app::telemetry`. `DbEventHandler` stays shell-local in desktop (coupled to `SqliteFileRepository`).
- `CoreError::Internal(String)` wraps orchestration-layer errors until Batch D's typed `CoreError` discriminated union lands.
- Watch (filesystem watcher) is shell-local by design (`crates/cli/src/cmd/watch.rs` + `crates/desktop/src/commands.rs::{start,stop,is}_watch`); both consume `state.container.events` for emission. Tracked in #120.
- New write paths do NOT populate `hlc` in Batch B — deferred to Batch C's writer actor where the single SQL entry point can generate the HLC once per command.
- Post-Batch-B narrowing of shell-side residuals (volume filter arg, `TagOutput::Attached` widening, `detach by id`, `VolumeCommand::FindOrCreate`, `write_manifest` home) tracked on #119.

### Connection model (Batch C landed 2026-04-22)

- One `SqliteWriter` actor per process, on a `std::thread::spawn` OS thread. Owns the sole writable `rusqlite::Connection` for its lifetime. Writer is an OS thread, NOT a second tokio runtime — one-runtime-per-binary rule preserved. (Shells currently hold auxiliary writer handles for pre-container construction ordering — CLI 3 sites, desktop 2 sites; tracked in #125 for consolidation, not a blocker.)
- Writes: adapter methods build `WriteCmd::*` variants with `flume::bounded(1)` reply channels, `writer.send(cmd)` (sync), `reply_rx.recv()` (sync). Port traits stay sync; UseCase `execute` methods are async for forward-compat but hold no `.await` on the repo call.
- One HLC value per `WriteCmd` (== one user-visible logical event), stamped on every HLC-bearing row the command writes. Two WriteCmds in the same millisecond get distinct HLCs via the process-global counter in `HLC_STATE`.
- Reads: `r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>`, size 4, `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX`, `with_init` pragmas (`busy_timeout=5000`, `temp_store=MEMORY`, `mmap_size=268435456`, `query_only=1`).
- Migrations: Refinery, run ONCE at startup inside `SqliteWriter::start`, synchronously, on the connection that becomes the writer. No per-command migration sniff.
- Domain events publish AFTER `COMMIT` from inside the writer. Handlers return `Result<(Out, Vec<FileEvent>), CoreError>`; writer fans out via `Arc<dyn EventBus>` passed from `AppContainer::new`. Rollback → zero events. Failed `emit()` logs via `tracing::warn!`, does not abort other handlers. Batch C handlers currently return empty event vecs — scaffolding in place for Batch E.
- Writer receives `Arc<dyn EventBus>` from `AppContainer::new`. `crates/db` does NOT construct `CompositeEventBus`. Preserves Batch B's single-construction-site invariant.
- No `open_and_migrate` callsite in production code under `crates/{cli,desktop,app}/src/**` (non-`#[cfg(test)]`) outside `SqliteWriter::start`. The function itself stays in `crates/db/src/connection.rs` as the writer's migration primitive.
- `SqliteFileRepository` and `SqliteSearchRepository` (and the other three adapters) are `{ writer: Sender<WriteCmd>, reads: ReadPool }` only — no `Inner::Legacy` / `new_legacy`. Re-introducing that enum arm or constructor is a regression.
- Port traits in `crates/core/src/ports/*.rs` unchanged — sync `&self` methods, `Send + Sync` adapter bounds.
- **Why `r2d2_sqlite` over `deadpool-sqlite`:** port traits are sync; `deadpool-sqlite`'s async `interact` API would force async ports (out-of-batch refactor). Library-audit §Q1 deferred the pool pick; Batch C resolved to `r2d2_sqlite 0.32`.
- **Why `flume` for reply channels:** `tokio::sync::oneshot::Receiver::blocking_recv` panics inside a tokio runtime context; `flume::Receiver::recv` is runtime-agnostic. Single dep covers command channel + reply channels.
- FTS5 proptests (`fts_consistent_under_tag_churn`, `fts_matches_ground_truth_under_soft_delete_churn`) are capped at 64 / 32 cases respectively. Raising the cap without an FTS-oracle rewrite is a regression (#124).

### IPC boundary contract (Batch D landed 2026-04-22)

- Tauri commands return `Result<T, CoreError>`. NO handler returns `Result<T, String>`. NO handler calls `.map_err(|e| e.to_string())`.
- `CoreError` lives in `crates/core::errors`. It derives `thiserror::Error + Serialize + Clone` always, and `specta::Type` behind the `specta` feature. `#[serde(tag = "kind", content = "data")]` produces a TypeScript discriminated union.
- `CoreError::Io { kind, message }` lowers `std::io::Error::kind()` (e.g. `"NotFound"`, `"PermissionDenied"`) to a string + the display message. Lossy by design — `std::io::Error` is `!Serialize`. Capture both fields, never just the message. Construct via `CoreError::from(io_err)` (single `From<io::Error>` impl in `crates/core/src/errors.rs`); `?`-propagation works through it.
- `crates/desktop` enables `perima-core/specta` and `perima-app/specta`. `crates/cli` does NOT — `crates/core` and `crates/app` stay framework-dep-free in CLI builds (verified: `cargo tree -i specta -p perima` is empty).
- `bindings.ts` is committed to git at `apps/desktop/src/bindings.ts`. It is generated by `tauri-specta` when `crates/desktop` is built with `--features specta-export`. CI runs `just bindings` (= `cargo build -p perima-desktop --features specta-export && git diff --exit-code apps/desktop/src/bindings.ts`) — drift fails the job. Local pre-push verification: `just bindings`.
- Production release builds do NOT enable `specta-export` (no surprise fs writes into the frontend source tree).
- `apps/desktop/src/types.ts` does NOT exist. Re-introducing it is a regression. All TS types come from `./bindings`.
- `apps/desktop/src/api.ts::fromInvoke` returns `ResultAsync<T, CoreError>` via `ResultAsync.fromPromise(invoke<T>(...), parseCoreError)`. `parseCoreError` falls back to `{ kind: "Internal", data: <stringified> }` for non-CoreError rejections.
- Re-introducing per-shell wire-type structs that mirror core types 1:1 is a regression — derive `specta::Type` on the core type instead. The exception is shell-private composite payloads that have no clean core analogue: `crates/desktop/src/payloads.rs` retains `FileWithMetadataPayload` (deliberate 17-field flat shape for grid binding) and `FileWithTagsPayload` (`#[serde(flatten)]` composition). Adding more shell-side composites is allowed only when the same 1:1-vs-composite decision is documented inline.
- The 10 specta-derived core types: `BlakeHash`, `FileSize`, `MediaPath`, `VolumeId`, `FileLocationRecord`, `VolumeRecord` (in `crates/core/src/types.rs`); `MediaMetadata`, `Tag`, `SearchHit`, `FileEvent` (one per file, same crate). `ScanReport` + `ScanReportEntry` from `crates/app/src/scan.rs` are also bindings-emitted (with `#[serde(skip)]` on shell-internal routing fields). Adding a new domain type that crosses the IPC boundary requires `#[cfg_attr(feature = "specta", derive(specta::Type))]` + `Serialize` on the type itself, NOT a wire-type mirror.
- `FileEvent` uses `#[serde(tag = "type")]` INLINE (no content key) to stay byte-compatible with the pre-Batch-D `FileEventPayload` channel contract; CoreError uses `#[serde(tag = "kind", content = "data")]`. Different shapes by design — CoreError is a Result error type, FileEvent is a v1-frozen channel payload.
- Adding a new `CoreError` variant: append to the enum with `#[error("…")]` + verify `bindings.ts` regenerates + extend frontend `parseCoreError`'s `KNOWN_KINDS` set + extend the `switch (err.kind)` block in `apps/desktop/src/components/StatusBar.tsx` (TypeScript exhaustiveness `never` default makes this a compile-error if missed).
- At least one frontend component (`StatusBar.tsx`) pattern-matches on `err.kind` with a TypeScript-exhaustive `never` default, proving the typed error flows end-to-end. Other components stringify via `apps/desktop/src/lib/coreError.ts::coreErrorMessage` until Batch H rewrites the toast/banner UX.

### Event bus (Batch E landed 2026-04-23)

- `EventBus` trait lives in `crates/core::events` as the publisher port. Single method: `fn emit(&self, event: &AppEvent) -> Result<(), CoreError>`. Sync — writer thread (Batch C) is not on a tokio runtime.
- `AppEvent` enum (in `crates/core::events`) has 3 variants:
  - `File(FileEvent)` — wraps the existing filesystem-watcher event.
  - `ScanCompleted { volume, files_seen, files_new, duration_ms }` — published by `ScanUseCase::execute` after a successful scan.
  - `IndexInvalidated { reason: InvalidationReason }` — published by the writer actor after every `WriteCmd` that changes a query-relevant index. Reasons: `TagsChanged`, `FilesChanged`, `MetadataChanged`, `SearchIndexRebuilt`.
- `AppEvent` derives `Serialize + Clone` always; `specta::Type` behind the `specta` feature. `#[serde(tag = "kind", content = "data")]` produces the TypeScript discriminated union the frontend pattern-matches on.
- The concrete `Bus` lives in `crates/app::bus` and is the SOLE production impl of `EventBus`. Built once in `AppContainer::new`. Uses `async_broadcast::Sender<AppEvent>` (capacity 256, backpressure mode = default — `set_overflow(false)`). Re-introducing `CompositeEventBus` is a regression.
- Handlers implement `EventHandler` (in `crates/app::events`) — `async fn handle(&mut self, event: AppEvent)`. Three production impls: `LogEventHandler` (`crates/app::telemetry`), `DbEventHandler` (shell-local: CLI's `crates/cli/src/cmd/watch.rs`, desktop's `crates/desktop/src/commands.rs`), `TauriEventHandler` (`crates/desktop/src/events.rs`).
- Handler tasks are spawned by `AppContainer::new` from a `Vec<Box<dyn EventHandler>>` parameter. Each task owns its own `Receiver` and runs a `recv_loop` that handles `Overflowed` (lag → log warn + continue) and `Closed` (shutdown → exit). Panics inside `handle` are caught + logged; the loop continues.
- `Bus::emit` is sync `try_broadcast`. On `Full` (slow subscriber): `tracing::warn!` + return Ok — the writer is best-effort. Capacity 256 + fast-handler invariant means Full should not fire under normal load.
- `tokio::sync::broadcast` is BANNED. It silently drops on lag. Use `async_broadcast::broadcast` (default = backpressure mode).
- Tauri channel: ONE channel `"app-event"` carries `AppEvent`. The previous `"file-event"` channel is gone. Frontend `apps/desktop/src/api.ts::subscribeToAppEvents` is the single subscription point.
- The writer's per-`WriteCmd` handler returns `Result<(Out, Vec<AppEvent>), CoreError>`. Empty Vec = no event. Each `IndexInvalidated::*` reason maps to a category of writes per the spec — adding a new write type that affects a query requires populating the Vec.
- `NoopBus` (`crates/db::test_utils::noop_bus`) is the test stub. Used by writer tests that don't care about events. `MockEventBus` (in `crates/fs/src/watcher.rs::tests`) collects events for assertion. Both impl the `EventBus` trait.
- Adding a new `AppEvent` variant: append to enum + add `#[serde(...)]` tag if needed + verify `bindings.ts` regenerates + extend frontend `App.tsx` switch on `event.kind` (no exhaustiveness check today — a future Batch H refactor may add one).
- Adding a new `EventHandler`: impl the trait + add to the shell's handler `Vec` passed into `AppContainer::new`.

### Frontend state (Batch H will fill in)

Stub: three-layer split. TanStack Query v5 for server-state via `bindings.ts` commands. Zustand 5 for global UI state. `useState` for component-local only. TanStack Router with `createHashHistory`. Cache invalidation driven by `AppEvent` listener, never by Rust-side `invalidate_query!`-style macros. React Compiler 1.0 is on (L2 landed); do not write manual `useMemo`/`useCallback`. Details in Batch H spec.

### Observability (Batch I will fill in)

Stub: `tracing` + `tracing-subscriber` JSON in release; `#[tracing::instrument]` on every `UseCase::execute`; `perima --debug-report` dumps last N traced events. Details in Batch I spec.

### Tokio runtime policy

- One tokio runtime per binary. Never spawn a second.
- Desktop: Tauri owns the runtime.
- CLI: `#[tokio::main]`.
- Tests: `#[tokio::test]` or single-threaded runtime per test.
- Writer actor (Batch C) is a dedicated OS thread, NOT a second runtime.

### Error handling

- `thiserror` in lib crates (`crates/core`, `db`, `fs`, `hash`, `media`, future `app`).
- `miette` with `fancy` feature in binary crates ONLY (`crates/cli`, `crates/desktop`) — landed L7 (`938eccc`).
- `CoreError` lives in `crates/core::errors`. It does NOT derive `miette::Diagnostic` — binary-UX-only concern that stays at the binary boundary. Batch D will add `#[derive(Serialize, specta::Type)]` + `#[serde(tag = "kind", content = "data")]` behind the `specta` feature.
- `anyhow` is rejected in `crates/cli` + `crates/desktop` declared deps (L7 removed it); transitive via deps is acceptable.
- `color-eyre` is rejected (archived upstream).

### Schema rules expansion (HLC + shared_doc — Batch A)

- Every mutable **user-editable + sync-eligible** row carries an `hlc INTEGER` column alongside `updated_at` (v1 posture: nullable, populated in Batch B+).
- Device-local rows (`volume_mounts`, `scan_progress`, `thumbnail_queue`, `device_config`) do NOT carry `hlc` — their identity is machine-scoped and they never sync.
- `shared_doc` table is reserved empty for post-v1 Loro integration. Do NOT add rows in v1. Do NOT add `loro` crate.
- HLC packing: 48 low-bits ms + 16 high-bits counter → non-negative i64. `crates/core::Hlc` provides `now()` + `pack()` + `unpack()`.
- Existing rules (unchanged): UUIDv7 PKs, `updated_at` + `device_id` on every mutable row, soft deletes, no UNIQUE on mutable columns, no FK cascades, FTS filters + triggers respect soft-delete + restore (V007→V008 bug class).

Note: for now, dont commit plans, specs, claude.md updates until the human says otherwise.
