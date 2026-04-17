import type { FileWithTags } from "../types";

/**
 * Search-related pure functions. No React imports; test directly.
 *
 * WHY a separate module: composing search hits + tag filters + BM25
 * rank sort is the exact logic that broke in v0.6.1 (issue #25).
 * Pulling it out of App.tsx makes it unit-testable and pins the
 * regression invariant in `App.compose.test.tsx`.
 */

/**
 * Sanitise raw user input into an FTS5-safe query string.
 *
 * Supports three FTS5 patterns:
 * - Phrase query: input wrapped in `"..."` passes through.
 * - Prefix query: input ending in `*` becomes `"tokens"*`.
 * - Implicit AND: plain tokens get individually quoted + joined with
 *   spaces (FTS5 implicit-AND semantics).
 *
 * Pre-strips parse-unsafe characters (bare parens, leading dash, bare
 * unpaired quote) to avoid FTS5 parse errors on normal-ish typing.
 * The user loses access to FTS5's `AND / OR / NEAR / NOT` keywords as
 * a side effect — by design; post-v1 query DSL picks that up.
 */
export function buildFtsQuery(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed === "") return "";

  // Pass-through: explicit phrase query already wrapped.
  // WHY this check is first: otherwise the strip pass below would kill
  // the outer quotes and re-quote the inner tokens, losing the phrase.
  if (
    trimmed.length >= 2 &&
    trimmed.startsWith('"') &&
    trimmed.endsWith('"')
  ) {
    return trimmed;
  }

  // Prefix query: `foo*` at end of input → take everything up to `*`
  // as tokens, re-emit last token as `"last"*`, keep earlier tokens
  // as plain implicit-AND.
  if (trimmed.endsWith("*") && trimmed.length >= 2) {
    const withoutStar = trimmed.slice(0, -1);
    const sanitized = stripUnsafe(withoutStar);
    const tokens = sanitized.split(/\s+/).filter((t) => t !== "");
    if (tokens.length === 0) return "";
    // WHY: the length guard above guarantees tokens is non-empty, making !
    // safe here at runtime. Any future change to stripUnsafe must preserve
    // the invariant that a non-empty `sanitized` string produces at least
    // one token after split+filter, or this assertion will throw.
    const last = tokens.pop()!;
    const prefix = tokens.map((t) => `"${t}"`).join(" ");
    const suffix = `"${last}"*`;
    return prefix === "" ? suffix : `${prefix} ${suffix}`;
  }

  // Plain tokens: strip unsafe, split, quote each.
  const sanitized = stripUnsafe(trimmed);
  const tokens = sanitized.split(/\s+/).filter((t) => t !== "");
  return tokens.map((t) => `"${t}"`).join(" ");
}

/** Strip FTS5-unsafe characters before tokenisation. */
function stripUnsafe(s: string): string {
  return s
    // WHY: parens and bare `"` are stripped here rather than relying on the
    // phrase-passthrough guard above. That guard only short-circuits when the
    // entire input is wrapped in balanced quotes; unpaired quotes and mid-token
    // quotes that reach this path would produce malformed FTS5 syntax
    // (e.g. `foo"bar` causes a parse error). Stripping them unconditionally is
    // safe because the tokens are immediately re-quoted by the callers.
    .replace(/[()"]/g, "")
    // WHY: FTS5 treats a token whose first character is `-` as a negation
    // operator (e.g. `-foo` means NOT foo). We only strip the dash when it
    // appears at the very start of a token (start-of-string or after
    // whitespace) — not mid-token — so hyphenated words like `file-name` are
    // preserved. A word-boundary (`\b`) match would incorrectly strip dashes
    // inside tokens on some Unicode ranges; the `(^|\s)` anchor is precise.
    .replace(/(^|\s)-/g, "$1");
}

/**
 * Tally tag occurrences across a file set.
 *
 * WHY client-side: for v0.6.2 the list is capped at 100 rows
 * (`listFilesWithTags(100)`) so counts are cheap and reactive.
 * Facet counts reflect the visible result set only; full-corpus
 * counts are a post-v1 optimization (spec Non-goals).
 *
 * WHY id-keyed not name-keyed: tag names are mutable (user rename),
 * tag ids are UUIDv7-stable. Keying on id survives renames.
 *
 * Unordered; callers sort if display order matters.
 */
export function computeFacets(files: FileWithTags[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const f of files) {
    for (const t of f.tags) {
      counts[t.id] = (counts[t.id] ?? 0) + 1;
    }
  }
  return counts;
}

/**
 * Compose the visible file set from base list + optional tag + optional search hits.
 *
 * - `selectedTagId === null`: no tag filter.
 * - `searchHits === null`: no search active (returns tag-filtered list).
 * - `searchHits instanceof Set` with 0 entries: search active, zero matches
 *   (returns []). Distinct from null — this is the spec's `searchActive`
 *   invariant.
 *
 * THE core invariant of #25: when both filters are active, the result
 * is the INTERSECTION, not one overriding the other.
 */
export function composeVisible(
  files: FileWithTags[],
  selectedTagId: string | null,
  searchHits: Set<string> | null,
): FileWithTags[] {
  return files.filter((f) => {
    if (selectedTagId !== null && !f.tags.some((t) => t.id === selectedTagId)) {
      return false;
    }
    if (searchHits !== null && !searchHits.has(f.hash)) {
      return false;
    }
    return true;
  });
}

/**
 * Sort files by BM25 rank (ascending — lower = better per FTS5 convention).
 *
 * Files whose hash is not in `hitRanks` are appended to the end. In practice
 * `composeVisible` ensures every visible file has a hit (when searchHits is
 * non-null), so the fallback only fires on defensive misuse.
 */
export function sortByRank(
  files: FileWithTags[],
  hitRanks: Map<string, number>,
): FileWithTags[] {
  const ranked: Array<{ file: FileWithTags; rank: number }> = [];
  const unranked: FileWithTags[] = [];
  for (const f of files) {
    const r = hitRanks.get(f.hash);
    if (r === undefined) unranked.push(f);
    else ranked.push({ file: f, rank: r });
  }
  ranked.sort((a, b) => a.rank - b.rank);
  return [...ranked.map((x) => x.file), ...unranked];
}
