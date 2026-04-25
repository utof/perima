/**
 * Search query: queryKey namespace + queryOptions factory + hook.
 *
 * WHY enabled-when-non-empty: empty query returns no results AND should NOT fire
 * a Tauri IPC. The dual-field store in SearchBar (Task 9) passes `debouncedQuery`
 * here; consumers see `data === undefined` when disabled (treated as "no search
 * active" by IndexRoute). MIN_QUERY_LEN = 2 prevents single-character FTS5
 * prefix scans that expand to the entire corpus.
 *
 * Bridge: neverthrow `ResultAsync` → Promise via `.match(ok, err => throw err)`.
 * Throwing inside `queryFn` causes TanStack Query to set `status = "error"` with
 * the thrown `CoreError` as `error`. Do NOT use `.unwrapOr(...)` which silently
 * swallows errors.
 */
import { queryOptions, useQuery } from "@tanstack/react-query";
import * as api from "../api";

export const MIN_QUERY_LEN = 2;

export const searchKeys = {
  all: ["search"] as const,
  query: (q: string, limit: number) =>
    [...searchKeys.all, "query", { q, limit }] as const,
} as const;

// WHY default 500 (was 50): paired with useFiles(1000) above; with limit=50
// the intersection silently dropped most matches in libraries >50 results
// (e.g. searching "mp4" in a 340-mp4 library returned 3 visible). 500 is
// generous for a single search page; full-corpus pagination is a separate
// effort tracked alongside virtualisation.
export function searchQueryOptions(query: string, limit = 500) {
  return queryOptions({
    queryKey: searchKeys.query(query, limit),
    queryFn: () =>
      api.search(query, limit).match(
        (data) => data,
        // WHY eslint-disable: TanStack Query queryFn accepts any thrown value;
        // CoreError is the registered defaultError type (queryClient.ts Register
        // augmentation) so useSearch().error is typed as CoreError | null.
        // Wrapping in Error would lose the typed discriminant.
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
    enabled: query.length >= MIN_QUERY_LEN,
  });
}

export function useSearch(query: string, limit = 50) {
  return useQuery(searchQueryOptions(query, limit));
}
