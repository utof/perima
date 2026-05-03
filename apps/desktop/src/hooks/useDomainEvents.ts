/**
 * Single subscription site for Tauri `app-event` channel.
 *
 * WHY this hook (not inline useEffect in App.tsx): clear separation —
 * AppEvent → invalidateQueries dispatch is a single concern; bundling
 * it into App.tsx muddies the root component.
 *
 * WHY mount once at App root: subscribing per-component would
 * multiplex the same Tauri channel with N listeners and re-fetch
 * `queryClient.invalidateQueries` N times per event.
 *
 * WHY per-`reason` invalidation (upgrade from Batch E TODO): the
 * IndexInvalidated.reason discriminator lets us invalidate ONLY the
 * affected domain. Coarse refetch on every event was acceptable in
 * Batch E without the Query layer; with Query keys, we can be
 * surgical.
 */
import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import * as api from "../api";
import type { UnsubscribeFn } from "../api";
import { filesKeys } from "../queries/files";
import { tagsKeys } from "../queries/tags";
import { searchKeys } from "../queries/search";
import { dedupKeys } from "../queries/dedup";
import { transcriptsKeys } from "../queries/transcripts";
import { useUiStore } from "../stores/ui";

const FILES_DEBOUNCE_MS = 300;
/** Time a `completed` job remains in the slice before auto-removal (ms). */
const COMPLETED_GRACE_MS = 5_000;
/** Time a `cancelled` job remains in the slice before auto-removal (ms). */
const CANCELLED_GRACE_MS = 3_000;

