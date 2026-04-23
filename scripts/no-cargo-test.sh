#!/bin/sh
# Enforce: no `cargo test` invocations in committed automation files.
#
# RATIONALE: SQLite has a lock-order inversion in unixClose (GLOBAL→PER-INODE)
# vs unixLock-from-sqlite3WalClose (PER-INODE→GLOBAL) that deadlocks when
# multiple rusqlite::Connection handles to the same DB file are dropped
# concurrently. `cargo test --test-threads=N>1` triggers it in ~20% of runs;
# `cargo test --test-threads=1` STILL triggers it in ~33% of runs because
# the deadlock fires within a single test fn's internal threads.
#
# `cargo nextest run` isolates each #[test] in its own forked process →
# eliminates the concurrent-close race + slow-timeout self-terminates any
# single-test deadlock.
#
# Exempt: `cargo test --doc` and `cargo test --workspace --doc` because
# nextest doesn't run doctests.
#
# See: RESEARCH-sqlite-deadlock.md + CLAUDE.md ("test stack" section) +
# GH #121 (Adopt cargo-nextest).

set -eu

# Restrict scan to automation files. Markdown docs are free to mention
# `cargo test` in WHY-comments / explanations.
violations=$(grep -rEn 'cargo test( |$)' \
    --include='*.yml' \
    --include='*.yaml' \
    --include='justfile' \
    --include='Justfile' \
    --include='lefthook.yml' \
    --include='*.sh' \
    --include='*.bash' \
    . 2>/dev/null \
    | grep -v -- '--doc' \
    | grep -v 'scripts/no-cargo-test.sh' \
    || true)

if [ -n "$violations" ]; then
    cat >&2 <<EOF
ERROR: 'cargo test' invocation(s) found in committed automation files.

Use 'cargo nextest run' instead. SQLite has a lock-order inversion in
unixClose vs unixLock that deadlocks when multiple Connection handles to
the same DB are dropped concurrently. cargo nextest's process-per-test
isolation prevents the race. Exempt: 'cargo test --doc' (nextest doesn't
run doctests).

Found at:
$violations
EOF
    exit 1
fi

echo "no-cargo-test: clean (0 violations)"
