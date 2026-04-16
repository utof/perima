# Phase 0 — Scaffold & Gates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the Rust workspace, lints, justfile, pre-commit hook, and GitHub Actions pipeline so phase 1's first line of product code ships under the full quality bar.

**Architecture:** Virtual Cargo workspace with five empty crates (`core`, `db`, `fs`, `hash`, `cli`). Workspace-level lints enforce doc coverage, clippy pedantic + complexity, rustdoc intra-doc-links. `justfile` composes `fmt-check + clippy + test + doctest + docs-coverage` into `just ci`. Pre-commit git hook runs `just ci`. GitHub Actions mirrors the hook across Linux/macOS/Windows; nightly workflow runs kani (step-level `continue-on-error`, never blocks main).

**Tech Stack:** Rust 1.85+, edition 2024, resolver 3; rusqlite 0.38 + refinery 0.9 (pinned pair); blake3, walkdir, notify, tauri 2 (phase-2 pre-pin); `just`, `actionlint`, native git hooks, GitHub Actions with `Swatinem/rust-cache@v2`, `dtolnay/rust-toolchain@stable`, `taiki-e/install-action@just`.

**Spec:** `docs/superpowers/specs/2026-04-15-phase-0-scaffold-design.md`

**Execution rule (from CLAUDE.md):** All work on `main`. Per-commit order: execute → `just ci` green → reviewer subagent approves → commit. Never commit unreviewed work. No branches, no worktrees, no `--force` push.

---

## File Structure

**Committed to git:**

```
perima/
├── Cargo.toml                          # Task 2 — virtual workspace manifest
├── Cargo.lock                          # generated first cargo build
├── rustfmt.toml                        # Task 3
├── .gitattributes                      # Task 4
├── .gitignore                          # Task 5 (modify)
├── justfile                            # Task 6
├── crates/
│   ├── core/{Cargo.toml,src/lib.rs}    # Task 2
│   ├── db/{Cargo.toml,src/lib.rs}      # Task 2
│   ├── fs/{Cargo.toml,src/lib.rs}      # Task 2
│   ├── hash/{Cargo.toml,src/lib.rs}    # Task 2
│   └── cli/{Cargo.toml,src/main.rs}    # Task 2
├── scripts/
│   ├── pre-commit                      # Task 7
│   └── test-precommit-hook.sh          # Task 8
└── .github/workflows/
    ├── ci.yml                          # Task 9
    └── kani.yml                        # Task 10
```

**Created but gitignored (`**/*.md` + other):**
- `DECISIONS.md` — ADR log (Task 11)
- `README.md` — local scratchpad (Task 12)

**Split rationale:** each crate owns one concern (`core` = domain + trait ports only, adapters and CLI wire them). Scripts under `scripts/`, workflows under `.github/workflows/` — standard layout.

---

## Task 0: Environment preflight

No files changed. Purely a gate — abort if the machine isn't ready.

- [ ] **Step 1: Rust toolchain ≥ 1.85**

Run: `rustc --version | awk '{print $2}'`
Expected: a version string ≥ `1.85.0`.
If missing: `rustup install stable && rustup default stable`.

- [ ] **Step 2: Cargo components present**

Run: `rustup component list --installed`
Expected: includes `clippy` and `rustfmt`.
If missing: `rustup component add clippy rustfmt`.

- [ ] **Step 3: `just` installed**

Run: `just --version`
Expected: any `just 1.x.y` line.
If missing: `cargo install just --locked` OR `brew install just` OR `apt install just`.

- [ ] **Step 4: `gh` CLI authenticated**

Run: `gh auth status`
Expected: "Logged in to github.com as utof" (or similar).
If not authenticated: `gh auth login` (interactive — must be done by user; pause plan until resolved).

- [ ] **Step 5: Remote state check — avoid non-fast-forward push later**

Run: `git ls-remote origin`
Expected: **empty output** (remote `main` does not yet exist) OR remote only contains refs we can safely fast-forward into.

