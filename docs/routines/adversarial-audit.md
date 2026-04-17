# Adversarial Audit Routine — Design Doc

**Status:** design proposal, not yet implemented
**Date:** 2026-04-16
**Author:** Claude (session with utof)
**Intended reader:** human or AI agent investigating how to turn this into an actual routine / GitHub Action / scheduled job.

---

## TL;DR

We want a scheduled automation that adversarially audits the `utof/perima` repo after each phase of work, files GitHub issues for any real bugs it finds, and stays honest over time (low false-positive rate, reproducible output, verifiable proof per claim). The hard part is not "tell an LLM to find bugs" — the hard part is preventing the LLM from *inventing* bugs to satisfy the directive.

This doc specifies **what the routine must do**, **the discipline required to prevent finding-inflation**, **the prompt template**, **two infrastructure options (Claude-only vs Claude+codex cross-model)**, and **the feedback loop to detect drift**.

A working example of this routine's effective output already exists in this repo's history: the `codex:codex-rescue` pass run on 2026-04-16 against phase 4 (v0.4.0..v0.4.1) produced 1 CRIT + 4 HIGH + 3 MED + 4 LOW findings, all with `file:line` pointers, all verifiable. That output is the target quality bar. See: GH issues #15, #16, #17, #18.

---

## Motivation

### Why we want adversarial audits

Per-commit code review (what we already do via `superpowers:code-reviewer` dispatches) catches local correctness issues. It does NOT reliably catch:

- Cross-commit regressions (a fix in commit A undoes a guarantee established in commit X).
- Silent architectural drift (`FileRepository &mut self` vs `Arc<Repo>` tension — latent for months before biting).
- Security gaps introduced by innocuous-looking config (Tauri `**` wildcard scope).
- Dead state (columns written but never read, or vice versa).
- Coverage gaps where the obvious broken path has no regression test.

Adversarial review — one agent specifically tasked to find what's wrong rather than validate what was requested — is a well-known pattern for catching these. We proved it works here: the codex:rescue pass caught 5 serious bugs (including 1 security issue) that all the per-commit reviewers missed.

### Why LLM-driven adversarial review has a failure mode

Instructing an LLM to "find issues" is a reward-hacking invitation. The model has strong priors that any non-trivial code has bugs somewhere, so it will invent plausible-sounding findings when none exist. Without discipline, the output degrades into:

- "This code smells" (no observation can refute it).
- "Could race under extreme load" (no demonstration).
- "Consider refactoring X to Y" (taste, not correctness).
- Findings that repeat from run to run with slight rewording.

After a few cycles of this the human stops reading the routine's output → routine becomes dead weight → we're worse off than having no routine.

**The fix is not "prompt nicely." The fix is structural — make the routine physically unable to file a finding without attaching a falsifiable, verifiable proof artifact.**

---

## The three disciplines

### 1. Falsifiability

A finding must be expressible as: *"if condition X holds, observable behavior Y occurs."* If no observation could refute it, the finding is prose and must be dropped.

- **Passes:** "`upsert_metadata` produces duplicate active rows under concurrent calls from two connections." You can construct the concurrent call and observe.
- **Passes:** "Tauri `convertFileSrc` with scope `**` resolves any absolute path." You can convertFileSrc("/etc/passwd") and observe the URL.
- **Fails:** "This code feels over-engineered." Nothing could prove this wrong.
- **Fails:** "Error handling could be better." Neither specific nor falsifiable.

### 2. Reproducibility

The same commit audited twice must produce the same finding set (or at least overlap ≥ 80%). This requires:

- **Canonical finding IDs.** Each finding has a stable hash = `sha256(file_path + line_range + claim_kind_enum)`. Finding IDs that match an existing open or closed issue are skipped — never re-filed.
- **Bounded output.** Max 5 findings per run. If more candidates survive filtering, rank by severity and file top-N; mention the rest in the audit-log for human review. This prevents the "generate 20 findings to look thorough" failure mode.
- **Deterministic scan order.** Alphabetical by file path. Makes cross-run diffs meaningful.

