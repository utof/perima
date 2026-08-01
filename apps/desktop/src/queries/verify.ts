/**
 * TanStack Query hooks for the location verify sweep + prune.
 *
 * WHY both mutations invalidate the file list: verify rewrites
 * `file_locations.status` and prune soft-deletes rows, so every
 * file-shaped query is stale afterwards. The backend also emits
 * `IndexInvalidated::FilesChanged`, but that is a coarse hint on a
 * channel the frontend treats as advisory — invalidating here keeps the
 * UI correct even if the event is dropped.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import * as api from "../api";
import type { PruneReport, VerifyReport } from "../bindings";
import { useUiStore } from "../stores/ui";

export const verifyKeys = {
  all: ["verify"] as const,
  missingCount: ["verify", "missing-count"] as const,
};

/** Live count of locations marked `missing` — what a prune would remove. */
export function useMissingCount() {
  return useQuery({
    queryKey: verifyKeys.missingCount,
    queryFn: async () =>
      api.countMissingLocations().match(
        (n) => n,
        (err) => {
          // eslint-disable-next-line @typescript-eslint/only-throw-error
          throw err;
        },
      ),
  });
}

/**
 * Run a verify sweep.
 *
 * WHY the notification distinguishes a partial sweep: when
 * `skipped_unmounted > 0` the sweep could not see part of the library,
 * and reporting only "0 missing" would read as a clean bill of health
 * over files it never looked at.
 */
export function useVerifyLocations() {
  const notify = useUiStore((s) => s.notify);
  const qc = useQueryClient();
  return useMutation({
    mutationKey: [...verifyKeys.all, "run"],
    mutationFn: async (dryRun: boolean) =>
      api.verifyLocations(dryRun).match(
        (r) => r,
        (err) => {
          // eslint-disable-next-line @typescript-eslint/only-throw-error
          throw err;
        },
      ),
    onSuccess: (r: VerifyReport) => {
      void qc.invalidateQueries({ queryKey: verifyKeys.missingCount });
      void qc.invalidateQueries({ queryKey: ["files"] });
      // WHY "info" even when files are missing: `NotificationKind` is
      // "info" | "error", and a missing file is a finding, not a failure
      // — the sweep did exactly what it was asked. Flagging it as an
      // error would train the user to dismiss the surface that carries
      // the skipped-volume caveat. The message itself leads with the
      // count, and the file rows badge themselves.
      notify("info", verifyMessage(r));
    },
  });
}

/** Human-readable summary of a {@link VerifyReport}. */
export function verifyMessage(r: VerifyReport): string {
  const parts: string[] = [`Checked ${String(r.checked)} file(s)`];
  if (r.newly_missing > 0) parts.push(`${String(r.newly_missing)} now missing`);
  if (r.recovered > 0) parts.push(`${String(r.recovered)} recovered`);
  if (r.newly_missing === 0 && r.recovered === 0) parts.push("no changes");
  if (r.skipped_unmounted > 0) {
    parts.push(
      `${String(r.skipped_unmounted)} skipped on unmounted volume(s) — not checked`,
    );
  }
  if (!r.completed) parts.push("cancelled; nothing written");
  return `${parts.join(" · ")}.`;
}

/** Soft-delete every location marked `missing`. Destructive. */
export function usePruneMissing() {
  const notify = useUiStore((s) => s.notify);
  const qc = useQueryClient();
  return useMutation({
    mutationKey: [...verifyKeys.all, "prune"],
    mutationFn: async (dryRun: boolean) =>
      api.pruneMissingLocations(dryRun).match(
        (r) => r,
        (err) => {
          // eslint-disable-next-line @typescript-eslint/only-throw-error
          throw err;
        },
      ),
    onSuccess: (r: PruneReport) => {
      void qc.invalidateQueries({ queryKey: verifyKeys.missingCount });
      void qc.invalidateQueries({ queryKey: ["files"] });
      notify(
        "info",
        r.rows_pruned > 0
          ? `Removed ${String(r.rows_pruned)} missing file(s) from the library.`
          : "Nothing to remove — no files are marked missing.",
      );
    },
  });
}
