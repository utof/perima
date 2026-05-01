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
import { MagnifyingGlassIcon } from "@phosphor-icons/react";
import { useUiStore } from "../stores/ui";
import { MIN_QUERY_LEN } from "../queries/search";
import { buildFtsQuery } from "../lib/search";

// WHY 180 (was 300, lowered 2026-04-25 by 40%): user feedback that 300ms
// felt sluggish for incremental typing. 180ms still coalesces fast typists
// (avg keystroke gap ~150-200ms) so we don't issue an IPC per keystroke,
// but feels "live" on slow typing or paste.
const DEBOUNCE_MS = 180;

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
    <div className="relative">
      <MagnifyingGlassIcon
        size={16}
        weight="regular"
        className="absolute left-3 top-1/2 -translate-y-1/2 text-muted-foreground pointer-events-none"
      />
      <input
        type="search"
        placeholder="Search files, tags, paths…"
        aria-label="Search files"
        value={searchQuery}
        onChange={(e) => { setSearchQuery(e.target.value); }}
        className="rounded-full bg-input text-foreground placeholder:text-muted-foreground
                   pl-9 pr-4 py-2 text-sm border border-border w-64
                   focus-visible:outline-none focus-visible:ring-2
                   focus-visible:ring-ring focus-visible:ring-offset-0
                   focus-visible:border-ring transition-colors duration-micro"
      />
    </div>
  );
}
