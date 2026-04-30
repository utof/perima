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
import type { FileWithTagsPayload } from "../bindings";
import { useComputeFullHash } from "../queries/dedup";

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
      className="w-72 flex-shrink-0 bg-gray-800 border-l border-gray-700 p-4 overflow-y-auto flex flex-col gap-4"
      aria-label="File detail"
    >
      {/* Header */}
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-gray-200">File detail</h2>
        <button
          onClick={onClose}
          aria-label="Close file detail"
          className="text-gray-400 hover:text-gray-100 text-lg leading-none"
        >
          ×
        </button>
      </div>

      {/* UUID */}
      <section>
        <p className="text-xs text-gray-400 uppercase tracking-wider mb-1">
          UUID
        </p>
        <code
          className="font-mono text-xs text-gray-200 break-all"
          data-testid="file-uuid"
        >
          {file.file_uuid.slice(0, 8)}…
        </code>
      </section>

      {/* Quick hash */}
      {file.quick_hash !== null && (
        <section>
          <p className="text-xs text-gray-400 uppercase tracking-wider mb-1">
            Quick hash
          </p>
          <code
            className="font-mono text-xs text-gray-500 break-all"
            data-testid="quick-hash"
          >
            {file.quick_hash}
          </code>
        </section>
      )}

      {/* Full hash */}
      <section>
        <p className="text-xs text-gray-400 uppercase tracking-wider mb-1">
          Full hash
        </p>
        {!isPlaceholder && file.hash !== null ? (
          // WHY click-to-copy on the <code> element: spec §4.6.3 UX win,
          // ≤10 LOC so we land it inline rather than deferring.
          <code
            className="font-mono text-xs text-gray-200 break-all cursor-copy hover:text-blue-300"
            title="Click to copy"
            onClick={handleCopyHash}
            data-testid="full-hash"
          >
            {file.hash}
          </code>
        ) : (
          <span className="text-xs text-gray-500 italic" data-testid="hash-pending">
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
          className="mt-auto bg-blue-600 hover:bg-blue-500 disabled:bg-gray-600 text-white text-xs font-medium rounded px-3 py-2 transition-colors"
        >
          {compute.isPending ? "Computing…" : "Compute canonical hash"}
        </button>
      )}
    </aside>
  );
}
