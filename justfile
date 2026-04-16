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

build-frontend:
    cd apps/desktop && bun install --frozen-lockfile && bun run build

test-frontend:
    cd apps/desktop && bun install --frozen-lockfile && bun run test

lint-frontend:
    cd apps/desktop && bun install --frozen-lockfile && bun run lint

ci: fmt-check clippy test doctest docs-coverage build-frontend test-frontend lint-frontend

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
