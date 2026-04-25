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
import { queryOptions, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import * as api from "../api";
import { filesKeys } from "./files";

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

/**
 * Mutation: attach a tag to a file by hash + tag name.
 *
 * On success, invalidates the files list and the tags list so the new tag
 * (or new attachment) shows up everywhere it's rendered. Errors propagate
 * as `CoreError` via the registered defaultError type.
 *
 * WHY useMutation (not raw api call): TanStack Query handles the
 * pending/success/error UI state + the cache-invalidation atomically,
 * keeping the `useDomainEvents` event-driven invalidation as a fallback
 * (events fire after the writer commits; the optimistic invalidate
 * here is just a UX nicety to refresh sooner than the round-trip).
 */
export function useAttachTag() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (vars: { hash: string; tagName: string }) =>
      api.attachTag(vars.hash, vars.tagName).match(
        (tag) => tag,
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: filesKeys.all });
      void qc.invalidateQueries({ queryKey: tagsKeys.all });
    },
  });
}

/**
 * Mutation: detach a tag from a file by hash + tag id.
 *
 * Symmetric to `useAttachTag`. Same invalidation strategy.
 */
export function useDetachTag() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (vars: { hash: string; tagId: string }) =>
      api.detachTag(vars.hash, vars.tagId).match(
        (v) => v,
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: filesKeys.all });
      void qc.invalidateQueries({ queryKey: tagsKeys.all });
    },
  });
}