If remote has a commit we do not have locally (e.g., auto-generated README from GitHub web UI), **STOP**. Do one of the following:
1. `git fetch origin && git reset --hard origin/main` and restart phase 0 against the existing base (preferred — preserves remote history).
2. Delete the remote content via GitHub UI and retry (destructive; only if remote content is known-empty auto-init).

Do NOT use `git push --force`; CLAUDE.md forbids it.

- [ ] **Step 6: No reviewer needed — this is a readiness gate, not a deliverable.**

---

## Task 1: Workspace `Cargo.toml` + first empty crate (`core`)

**Files:**
- Create: `Cargo.toml`
- Create: `crates/core/Cargo.toml`
- Create: `crates/core/src/lib.rs`

Fused with Task 2's remaining crates to avoid an intermediate "expected-fail" state. Task 1 stands alone as the minimum that builds.

- [ ] **Step 1: Create `Cargo.toml`**

Content:

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
# Pinned against crates.io as of 2026-04-15. rusqlite held at 0.38
# to pair with refinery 0.9 (refinery-core caps rusqlite at 0.38).
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
tauri        = { version = "2", features = [] }
tauri-specta = "=2.0.0-rc.24"

[profile.release]
lto = "thin"
codegen-units = 1
```

- [ ] **Step 2: Create `crates/core/Cargo.toml`**

Content:

```toml
[package]
name = "perima-core"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]

[lints]
workspace = true
```

- [ ] **Step 3: Create `crates/core/src/lib.rs`**

Content:

```rust
//! Domain types and trait ports for perima.
//!
//! This crate has zero framework dependencies. Every other crate in
//! the workspace either defines types consumed here or adapts this
//! crate's traits to a concrete backend.

/// Marker placeholder. Replaced with real domain types in phase 1.
pub const CRATE_NAME: &str = "perima-core";
```

- [ ] **Step 4: Verify the minimum workspace builds**

Run: `cargo build -p perima-core`
Expected: success.

---

## Task 2: Remaining empty crates

**Files:**
- Create: `crates/db/{Cargo.toml,src/lib.rs}`
- Create: `crates/fs/{Cargo.toml,src/lib.rs}`
- Create: `crates/hash/{Cargo.toml,src/lib.rs}`
- Create: `crates/cli/{Cargo.toml,src/main.rs}`

- [ ] **Step 1: Create `crates/db/Cargo.toml`**

```toml
[package]
name = "perima-db"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]

[lints]
workspace = true
```

- [ ] **Step 2: Create `crates/db/src/lib.rs`**

```rust
//! SQLite adapter for perima (phase 1 brings the real implementation).

/// Marker placeholder.
pub const CRATE_NAME: &str = "perima-db";
```

- [ ] **Step 3: Create `crates/fs/Cargo.toml`**

```toml
[package]
name = "perima-fs"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]

[lints]
workspace = true
```

- [ ] **Step 4: Create `crates/fs/src/lib.rs`**

```rust
//! Filesystem scanning, watching, and path normalization for perima.

/// Marker placeholder.
pub const CRATE_NAME: &str = "perima-fs";
```

- [ ] **Step 5: Create `crates/hash/Cargo.toml`**

```toml
[package]
name = "perima-hash"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]

[lints]
workspace = true
```

- [ ] **Step 6: Create `crates/hash/src/lib.rs`**

```rust
//! BLAKE3-based content-hashing adapter for perima.

/// Marker placeholder.
pub const CRATE_NAME: &str = "perima-hash";
```

- [ ] **Step 7: Create `crates/cli/Cargo.toml`**

```toml
[package]
name = "perima"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]

[lints]
workspace = true
```

(`src/main.rs` is auto-discovered when `name = "perima"`; no `[[bin]]` block needed.)

- [ ] **Step 8: Create `crates/cli/src/main.rs`**

```rust
//! `perima` command-line entry point (phase 1 adds real subcommands).

