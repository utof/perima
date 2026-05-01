/**
 * `/dedup` route — candidate-duplicate management UI per spec §4.6.2.
 *
 * Surfaces:
 *   - Virtualised list of candidate groups (`@tanstack/react-virtual`).
 *   - Per-group "Verify this group" button → `compute_full_hash_batch`
 *     for that group's `file_uuid`s only.
 *   - Global "Verify all" button → batch over the union of every group's
 *     `file_uuid`s (footgun-aware: explicit opt-in for 10TB libraries).
 *   - Live progress display from the Zustand `verifyBatch` slice
 *     ("X of Y done — last: <filename>"). The slice is populated by
 *     `useDomainEvents` from `AppEvent::VerifyProgress` events.
 *   - "Cancel verify batch" button — only visible when a batch is active.
 *
 * State sources:
 *   - server: `useCollisions` (TanStack Query, invalidated on
 *             `IndexInvalidated::CollisionsChanged` + `VerifyComplete`).
 *   - UI:     `useUiStore.verifyBatch` (push value from VerifyProgress).
 *   - mutation: `useVerifyBatch` / `useCancelVerifyBatch` (TanStack
 *             Query mutations; errors flow into the notification toast).
 *
 * WHY virtualised list: collision groups can scale into the thousands on
 * large media libraries (many small recurring filenames). Rendering them
 * all up-front would blow the React commit budget. `react-virtual` only
 * mounts visible rows, with `measureElement` for variable-height groups
 * (a 2-file group is shorter than a 6-file group).
 *
 * WHY no manual `useMemo` on derivations: React Compiler 1.0 (L2) handles
 * referentially-stable inputs automatically per Batch H standing
 * constraints.
 */
import { useRef } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useUiStore } from "../stores/ui";
import {
  useCollisions,
  useVerifyBatch,
  useCancelVerifyBatch,
} from "../queries/dedup";
import type { CollisionGroup, FileLocationRecord } from "../bindings";

/** Human-readable byte size (e.g. `"1.5 MB"`). Mirrors FileTable.humanSize. */
function humanSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/**
 * Extract a leaf filename from a forward-slash relative path.
 *
 * WHY no Node `path.basename`: Tauri WebView is browser-side; `path` is
 * not available. The backend already enforces forward-slash separators
 * (`MediaPath`), so a single `lastIndexOf("/")` is sufficient.
 */
function basename(p: string): string {
  const i = p.lastIndexOf("/");
  return i === -1 ? p : p.slice(i + 1);
}

interface GroupRowProps {
  group: CollisionGroup;
  onVerify: () => void;
  isVerifying: boolean;
}

