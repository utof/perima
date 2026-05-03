/**
 * File-detail sidebar — minimal stub (refs #153).
 *
 * Shows the file UUID, quick hash, full hash (if promoted), and a "Compute
 * canonical hash" button when the file is still in placeholder state.
 *
 * WHY minimal stub: GH #153 (full file-detail sidebar with metadata,
 * preview, tags) is open. We land only the hash-related fields + the
 * Compute button so the fast-hashing workflow (#155) is usable
 * immediately. Remaining #153 fields (EXIF metadata, tags, preview) land
 * in a follow-up once #153 closes.
 *
 * WHY "hash === null || hash === quick_hash" is the placeholder condition:
 * pre-V012 `blake3_hash` is NOT NULL (placeholder convention) — the scan
 * path stores `quick_hash` there until `compute_full_hash` promotes the
 * real hash. So `hash === null` never fires in production today. Exposing
 * `quick_hash` from the payload lets us detect the placeholder state via
 * value equality: when `hash === quick_hash` the full canonical hash has
 * not been computed yet. Post-V012 (when blake3_hash becomes nullable)
 * the `hash === null` arm becomes the primary signal; both arms are kept
 * for forward-compat.
 *
 * WHY click-to-copy via navigator.clipboard: spec §4.6.3 calls this out as
 * a small UX win. We keep it to a one-line click handler; no third-party
 * clipboard lib needed.
 */
import { XIcon, GearIcon } from "@phosphor-icons/react";
import type { FileWithTagsPayload } from "../bindings";
import { useComputeFullHash } from "../queries/dedup";
import { TranscribeButton } from "./TranscribeButton";

/** Props for {@link FileSidebar}. */
export interface FileSidebarProps {
  /** The currently-selected file whose details to display. */
  file: FileWithTagsPayload;
  /** Called when the user dismisses the sidebar (e.g. close button). */
  onClose: () => void;
}

/**
 * Sidebar panel showing file UUID, hash status, and a Compute action.
 *
 * Renders inside a fixed-width aside (~300 px). The parent layout is
 * responsible for positioning this alongside the file list.
 */
export default function FileSidebar({ file, onClose }: FileSidebarProps) {
  const compute = useComputeFullHash();

  // WHY two conditions: see module docstring.
  // hash === null → post-V012 nullable convention (forward-compat).
  // hash === quick_hash → pre-V012 placeholder convention (current production).
  const isPlaceholder =
    file.hash === null || (file.quick_hash !== null && file.hash === file.quick_hash);

  function handleCompute() {
    compute.mutate({ fileUuid: file.file_uuid });
  }

  function handleCopyHash() {
    if (file.hash !== null) {
      void navigator.clipboard.writeText(file.hash);
    }
  }

  return (
    <aside
      className="w-96 flex-shrink-0 bg-popover border-l border-border p-6 overflow-y-auto flex flex-col gap-4"
      aria-label="File detail"
    >
      {/* Header */}
      <div className="flex items-center justify-between">
        <h2 className="text-lg font-semibold text-foreground">File detail</h2>
        <button
          onClick={onClose}
          aria-label="Close file detail"
          className="inline-flex items-center justify-center rounded-md p-1 text-muted-foreground hover:text-foreground hover:bg-muted transition-colors duration-micro ease-perima focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <XIcon size={14} weight="bold" />
        </button>
      </div>

      {/* Transcription action row */}
      {/* WHY adjacent to Settings gear: spec §9 places the transcribe trigger
          beside a gear icon that will open the provider settings modal (T10).
          The gear is a stub here — onClick is wired in T10. */}
      <div className="flex items-center gap-2">
        <TranscribeButton
          fileUuid={file.file_uuid}
          fileName={file.relative_path.split("/").pop() ?? file.relative_path}
          source={file.absolute_path}
        />
        {/* TODO(T10): replace stub onClick with settings modal open */}
        <button
          type="button"
          onClick={() => { /* TODO(T10): open transcription settings modal */ }}
          aria-label="Transcription settings"
          className="inline-flex items-center justify-center rounded-md p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted transition-colors duration-micro ease-perima focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <GearIcon size={16} weight="regular" />
        </button>
      </div>

      {/* UUID */}
      <section>
        <p className="eyebrow text-muted-foreground mb-1">
          UUID
        </p>
        <code
          className="mono-metadata text-foreground break-all"
          data-testid="file-uuid"
        >
          {file.file_uuid.slice(0, 8)}…
        </code>
      </section>

      {/* Quick hash */}
      {file.quick_hash !== null && (
        <section>
          <p className="eyebrow text-muted-foreground mb-1">
            Quick hash
          </p>
          <code
            className="mono-metadata text-muted-foreground break-all"
            data-testid="quick-hash"
          >
            {file.quick_hash}
          </code>
        </section>
      )}

      {/* Full hash */}
      <section>
        <p className="eyebrow text-muted-foreground mb-1">
          Full hash
        </p>
        {!isPlaceholder && file.hash !== null ? (
          // WHY click-to-copy on the <code> element: spec §4.6.3 UX win,
          // ≤10 LOC so we land it inline rather than deferring.
          <code
            className="mono-metadata text-foreground break-all cursor-copy hover:text-primary transition-colors duration-micro"
            title="Click to copy"
            onClick={handleCopyHash}
            data-testid="full-hash"
          >
            {file.hash}
          </code>
        ) : (
          <span className="mono-metadata text-muted-foreground italic" data-testid="hash-pending">
            pending
          </span>
        )}
      </section>

      {/* Compute button — visible only when hash is placeholder */}
      {isPlaceholder && (
        <button
          onClick={handleCompute}
          disabled={compute.isPending}
          aria-label="Compute canonical hash"
          data-testid="compute-hash-btn"
          className="mt-auto inline-flex items-center justify-center gap-2 rounded-full px-5 py-2 text-sm font-medium bg-primary text-primary-foreground hover:bg-primary/90 transition-colors duration-micro ease-perima focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:opacity-40 disabled:pointer-events-none"
        >
          {compute.isPending ? "Computing…" : "Compute canonical hash"}
        </button>
      )}
    </aside>
  );
}