/// Entry point — prints a placeholder until phase 1 wires real commands.
fn main() {
    println!("perima 0.1.0 (phase 0 scaffold)");
}
```

- [ ] **Step 9: Verify workspace build**

Run: `cargo build --workspace`
Expected: success. `Cargo.lock` is created.

Note: `[workspace.dependencies]` does NOT pull those deps into the lockfile unless a crate's own `[dependencies]` lists them. The empty crates don't — so the lockfile stays minimal until phase 1.

- [ ] **Step 10: Verify clippy is clean**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: exit 0, zero warnings.

- [ ] **Step 11: Verify doc coverage**

Run: `cargo doc --workspace --no-deps`
Expected: exit 0. (Enforcement comes from `[workspace.lints.rust] missing_docs = "deny"` plus `[workspace.lints.rustdoc] broken_intra_doc_links = "deny"`.)

---

## Task 3: `rustfmt.toml`

**Files:**
- Create: `rustfmt.toml`

- [ ] **Step 1: Create the config**

```toml
edition = "2024"
max_width = 100
use_field_init_shorthand = true
use_try_shorthand = true
imports_granularity = "Crate"
group_imports = "StdExternalCrate"
```

- [ ] **Step 2: Verify fmt passes**

Run: `cargo fmt --all -- --check`
Expected: exit 0.

Note: `imports_granularity` / `group_imports` are nightly-only; inert on stable.

---

## Task 4: `.gitattributes`

**Files:**
- Create: `.gitattributes`

- [ ] **Step 1: Create the file**

```
* text=auto eol=lf
*.sh text eol=lf
*.bat text eol=crlf
```

- [ ] **Step 2: Renormalize the working tree**

Run: `git add --renormalize .`
Expected: no error.

---

## Task 5: Update `.gitignore`

**Files:**
- Modify: `.gitignore`

- [ ] **Step 1: Replace content**

Final `.gitignore`:

```
**/*.md
target/
.DS_Store
*.swp
```

- [ ] **Step 2: Verify no `.md` is tracked**

Run: `git ls-files '*.md'`
Expected: empty.

---

## Task 6: `justfile` + first commit

**Files:**
- Create: `justfile`

- [ ] **Step 1: Create the justfile**

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

Note: `docs-coverage` is now a plain `cargo doc --workspace --no-deps`. The hard-failure gate comes from `[workspace.lints.rust] missing_docs = "deny"` (catches missing doc comments) plus `[workspace.lints.rustdoc] broken_intra_doc_links = "deny"` (catches dangling `[link]` references). No string parsing, no `RUSTDOCFLAGS` (the `rustdoc::missing_docs` lint name does not exist — `missing_docs` is a rustc lint, not a rustdoc lint).

- [ ] **Step 2: Run `just ci`**

Run: `just ci`
Expected: all five targets pass; exit 0.

- [ ] **Step 3: Dispatch reviewer subagent (checkpoint #1 — Tasks 0–6)**

Reviewer checklist:
- [ ] Workspace manifest parses and resolves.
- [ ] All five crates build with empty `[dependencies]`.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- [ ] `cargo fmt --all -- --check` is clean.
- [ ] `docs-coverage` emits zero missing-doc warnings.
- [ ] `just ci` exits 0.
- [ ] `.gitignore` excludes `**/*.md`, `target/`, `.DS_Store`, `*.swp`.
- [ ] `.gitattributes` pins LF.
- [ ] `rustfmt.toml` is edition 2024.
- [ ] No `.md` file in `git ls-files`.

Must return APPROVED before step 4.

- [ ] **Step 4: Commit (only after APPROVED)**

```bash
git add Cargo.toml Cargo.lock rustfmt.toml .gitattributes .gitignore justfile crates/
git commit -m "feat(phase-0): workspace scaffold, lints, justfile

Virtual Cargo workspace with five empty crates (core/db/fs/hash/cli).
Workspace-level doc + clippy + rustdoc gates. justfile composes
fmt-check + clippy + test + doctest + docs-coverage into 'just ci'.