### 3. Verifiability (the hard one)

Every finding must attach exactly ONE **mechanically executable** proof artifact:

**(a) Failing Rust test.** Copy-pastable snippet in `crates/*/tests/*.rs` format that fails against current HEAD. The routine must mentally execute the test against the actual code path and confirm the assertion would fail.

**(b) Shell reproducer.** Exact command sequence + expected (wrong) output. A human or CI can run it verbatim and observe the bug.

**(c) Authoritative citation.** URL with line anchor to the library's source or docs (e.g. `github.com/image-rs/image/blob/v0.25.0/src/codecs/webp.rs#L42`) and a quoted sentence of the relevant claim.

**(d) Regression SHA.** `git log -S <token>` output identifying the commit that introduced the bug, plus the diff hunk demonstrating the behavior flip.

Prose-only claims are **forbidden**. No exceptions.

This discipline biases findings toward mechanical bugs (races, SQL parse errors, CRDT schema drift, security scopes, dead state) and away from design smells (bad abstractions, ergonomics critique). That's the correct bias for this routine — design smells need a human.

---

## The hardened prompt

```
Role: Adversarial auditor for utof/perima. HARD CONTRACT: no finding
without executable proof. Hallucinated findings waste engineering
time and erode trust in this routine.

## Discipline — read before generating

1. FALSIFIABILITY. Every finding MUST name the observation that would
   prove it wrong. If you can't name one, drop the finding.

2. EXECUTABLE PROOF. Every finding MUST attach ONE of:
   (a) A Rust test (crates/*/tests/*.rs format) that copies verbatim
       into the repo and FAILS on the current HEAD. Mentally execute
       it: step through the code path, confirm the assertion will fail.
       If uncertain, sharpen or drop.
   (b) A shell reproducer: exact commands + expected (wrong) output.
       Must be runnable against the actual current code.
   (c) A library-behavior citation: URL with line anchor + quoted
       sentence.
   (d) A regression introduced by a specific commit: git log -S output
       + diff hunks.

   Prose-only claims are FORBIDDEN.

3. CONFIDENCE + STEELMAN. For each candidate finding:
   - Declare confidence: HIGH (proof directly demonstrates bug),
     MEDIUM (likely, edge cases), LOW (might be wrong).
   - Write ONE sentence steelmanning AGAINST the finding.
   - If the steelman is compelling (>20% the code is right),
     DROP the finding. Note in audit-log.
   - Only HIGH + MEDIUM get filed. LOW goes to the audit-log.

4. CANONICAL FINDING ID. Compute:
     finding_id = sha256(
       file_path_canonical + ":" + line_range + ":" + claim_kind
     )
   where claim_kind ∈ {race, sql_parse, unhandled_error,
     security_scope, crdt_gap, dead_state, test_gap, api_misuse,
     memory_leak, correctness_bug}.
   Before filing: gh issue list --search "finding_id:<hex>". If
   found (open OR closed), DO NOT RE-FILE. Note as "skipped (dup of
   #N)" in the audit-log.

5. OUTPUT BUDGET. Max 5 findings per run. If more candidates survive
   filters, file top 5 by severity; list the rest in the audit-log
   for human review.

6. READ-ONLY. No code modifications. Read + analyze only.

## Input

- Git range: <last-tag>..HEAD (or last 14 days, whichever is larger).
- Repo: utof/perima.
- Files in scope: every .rs, .sql, .ts, .tsx touched in the diff.
- Constraint: CLAUDE.md at repo root — obey schema + CRDT rules when
  evaluating findings against it.

## Output per finding (markdown issue body template)

**finding_id:** `<hex>`
**severity:** CRIT | HIGH | MED
**confidence:** HIGH | MEDIUM
**file:line:sha:** `crates/db/src/foo.rs:123 (commit abc123)`
**claim_kind:** `race`

## Claim
<One sentence. "X produces Y when Z.">

## Falsifier
<One sentence. "This claim is wrong if W holds.">

## Proof artifact
<Exactly ONE of (a), (b), (c), (d). Copy-pastable / clickable.>

## Steelman
<One sentence arguing FOR the code being correct.>

## Why I filed anyway
<One sentence defeating the steelman.>

## Why this matters
<User-visible impact OR downstream phase blocked.>

---

Issue labels: `adversarial-audit` + one of `bug` / `security` /
`enhancement`.
Title: `[sev/conf] area: <short>` (e.g. `[HIGH/HIGH] db: upsert_metadata
clobbers thumbnail state`).

## Audit-log comment (every run — even empty ones)

gh issue comment <AUDIT_LOG_ISSUE_NUMBER> --body <<EOF
### Audit run YYYY-MM-DD
- Git range: <tag>..<sha>
- Commits scanned: N
- Findings filed: K (#a, #b, ...)
- Findings skipped (duplicate of existing issue): M — [links]
- Findings dropped at steelman: J — [one-line reasons]
EOF
```

