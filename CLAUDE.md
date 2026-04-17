# perima — cross-platform media asset manager

Rust hexagonal core + Tauri 2 / React desktop. Mobile (Expo + UniFFI) and
Obsidian (axum HTTP API) are downstream shells. Full rationale in
`2026-04-09-multiplatform-rust-perima.md` and `2026-04-09-documentation-research.md`
— read those before decisions, not this file.

## Autonomous mode

This repo is developed without human confirmation. Never stop to ask
questions. If the human speaks it is steering, not an interruption — keep
going. Chip tasks one at a time; do not batch-defer work for approval.

## Phase loop (repeat per phase/step)

1. `superpowers:brainstorming` — explore intent, requirements, design.
2. Write spec → dispatch a reviewer subagent → revise until clean.
3. `superpowers:writing-plans` → dispatch a reviewer subagent → revise.
4. `superpowers:executing-plans` with `superpowers:subagent-driven-development`.
5. After every task: reviewer subagent checks code against spec + plan
   (`superpowers:requesting-code-review`).
6. `superpowers:verification-before-completion` before claiming done.
7. `superpowers:test-driven-development` throughout.

Without human e2e feedback, correctness rides on tests: unit + integration
always, e2e where feasible. No feature is done without them.

## Workspace layout (target)

Flat `crates/` layout, virtual workspace manifest at root. `crates/core`
holds domain + trait ports with zero framework deps. `crates/{db,fs,hash}`
are adapters. `crates/{desktop,api,cli,ffi}` wire adapters to core.
`apps/desktop` holds the React frontend (Vite + Tailwind). Build
automation lives in `justfile`; reintroduce `crates/xtask` only when
shell scripts stop scaling.

## Tooling

- JS/TS: **always `bun`** (install, run, test, build) unless incompatible.
- Rust: `cargo`, `cargo clippy`, `cargo test`, `cargo doc`. Use
  `rusqlite` (bundled), not sqlx. `thiserror` in libs, `anyhow` in apps.
- Build automation: `justfile` at root (Rust `xtask` deferred until needed).
- **`codebase-memory-mcp` is available** — use it for indexed code search,
  graph queries, call-path tracing, ADRs. Prefer it over Grep for
  cross-crate structural questions ("who calls X?", "what depends on Y?").
- **`LSP` tool (rust-analyzer plugin) is available** — deferred, load
  via `ToolSearch select:LSP`. For Rust symbol queries ("where is
  `TagRepository` defined?", "who implements `MetadataExtractor`?",
  "callers of `upsert_metadata`") prefer LSP over grep; it uses
  rust-analyzer's semantic index and won't false-match comments or
  similar names in other languages. Fall back to Grep only for text
  in docs/comments or cross-language search.

## Test stack (pinned, don't re-litigate)

- Rust: `cargo test` + `insta` (snapshots for schemas, hashes, path
  tables) + `proptest` (hash determinism, path round-trip). Integration
  tests hit real SQLite, never mocks.
- TS: `bun test` for units; `vitest` + jsdom for React components;
  Playwright only when a phase ships UI worth e2e-ing.

## Doc discipline (strict, enforced in CI)

- Rust: `#![deny(rustdoc::broken_intra_doc_links)]` +
  `#![warn(missing_docs)]` workspace-wide.
- Clippy wall: `-D warnings`, plus complexity lints
  (`clippy::cognitive_complexity`, `clippy::too_many_lines`,
  `clippy::excessive_nesting`). Keep cyclomatic <10, cognitive <15.
- `kani` (formal verifier) on a curated set of `#[kani::proof]`
  invariants (hashing, path normalization). **On-demand / scheduled
  only** (`just verify` + nightly CI), never per-commit — proofs are
  minutes-to-hours slow.
- Readability / nestedness lints gate merges alongside clippy. Wire
  into `justfile`.
- TS: `eslint-plugin-tsdoc`, TypeDoc `--validation.notDocumented`.
- Every non-obvious decision gets a `// WHY:` comment. Standard
  doc-comments explain the API; WHY-comments explain the reasoning.
- Architectural decisions: CLAUDE.md (living rules) + phase specs
  (intent snapshots) + commit WHY blocks (ground truth) carry the load.
  No separate DECISIONS.md — per 2026-04-17 research, flat decision
  logs rot faster than they're maintained in a solo AI-heavy workflow.
- Unified doc site: **Astro Starlight** (Tauri-stack aligned, handles
  polyglot via MDX). Rust doctests via `mdbook test` in CI on the core
  crate. `cargo doc` + TypeDoc feed Starlight; introduce the site
  itself in a later phase.

## Schema rules (CRDT-ready from day one)

UUIDv7 primary keys, `updated_at` + `device_id` on every mutable row,
soft deletes (`deleted_at`), no UNIQUE on mutable columns, no FK
cascades. Cheap now, prevents a rewrite when sync lands.

## Git

- All work on `main`. No branches, no worktrees.
- **Releases = semver tags** (`v0.N.x`). No fixed `v1.0.0` milestone;
  stay on `0.x` until an explicit API-stability commitment. "Phase"
  is internal planning vocabulary — never in tags, commits, or CHANGELOG.
- **Conventional Commits** with *component* scopes (`core`, `db`, `fs`,
  `hash`, `cli`, `desktop`, `ci`, `deps`, `docs`, `release`), never
  milestones. `release-plz` handles bumps + CHANGELOG from v0.4.0 on.
- Commit order: execute → tests green → reviewer green → commit.
- `**/*.md` is gitignored **except `CHANGELOG.md`** and
  `docs/superpowers/**/*.md` (specs + plans are tracked so scheduled
  remote agents can continue work across sessions). Other in-progress
  notes stay local.
- New commits only; no amend, no `--no-verify`.
## Model defaults
Claude Opus 4.6 (1M ctx) for planning/review; Sonnet 4.6 for bulk
subagent execution; Haiku 4.5 for trivial lookups.

Local override: never include `Co-Authored-By:` in commits. ever.