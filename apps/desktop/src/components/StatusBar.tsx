/**
 * Status footer. Reads scan slice from useUiStore + collision groups from
 * useCollisions().
 *
 * WHY useShallow for scan slice: it returns an object with status + lastReport;
 * inline destructuring without useShallow would crash with
 * "Maximum update depth exceeded" in Zustand v5.
 *
 * WHY errorKindLabel: StatusBar is the canonical component that proves typed
 * CoreError flows end-to-end (CLAUDE.md IPC boundary contract). The exhaustive
 * switch(err.kind) with a `never` default turns a missing variant into a
 * TypeScript compile error — no silent degradation when a new CoreError variant
 * is added.
 *
 * WHY CollisionPill alongside scan summary: the status bar is the persistent
 * global chrome visible on every route; surfacing collision count here gives
 * a persistent nudge without navigating away from the current task.
 */
import { useShallow } from "zustand/shallow";
import { useUiStore } from "../stores/ui";
import { useCollisions } from "../queries/dedup";
import { useBackupDatabase } from "../queries/backup";
import CollisionPill from "./CollisionPill";
import { coreErrorMessage } from "../lib/coreError";
import type { BackupFailureReason, CoreError, FullHashUnavailableReason } from "../bindings";

/**
 * Returns a short human-readable label for a {@link CoreError}, branching on
 * `err.kind` with a TypeScript-exhaustive `never` default.
 *
 * WHY: proves the typed CoreError discriminated union flows end-to-end through
 * the IPC boundary. The `never` default is a compile-time assertion — adding a
 * new `CoreError` variant without extending this switch becomes a type error.
 */
export function errorKindLabel(err: CoreError): string {
  switch (err.kind) {
    case "NotFound":
      return `Not found: ${err.data}`;
    case "Duplicate":
      return `Duplicate: ${err.data}`;
    case "InvalidPath":
      return `Invalid path: ${err.data}`;
    case "InvalidHash":
      return `Invalid hash: ${err.data}`;
    case "InvalidTag":
      return `Invalid tag: ${err.data}`;
    case "Io":
      return `I/O error [${err.data.kind}]: ${err.data.message}`;
    case "Unsupported":
      return `Unsupported: ${err.data}`;
    case "Internal":
      return `Internal error: ${err.data}`;
    case "FullHashUnavailable":
      return `Full hash unavailable: ${fullHashUnavailableReasonLabel(err.data.reason)}`;
    case "BackupFailed":
      return `Backup failed: ${backupFailureReasonLabel(err.data.reason)}`;
    case "Transcription":
      // WHY delegate to coreErrorMessage: the inner TranscriptionError variant
      // switch lives in lib/coreError.ts (single source of truth). StatusBar
      // only needs the user-facing string; per-variant rendering is a follow-up.
      return `Transcription error: ${coreErrorMessage(err)}`;
    default: {
      // WHY never: TypeScript exhaustiveness check. If a new CoreError variant
      // is added to bindings.ts without a matching case above, this line
      // becomes a type error at compile time (not at runtime).
      const _exhaustive: never = err;
      return `Unknown error: ${String(_exhaustive)}`;
    }
  }
}

/**
 * Returns a short label for a {@link BackupFailureReason}.
 */
function backupFailureReasonLabel(reason: BackupFailureReason): string {
  switch (reason.kind) {
    case "TargetExists":
      return `file already exists at ${reason.data.path}`;
    case "TargetUnwritable":
      return `cannot write to ${reason.data.path}: ${reason.data.message}`;
    case "DiskFull":
      return `disk full at ${reason.data.path}`;
    case "AlreadyInProgress":
      return "already in progress";
    case "Internal":
      return reason.data;
    default: {
      const _exhaustive: never = reason;
      return String(_exhaustive);
    }
  }
}

/**
 * Returns a short label for a {@link FullHashUnavailableReason}.
 */
function fullHashUnavailableReasonLabel(reason: FullHashUnavailableReason): string {
  switch (reason.kind) {
    case "NotMounted":
      return `volume not mounted (${reason.volume_id})`;
    case "NotComputed":
      return "not yet computed";
    case "IoError":
      return `I/O: ${reason.message}`;
    default: {
      const _exhaustive: never = reason;
      return String(_exhaustive);
    }
  }
}

export default function StatusBar() {
  const { status, lastReport } = useUiStore(
    useShallow((s) => ({ status: s.scan.status, lastReport: s.scan.lastReport })),
  );
  // WHY data ?? []: when the query is loading or errored, default to empty
  // so CollisionPill renders the neutral "no candidate duplicates" state
  // rather than crashing on undefined.
  const { data: collisions = [] } = useCollisions();
  const backupMutation = useBackupDatabase();

  let summary: string;
  if (status === "scanning") {
    summary = "Scanning…";
  } else if (lastReport !== null) {
    summary = `Last scan: ${lastReport.files_seen} files`;
  } else {
    summary = "Ready";
  }

  return (
    <div className="px-6 py-2 bg-card text-muted-foreground text-xs border-t border-border flex justify-between items-center">
      <span>{summary}</span>
      <div className="flex items-center gap-3">
        {/* WHY type="button": prevents form submission if StatusBar is ever
            rendered inside a form element. */}
        <button
          type="button"
          onClick={() => { backupMutation.mutate({}); }}
          disabled={backupMutation.isPending}
          className="rounded-full px-3 py-1 text-sm font-medium bg-secondary text-secondary-foreground
                     hover:bg-popover transition-colors duration-micro ease-perima
                     focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring
                     focus-visible:ring-offset-2 focus-visible:ring-offset-background
                     disabled:opacity-40 disabled:pointer-events-none"
        >
          {backupMutation.isPending ? "Backing up…" : "Backup"}
        </button>
        <CollisionPill groups={collisions} />
      </div>
    </div>
  );
}
