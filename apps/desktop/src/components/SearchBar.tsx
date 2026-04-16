import { useEffect, useRef, useState } from "react";
import * as api from "../api";
import type { SearchHit } from "../types";

/** Milliseconds to wait after the user stops typing before firing a search. */
const DEBOUNCE_MS = 300;

interface SearchBarProps {
  /** Called when the user clicks a search result row. */
  onResultClick: (hit: SearchHit) => void;
}

/**
 * Debounced full-text search bar.
 *
 * WHY self-contained (not lifting query state to App): the search panel is
 * ephemeral — it appears when a query is active and disappears on clear or
 * blur. Lifting state would force App to re-render the entire tree on every
 * keystroke. The parent only needs the selected result, not the live query.
 */
export default function SearchBar({ onResultClick }: SearchBarProps) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [open, setOpen] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    if (query.trim() === "") {
      setHits(null);
      setOpen(false);
      setSearching(false);
      return;
    }
    setSearching(true);
    timerRef.current = setTimeout(() => {
      api.search(query.trim(), 50).match(
        (results) => {
          setHits(results);
          setOpen(true);
          setSearching(false);
        },
        () => {
          // WHY: search errors are non-fatal; show empty results rather than
          // surfacing a red banner for a transient FTS5 syntax error.
          setHits([]);
          setOpen(true);
          setSearching(false);
        },
      );
    }, DEBOUNCE_MS);
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [query]);

  // Close dropdown on outside click.
  useEffect(() => {
    function handlePointerDown(e: PointerEvent) {
      if (
        containerRef.current &&
        !containerRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
      }
    }
    document.addEventListener("pointerdown", handlePointerDown);
    return () => document.removeEventListener("pointerdown", handlePointerDown);
  }, []);

  function handleClear() {
    setQuery("");
    setHits(null);
    setOpen(false);
  }

  function handleHitClick(hit: SearchHit) {
    onResultClick(hit);
    setOpen(false);
  }

  return (
    <div ref={containerRef} className="relative w-72">
      <div className="flex items-center bg-gray-700 rounded border border-gray-600 focus-within:border-blue-500">
        <span className="pl-3 text-gray-400 text-sm select-none">🔍</span>
        <input
          type="search"
          aria-label="Search files"
          placeholder="Search…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onFocus={() => {
            if (hits !== null) setOpen(true);
          }}
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

      {open && (
        <div
          role="listbox"
          aria-label="Search results"
          className="absolute z-50 w-full mt-1 bg-gray-800 border border-gray-600 rounded shadow-lg max-h-72 overflow-y-auto"
        >
          {searching && (
            <p className="px-3 py-2 text-sm text-gray-400">Searching…</p>
          )}
          {!searching && hits !== null && hits.length === 0 && (
            <p className="px-3 py-2 text-sm text-gray-400">(no results)</p>
          )}
          {!searching &&
            hits !== null &&
            hits.map((hit) => (
              <button
                key={hit.blake3_hash}
                type="button"
                role="option"
                aria-selected={false}
                onClick={() => handleHitClick(hit)}
                className="w-full text-left px-3 py-2 text-sm text-gray-200 hover:bg-gray-700 focus:bg-gray-700 focus:outline-none truncate"
              >
                {hit.relative_path}
              </button>
            ))}
        </div>
      )}
    </div>
  );
}
