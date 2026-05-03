/**
 * TranscriptionPill — StatusBar widget showing in-flight transcription jobs.
 *
 * Behaviour summary:
 * - Hidden when the `transcription.jobs` slice is empty.
 * - Shows "Transcribing (N)" where N = total jobs in slice (all statuses,
 *   including grace-period terminal ones). The grace timer in useDomainEvents
 *   auto-removes them; the pill naturally settles back to hidden.
 * - Click → absolute-positioned popover listing each job with:
 *     - File name
 *     - Status badge ("Queued", "Transcribing 32%", "Done!", "Cancelled", "Failed: …")
 *     - Cancel button for queued/running jobs (calls api.cancelTranscription)
 *     - Title tooltip on failed badge (coreErrorMessage of the inner error)
 * - Jobs sorted newest-first by started_at_ms.
 * - Popover closes on outside click or ESC.
 *
 * WHY no useMemo / useCallback: React Compiler 1.0 is enabled (CLAUDE.md §Frontend).
 * WHY single-property selector for `jobs`: scalar read is a stable reference in
 * Zustand 5 without useShallow (CLAUDE.md §Frontend state).
 * WHY plain absolute-positioned div (not a library popover): no Radix/shadcn in
 * this project; CollisionPill uses a TanStack Link. A minimal controlled div with
 * a backdrop click handler matches the project's existing CSS pattern.
 * WHY useEffect for ESC listener: keyboard events must be bound on the document,
 * not on the pill button, so the popover closes regardless of focus location.
 */
import { useState, useEffect, useRef } from "react";
import { useUiStore } from "../stores/ui";
import { coreErrorMessage } from "../lib/coreError";
import * as api from "../api";
import type { TranscriptionJob, TranscriptionJobStatus } from "../stores/ui";

// ── Status badge ─────────────────────────────────────────────────────────────

/**
 * Returns a short display label for a {@link TranscriptionJobStatus}.
 * Used for the badge text in the popover rows.
 */
function statusLabel(status: TranscriptionJobStatus): string {
  switch (status.kind) {
    case "queued":
      return `Queued #${String(status.queue_position)}`;
    case "running": {
      const percent =
        status.total_ms !== null && status.total_ms > 0
          ? Math.round((status.processed_ms / status.total_ms) * 100)
          : null;
      return percent !== null ? `Transcribing ${String(percent)}%` : "Transcribing…";
    }
    case "completed":
      return "Done!";
    case "cancelled":
      return "Cancelled";
    case "failed":
      return "Failed";
  }
}

/** True when the job can still be cancelled (not yet in a terminal state). */
function isCancellable(status: TranscriptionJobStatus): boolean {
  return status.kind === "queued" || status.kind === "running";
}

// ── Job row ───────────────────────────────────────────────────────────────────

interface JobRowProps {
  job: TranscriptionJob;
}

/** Single row in the popover — file name, status badge, optional Cancel. */
function JobRow({ job }: JobRowProps) {
  const notifyError = useUiStore((s) => s.notifyError);
  const { status } = job;

  const label = statusLabel(status);
  // WHY title on failed badge only: only failed jobs carry actionable error text.
  const failedTitle =
    status.kind === "failed"
      ? coreErrorMessage({ kind: "Transcription", data: status.error })
      : undefined;

  function onCancel() {
    api
      .cancelTranscription(job.request_uuid)
      .mapErr((err) => { notifyError(err); });
  }

  return (
    <div
      data-testid="transcription-job-row"
      className="flex items-center justify-between gap-3 py-1.5 px-3 hover:bg-muted/50 rounded-md"
    >
      <span className="truncate text-xs text-foreground max-w-[180px]" title={job.file_name}>
        {job.file_name}
      </span>
      <div className="flex items-center gap-2 shrink-0">
        <span
          className="text-xs font-medium text-muted-foreground"
          title={failedTitle}
        >
          {label}
        </span>
        {isCancellable(status) && (
          <button
            type="button"
            onClick={onCancel}
            aria-label={`Cancel transcription for ${job.file_name}`}
            className="text-xs text-destructive hover:underline focus-visible:outline-none
                       focus-visible:ring-1 focus-visible:ring-ring"
          >
            Cancel
          </button>
        )}
      </div>
    </div>
  );
}

// ── Pill ─────────────────────────────────────────────────────────────────────

/**
 * TranscriptionPill — reads `s.transcription.jobs` from the Zustand store
 * and renders a count pill + popover for the StatusBar footer.
 *
 * @example
 * ```tsx
 * <CollisionPill groups={collisions} />
 * <TranscriptionPill />
 * ```
 */
export function TranscriptionPill() {
  const jobs = useUiStore((s) => s.transcription.jobs);
  const jobList = Object.values(jobs);

  const [open, setOpen] = useState(false);
  const pillRef = useRef<HTMLDivElement>(null);

  // WHY close on outside click: standard popover UX — clicking any element
  // outside the pill+popover container collapses the panel.
  useEffect(() => {
    if (!open) return;

    function onPointerDown(e: PointerEvent) {
      if (pillRef.current && !pillRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    document.addEventListener("pointerdown", onPointerDown);
    return () => { document.removeEventListener("pointerdown", onPointerDown); };
  }, [open]);

  // WHY ESC listener: keyboard accessibility — ESC should always close the
  // popover regardless of focus position, matching the platform convention.
  useEffect(() => {
    if (!open) return;

    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setOpen(false);
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => { document.removeEventListener("keydown", onKeyDown); };
  }, [open]);

  // Hidden when no jobs are in the slice.
  if (jobList.length === 0) return null;

  // Newest first.
  const sorted = [...jobList].sort((a, b) => b.started_at_ms - a.started_at_ms);

  return (
    <div ref={pillRef} className="relative">
      {/* Pill trigger */}
      <button
        type="button"
        onClick={() => { setOpen((prev) => !prev); }}
        aria-expanded={open}
        aria-haspopup="true"
        className="inline-flex items-center gap-1.5 rounded-full px-3 py-0.5 text-xs font-medium
                   bg-primary text-primary-foreground hover:bg-primary/90
                   transition-colors duration-micro ease-perima
                   focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring
                   focus-visible:ring-offset-2 focus-visible:ring-offset-background"
      >
        Transcribing ({jobList.length})
      </button>

      {/* Popover panel */}
      {open && (
        <div
          role="dialog"
          aria-label="Transcription jobs"
          className="absolute bottom-full right-0 mb-2 z-50
                     min-w-[280px] max-w-[360px] rounded-lg border border-border
                     bg-popover text-popover-foreground shadow-lg
                     py-1"
        >
          <div className="px-3 py-1.5 text-xs font-semibold text-muted-foreground border-b border-border mb-1">
            Transcription jobs
          </div>
          {sorted.map((job) => (
            <JobRow key={job.request_uuid} job={job} />
          ))}
        </div>
      )}
    </div>
  );
}
