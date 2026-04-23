/**
 * Search input — dual-field store dispatch.
 *
 * `searchQuery` (raw input) drives the `<input value>` for responsive
 * typing. `debouncedQuery` (post-300ms-debounce + sanitised) is what
 * `useSearch` keys on; SearchBar dispatches both. IPC fires only on
 * debounce-fire, not per keystroke.
 *
 * Existing concerns preserved: MIN_QUERY_LEN floor, buildFtsQuery
 * sanitiser (escape FTS5 metacharacters), clearedRef deduplication.
 */
import { useEffect, useRef } from "react";
import { useUiStore } from "../stores/ui";
import { MIN_QUERY_LEN } from "../queries/search";
import { buildFtsQuery } from "../lib/search";

const DEBOUNCE_MS = 300;

export default function SearchBar() {
  const searchQuery = useUiStore((s) => s.searchQuery);
  const setSearchQuery = useUiStore((s) => s.setSearchQuery);
  const setDebouncedQuery = useUiStore((s) => s.setDebouncedQuery);
  const clearedRef = useRef(false);

  useEffect(() => {
    if (searchQuery.length < MIN_QUERY_LEN) {
      // Empty / too-short input — clear the debounced query (which
      // disables `useSearch`). Dedupe via clearedRef so we don't
      // setState every keystroke when already cleared.
      if (!clearedRef.current) {
        setDebouncedQuery("");
        clearedRef.current = true;
      }
      return;
    }
    clearedRef.current = false;
    const timer = setTimeout(() => {
      setDebouncedQuery(buildFtsQuery(searchQuery));
    }, DEBOUNCE_MS);
    return () => { clearTimeout(timer); };
  }, [searchQuery, setDebouncedQuery]);

  return (
    <input
      type="search"
      placeholder="Search…"
      value={searchQuery}
      onChange={(e) => { setSearchQuery(e.target.value); }}
      className="px-3 py-1.5 bg-gray-900 text-gray-100 rounded border border-gray-700 focus:outline-none focus:ring-2 focus:ring-blue-500 text-sm w-64"
    />
  );
}
