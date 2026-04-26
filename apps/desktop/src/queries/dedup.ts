/**
 * Dedup query key namespace.
 *
 * WHY keys-only file (no queryOptions hook yet): Task 13 (`/dedup` route)
 * will introduce `useCollisions()` + `useComputeFullHash()` hooks backed by
 * real IPC. This file exists so `useDomainEvents.ts` can import `dedupKeys`
 * and invalidate `["dedup", "collisions"]` on `VerifyComplete` without
 * coupling the hook layer prematurely.
 *
 * WHY "dedup" root key: matches the domain name (`DedupUseCase`) and avoids
 * collision with the "files" root used by `filesKeys` (even though collisions
 * are file-adjacent data, their lifecycle differs — they are refreshed on
 * `VerifyComplete`, not on generic `IndexInvalidated::FilesChanged`).
 */

export const dedupKeys = {
  /** Root key — invalidate to bust all dedup-related queries. */
  all: ["dedup"] as const,
  /** Collision-group list. */
  collisions: () => [...dedupKeys.all, "collisions"] as const,
} as const;
