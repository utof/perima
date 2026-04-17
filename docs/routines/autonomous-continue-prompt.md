# Autonomous Continue — Cloud Trigger Prompt

Paste this prompt into a Claude Code scheduled trigger (https://claude.ai/code/scheduled) when you want a remote agent to pick up perima development from the current `main` branch state. Agent picks the first matching priority and ships tight, reviewed commits.

**Required trigger config:**
- Repo source: `https://github.com/utof/perima`
- Model: `claude-sonnet-4-6` (or newer)
- Allowed tools: `Bash, Read, Write, Edit, Glob, Grep, WebFetch, WebSearch, Agent, Task`
- Cron: any interval you like (minimum 1h); start with `0 */4 * * *` (every 4h) and tune.
- Environment: any `anthropic_cloud` environment with apt available for Tauri deps.

**Key binding rules baked into this prompt:**
1. One plan task = one commit — never bundle.
2. Commit scope must match the majority (≥50%) of changed file paths.
3. Every `feat`/`fix` commit must be followed by two-stage review (spec + code quality). Zero review-fix commits across a phase = red flag; must justify in the release body.
4. Any migration with sync triggers must have tests for rename + soft-delete + multi-row paths, not just insert/update happy paths.
5. Any UI change must include `headless-tested: yes|no` with justification in the commit body.
6. Pre-commit hook runs `just ci`; fix root causes, never skip.

---

## PROMPT (paste verbatim)

You are continuing autonomous development on perima (cross-platform Rust media asset manager, github.com/utof/perima). Work on `main` branch. You have no prior session context — everything you need is in this prompt, the repo, or reachable via WebFetch/WebSearch.

# FIRST READ THESE COMMITTED FILES
- `CLAUDE.md` — binding rules. NEVER violate these.
- `CHANGELOG.md` — what has shipped.
- Latest `docs/superpowers/plans/*.md` — current phase's plan.
- Latest `docs/superpowers/specs/*.md` — current phase's spec.
- `docs/routines/adversarial-audit.md` if it exists — review discipline.
- Earlier phase plans/specs — reference for existing patterns.

# DISCOVERY — ALWAYS DO THIS FIRST EACH RUN
```
git log --oneline -30
git tag --sort=-version:refname | head -10
gh issue list --state open
ls docs/superpowers/plans/ | tail -5
```
Then determine current phase/task. Run `just ci` (with env setup below) to confirm green baseline.

# ENV SETUP (required before any cargo/just)
```
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
export PKG_CONFIG_PATH="/tmp/tauri-pc:/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig"
export LIBRARY_PATH=/tmp/tauri-libs:/usr/lib/x86_64-linux-gnu
export RUSTFLAGS="-L/tmp/tauri-libs -L/usr/lib/x86_64-linux-gnu"
```
If Tauri system libs are missing, install via `sudo apt-get install -y libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev libglib2.0-dev libatk1.0-dev libgdk-pixbuf-2.0-dev libgtk-3-dev`. If `just` is not installed, `cargo install just`.

# ==========================================================================
# BINDING PROCESS RULES — violations = aborted task + retry next run
# ==========================================================================

## Rule 1: ONE plan task = ONE commit. NEVER bundle.
If the plan has `Task 1: feat(db)` and `Task 2: feat(core)`, ship them as TWO separate commits. Even if Task 2 is only 20 lines. The plan's granularity is the commit granularity. If a task feels too small to be its own commit, split it further. Never merge.

## Rule 2: Commit scope MUST match majority of changed files.
Before committing, run `git diff --stat --staged` and count by path:
- `crates/core/**` → `feat(core)` / `fix(core)`
- `crates/db/**` → `feat(db)` / `fix(db)`
- `crates/cli/**` → `feat(cli)` / `fix(cli)`
- `crates/desktop/**` OR `apps/desktop/**` → `feat(desktop)` / `fix(desktop)`
- `crates/media/**` → `feat(media)` / `fix(media)`
- `.github/**` or CI config → `chore(ci)`
- `Cargo.toml` + version bumps → `chore(release)`
- `docs/**` → `docs(specs)` / `docs(plans)` / `docs(routines)` / `docs(...)` appropriate sub-scope
If any single scope owns ≥50% of changed lines, that IS the scope. If no scope owns ≥50%, split into multiple commits. MISLABELING = RED FLAG in review.

## Rule 3: Every `feat(...)` / `fix(...)` commit MUST be followed by TWO-STAGE REVIEW.
Use the Agent tool to dispatch subagents (subagent_type: general-purpose):

**Stage 1 — Spec-compliance reviewer.** Give it:
- Full task requirements copied from the plan file
- List of commits since the last review
- Instruction: READ THE ACTUAL CODE (not commit messages) and verify line-by-line
Verdict: ✅ compliant OR ❌ issues with file:line refs.

**Stage 2 — Code quality reviewer.** Give it:
- The same files + `git diff <prev-sha>..HEAD`
- Instruction: Report Strengths, Issues (Critical/Important/Minor), WHY-comment coverage, error handling, test coverage, commit scope accuracy.

For every Important+ issue: implement the fix as a SEPARATE `fix(scope): ...` commit. Push. Re-dispatch stage 1+2 on the fix. Proceed only when both stages are clean.

### ZERO REVIEW-FIX COMMITS ACROSS A PHASE IS A RED FLAG.
If you ship a phase release (chore(release)) with NO `fix(...)` commits in between feat commits, you must EXPLAIN in the release commit body WHY. Example: "feat commits reviewed; no changes needed — reviewers approved as-is". Missing this explanation = auto-fail of the phase; next run will revert.

## Rule 4: Migration test coverage for sync triggers.
Any migration that adds a trigger or side-table keyed off another table MUST include a test for EVERY mutation path the sync covers:
- INSERT on tracked tables
- UPDATE on tracked tables (including soft-delete + rename paths)
- DELETE / soft-delete on tracked tables
- Multi-row relationships (2 locations same hash, 2 tags same file, etc.)
If the spec says "sync on file_metadata and file_tags changes", tests MUST exercise metadata INSERT, metadata UPDATE, tag attach, tag detach. Missing any = test-coverage fail.

### Specifically for v0.6.x FTS5 followup (issue #22):
The shipped V006 migration is MISSING a trigger on `file_locations` UPDATE. When a user renames a file via `update_location_path`, the search index shows the OLD path. Fix with a V007 migration that adds AFTER UPDATE triggers on `file_locations(relative_path, deleted_at)` that update `search_rowid_map` + re-insert the FTS5 doc. Add a regression test that scans, renames, searches new name, asserts hit + searches old name asserts miss.

## Rule 5: Headless UI discipline.
Any Tauri command or React component change MUST have one line in the commit body: `headless-tested: yes|no` with explanation. `no` requires a justification ("UI validated via vitest only; runtime untested") and a follow-up task in the next phase plan to exercise via real Tauri dev/build.

## Rule 6: NEVER bundle plan tasks even if small.
Many small commits are always preferred over fewer large ones. Bundle = aborted + revert.

# OPERATIONAL REMINDERS (from CLAUDE.md)
- All work on `main`. No branches/worktrees. No `--force`, `--no-verify`, or `git amend`. New commits only.
- NO `Co-Authored-By:` trailers in commit messages.
- Autonomous mode: never stop to ask. If blocked, commit progress + push + stop. Next run picks up.
- Always `bun` for JS/TS.
- Pre-commit hook runs `just ci`; fix, don't skip.
- Release = semver tag via release-plz auto-tag on `chore(release): vX.Y.Z` commit bumping workspace version in Cargo.toml + apps/desktop/package.json + CHANGELOG.md.
- `**/*.md` is gitignored EXCEPT `CHANGELOG.md`, `CLAUDE.md`, `docs/superpowers/**/*.md`, `docs/routines/**/*.md`. Commit spec/plan updates so future cloud runs see them.

# TASK ORDER (pick the first that applies)

## PRIORITY 1 — Address open GH issues flagged HIGH
Check `gh issue list --state open --label bug` before picking phase work. Issue #22 (FTS5 stale-rename) is HIGH and blocks a clean v0.6.x. Ship fix as v0.6.2:
1. V007 migration + `file_locations` triggers (feat(db))
2. Regression test: scan, rename via update_location_path, search new+old names
3. chore(release) v0.6.2 — body notes it closes #22
4. push

## PRIORITY 2 — Phase 5c (if 5b is clean) — brainstorm next meta-plan phase
Read meta-plan spec + CHANGELOG to identify what's next. Likely candidates:
- Duplicate detection (already content-addressed via blake3 — needs UI surface)
- Sync / CRDT merge (long-term; complex)
- Mobile Expo + UniFFI shell
- Binary distribution (tauri bundles, issue #11)
Follow the CLAUDE.md phase loop: brainstorm + commit spec + commit plan + execute per-task with per-task reviews.

## PRIORITY 3 — Address other issues if time permits
#6 kani proofs, #10 release-plz tuning, #12 unused deps audit, #17 phase 4 test gaps, #18 arch cleanups, #19 pending-rows UX, #14 release-plz dep-graph. Pick one that matches your task budget.

# STOPPING CONDITIONS
- **Normal exit**: phase/task complete, tag shipped OR issue closed. Commit + push + stop.
- **Blocker**: test failure you can't fix, missing dep, spec ambiguity. Commit progress with a clear NEXT-STEP message in the commit body. Push. Stop. Next run picks up.
- **Budget**: if >60 min on one task, commit progress + push + stop.
- **Never leave `main` red**: CI failure after your change → `git revert HEAD` + push immediately OR forward-fix with a new commit. Never push a broken HEAD to main.

# RED FLAG CHECKS BEFORE YOUR FINAL MESSAGE
Before you finish, confirm ALL of these OR stop and fix:

1. `git log --oneline origin/main...HEAD` shows 0 commits (everything pushed)
2. Every `feat/fix` commit in your run has ACCURATE scope per Rule 2
3. Every `feat` commit in your run is followed by at least one `fix(...)` review-fix commit OR the release commit body justifies zero fixes
4. New migrations have tests for rename + soft-delete + multi-row paths, not just insert/update happy paths
5. Any UI change has a `headless-tested:` line in its commit body
6. `just ci` passed on the final HEAD
7. CHANGELOG.md updated if a tag was shipped

# GIT AUTH NOTE
If `git push` fails with auth error, GitHub app access isn't connected. Commit locally (so next run sees the work) + end your message with: "git push FAILED; user must run /web-setup to connect GitHub".

# DOCS YOU CAN FETCH IF NEEDED
- Tauri 2: https://tauri.app/reference/
- rusqlite: https://docs.rs/rusqlite/
- refinery: https://docs.rs/refinery/
- release-plz: https://release-plz.dev/docs
- SQLite FTS5: https://sqlite.org/fts5.html

Go. Start with discovery + `just ci`, then priorities 1→2→3.
