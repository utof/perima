/**
 * Tags-list query: queryKey namespace + queryOptions factory + hook.
 *
 * WHY factory pattern: same key shape used for both `useTags()` and
 * `queryClient.invalidateQueries()` calls driven by `AppEvent.IndexInvalidated`
 * (TagsChanged reason). Single source of truth avoids key drift.
 *
 * Bridge: neverthrow `ResultAsync` → Promise via `.match(ok, err => throw err)`.
 * Throwing inside `queryFn` causes TanStack Query to set `status = "error"` with
 * the thrown `CoreError` as `error`. Do NOT use `.unwrapOr(...)` which silently
 * swallows errors.
 */
import { queryOptions, useQuery } from "@tanstack/react-query";
import * as api from "../api";

export const tagsKeys = {
  all: ["tags"] as const,
  list: () => [...tagsKeys.all, "list"] as const,
} as const;

export function tagsQueryOptions() {
  return queryOptions({
    queryKey: tagsKeys.list(),
    queryFn: () =>
      api.listTags().match(
        (data) => data,
        // WHY eslint-disable: TanStack Query queryFn accepts any thrown value;
        // CoreError is the registered defaultError type (queryClient.ts Register
        // augmentation) so useTags().error is typed as CoreError | null.
        // Wrapping in Error would lose the typed discriminant.
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
  });
}

export function useTags() {
  return useQuery(tagsQueryOptions());
}
