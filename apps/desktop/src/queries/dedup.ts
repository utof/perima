/**
 * Dedup query key namespace and hooks.
 *
 * WHY "dedup" root key: matches the domain name (`DedupUseCase`) and avoids
 * collision with the "files" root used by `filesKeys` (even though collisions
 * are file-adjacent data, their lifecycle differs — they are refreshed on
 * `VerifyComplete` / `IndexInvalidated::CollisionsChanged`, not on generic
 * `IndexInvalidated::FilesChanged`).
 */
import { queryOptions, useQuery } from "@tanstack/react-query";
import * as api from "../api";

export const dedupKeys = {
  /** Root key — invalidate to bust all dedup-related queries. */
  all: ["dedup"] as const,
  /** Collision-group list. */
  collisions: () => [...dedupKeys.all, "collisions"] as const,
} as const;

/**
 * Query options factory for the collision-group list.
 *
 * WHY a separate factory: allows consumers to pre-fetch or share the same
 * query without importing the hook (e.g. route loaders in Task 13).
 */
export function collisionsQueryOptions() {
  return queryOptions({
    queryKey: dedupKeys.collisions(),
    queryFn: () =>
      api.listQuickHashCollisions().match(
        (data) => data,
        // WHY eslint-disable: TanStack Query queryFn accepts any thrown value;
        // CoreError is the registered defaultError type (queryClient.ts Register
        // augmentation) so useCollisions().error is typed as CoreError | null.
        // Wrapping in Error would lose the typed discriminant.
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
  });
}

/**
 * Subscribe to the collision-group list query.
 *
 * WHY no staleTime override: the global queryClient default (5 min) is
 * appropriate — collisions change only when a scan or verify completes,
 * both of which emit `IndexInvalidated::CollisionsChanged` / `VerifyComplete`
 * that trigger a manual invalidation via `useDomainEvents`.
 */
export function useCollisions() {
  return useQuery(collisionsQueryOptions());
}
