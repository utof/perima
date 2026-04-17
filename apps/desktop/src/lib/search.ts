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
    .replace(/[()"]/g, "")   // bare parens and bare quotes
    .replace(/(^|\s)-/g, "$1"); // leading dash on a token
}