function GroupRow({ group, onVerify, isVerifying }: GroupRowProps) {
  const fileCount = group.files.length;
  // WHY representative size from first file: candidate groups share the
  // same `quick_hash`, which by spec §4.1.1 derives from `(size, head,
  // tail)` — every member has the same byte size. Showing the first is
  // accurate without iterating.
  const sizeLabel = fileCount > 0 ? humanSize(group.files[0]!.size) : "0 B";
  const stateLabel =
    group.verified_state === "VerifiedDuplicate"
      ? " ✓ duplicate"
      : group.verified_state === "VerifiedDistinct"
        ? " ✓ distinct"
        : group.verified_state === "Mixed"
          ? " ⚠ mixed"
          : "";

  return (
    <div
      className="bg-card rounded-md shadow-e1 p-4 mb-3 border border-border"
      data-testid="dedup-group-row"
    >
      <header className="flex items-center justify-between mb-2">
        <div className="text-sm text-foreground font-medium">
          Candidate group — {fileCount} file{fileCount === 1 ? "" : "s"}, {sizeLabel} each
          <span className="text-xs text-muted-foreground ml-2">
            {group.quick_hash.slice(0, 12)}…{stateLabel}
          </span>
        </div>
        <button
          type="button"
          onClick={onVerify}
          disabled={isVerifying}
          className="inline-flex items-center justify-center rounded-full px-3 py-1 text-xs font-medium bg-primary text-primary-foreground hover:bg-primary/90 transition-colors duration-micro disabled:opacity-40 disabled:pointer-events-none focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
        >
          {isVerifying ? "Verifying…" : "Verify this group"}
        </button>
      </header>
      <ul className="space-y-1 pl-2 mono-metadata text-foreground">
        {group.files.map((f: FileLocationRecord) => (
          <li
            key={f.file_uuid}
            title={`${f.volume_id}/${f.relative_path}`}
            className="truncate"
          >
            <span className="text-muted-foreground">{f.volume_id.slice(0, 6)}/</span>
            {basename(f.relative_path)}
            <span className="text-muted-foreground"> ({f.relative_path})</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

export default function DedupRoute() {
  const { data: groups = [], isLoading, error } = useCollisions();
  const verifyBatch = useUiStore((s) => s.verifyBatch);
  const notifyError = useUiStore((s) => s.notifyError);

  const verifyMutation = useVerifyBatch();
  const cancelMutation = useCancelVerifyBatch();

  const parentRef = useRef<HTMLDivElement>(null);
  // WHY estimateSize=160: typical group with 3 files renders ~160px tall
  // (header + 3 paths + margins). `measureElement` corrects after first
  // mount so the estimate only matters until the row is observed.
  // WHY eslint-disable react-hooks/incompatible-library: React Compiler
  // 1.0 cannot safely memoize `useVirtualizer`'s return (returns functions
  // that close over scroll-position state and would go stale if memoized).
  // The compiler explicitly skips this hook; we acknowledge the warning
  // here rather than re-tooling the route to side-step virtualisation.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: groups.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 160,
    overscan: 4,
  });

  const handleVerifyGroup = (group: CollisionGroup) => {
    const fileUuids = group.files.map((f) => f.file_uuid);
    verifyMutation.mutate(
      { fileUuids },
      {
        onError: (err) => {
          notifyError(err);
        },
      },
    );
  };

  const handleVerifyAll = () => {
    // WHY union (not deduped): Set semantics aren't required — a single
    // file_uuid only appears in one collision group (groups partition by
    // quick_hash, and a file has exactly one quick_hash). Flat-mapping is
    // sufficient and avoids importing a Set just to spread it back.
    const fileUuids = groups.flatMap((g) => g.files.map((f) => f.file_uuid));
    if (fileUuids.length === 0) return;
    verifyMutation.mutate(
      { fileUuids },
      {
        onError: (err) => {
          notifyError(err);
        },
      },
    );
  };

  const handleCancel = () => {
    if (verifyBatch === null) return;
    cancelMutation.mutate(
      { batchId: verifyBatch.batchId },
      {
        onError: (err) => {
          // WHY swallow NotFound: batch already finished; nothing to do.
          if (err.kind === "NotFound") return;
          notifyError(err);
        },
      },
    );
  };

  // ── Render branches ────────────────────────────────────────────────────
  if (isLoading) {
    return (
      <div className="flex-1 p-8 bg-background text-muted-foreground">
        <p>Loading candidate groups…</p>
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex-1 p-8 bg-background text-destructive">
        <p>Failed to load candidate groups: [{error.kind}]</p>
      </div>
    );
  }

  if (groups.length === 0) {
    return (
      <div className="flex-1 p-8 bg-background text-muted-foreground" data-testid="dedup-empty-state">
        <p>No candidate duplicates.</p>
      </div>
    );
  }

  // WHY pre-flatten progress label: keeps JSX terse + avoids inline
  // truthy chaining when verifyBatch is null. Computed once per render.
  const progressLabel = verifyBatch
    ? `${verifyBatch.filesDone} of ${verifyBatch.filesTotal} done${
        verifyBatch.latestOutcome
          ? ` — last: ${
              verifyBatch.latestOutcome.outcome === "Computed"
                ? verifyBatch.latestOutcome.data.file_uuid.slice(0, 8)
                : `error on ${verifyBatch.latestOutcome.data.file_uuid.slice(0, 8)}`
            }`
          : ""
      }`
    : null;

  const items = virtualizer.getVirtualItems();
  const totalSize = virtualizer.getTotalSize();

  return (
    <div className="flex-1 flex flex-col overflow-hidden bg-background p-6">
      <header className="mb-6 flex items-center justify-between">
        <h1 className="text-lg font-semibold text-foreground">
          Candidate duplicate groups ({groups.length})
        </h1>
        <div className="flex items-center gap-2">
          {progressLabel && (
            <span
              className="text-xs text-info"
              data-testid="dedup-progress-label"
            >
              {progressLabel}
            </span>
          )}
          {verifyBatch !== null && (
            <button
              type="button"
              onClick={handleCancel}
              disabled={cancelMutation.isPending}
              className="inline-flex items-center justify-center rounded-full px-3 py-1 text-xs font-medium bg-destructive text-destructive-foreground hover:opacity-90 transition-opacity duration-micro disabled:opacity-40 disabled:pointer-events-none focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
              data-testid="dedup-cancel-button"
            >
              Cancel verify batch
            </button>
          )}
          <button
            type="button"
            onClick={handleVerifyAll}
            disabled={verifyMutation.isPending || verifyBatch !== null}
            className="inline-flex items-center justify-center rounded-full px-3 py-1 text-xs font-medium bg-warning text-warning-foreground hover:opacity-90 transition-opacity duration-micro disabled:opacity-40 disabled:pointer-events-none focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
            data-testid="dedup-verify-all-button"
            title="Slow — full hash on every candidate"
          >
            Verify all (slow)
          </button>
        </div>
      </header>
      <div
        ref={parentRef}
        className="flex-1 overflow-auto rounded-md border border-border"
        style={{ contain: "strict" }}
      >
        <div
          style={{
            height: `${totalSize}px`,
            width: "100%",
            position: "relative",
          }}
        >
          {items.map((virtualRow) => {
            const group = groups[virtualRow.index];
            if (!group) return null;
            return (
              <div
                key={virtualRow.key}
                data-index={virtualRow.index}
                ref={virtualizer.measureElement}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  transform: `translateY(${virtualRow.start}px)`,
                }}
              >
                <div className="px-2 py-1">
                  <GroupRow
                    group={group}
                    onVerify={() => { handleVerifyGroup(group); }}
                    isVerifying={verifyMutation.isPending || verifyBatch !== null}
                  />
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
