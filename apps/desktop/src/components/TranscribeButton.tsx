/**
 * TranscribeButton — per-file transcription action with 3 visual states:
 *   - Idle: "Transcribe" → fires api.transcribe.
 *   - Running (queued or in-progress): shows queue position or percent → click cancels.
 *   - Terminal (completed / cancelled / failed): brief disabled flash, then auto-removes
 *     (handled by T8's useDomainEvents grace period). Failed jobs persist until retry.
 *
 * WHY plain HTML buttons (not shadcn): no shadcn ui/ components exist in this project.
 * WHY no useMemo / useCallback: React Compiler 1.0 is enabled (CLAUDE.md).
 * WHY single-property selectors: Zustand 5 scalar reads are stable references without
 * useShallow; multi-property reads use useShallow (not needed here — one selector per hook).
 */
import { useUiStore } from "../stores/ui";
import { coreErrorMessage } from "../lib/coreError";
import * as api from "../api";

/** Props for {@link TranscribeButton}. */
export interface TranscribeButtonProps {
  /** Stable file surrogate (FileUuid). Used to match in-flight jobs. */
  fileUuid: string;
  /** Display name for the transcription job (shown in the TranscriptionPill). */
  fileName: string;
  /**
   * Path to the media file passed to the backend for transcription.
   *
   * WHY relative_path (not absolute_path): `FileWithTagsPayload` only exposes
   * `relative_path`; the Rust side resolves the volume mount-point internally.
   * When T10 or a later task adds `absolute_path` to the payload, replace here.
   */
  source: string;
}

/**
 * Per-file transcription control that reflects the live job slice from
 * `useUiStore`. Renders a context-sensitive button based on `activeJob?.status.kind`.
 */
export function TranscribeButton({ fileUuid, fileName, source }: TranscribeButtonProps) {
  const activeJob = useUiStore((s) =>
    Object.values(s.transcription.jobs).find((j) => j.file_uuid === fileUuid),
  );
  const notifyError = useUiStore((s) => s.notifyError);

  function onClickStart() {
    api
      .transcribe({ fileUuid, fileName, source, languageHint: null })
      .mapErr((err) => { notifyError(err); });
  }

  function onClickCancel() {
    if (!activeJob) return;
    api
      .cancelTranscription(activeJob.request_uuid)
      .mapErr((err) => { notifyError(err); });
  }

  // ── Shared button class bases ────────────────────────────────────────────────
  const baseClass =
    "inline-flex items-center justify-center gap-1.5 rounded-full px-4 py-1.5 text-sm font-medium " +
    "transition-colors duration-micro ease-perima " +
    "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring " +
    "focus-visible:ring-offset-2 focus-visible:ring-offset-background " +
    "disabled:opacity-40 disabled:pointer-events-none";

  const primaryClass = `${baseClass} bg-primary text-primary-foreground hover:bg-primary/90`;
  const ghostClass = `${baseClass} bg-muted text-foreground hover:bg-muted/70`;
  const destructiveClass = `${baseClass} bg-destructive text-destructive-foreground hover:bg-destructive/90`;

  // ── No active job → Idle ─────────────────────────────────────────────────────
  if (!activeJob) {
    return (
      <button
        type="button"
        onClick={onClickStart}
        className={primaryClass}
        aria-label={`Transcribe ${fileName}`}
      >
        Transcribe
      </button>
    );
  }

  const { status } = activeJob;

  // ── Queued ───────────────────────────────────────────────────────────────────
  if (status.kind === "queued") {
    return (
      <button
        type="button"
        onClick={onClickCancel}
        className={ghostClass}
        aria-label={`Cancel queued transcription (position ${String(status.queue_position)})`}
      >
        Cancel (Queued #{status.queue_position})
      </button>
    );
  }

  // ── Running ──────────────────────────────────────────────────────────────────
  if (status.kind === "running") {
    const percent =
      status.total_ms !== null && status.total_ms > 0
        ? Math.round((status.processed_ms / status.total_ms) * 100)
        : null;
    const label = percent !== null ? `${String(percent)}%` : "...";
    return (
      <button
        type="button"
        onClick={onClickCancel}
        className={ghostClass}
        aria-label="Cancel in-progress transcription"
      >
        Cancel (Transcribing {label})
      </button>
    );
  }

  // ── Completed ────────────────────────────────────────────────────────────────
  // WHY disabled: the job auto-removes after 5s (useDomainEvents grace period),
  // at which point the component re-renders as Idle. Disabling prevents double-clicks.
  if (status.kind === "completed") {
    return (
      <button
        type="button"
        disabled
        className={primaryClass}
        aria-label="Transcription complete"
      >
        Done!
      </button>
    );
  }

  // ── Cancelled ────────────────────────────────────────────────────────────────
  // WHY same handler as Idle: clicking re-submits the job. The slice auto-removes
  // the cancelled entry after 3s (useDomainEvents); the button then re-renders Idle.
  if (status.kind === "cancelled") {
    return (
      <button
        type="button"
        onClick={onClickStart}
        className={ghostClass}
        aria-label={`Retry transcription for ${fileName}`}
      >
        Cancelled — Retry?
      </button>
    );
  }

  // ── Failed ───────────────────────────────────────────────────────────────────
  // WHY tooltip with title: simple native tooltip for the error message; T10 can
  // upgrade to a popover when the Settings modal lands.
  // WHY data-variant: surfaced to tests that verify the destructive variant is applied
  // (no shadcn attr; we carry it as a data- attribute so assertions are explicit).
  const errorMessage = coreErrorMessage({ kind: "Transcription", data: status.error });
  return (
    <button
      type="button"
      onClick={onClickStart}
      className={destructiveClass}
      data-variant="destructive"
      title={errorMessage}
      aria-label={`Transcription failed — retry ${fileName}`}
    >
      Failed — Retry
    </button>
  );
}
