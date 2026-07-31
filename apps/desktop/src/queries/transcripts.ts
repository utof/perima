/**
 * Transcripts query keys + queryOptions + hook for reading persisted
 * `transcript` rows by `file_uuid`.
 *
 * WHY a separate query module: `useDomainEvents` invalidates this surface on
 * `AppEvent::TranscriptionCompleted` for the matching `file_uuid`, so the
 * key shape MUST be in one place — sidebar reads + event invalidations both
 * derive from `transcriptsKeys.byFileUuid(uuid)`.
 *
 * WHY `select:` returning only the latest row by `completed_at` DESC: spec
 * § "Multi-row selection on the file-detail sidebar" — v1 surfaces a single
 * transcript per file (the most recent successful one). A future picker UI
 * is queued as a follow-up; until then `select:` collapses the array to one
 * row and consumers don't need to handle the multi-row shape.
 *
 * BACKEND COMMAND PENDING (T9 follow-up): the underlying
 * `list_transcripts_by_file_uuid` Tauri command is not yet implemented (T7
 * shipped the eight write-side commands only). The `queryFn` below throws a
 * typed `CoreError::Unsupported` so the cache machinery + key shape land
 * cleanly today; the first sidebar consumer (T9 / `<TranscribeButton>`)
 * surfaces the error and the orchestrator files the command-add issue. This
 * keeps `useDomainEvents`'s invalidation arm wired without blocking T8.
 */
import { queryOptions, useQuery } from "@tanstack/react-query";
import type { CoreError } from "../bindings";

/**
 * One persisted transcript header row (no segments).
 *
 * Mirrors the shape `crates/db/src/transcript_repo.rs::TranscriptRow` will
 * cross over the IPC boundary as once the backend `list_transcripts_by_file_uuid`
 * Tauri command lands. Hand-crafted today because no command emits this
 * type yet — replace with a `bindings.ts` import once T9's command lands.
 */
export interface TranscriptHeader {
  /** UUIDv7 simple-hex transcript id. */
  id: string;
  /** FK to `files.file_uuid`. */
  file_uuid: string;
  /** Backend identifier (`provider:model`). */
  backend: string;
  /** Detected language (BCP-47 short code) when reported. */
  language: string | null;
  /** Total source media duration in milliseconds. */
  duration_ms: number;
  /** ISO-8601 completion timestamp. */
  completed_at: string | null;
}

export const transcriptsKeys = {
  /** Root key — invalidate to bust every per-file transcripts query. */
  all: ["transcripts"] as const,
  /** Per-file query key. Pairs with `transcriptsQueryOptions(fileUuid)`. */
  byFileUuid: (fileUuid: string) => [...transcriptsKeys.all, "byFileUuid", fileUuid] as const,
} as const;

/**
 * Query options factory: load transcripts for a single file, sorted latest
 * first via the `select:` callback. Always returns 0 or 1 rows.
 *
 * @param fileUuid - FileUuid (immutable surrogate per V011) to fetch transcripts for.
 */
export function transcriptsQueryOptions(fileUuid: string) {
  return queryOptions({
    queryKey: transcriptsKeys.byFileUuid(fileUuid),
    queryFn: (): Promise<TranscriptHeader[]> => {
      // WHY explicit Promise.reject (not `throw` from sync body): TanStack
      // Query expects `queryFn` to return a Promise; throwing here would be
      // wrapped automatically but the `reject` form keeps the rejection path
      // explicit + matches the future `fromInvoke(...)` shape one-to-one.
      // WHY CoreError::Unsupported (not Internal): the backend command is
      // intentionally absent in v1; the Unsupported variant signals "future
      // feature" precisely. Frontend fallback branches on `err.kind`.
      const err: CoreError = {
        kind: "Unsupported",
        data: "list_transcripts_by_file_uuid Tauri command not yet implemented (T9 follow-up)",
      };
      // eslint-disable-next-line @typescript-eslint/prefer-promise-reject-errors
      return Promise.reject(err);
    },
    /**
     * Reduce the multi-row response to the latest by `completed_at` DESC.
     * Returns `null` when no transcripts exist for the file.
     *
     * WHY `select:` (not `queryFn` reduction): keeps the cached entry as the
     * full array — a future picker UI can read it without re-fetching — while
     * the v1 sidebar consumer gets the single-row view it wants.
     */
    select: (rows: TranscriptHeader[]): TranscriptHeader | null => {
      if (rows.length === 0) return null;
      // WHY copy before sort: sort mutates; mutating a TanStack-cached value
      // breaks structural-share invariants. `[...rows]` is the canonical guard.
      const sorted = [...rows].sort((a, b) => {
        // WHY null pushed to end: in-progress (V013-resumability) transcripts
        // have no `completed_at` yet; the sidebar should prefer terminal rows.
        if (a.completed_at === null && b.completed_at === null) return 0;
        if (a.completed_at === null) return 1;
        if (b.completed_at === null) return -1;
        // String comparison works on ISO-8601 lexicographically.
        return b.completed_at.localeCompare(a.completed_at);
      });
      return sorted[0] ?? null;
    },
  });
}

/**
 * Hook: subscribe to the latest transcript for a file. Returns `null` in
 * `data` when no transcripts exist; `error: CoreError | null` follows the
 * registered defaultError type (CoreError per `queryClient.ts`).
 *
 * @param fileUuid - FileUuid for the target file.
 */
export function useTranscriptsByFileUuid(fileUuid: string) {
  return useQuery(transcriptsQueryOptions(fileUuid));
}
