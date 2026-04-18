# Contributing to perima

perima is a solo-dev, AI-agent-dense project. This file documents conventions
so future contributors — human **or** AI — can file issues, write commits, and
land changes consistent with the existing workflow.

---

## How we work

- All code lands on `main`. No feature branches, no worktrees.
- Conventional Commits with component scopes (see below).
- Autonomous workflow: brainstorm → spec → plan → execute → review, primarily
  AI-driven. Human steering happens at checkpoints; see `CLAUDE.md` for rules.
- Releases are semver tags (`v0.N.x`) cut by `release-plz`.

### First-time setup

Install git hooks after a fresh clone so pre-commit (format + spellcheck)
and pre-push (tests + build + lint) fire automatically:

```bash
just install-hooks    # runs `bunx lefthook install`
```

**Prereqs:** `bun` (hooks invoke `bunx lefthook`). Install from
<https://bun.sh> or via your package manager. `cargo install lefthook` does
NOT work — the crate isn't on crates.io.

**Escape hatches** (use sparingly; CI is the authoritative gate):

- `LEFTHOOK=0 git commit …` — skip all hooks on commit.
- `LEFTHOOK=0 git push …` — skip all hooks on push.
- `LEFTHOOK_EXCLUDE=clippy,cargo-test git push …` — skip specific commands.

Prefer these over `--no-verify` (which is forbidden by project rule —
`LEFTHOOK=0` is auditable; `--no-verify` silently bypasses everything).

For manual full-pipeline runs: `just ci` (equivalent to the pre-push
surface + frontend lint). For the thin pre-commit surface only:
`just ci-fast`.

---

## Opening an issue

Use the templates in [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/).
Pick the category that best fits:

| Template | When to use |
|---|---|
| **Bug** | Something is broken or panicking |
| **Enhancement** | New capability or improvement to existing behaviour |
| **Meta** | Workflow, repo hygiene, tooling, process |
| **Idea** | Speculative direction — not committed to ship |

If you're unsure whether something is a bug or enhancement, open as **Meta**
and we'll re-label.

Blank issues are disabled — you must pick a template.

---

## Label vocabulary

Labels carry two orthogonal signals: **category** and **pre-work state**.
Apply both axes independently; they are not either/or.

### Category labels

| Label | Meaning |
|---|---|
| `bug` | Something isn't working as designed |
| `enhancement` | New feature or improvement |
| `chore` | Routine maintenance / repo hygiene (deps, CI tweaks, cleanup) |
| `meta` | Issue about the project's process, not the code itself |
| `idea` | Speculative; not committed to ship |
| `documentation` | Improvements or additions to docs only |
| `duplicate` | Duplicate of an existing issue |
| `invalid` | Not a valid bug / request |
| `wontfix` | Valid but deliberately not addressed |
| `question` | Needs clarification before actionable |
| `good first issue` | Low-complexity, well-scoped; good onboarding task |
| `help wanted` | Any size; extra attention or outside input welcome |
| `user-visible-ux` | UX defect or friction that end users encounter directly |

### Scope labels (v1 planning axis)

These answer "when does this ship?". Apply alongside a category label.

| Label | Meaning |
|---|---|
| `v1-polish` | Within v1 scope but non-critical; nice-to-have polish |
| `post-v1` | Out of scope for v1 per meta-plan; tracked for a future release |

Leave unlabelled if the issue is clearly pre-v1 must-have — that's the default
assumption. Apply `post-v1` or `v1-polish` only when deferral is a conscious
decision.

### Research / pre-work state labels

These answer "what needs to happen before implementation?". Apply when the issue
is not yet ready to execute. Can be combined (e.g. `needs-web-search` AND
`needs-brainstorm`).

| Label | Meaning |
|---|---|
| `needs-brainstorm` | Design decision unresolved; a brainstorming session is required before implementation |
| `needs-web-search` | Library or pattern choice should be web-verified against 2025-26 best practice (AI training data lags) |
| `needs-research` | Deep multi-source research required before committing to an approach |

Remove the label once the pre-work is done and captured in the issue body or a
linked spec.

---

## Commit conventions

Format: `type(scope): summary`

### Types

`feat` · `fix` · `refactor` · `test` · `chore` · `docs` · `ci` · `build` ·
`perf` · `style`

### Scopes

`core` · `db` · `fs` · `hash` · `cli` · `desktop` · `ci` · `deps` · `docs` ·
`release`

These match the crate / subsystem names in `CLAUDE.md`.

### Body

Write **why**, not what. The diff already shows what changed. A good body
answers: "why was this approach chosen over alternatives?"

### Trailers

`feat(*)`, `fix(*)`, and `test(*)` commits require:

```
headless-tested: yes|no
```

Set `yes` when the change has automated test coverage (unit / integration /
snapshot). Set `no` with a brief justification (e.g. `no (docs; template render
only verifiable via GH UI)`).

No `Co-Authored-By:` lines in this repo (local policy).

---

## PR model

We do not use PRs for most changes. Direct commits to `main` are the norm.
`chore(release): v0.N.x` commits trigger `release-plz` to cut a tag.

For large design changes that warrant human review before code is written:

1. Open a GH Discussion (if enabled) or a `meta` issue with the proposal.
2. Wait for explicit sign-off.
3. Commit to `main` after sign-off.

---

## More

- `CLAUDE.md` — binding rules for AI agents (and useful for humans too).
- `CHANGELOG.md` — versioned release log auto-maintained by `release-plz`.
- `docs/superpowers/` — phase specs and implementation plans (tracked for
  remote agents continuing work across sessions).
