/**
 * Dedup query key namespace and hooks.
 *
 * WHY "dedup" root key: matches the domain name (`DedupUseCase`) and avoids
 * collision with the "files" root used by `filesKeys` (even though collisions
 * are file-adjacent data, their lifecycle differs — they are refreshed on
 * `VerifyComplete` / `IndexInvalidated::CollisionsChanged`, not on generic
 * `IndexInvalidated::FilesChanged`).
 */
import { queryOptions, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import * as api from "../api";
import type { BatchId, FileUuid } from "../bindings";
import { filesKeys } from "./files";

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

/**
 * Mutation: compute the canonical `full_hash` for a single file.
 *
 * Synchronous on the IPC surface — the mutation resolves only once the
 * writer has persisted the hash. Invalidates `filesKeys.all` (so a
 * pending file's hash column refreshes) and `dedupKeys.all` (so the
 * collision-group list reflects the newly-known full hash).
 */
export function useComputeFullHash() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (vars: { fileUuid: FileUuid }) =>
      api.computeFullHash(vars.fileUuid).match(
        (hash) => hash,
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: filesKeys.all });
      void qc.invalidateQueries({ queryKey: dedupKeys.all });
    },
  });
}

/**
 * Mutation: spawn a batch full-hash compute over the given `file_uuids`.
 *
 * The `BatchHandle` resolves immediately; progress arrives via the
 * AppEvent channel and is mirrored into the Zustand `verifyBatch` slice
 * by `useDomainEvents`. The dedup query gets invalidated on the
 * subsequent `VerifyComplete` event (also in `useDomainEvents`); we
 * additionally fire a coarse `dedupKeys.all` invalidate on `onSuccess`
 * so the UI reacts even if a future spec change drops `VerifyComplete`.
 */
export function useVerifyBatch() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (vars: { fileUuids: FileUuid[] }) =>
      api.computeFullHashBatch(vars.fileUuids).match(
        (handle) => handle,
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: dedupKeys.all });
    },
  });
}

/**
 * Mutation: cancel an in-flight verify batch by `batch_id`.
 *
 * Treats `CoreError::NotFound` as success — the batch finished before
 * cancel raced through. The Zustand slice is cleared by the
 * `VerifyComplete` event handler (or the writer's terminal flush);
 * the mutation does not touch the slice directly.
 */
export function useCancelVerifyBatch() {
  return useMutation({
    mutationFn: (vars: { batchId: BatchId }) =>
      api.cancelVerifyBatch(vars.batchId).match(
        (v) => v,
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
  });
}

/**
 * Mutation: memorise that the supplied `file_uuids` were verified to have
 * distinct `full_hash` values despite sharing a `quick_hash`.
 *
 * Invalidates `dedupKeys.all` so the verified-distinct group disappears
 * from the candidate list.
 */
export function useMarkVerifiedDistinct() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (vars: { fileUuids: FileUuid[] }) =>
      api.markVerifiedDistinct(vars.fileUuids).match(
        (v) => v,
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: dedupKeys.all });
    },
  });
}