Why the audit-log comment matters: **transparency about what the
routine considered and rejected.** If a human spots a bug manually
that the routine dropped at steelman, that's a prompt-tuning signal.

---

## Infrastructure options

### Option A — Claude routine, Claude-only

**What:** A scheduled Claude Code routine (per the Nov 2025 routines
feature) runs the hardened prompt weekly.

**Setup:**
1. Log in to Claude Code's routine UI.
2. Create a new scheduled routine with cadence `0 22 * * 0` (Sundays
   22:00 UTC).
3. Paste the hardened prompt above.
4. Connect the `utof/perima` GitHub repo.
5. Authorize `gh` connector (so the routine can `gh issue create`).

**Pros:**
- No extra infra beyond the Claude Code routine UI.
- No secondary API key.
- Prompt iterations are a single-file edit.
- Uses the same model family we've been using — consistent with the
  rest of the dev loop.

**Cons:**
- Same-model-family reviewing its own recent work is less independent
  than cross-model. Claude's blind spots stay blind.
- Limited to whatever the routine runtime supports (tool access,
  file read bandwidth, output length).

**Recommended for:** MVP. Run it for a month, measure false-positive
rate, decide whether to upgrade to Option B.

### Option B — Claude routine + codex via GitHub Action (cross-model)

**What:** A GitHub Action runs `codex exec` with the hardened prompt
an hour before the Claude routine; emits findings as structured JSON;
posts to the audit-log issue as a comment. The Claude routine reads
that comment, applies the dedup + steelman filter, and files issues.

**Architecture:**

```
┌─────────────────────┐           ┌─────────────────────────┐
│ GH Action (scheduled)│           │ Claude routine          │
│ Sun 21:00 UTC        │           │ Sun 22:00 UTC           │
│                      │           │                         │
│ - checkout repo      │           │ - read audit-log comment│
│ - install codex CLI  │           │ - apply filters         │
│ - run codex exec     │  artifact │ - file issues           │
│   with hardened      │──────────▶│ - write audit-log       │
│   prompt             │  (JSON)   │                         │
│ - gh issue comment   │           │                         │
│   AUDIT_LOG with     │           │                         │
│   findings.json      │           │                         │
└─────────────────────┘           └─────────────────────────┘
```

**Setup sketch:**

`.github/workflows/adversarial-audit.yml`:

```yaml
name: adversarial-audit (codex)

on:
  schedule: [{ cron: "0 21 * * 0" }]   # Sun 21:00 UTC
  workflow_dispatch:

jobs:
  codex-audit:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      issues: write
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }

      - name: Install codex CLI
        run: npm install -g @openai/codex   # or whichever package name applies at the time

      - name: Run codex adversarial audit
        run: |
          codex exec --cd . \
            --output findings.json \
            --prompt-file .github/adversarial-audit-prompt.md
        env:
          OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}

      - name: Post findings to audit-log
        run: |
          gh issue comment ${{ vars.AUDIT_LOG_ISSUE_NUMBER }} \
            --body-file findings.json
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
```

`.github/adversarial-audit-prompt.md` contains the hardened prompt
from above.

