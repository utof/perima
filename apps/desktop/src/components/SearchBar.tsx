import { useEffect, useRef, useState } from "react";
import * as api from "../api";
import { buildFtsQuery } from "../lib/search";
import type { SearchHit } from "../types";

/** Milliseconds to wait after the user stops typing before firing a search. */
const DEBOUNCE_MS = 300;

/**
 * Minimum query length before firing a search.
 *
 * WHY 2: single-char FTS5 queries on a large corpus are expensive and
 * produce noisy high-recall results. Two chars is the smallest window
 * that meaningfully narrows the index while staying responsive for
 * short tag names like "UI" or "JP".
 */
const MIN_QUERY_LEN = 2;

/**
 * Upper bound for a single search call.
 *
 * WHY 500: the v0.6.2 design re-sorts the visible list by BM25 rank
 * while a search is active. The list itself is capped at 100 rows via
 * `listFilesWithTags(100)`, so 500 is ≥ 5× headroom — enough to cover
 * the visible set even when many files outside the visible set also
 * match (they're filtered out by `composeVisible`). The Tauri `search`
 * command also clamps to 500 server-side (SEARCH_LIMIT_MAX in
 * crates/desktop/src/commands.rs).
 */
const SEARCH_LIMIT = 500;

interface SearchBarProps {
  /**
   * Fires whenever the debounced query resolves (with hits) or clears.
   *
   * Three cases for the two arguments:
   * 1. `(raw, hits)` — user typed ≥ MIN_QUERY_LEN; `hits` may be `[]` if
   *    the query matched nothing.
   * 2. `("", null)` — user cleared input (via ✕ or deleting chars) or
   *    typed less than MIN_QUERY_LEN. App should reset `searchHits` to
   *    `null` (distinct from `new Set()` — the latter means "searched,
   *    zero results").
   * 3. Same as #2 on backend error (swallowed; non-fatal).
   */
  onQueryChange: (query: string, hits: SearchHit[] | null) => void;
}

/**
 * Debounced FTS5 search input.
 *
 * WHY self-contained sanitiser + IPC call: keeps the input component
 * deciding *when* to query (debounce, min-length guard) while the
 * *what* (buildFtsQuery) lives in the shared lib/search module. The
 * parent App.tsx only needs to know about the resolved (raw, hits)
 * pair — not the FTS5 grammar.
 */
export default function SearchBar({ onQueryChange }: SearchBarProps) {
  const [query, setQuery] = useState("");
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Track whether the last fire was "cleared" to avoid refiring on
  // an already-cleared state when the user backspaces past MIN_QUERY_LEN.
  const clearedRef = useRef(true);

  useEffect(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    const trimmed = query.trim();

    if (trimmed.length < MIN_QUERY_LEN) {
      // Covers: empty input, 1-char input, whitespace-only input.
      // Fire clear exactly once per transition into the cleared state.
      if (!clearedRef.current) {
        clearedRef.current = true;
        onQueryChange("", null);
      }
      return;
    }

    timerRef.current = setTimeout(() => {
      const ftsQuery = buildFtsQuery(trimmed);
      if (ftsQuery === "") {
        // Sanitiser returned nothing (all chars were unsafe). Treat as cleared.
        clearedRef.current = true;
        onQueryChange("", null);
        return;
      }
      api.search(ftsQuery, SEARCH_LIMIT).match(
        (hits) => {
          clearedRef.current = false;
          onQueryChange(trimmed, hits);
        },
        () => {
          // WHY swallow: FTS5 parse errors on edge-case input
          // (unbalanced quotes after sanitiser, weird unicode). Showing
          // an empty result list is honest; a red banner would flash on
          // every keystroke that happens to produce transient bad input.
          clearedRef.current = false;
          onQueryChange(trimmed, []);
        },
      );
    }, DEBOUNCE_MS);

    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [query, onQueryChange]);

  function handleClear() {
    setQuery("");
    // The effect above will fire onQueryChange("", null) on the next render.
  }

  return (
    <div className="relative w-72">
      <div className="flex items-center bg-gray-700 rounded border border-gray-600 focus-within:border-blue-500">
        <span className="pl-3 text-gray-400 text-sm select-none">🔍</span>
        <input
          type="search"
          aria-label="Search files"
          placeholder="Search…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          className="flex-1 bg-transparent px-2 py-1.5 text-sm text-gray-100 placeholder-gray-400 outline-none"
        />
        {query && (
          <button
            type="button"
            aria-label="Clear search"
            onClick={handleClear}
            className="pr-3 text-gray-400 hover:text-gray-200"
          >
            ✕
          </button>
        )}
      </div>
    </div>
  );
}