export function useDomainEvents(): void {
  const queryClient = useQueryClient();
  const notifyError = useUiStore((s) => s.notifyError);
  const setVerifyBatchProgress = useUiStore((s) => s.setVerifyBatchProgress);
  const clearVerifyBatch = useUiStore((s) => s.clearVerifyBatch);
  // WHY single-property selectors per CLAUDE.md "useShallow for any
  // multi-property Zustand v5 selector": the four transcription actions are
  // each a single-property pull, so no useShallow is needed.
  const startJob = useUiStore((s) => s.transcription.startJob);
  const updateJob = useUiStore((s) => s.transcription.updateJob);
  const removeJob = useUiStore((s) => s.transcription.removeJob);

  useEffect(() => {
    let active = true;
    let unsubscribe: UnsubscribeFn | null = null;
    let filesDebounceTimer: ReturnType<typeof setTimeout> | null = null;
    // WHY tracked Set: terminal-state auto-remove timers must be cleared on
    // unmount (StrictMode double-effect; HMR; route teardown). Tracking lets
    // the cleanup loop fire `clearTimeout` on every pending grace timer
    // without leaking a callback that would mutate a stale slice.
    const transcriptionTimers = new Set<ReturnType<typeof setTimeout>>();

    const invalidateFiles = () => {
      void queryClient.invalidateQueries({ queryKey: filesKeys.all });
    };

    const debounceFiles = () => {
      if (filesDebounceTimer) clearTimeout(filesDebounceTimer);
      filesDebounceTimer = setTimeout(invalidateFiles, FILES_DEBOUNCE_MS);
    };

    api
      .subscribeToAppEvents((event) => {
        switch (event.kind) {
          case "File":
            // 300ms debounce — bursty filesystem events
            debounceFiles();
            break;
          case "ScanCompleted":
            // Immediate — user is waiting; cancel any pending debounce
            if (filesDebounceTimer) clearTimeout(filesDebounceTimer);
            void queryClient.invalidateQueries({ queryKey: filesKeys.all });
            void queryClient.invalidateQueries({ queryKey: tagsKeys.all });
            break;
          case "IndexInvalidated":
            switch (event.data.reason) {
              case "TagsChanged":
                void queryClient.invalidateQueries({ queryKey: tagsKeys.all });
                break;
              case "FilesChanged":
                debounceFiles();
                break;
              case "MetadataChanged":
                debounceFiles();
                break;
              case "SearchIndexRebuilt":
                void queryClient.invalidateQueries({ queryKey: searchKeys.all });
                break;
              case "CollisionsChanged":
                // WHY: CollisionsChanged invalidates the dedup/collision-group
                // query surface (Task 13). No query key exists yet — placeholder
                // to silence the exhaustive-never check until Task 13 lands.
                break;
              default: {
                const _exhaustive: never = event.data.reason;
                throw new Error(
                  `Unhandled IndexInvalidated.reason: ${JSON.stringify(_exhaustive)}`,
                );
              }
            }
            break;
          case "VerifyProgress":
            // WHY push into Zustand (not TanStack Query): progress is a
            // transient push value — not server state that should be cached or
            // refetched. Task 13 (`/dedup` route) reads the `verifyBatch` slice
            // from the store to drive a progress bar without polling IPC.
            setVerifyBatchProgress(
              event.data.batch_id,
              event.data.files_done,
              event.data.files_total,
              event.data.latest_outcome,
            );
            break;
          case "VerifyComplete":
            // WHY two actions on complete:
            // 1. Reset the Zustand progress slice — batch is done.
            // 2. Invalidate the collision-group query so the dedup table
            //    reflects the newly-computed full-hash data.
            clearVerifyBatch();
            void queryClient.invalidateQueries({ queryKey: dedupKeys.all });
            break;
          case "TranscriptionStarted":
            // WHY startJob (not updateJob): the worker may have skipped the
            // queued state entirely (single-worker queue with no preceding
            // dispatch event in v1) — the in-flight slice's `running` entry
            // is born here. Status starts at 0/total_ms unknown; the first
            // Progress event flips processed_ms forward.
            startJob({
              request_uuid: event.data.request_uuid,
              file_uuid: event.data.file_uuid,
              file_name: event.data.file_name,
              status: { kind: "running", processed_ms: 0, total_ms: null },
              started_at_ms: Date.now(),
            });
            // Invalidate transcripts query for this file — a future "in-progress"
            // row may exist (V013 resumability); v1 has none, so this is a
            // forward-compat no-op today.
            void queryClient.invalidateQueries({
              queryKey: transcriptsKeys.byFileUuid(event.data.file_uuid),
            });
            break;
          case "TranscriptionProgress":
            updateJob(event.data.request_uuid, {
              kind: "running",
              processed_ms: event.data.processed_ms,
              total_ms: event.data.total_ms,
            });
            break;
          case "TranscriptionCompleted": {
            const { request_uuid, transcript_id, file_uuid, segment_count, language } = event.data;
            updateJob(request_uuid, {
              kind: "completed",
              transcript_id,
              segment_count,
              language,
            });
            // WHY invalidate transcripts (per file) AND search: completion
            // inserts a new row that the file-detail sidebar must refresh,
            // and `transcript_search` FTS5 changes mean global search may
            // surface a new hit. files/tags untouched.
            void queryClient.invalidateQueries({
              queryKey: transcriptsKeys.byFileUuid(file_uuid),
            });
            void queryClient.invalidateQueries({ queryKey: searchKeys.all });
            // Auto-remove the slice entry after the user has had time to
            // see "done!" — terminal completion is celebratory, not blocking.
            // The slice's removeJob no-ops if the user dismissed first.
            const completedTimer = setTimeout(() => {
              removeJob(request_uuid);
              transcriptionTimers.delete(completedTimer);
            }, COMPLETED_GRACE_MS);
            transcriptionTimers.add(completedTimer);
            break;
          }
          case "TranscriptionCancelled": {
            const { request_uuid } = event.data;
            updateJob(request_uuid, { kind: "cancelled" });
            // Auto-remove a bit faster than completed — cancellation is
            // user-initiated; the user already knows; no celebration needed.
            const cancelledTimer = setTimeout(() => {
              removeJob(request_uuid);
              transcriptionTimers.delete(cancelledTimer);
            }, CANCELLED_GRACE_MS);
            transcriptionTimers.add(cancelledTimer);
            break;
          }
          case "TranscriptionFailed":
            updateJob(event.data.request_uuid, {
              kind: "failed",
              error: event.data.error,
            });
            // WHY also notify: failures are user-actionable (Auth, FileTooLarge,
            // QuotaExceeded). Surfacing through the toast stack means the user
            // sees the typed error even when the StatusBar pill is collapsed.
            notifyError({ kind: "Transcription", data: event.data.error });
            // WHY no auto-remove: failures persist until the user dismisses
            // them via the popover Dismiss button (spec § "TranscriptionPill").
            break;
          default: {
            const _exhaustive: never = event;
            throw new Error(`Unhandled AppEvent kind: ${JSON.stringify(_exhaustive)}`);
          }
        }
      })
      .then((fn) => {
        if (active) unsubscribe = fn;
        else fn();
      })
      .catch((err: unknown) => {
        const msg = err instanceof Error ? err.message : String(err);
        notifyError({ kind: "Internal", data: `Failed to subscribe to app events: ${msg}` });
      });

    return () => {
      active = false;
      if (filesDebounceTimer) clearTimeout(filesDebounceTimer);
      // Clear every pending transcription auto-remove timer to prevent slice
      // mutations after unmount.
      for (const t of transcriptionTimers) clearTimeout(t);
      transcriptionTimers.clear();
      if (unsubscribe) unsubscribe();
    };
  }, [queryClient, notifyError, setVerifyBatchProgress, clearVerifyBatch, startJob, updateJob, removeJob]);
}
