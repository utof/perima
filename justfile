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

deny:
    cargo deny check

typos:
    typos

# Thin gate — per-commit surface (matches lefthook pre-commit).
ci-fast: fmt-check typos lint-frontend

# Thick gate — pre-push + manual surface. Equivalent to old `ci`;
# kept for back-compat so `just ci` still runs the full pipeline.
ci: fmt-check clippy test no-cargo-test doctest docs-coverage deny typos build-frontend test-frontend lint-frontend

verify:
    @if command -v cargo-kani >/dev/null 2>&1; then \
        cargo kani --workspace; \
    else \
        echo "cargo-kani not installed; skipping"; \
    fi

install-hooks:
    bunx lefthook install
