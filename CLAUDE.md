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
- **`codebase-memory-mcp`** — prefer over Grep for cross-crate
  structural questions ("who calls X?", "what depends on Y?").
- **`LSP` (rust-analyzer, deferred)** — `ToolSearch select:LSP`.
  Prefer over grep for Rust symbol queries; semantic-index-backed so no
  comment/cross-language false matches. Fall back to Grep for docs/comments.

## Test stack (pinned, don't re-litigate)

- Rust: `cargo test` + `insta` (snapshots) + `proptest` (hash
  determinism, path round-trip). Integration tests hit real SQLite.
- TS: `bun test` units; `vitest` + jsdom for React; Playwright only when
  a phase ships UI worth e2e-ing.

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
deletes, no UNIQUE on mutable columns, no FK cascades.

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

Opus 4.6 (1M) for planning/review; Sonnet 4.6 for bulk subagent execution;
Haiku 4.5 for trivial lookups. **Never include `Co-Authored-By:` in commits.**
