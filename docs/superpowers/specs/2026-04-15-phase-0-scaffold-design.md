# Phase 0 — Scaffold & gates

**Status:** draft awaiting reviewer pass
**Author:** Claude Opus 4.6 (autonomous mode)
**Date:** 2026-04-15
**Parent meta-plan:** `2026-04-15-meta-plan-design.md`

---

## Goal

Stand up the Rust workspace, lints, `justfile`, pre-commit hook, and
GitHub Actions pipeline. Zero product code. Every gate defined in
`CLAUDE.md` must be enforced on an empty (but building) workspace by
the end of this phase. Phase 0 exists so that phase 1's first line
of real code ships under the full quality bar — not under a partial
one that gets tightened later.

## Non-goals

- No Tauri, no React, no Bun, no TS. Frontend toolchain arrives in
  phase 2.
- No domain types, traits, adapters. Those are phase 1.
- No `xtask` crate. Deferred until shell/`just` stop scaling.
- No migrations, no DB code. Phase 1.
- No kani proofs. Wiring only — first proof is phase 1 property.

## Deliverables

### 1. Workspace manifest

`Cargo.toml` at repo root (virtual):

```toml
[workspace]
members = ["crates/*"]
resolver = "3"

[workspace.package]
edition = "2024"
rust-version = "1.85"
license = "MIT OR Apache-2.0"
repository = "https://github.com/utof/perima"

[workspace.lints.rust]
missing_docs = "deny"
unsafe_code = "deny"

[workspace.lints.rustdoc]
broken_intra_doc_links = "deny"
private_intra_doc_links = "deny"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
nursery = { level = "warn", priority = -1 }
cognitive_complexity = "warn"
too_many_lines = "warn"
excessive_nesting = "warn"
module_name_repetitions = "allow"
missing_errors_doc = "warn"

[workspace.dependencies]
# Pinned against crates.io as of 2026-04-15 — upgrades are explicit
# tasks, never drifted. rusqlite held at 0.38 to pair with refinery 0.9
# (refinery-core 0.9.1 caps rusqlite at 0.38; bump both together when
# refinery 0.10 ships).
anyhow       = "1"
thiserror    = "2"
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"
tracing      = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
tokio        = { version = "1", features = ["full"] }
uuid         = { version = "1", features = ["v4", "v7", "serde"] }
blake3       = "1"
rusqlite     = { version = "0.38", features = ["bundled"] }
refinery     = { version = "0.9", features = ["rusqlite"] }
walkdir      = "2"
notify       = "8.2"
notify-debouncer-full = "0.7"
sysinfo      = "0.38"
directories  = "6"
unicode-normalization = "0.1"
path-slash   = "0.2"
dunce        = "1"
file-id      = "0.2"
# Phase 2+ deps pinned now to stabilize the lockfile early.
# tauri-specta is exact-pinned because 2.x is still in rc.
tauri        = { version = "2", features = [] }
tauri-specta = "=2.0.0-rc.24"

[profile.release]
lto = "thin"
codegen-units = 1
```

### 2. Empty crates

`crates/{core,db,fs,hash,cli}` each with:
- `Cargo.toml` inheriting `[package].edition.workspace = true` etc.
- `src/lib.rs` (or `src/main.rs` for `cli`) containing only
  `#![cfg_attr(not(any(test, feature = "test")), deny(missing_docs))]`
  commented out for now (lints come from workspace); a doc-comment
  crate-level description; a single placeholder item with a doc
  comment so `missing_docs` has something to pass against.

Example `crates/core/src/lib.rs`:

```rust
//! Domain types and trait ports for perima.
//!
//! This crate has zero framework dependencies. Every other crate in
//! the workspace either defines types consumed here or adapts this
//! crate's traits to a concrete backend.

/// Marker placeholder. Replaced with real domain types in phase 1.
pub const CRATE_NAME: &str = "perima-core";
```

### 3. `justfile`

