#!/bin/sh
# Enforce: every tracked shell script is mode 100755 in the git INDEX.
#
# RATIONALE: a shell script committed as 100644 is a CI-only failure. CI
# invokes these directly (`just sidecar` -> `./scripts/fetch-ffmpeg-sidecar.sh`),
# so a non-executable script aborts the run with "permission denied" — after
# the runner has already spent minutes on toolchain setup.
#
# WHY this cannot be caught by looking at the filesystem: several dev setups
# here have `core.fileMode = false` (required when the worktree lives on a
# mount that reports every file as 0777 — exFAT/NTFS/VirtualBox shared
# folders). Under that config git IGNORES the on-disk permission bit
# entirely, and `ls -l` reports 0777 for everything. So the script looks
# executable locally, `ls -l` agrees, and it is still committed as 100644.
#
# The ONLY source of truth is the index: `git ls-files -s`, first field.
#
# Exempt: nothing. Every tracked *.sh / *.bash is expected to be runnable.
#
# See: lefthook.yml (pre-commit exec-bits) + GH #183.

set -eu

cd "$(git rev-parse --show-toplevel)"

# WHY `-F'\t'`: `git ls-files -s` emits "<mode> <sha> <stage>\t<path>".
# Splitting on tab keeps paths containing spaces intact; the mode is the
# first whitespace-delimited token of field 1.
violations=$(git ls-files -s -- '*.sh' '*.bash' \
    | awk -F'\t' '{ split($1, meta, " "); if (meta[1] != "100755") print $2 }')

if [ -n "$violations" ]; then
    cat >&2 <<EOF
ERROR: tracked shell script(s) are not executable in the git index.

These are committed as mode 100644. CI runs them directly, so this fails
the build with "permission denied" on a GitHub runner.

IMPORTANT — the file almost certainly LOOKS executable on your machine.
If your clone has \`core.fileMode = false\` (check: git config core.fileMode),
git ignores the on-disk permission bit and \`ls -l\` tells you nothing. The
index is the only thing that matters here, and the index says 100644.

Non-executable:
$violations

Fix (stages the mode change; no content change, no re-edit needed):
$(printf '%s\n' "$violations" | sed "s|^|    git update-index --chmod=+x '|; s|\$|'|")

Then re-run your commit. Verify with:
    git ls-files -s -- '*.sh' '*.bash'
EOF
    exit 1
fi

echo "exec-bits: clean ($(git ls-files -- '*.sh' '*.bash' | wc -l | tr -d ' ') scripts, all 100755)"