Refs: docs/superpowers/specs/2026-04-15-phase-0-scaffold-design.md"
```

Run: `git status`
Expected: clean tree.

---

## Task 7: Pre-commit hook

**Files:**
- Create: `scripts/pre-commit`

- [ ] **Step 1: Create the script**

```bash
#!/usr/bin/env bash
set -euo pipefail
# Git hooks run in non-interactive shells that may not source ~/.bashrc,
# so cargo-installed binaries may not be on PATH. Prepend the standard
# location explicitly to make this hook portable across shells / git GUIs.
export PATH="$HOME/.cargo/bin:$PATH"
just ci
```

- [ ] **Step 2: Make executable**

Run: `chmod +x scripts/pre-commit`

- [ ] **Step 3: Install**

Run: `just install-hooks`
Expected: `.git/hooks/pre-commit` exists and is executable.

- [ ] **Step 4: Verify**

Run: `test -x .git/hooks/pre-commit && echo ok`
Expected: `ok`.

---

## Task 8: Scripted hook test + commit

**Files:**
- Create: `scripts/test-precommit-hook.sh`

- [ ] **Step 1: Create the script**

```bash
#!/usr/bin/env bash
set -euo pipefail
# Same rationale as scripts/pre-commit: ensure cargo-installed binaries
# (notably `just`) are reachable when this script runs from a nested or
# non-interactive shell.
export PATH="$HOME/.cargo/bin:$PATH"
cd "$(git rev-parse --show-toplevel)"

target="crates/core/src/lib.rs"
if [[ ! -f "$target" ]]; then
    echo "FAIL: expected $target to exist (phase 0 scaffolding incomplete)" >&2
    exit 2
fi

just ci >/dev/null
echo "baseline: just ci green"

backup="$(mktemp)"
cp "$target" "$backup"
trap 'cp "$backup" "$target"; rm -f "$backup"' EXIT INT TERM HUP
printf '\npub const __HOOK_TEST: i32 =\t0 ;   \n' >> "$target"

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

- [ ] **Step 2: Executable bit**

Run: `chmod +x scripts/test-precommit-hook.sh`

- [ ] **Step 3: Run the test**

Run: `just test-hook`
Expected:
```
baseline: just ci green
OK: just ci blocked a commit-equivalent with planted violation (rc=...)
```

- [ ] **Step 4: Confirm no residue**

Run: `git status --porcelain crates/core/src/lib.rs`
Expected: empty.