```just
set shell := ["bash", "-eo", "pipefail", "-c"]

default: ci

test:
    cargo test --workspace --all-targets

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

doctest:
    cargo test --workspace --doc

mdbook-test:
    cargo test --workspace --doc -- --show-output

docs-coverage:
    cargo doc --workspace --no-deps

fmt-check:
    cargo fmt --all -- --check

fmt:
    cargo fmt --all

ci: fmt-check clippy test doctest docs-coverage

verify:
    @if command -v cargo-kani >/dev/null 2>&1; then \
        cargo kani --workspace; \
    else \
        echo "cargo-kani not installed; skipping"; \
    fi

install-hooks:
    cp scripts/pre-commit .git/hooks/pre-commit
    chmod +x .git/hooks/pre-commit

test-hook:
    bash scripts/test-precommit-hook.sh
```

Note: `mdbook-test` is currently an alias for `cargo test --doc` — the
`mdbook test` binary is only meaningful once the Starlight site has
mdbook-sourced pages (phase 7). Keeping the target name stable lets
phase 7 swap the implementation without touching CI wiring.

### 4. Pre-commit hook (`scripts/pre-commit`)

```bash
#!/usr/bin/env bash
set -euo pipefail
# Git hooks run in non-interactive shells that may not source ~/.bashrc,
# so cargo-installed binaries may not be on PATH. Prepend the standard
# location explicitly to make this hook portable across shells / git GUIs.
export PATH="$HOME/.cargo/bin:$PATH"
just ci
```

Installed via `just install-hooks`. Native git hook only — no external
`pre-commit` framework (no Python dep).

**Known gap (logged in `DECISIONS.md`):** the pre-commit hook enforces
clippy + tests but does NOT enforce the reviewer-approval step from
CLAUDE.md ("execute → tests green → reviewer approves → commit").
Reviewer approval is enforced by process (subagent dispatch before
`git commit`), not by the hook. A future hook enhancement may check
for a `.review-token` artifact; deferred.

### 4a. Scripted pre-commit test (`scripts/test-precommit-hook.sh`)

Makes exit criterion 9 autonomously verifiable. Runs against the real
workspace (`just ci` needs the justfile, Cargo.toml, and crates to be
present), plants a file that is guaranteed to fail `fmt --check`,
asserts `just ci` fails, then cleans up. Does not touch git state.

```bash
#!/usr/bin/env bash
set -euo pipefail
# Same rationale as scripts/pre-commit: ensure cargo-installed binaries
# (notably `just`) are reachable when this script runs from a nested or
# non-interactive shell.
export PATH="$HOME/.cargo/bin:$PATH"
cd "$(git rev-parse --show-toplevel)"

# Target an existing tracked file so cargo fmt actually inspects it.
target="crates/core/src/lib.rs"
if [[ ! -f "$target" ]]; then
    echo "FAIL: expected $target to exist (phase 0 scaffolding incomplete)" >&2
    exit 2
fi

# Baseline: just ci must be green before we plant a violation.
just ci >/dev/null
echo "baseline: just ci green"

# Back up and plant a guaranteed fmt violation by appending a line
# with trailing whitespace + a tab — cargo fmt --check will flag it.
backup="$(mktemp)"
cp "$target" "$backup"
trap 'cp "$backup" "$target"; rm -f "$backup"' EXIT INT TERM HUP
printf '\npub const __HOOK_TEST: i32 =\t0 ;   \n' >> "$target"

# Hook body is `just ci`; testing just ci with a violation is
# equivalent to testing the hook would block that commit.
set +e
just ci >/dev/null 2>&1
rc=$?
set -e
if [[ $rc -eq 0 ]]; then
    echo "FAIL: just ci passed with a planted fmt violation" >&2
    exit 1
fi
echo "OK: just ci blocked a commit-equivalent with planted violation (rc=$rc)"
```

The planted text is appended to `crates/core/src/lib.rs` — a file
that is reachable from the workspace and inspected by
`cargo fmt --all -- --check`. The trap restores the original contents
whether the script succeeds or fails.

### 5. GitHub Actions

