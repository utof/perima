/**
 * Files-list query: queryKey namespace + queryOptions factory + hook.
 *
 * WHY factory pattern: consumers pass the same `filesQueryOptions(limit, volume)`
 * to both `useFiles()` and `queryClient.invalidateQueries()` — single source of
 * truth for key shape. queryOptions() (not raw object) gives TypeScript the
 * inferred `data` type without per-call generics.
 *
 * Bridge: neverthrow `ResultAsync` → Promise via `.match(ok, err => throw err)`.
 * Throwing inside `queryFn` causes TanStack Query to set `status = "error"` with
 * the thrown `CoreError` as `error`. Do NOT use `.unwrapOr(...)` which silently
 * swallows errors.
 */
import { queryOptions, useQuery } from "@tanstack/react-query";
import * as api from "../api";

export const filesKeys = {
  all: ["files"] as const,
  list: (limit: number, volume?: string) =>
    [...filesKeys.all, "list", { limit, volume: volume ?? null }] as const,
} as const;

export function filesQueryOptions(limit: number, volume?: string) {
  return queryOptions({
    queryKey: filesKeys.list(limit, volume),
    queryFn: () =>
      api.listFilesWithTags(limit, volume).match(
        (data) => data,
        // WHY eslint-disable: TanStack Query queryFn accepts any thrown value;
        // CoreError is the registered defaultError type (queryClient.ts Register
        // augmentation) so useFiles().error is typed as CoreError | null.
        // Wrapping in Error would lose the typed discriminant.
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
  });
}

export function useFiles(limit: number, volume?: string) {
  return useQuery(filesQueryOptions(limit, volume));
}