The Claude routine then runs at 22:00 UTC, pulls the fresh comment,
processes.

**Pros:**
- Genuinely cross-model (GPT-5-class reviewing Claude-generated code
  and vice versa).
- Catches blind-spot classes that a same-model review misses.
- Both models apply the SAME discipline (proof-required contract).
  Any finding surviving both passes is extremely likely to be real.

**Cons:**
- Two systems, two API keys, two failure modes.
- OpenAI billing per run.
- More setup + more moving parts to maintain.
- Latency: 1-hour gap between the two runs for the handoff.

**Recommended for:** mature projects where false-negative cost is
high. For `perima` in its pre-v1 state, Option A is enough.

### Option C — pure GitHub Action (no routine at all)

If routines aren't available / too expensive / undesired: a single
scheduled GitHub Action that invokes an LLM CLI (codex or Claude API)
with the hardened prompt and files issues via `gh`.

**Pros:** fully self-hosted, no Claude Code routine dependency.
**Cons:** you lose Claude Code's persistent context across runs (each
run starts fresh with no memory of previously-dismissed findings
beyond what the `finding_id` dedup catches).

---

## Feedback loop — detecting drift over time

A single adversarial routine left unsupervised drifts toward one of
two failure modes:

- **Over-eager:** too many false positives. Humans stop reading.
- **Over-conservative:** too many false negatives. Bugs slip through.

The audit-log issue is the monitoring surface. Three labels track drift:

- **`adversarial-false-positive`** — applied manually when a
  routine-filed issue is closed as wrong / not-a-bug. Tracks over-
  eagerness.
- **`adversarial-missed`** — applied manually when a bug is found the
  hard way (production failure, manual code review) and we realize the
  adversarial audit should have caught it. Tracks over-conservatism.
- **`adversarial-audit`** — applied to every finding; the universe.

### Monthly drift check (15 min, manual)

1. Count: `gh issue list --label adversarial-false-positive --state closed --search "closed:>=YYYY-MM-DD"`.
2. Divide by total closed with `adversarial-audit` in the same window.
3. If false-positive ratio > 20%: prompt is too eager. Tune: raise
   the steelman bar (`>40% the code is right → drop`).
4. If false-negative (missed) count > 1 per month: prompt is too
   conservative. Tune: lower the steelman bar (`>10%` → drop).

### Monthly honesty check (also 15 min)

Pick 3 random adversarial-audit issues filed in the month. For each:

1. Copy the proof artifact.
2. Paste into the repo / run the command.
3. Confirm: does the test fail? Does the repro output match?
4. If 3/3 check out → routine is honest. Continue.
5. If ≤ 2/3 check out → the routine is claiming proofs that don't
   actually reproduce. Add the failing example as a NEVER-FILE exemplar
   in the prompt and re-deploy.

---

## The tradeoff — what this routine does NOT do

**Design smells:** abstractions, API ergonomics, module cohesion.
Falsifiability rules them out. They need a human reviewer.

**Performance regressions:** without instrumented benchmarks, claims
are pretend. Consider a separate benchmarking routine if performance
starts to matter.

**Cross-component integration behavior:** the routine reads diffs,
not the whole codebase. A bug that requires understanding 4 modules
at once might slip. Mitigate by periodically running a larger
whole-codebase audit (like the phase-4 codex:rescue we did).

---

## Implementation roadmap

For a human or AI to actually build this:

### Phase 1 — scaffolding (2-4 hours)

1. Create the audit-log tracking issue:
   ```bash
   gh issue create --repo utof/perima \
     --title "Audit-log: adversarial-audit routine runs" \
     --label adversarial-audit \
     --body "Running log of adversarial-audit findings, skips, and drops. See docs/routines/adversarial-audit.md."
   ```
   Note the issue number → `AUDIT_LOG_ISSUE_NUMBER` env var below.