`.github/workflows/ci.yml`:
- Triggers: `push` to main, `pull_request` (even though we don't use
  PRs, the main-only rule doesn't preclude external contributors later).
- Matrix: `ubuntu-latest`, `macos-latest`, `windows-latest` on stable Rust.
- Steps: checkout → `dtolnay/rust-toolchain@stable` with `clippy` +
  `rustfmt` → `Swatinem/rust-cache@v2` → `just ci`.

`.github/workflows/kani.yml`:
- Trigger: `schedule: cron: "0 3 * * *"` (nightly) + `workflow_dispatch`.
- Runs `cargo kani --workspace` on `ubuntu-latest` only.
- Does **not** block main.

### 6. `rustfmt.toml`

```toml
edition = "2024"
max_width = 100
use_field_init_shorthand = true
use_try_shorthand = true
imports_granularity = "Crate"
group_imports = "StdExternalCrate"
```

(`imports_granularity` and `group_imports` are nightly-only rustfmt
features; keep but tolerate they're no-ops on stable toolchain. The
stable parts still apply.)

### 7. `.gitattributes`

Pin line endings to LF so Windows runners don't break `fmt-check` on
autocrlf round-trip.

```
* text=auto eol=lf
*.sh text eol=lf
*.bat text eol=crlf
```

### 7a. `.gitignore` additions

Existing `**/*.md` preserved. Append:

```
target/
.DS_Store
*.swp
```

`Cargo.lock` is **committed** (workspace ships a binary).

### 8. `DECISIONS.md` (local, gitignored)

Seed one ADR-format entry (date, context, decision, alternatives,
consequences) for each of:

- Rust edition 2024, resolver 3, `rust-version = "1.85"`.
- No `crates/xtask` yet; `justfile` is the build automation surface.
- Native git pre-commit hook (no Python `pre-commit` framework).
- Pre-commit enforces `just ci` only; **reviewer-attestation is NOT
  enforced by the hook** — it is a process rule executed via
  subagent dispatch before `git commit`. Future hook may check for
  a `.review-token` artifact; deferred.
- GitHub Actions for CI (matches repo host).
- `rustfmt.toml` with nightly-only options accepted as inert on
  stable.
- `rusqlite = "0.38"` pinned to pair with `refinery = "0.9"`; bump
  both when `refinery` 0.10 ships with 0.39+ support.
- Migrations via `refinery` (SQL files, not Rust code).
- Cargo.lock is committed (workspace ships binaries).

### 9. `README.md` (local, gitignored)

Local scratchpad only — never committed. One-liner pointing to
`CLAUDE.md` and the meta-plan spec.

## Tests in phase 0

- Zero unit tests (no domain code).
- `cargo test` must still pass (it will — no tests to fail).
- `just ci` green on a fresh clone + `cargo build --workspace`.

## Exit criteria (autonomously verifiable)

1. `cargo build --workspace` succeeds from a fresh clone.
2. `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
3. `cargo test --workspace` exits 0 (no tests, still success).
4. `cargo fmt --all -- --check` exits 0.
5. `cargo doc --workspace --no-deps` produces zero `missing
   documentation` warnings.
6. `just ci` exits 0.
7. GitHub Actions `ci.yml` workflow is green on the first pushed
   commit across all three OS runners.
8. GitHub Actions `kani.yml` is present and syntactically valid
   (does not need to have run yet).
9. `just test-hook` exits 0 (scripted pre-commit test at
   `scripts/test-precommit-hook.sh` confirms the hook blocks a
   bad commit).
10. `.gitignore` still excludes `**/*.md`; no `.md` file appears in
    `git status --porcelain` output after all phase 0 edits.

## Risks

- **Rustfmt nightly options on stable toolchain.** Mitigation:
  accept they're inert on stable; document in `DECISIONS.md`.
- **Clippy pedantic noise.** Mitigation: `module_name_repetitions`
  alone is allowed; `missing_errors_doc` is `warn` (per CLAUDE.md's
  strict doc rule). Extend the allow-list only via explicit task in
  phase 1 if a lint genuinely blocks real work, with justification
  in `DECISIONS.md`.
- **CI matrix flakiness on Windows/macOS for greenfield.** Mitigation:
  none preemptive. If a runner fails for environmental reasons, open
  a phase-0 task to diagnose — do not mask with `continue-on-error`.
- **`just ci` runtime.** Should stay under 30 s on empty workspace.
  If it doesn't, investigate before phase 1.
