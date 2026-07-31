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
