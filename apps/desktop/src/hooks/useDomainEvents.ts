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
import { useUiStore } from "../stores/ui";

const FILES_DEBOUNCE_MS = 300;

export function useDomainEvents(): void {
  const queryClient = useQueryClient();
  const notifyError = useUiStore((s) => s.notifyError);

  useEffect(() => {
    let active = true;
    let unsubscribe: UnsubscribeFn | null = null;
    let filesDebounceTimer: ReturnType<typeof setTimeout> | null = null;

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
            // WHY: VerifyProgress events are consumed by the dedup route
            // (Task 13) and the file-detail sidebar (Task 14). No query key
            // to invalidate here — progress is pushed via the event, not polled.
            break;
          case "VerifyComplete":
            // WHY: VerifyComplete triggers a final collision-group refresh.
            // No query key exists yet — placeholder until Task 13 lands.
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
      if (unsubscribe) unsubscribe();
    };
  }, [queryClient, notifyError]);
}