2. Save the hardened prompt as `.github/adversarial-audit-prompt.md`
   (COMMITTED — this one .md file is allowed despite repo's gitignore
   rule, since it's required by the CI workflow. Add `!.github/adversarial-audit-prompt.md` to `.gitignore`).

3. Pick Option A (Claude routine) or Option B (Action+routine hybrid)
   and configure.

### Phase 2 — first run + calibration (1 week)

4. Run the routine manually once via `workflow_dispatch` (Option B) or
   "Run now" button (Option A).
5. Read every filed issue. For each, verify the proof artifact.
6. Close any that are wrong, labeling `adversarial-false-positive`.
7. File any bugs the routine should have caught but didn't, labeling
   `adversarial-missed`.
8. Tune the prompt: if false-positive rate > 20%, raise steelman bar;
   if false-negative rate > 1 / week, lower it.

### Phase 3 — steady state (monthly review)

9. Once false-positive rate is consistently < 20% and false-negative
   rate < 1/month, declare the routine "calibrated."
10. Monthly 15-min drift check per the protocol above.
11. When phase structure changes (e.g., starting phase 5+), refresh
    the prompt's "files in scope" section.

### Phase 4 (optional) — upgrade to Option B cross-model

12. If Option A's findings plateau (same classes of bugs caught, blind
    spots to other classes), add the codex Action.
13. Compare: what does codex find that Claude misses? What does
    Claude find that codex misses?
14. Keep both running; they're complementary.

---

## Proof-of-concept already in the repo

The `codex:codex-rescue` adversarial pass run on 2026-04-16 against
phase 4 (commit range `v0.3.2..v0.4.1`) produced findings matching
this spec's quality bar. See:

- GH issue #15 (CRIT + HIGH v0.4.2 hotfix meta)
- GH issue #16 (MED queue cancellation + HIGH TOCTOU)
- GH issue #17 (MED test coverage gaps)
- GH issue #18 (LOW architectural cleanups)

Every finding in those issues had a `file:line` pointer. Most had
implicit proof artifacts (code snippets quoted, behavior descriptions
matched to the diff). The routine's job is to formalize that quality
level and demand the proof artifacts explicitly — so even when a
run is sloppy, the structure forces accountability.

---

## Open questions for the implementer

1. **Claude routine vs Anthropic API + GitHub Action?** The routine
   feature is Claude Code-specific. If the implementer doesn't have
   Claude Code access, they could run the hardened prompt via a
   scheduled Action that calls the Anthropic API directly. Same
   prompt, different harness.

2. **Finding ID canonicalization.** `file_path_canonical` — normalize
   to forward slashes, strip repo prefix. `line_range_canonical` —
   use `start-end` of the primary defect location, not the full
   surrounding context. `claim_kind_canonical` — the small enum is
   non-negotiable; bigger enums create more ID collisions.

3. **What happens if the routine crashes mid-run?** It should be
   idempotent — re-running with the same commit range produces the
   same findings (by ID). Partial filings are OK because dupes are
   skipped.

4. **Permission model.** The routine needs `issues:write` + `contents:read`.
   The GH Action route uses `GITHUB_TOKEN` with scoped permissions.
   A Claude routine needs equivalent — verify Claude Code's connector
   model supports issue creation without broader repo access.

5. **Chain to autonomous-fix routine?** A separate routine could pick
   up `[HIGH/HIGH] adversarial-audit` labeled issues and attempt a
   draft PR. The human stays in the loop for judgment calls. Not in
   scope for this doc — flagged as natural follow-up.

---

## Summary for a fresh reader

- Adversarial audits catch real bugs that per-commit review misses.
- LLM-driven adversarial audits hallucinate if you don't enforce
  falsifiability, reproducibility, and verifiability.
- The hardened prompt above encodes those disciplines structurally:
  no proof → no filing.
- Option A (Claude routine) is the MVP. Option B (Claude + codex) is
  the upgrade path when blind spots appear.
- Monthly 15-min feedback loop (false-positive + false-negative
  tracking) keeps the routine honest.
- Bias toward mechanical bugs; leave design review to humans.

The codex:rescue pass on phase 4 is the working example this routine
systematizes.
