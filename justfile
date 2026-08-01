set shell := ["bash", "-eo", "pipefail", "-c"]

# WHY these exports: perima-desktop's tauri-build step needs
# PKG_CONFIG_PATH + LIBRARY_PATH pointing at the locally-built Tauri
# toolchain (webkit2gtk/libsoup/etc.) under /tmp. RUSTFLAGS moved to
# `.cargo/config.toml` ([build] rustflags). PATH prepended so
# cargo-installed binaries (just itself, etc.) are findable from
# non-interactive shells (git hooks, CI) that don't source ~/.bashrc.
# Harmless on machines without /tmp/tauri-* — ld only consults -L
# paths when a missing symbol needs them.
export PATH := env_var("HOME") + "/.cargo/bin:" + env_var("HOME") + "/.local/bin:" + env_var("PATH")
export PKG_CONFIG_PATH := "/tmp/tauri-pc"
export LIBRARY_PATH := "/tmp/tauri-libs:/usr/lib/x86_64-linux-gnu"

default: ci

# WHY nextest not `cargo test`: SQLite lock-order inversion in
# unixClose vs unixLock-from-WAL-close deadlocks when multiple Connection
# handles to the same DB drop concurrently (caught locally + on CI several
# times during arch-audit). cargo nextest's process-per-test isolation
# eliminates the race. The `no-cargo-test` recipe below enforces the rule
# repo-wide so accidental reintroduction fails CI.
test:
    cargo nextest run --workspace --all-targets

# Forbid `cargo test` invocations in committed automation files.
# Doctest forms (cargo test --doc / --workspace --doc) are exempt because
# nextest doesn't run doctests. See scripts/no-cargo-test.sh + CLAUDE.md.
no-cargo-test:
    ./scripts/no-cargo-test.sh

# Every tracked shell script must be mode 100755 in the git INDEX. A 100644
# script is invisible on clones with core.fileMode=false and only surfaces as
# "permission denied" on a runner. See scripts/check-exec-bits.sh + GH #183.
#
# WHY `sh scripts/...` and not `./scripts/...` like no-cargo-test above: this
# is the one script whose own exec bit must not gate it, or a missing bit on
# the checker produces the same bare "permission denied" it exists to explain.
exec-bits:
    sh scripts/check-exec-bits.sh

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

doctest:
    cargo test --workspace --doc

mdbook-test:
    cargo test --workspace --doc -- --show-output

docs-coverage:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

fmt-check:
    cargo fmt --all -- --check

fmt:
    cargo fmt --all

build-frontend:
    cd apps/desktop && bun install --frozen-lockfile && bun run build

test-frontend:
    cd apps/desktop && bun install --frozen-lockfile && bun run test

lint-frontend:
    cd apps/desktop && bun install --frozen-lockfile && bun run lint

# Regenerate apps/desktop/src/bindings.ts via tauri-specta and verify
# it doesn't drift from the committed copy. Mirrors the CI bindings-drift
# job. Run pre-push when you've touched a #[tauri::command] signature, a
# specta-derived type, or a CoreError variant.
bindings:
    cargo build -p perima-desktop --features specta-export
    git diff --exit-code apps/desktop/src/bindings.ts

# Fetch the ffmpeg static binary used by perima-desktop's externalBin
# sidecar slot. Required before any `cargo build/clippy/test` of
# `perima-desktop`: tauri-build validates externalBin paths during every
# compile (issue tauri-apps/tauri#14602). Linux ships the real binary;
# macOS + Windows write a stub until T12 follow-up issues land.
sidecar:
    ./scripts/fetch-ffmpeg-sidecar.sh

# WHY `cd crates/desktop`: the Tauri CLI resolves tauri.conf.json relative
# to its own cwd, and this repo keeps that file in crates/desktop rather
# than the conventional src-tauri/. Running it from anywhere else fails
# with a config-not-found error.
#
# WHY the node_modules path rather than a bare `tauri`: @tauri-apps/cli is
# a devDependency of apps/desktop, so the binary is never on PATH. The
# relative form is safe here because just always runs recipes from the
# justfile's directory regardless of the caller's cwd — which is exactly
# the trap when typing this by hand from a subdirectory.
#
# The vite dev server starts itself via `beforeDevCommand` in
# tauri.conf.json, so this is the only command needed.
# Launch the desktop app in dev mode (vite + Tauri).
dev:
    cd crates/desktop && ../../apps/desktop/node_modules/.bin/tauri dev

deny:
    cargo deny check

typos:
    typos

# Thin gate — per-commit surface (matches lefthook pre-commit).
ci-fast: fmt-check typos exec-bits lint-frontend

# Thick gate — pre-push + manual surface. Equivalent to old `ci`;
# kept for back-compat so `just ci` still runs the full pipeline.
ci: fmt-check clippy test no-cargo-test exec-bits doctest docs-coverage deny typos build-frontend test-frontend lint-frontend

verify:
    @if command -v cargo-kani >/dev/null 2>&1; then \
        cargo kani --workspace; \
    else \
        echo "cargo-kani not installed; skipping"; \
    fi

install-hooks:
    bunx lefthook install