- [ ] **Step 5: Reviewer subagent (checkpoint #2 — Tasks 7–8)**

Reviewer checklist:
- [ ] `scripts/pre-commit` contains `just ci` and is executable.
- [ ] `.git/hooks/pre-commit` installed via `just install-hooks`.
- [ ] `scripts/test-precommit-hook.sh` backs up the target, plants into a tracked file, restores via trap on EXIT/INT/TERM/HUP.
- [ ] `just test-hook` passes.
- [ ] After `just test-hook`, `git status` shows no changes to `crates/core/src/lib.rs`.

Must return APPROVED before step 6.

- [ ] **Step 6: Commit**

```bash
git add scripts/pre-commit scripts/test-precommit-hook.sh
git commit -m "feat(phase-0): pre-commit hook + scripted verification

Native git pre-commit hook runs 'just ci'. test-precommit-hook.sh
plants a fmt violation against crates/core/src/lib.rs, asserts
'just ci' fails, then restores via trap (EXIT/INT/TERM/HUP).

Refs: docs/superpowers/specs/2026-04-15-phase-0-scaffold-design.md"
```

---

## Task 9: GitHub Actions CI

**Files:**
- Create: `.github/workflows/ci.yml`

- [ ] **Step 1: Create the workflow**

```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

jobs:
  build:
    name: just ci (${{ matrix.os }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]

    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt

      - uses: Swatinem/rust-cache@v2

      - uses: taiki-e/install-action@just

      - name: Run just ci
        shell: bash
        run: just ci
```

Note: `taiki-e/install-action@just` fetches a prebuilt `just` binary (fast, no compile). `shell: bash` makes the `just` step identical on Windows (Git Bash is preinstalled on `windows-latest`).

- [ ] **Step 2: Install `actionlint` if missing**

`actionlint` is a Go binary; it is not on crates.io. Use the official
downloader to install into `~/.local/bin` (no sudo, cross-platform).

Run:
```bash
if ! command -v actionlint >/dev/null; then
    mkdir -p "$HOME/.local/bin"
    curl -sSfL https://raw.githubusercontent.com/rhysd/actionlint/main/scripts/download-actionlint.bash \
        | bash -s -- latest "$HOME/.local/bin"
fi
export PATH="$HOME/.local/bin:$PATH"
command -v actionlint
```
Expected: prints a path ending in `actionlint` (e.g., `$HOME/.local/bin/actionlint`).

Alternative per-platform: `brew install actionlint` (macOS) / `apt install actionlint` (Debian/Ubuntu, if packaged) / `go install github.com/rhysd/actionlint/cmd/actionlint@latest` (if Go is available).

- [ ] **Step 3: Validate the workflow syntax**

Use the explicit path in case `$PATH` did not persist from Step 2
(each task step may run in a fresh shell in autonomous execution):

Run: `"$HOME/.local/bin/actionlint" .github/workflows/ci.yml || actionlint .github/workflows/ci.yml`
Expected: empty output (no errors).

---

## Task 10: GitHub Actions kani

**Files:**
- Create: `.github/workflows/kani.yml`

- [ ] **Step 1: Create the workflow**

```yaml
name: kani

on:
  schedule:
    - cron: "0 3 * * *"
  workflow_dispatch:

jobs:
  kani:
    name: cargo kani
    runs-on: ubuntu-latest

    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable

      - uses: Swatinem/rust-cache@v2

      - name: Install kani
        run: cargo install --locked kani-verifier && cargo kani setup

      # Step-level continue-on-error so job result reflects reality
      # but the workflow run never fails. Phase 1 adds the first
      # actual proof; until then this is a wiring check.
      - name: Run kani proofs
        continue-on-error: true
        run: cargo kani --workspace
```

- [ ] **Step 2: Validate**

Run: `"$HOME/.local/bin/actionlint" .github/workflows/kani.yml || actionlint .github/workflows/kani.yml`
Expected: empty output.

- [ ] **Step 3: Reviewer (checkpoint #3 — Tasks 9–10)**

Reviewer checklist:
- [ ] `ci.yml`: 3-OS matrix, uses `taiki-e/install-action@just`, runs `just ci`, `fail-fast: false`.
- [ ] `kani.yml`: schedule + workflow_dispatch, `continue-on-error` is at step level only (job status is honest).
- [ ] Both files pass `actionlint`.
- [ ] No secrets, no self-hosted runners introduced.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml .github/workflows/kani.yml
git commit -m "feat(phase-0): GitHub Actions CI + nightly kani

ci.yml: 3-OS matrix (Linux/macOS/Windows), just ci via
taiki-e/install-action for fast just install.
kani.yml: nightly schedule + workflow_dispatch; step-level
continue-on-error so workflow never blocks main.

Refs: docs/superpowers/specs/2026-04-15-phase-0-scaffold-design.md"
```

---

## Task 11: Seed `DECISIONS.md` (local, gitignored)

**Files:**
- Create: `DECISIONS.md` (never committed)

- [ ] **Step 1: Create the file**

```markdown
# perima — architectural decisions log

Local-only (gitignored). ADR-style. Append-only.

---

## 2026-04-15: Rust edition 2024, resolver 3, rust-version 1.85

**Context:** Phase 0 workspace setup.
**Decision:** Edition 2024 + resolver 3, pin `rust-version = "1.85"`.
**Alternatives:** Edition 2021 (longer support), resolver 2 (older).
**Consequences:** Requires stable Rust ≥ 1.85; gains edition 2024 improvements.

## 2026-04-15: No `crates/xtask` yet

**Context:** Research doc suggests `xtask`.
**Decision:** Defer; `justfile` suffices.
**Alternatives:** Create xtask upfront.
**Consequences:** Revisit when a shell recipe becomes painful.

## 2026-04-15: Native git pre-commit hook (no Python `pre-commit` framework)

**Context:** Commit-time gate.
**Decision:** Bash script at `scripts/pre-commit`, installed via `just install-hooks`.
**Alternatives:** pre-commit Python framework.
**Consequences:** Zero extra language deps; contributors run `just install-hooks` once.

## 2026-04-15: Pre-commit hook does NOT enforce reviewer-attestation

**Context:** CLAUDE.md requires reviewer approval before commit.
**Decision:** Hook runs `just ci` only; reviewer-attestation is a process rule enforced by subagent dispatch.
**Alternatives:** Hook checks for `.review-token` artifact.
**Consequences:** Process discipline required; future hook enhancement may close this loop.

## 2026-04-15: GitHub Actions for CI

**Context:** Repo hosted on GitHub.
**Decision:** Actions with 3-OS matrix; nightly kani workflow.
**Alternatives:** Self-hosted runners.
**Consequences:** Standard, zero-config for contributors.

## 2026-04-15: `rusqlite = "0.38"` pinned to pair with `refinery = "0.9"`

**Context:** rusqlite 0.39 exists; refinery-core 0.9.1 caps at 0.38.
**Decision:** Pin both; bump together when refinery 0.10 ships.
**Alternatives:** Drop refinery and hand-roll migrations.
**Consequences:** Single coordinated upgrade later.

## 2026-04-15: Migrations via `refinery`

**Context:** Phase 1 will ship SQLite schema.
**Decision:** SQL-file-driven migrations via refinery.
**Alternatives:** `rusqlite_migration` (Rust-code closures).
**Consequences:** SQL as source of truth.

## 2026-04-15: `Cargo.lock` is committed

**Context:** Workspace ships binaries.
**Decision:** Commit Cargo.lock.
**Alternatives:** Gitignore it (library convention).
**Consequences:** Reproducible CI + contributor builds.

## 2026-04-15: `missing_docs = "deny"` in workspace lints; `docs-coverage` is plain `cargo doc`

**Context:** Need a hard failure on undocumented public items.
**Decision:** Set `[workspace.lints.rust] missing_docs = "deny"` so every `cargo build` and `cargo doc` hard-fails on undocumented public items. `docs-coverage` is just `cargo doc --workspace --no-deps` and additionally validates `broken_intra_doc_links` (which only fires under `cargo doc`).
**Alternatives:** `RUSTDOCFLAGS="-D rustdoc::missing_docs"` (rejected — `rustdoc::missing_docs` is not a real lint; `missing_docs` is a rustc lint, not a rustdoc lint, so the env-var approach was silently a no-op). `grep` parsing of doc output (fragile to message changes / i18n).
**Consequences:** Every build catches missing docs immediately, not just `cargo doc`. Verified via gate-proof: removing one `///` line on a public item produces EXIT:101 with "missing documentation for a constant"; restoring returns to EXIT:0.

## 2026-04-15: CI installs `just` via `taiki-e/install-action@just`

**Context:** `cargo install just` adds 30–90s per CI run before rust-cache activates.
**Decision:** Use taiki-e's prebuilt binary install.
**Alternatives:** Official install script; `cargo install just --locked`.
**Consequences:** ~1 min saved per CI run; trivially swappable if the action breaks.
```

- [ ] **Step 2: Verify untracked**

Run: `git status --porcelain DECISIONS.md`
Expected: empty (file exists but ignored).

---

## Task 12: Seed local `README.md`

**Files:**
- Create: `README.md` (gitignored)

- [ ] **Step 1: Create the file**

```markdown
# perima

Cross-platform media asset manager. See `CLAUDE.md` for project rules
and `docs/superpowers/specs/2026-04-15-meta-plan-design.md` for phase
sequencing.

Local-only (gitignored).
```

- [ ] **Step 2: Verify untracked**

Run: `git status --porcelain README.md`
Expected: empty.

---

## Task 13: Final verification + push

- [ ] **Step 1: Clean build**

Run: `cargo clean && just ci`
Expected: green.

- [ ] **Step 2: Scripted hook test**

Run: `just test-hook`
Expected: green.

- [ ] **Step 3: `.md` hygiene**

Run: `git ls-files '*.md'`
Expected: empty.

- [ ] **Step 4: Exit criteria checklist (from spec §Exit criteria)**

- [ ] 1. `cargo build --workspace` — PASS
- [ ] 2. `cargo clippy --workspace --all-targets -- -D warnings` — PASS
- [ ] 3. `cargo test --workspace` — PASS
- [ ] 4. `cargo fmt --all -- --check` — PASS
- [ ] 5. `cargo doc --workspace --no-deps` zero missing-doc errors — PASS (enforced via `[workspace.lints.rust] missing_docs = "deny"` + `[workspace.lints.rustdoc] broken_intra_doc_links = "deny"`)
- [ ] 6. `just ci` — PASS
- [ ] 7. GitHub Actions `ci.yml` green (verified after push in step 7)
- [ ] 8. `.github/workflows/kani.yml` valid — PASS (`actionlint`)
- [ ] 9. `just test-hook` — PASS
- [ ] 10. `git ls-files '*.md'` empty — PASS

- [ ] **Step 5: Reviewer (checkpoint #4 — final phase 0)**

Reviewer checklist:
- [ ] All 10 spec exit criteria pass.
- [ ] No reopened must/should-fix from earlier review passes.
- [ ] `git status` clean.
- [ ] `DECISIONS.md` present locally with all 10 seeded entries.

- [ ] **Step 6: Verify clean tree**

Run: `git status`
Expected: clean.

- [ ] **Step 7: Push (first push to origin/main)**

Safe because Task 0 Step 5 already confirmed remote is clean (or was synced down).

Run: `git push -u origin main`
Expected: creates `main` remotely (or fast-forwards).

- [ ] **Step 8: Poll CI**

Run: `gh run list --workflow=ci.yml --limit 1 --json status,conclusion,name`
Expected eventually: `{"conclusion":"success"}` on all three matrix jobs (2–5 min).

If a runner fails: open a bugfix task for phase 0; do NOT mask with `continue-on-error`. Known mitigations:
- Windows line endings → already handled by `.gitattributes`.
- `/tmp` on Windows → already handled by `mktemp` in `docs-coverage`.
- `taiki-e/install-action` transient network failure → retry the job.

- [ ] **Step 9: Tag phase boundary**

Run:
```bash
git tag -a phase-0-complete -m "Phase 0: scaffold & gates complete"
git push origin phase-0-complete
```

---

## Self-review

**Spec coverage:**
- §1 Workspace manifest → Task 1 ✓
- §2 Empty crates → Tasks 1, 2 ✓
- §3 justfile → Task 6 ✓
- §4 Pre-commit hook → Task 7 ✓
- §4a Scripted test → Task 8 ✓
- §5 GitHub Actions → Tasks 9, 10 ✓
- §6 rustfmt.toml → Task 3 ✓
- §7 .gitattributes → Task 4 ✓
- §7a .gitignore → Task 5 ✓
- §8 DECISIONS.md seed → Task 11 ✓
- §9 README.md → Task 12 ✓
- Exit criteria 1–10 → Task 13 ✓
- Environment preflight → Task 0 (plan addition)

**Placeholder scan:** no TBD/TODO in any task; every file has exact content.

**Type consistency:** crate names (`perima-core`, `perima-db`, `perima-fs`, `perima-hash`, `perima`) consistent across `Cargo.toml`, `src/lib.rs`, and `[[bin]]` paths. Reviewer checkpoint names (#1–#4) consistent. `docs-coverage` implementation consistent between spec §3 and justfile in Task 6.

**Commit discipline:** four reviewer checkpoints (Tasks 6, 8, 10, 13) each gate a single commit; commit messages reference the spec. Push happens once, at the final checkpoint, after tag.
