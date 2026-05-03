/**
 * Global UI state — Zustand 5, slice pattern.
 *
 * 5 slices:
 *   - ViewSlice         — viewMode, selectedTagId
 *   - SearchSlice       — searchQuery (raw input), debouncedQuery (drives useSearch)
 *   - ScanSlice         — status, lastReport (StatusBar reads these)
 *   - NotificationsSlice — id-keyed toast queue
 *   - SelectionSlice    — selectedFileUuid (drives FileSidebar)
 *
 * WHY single store (not 5 separate stores): components read across
 * concerns (e.g. StatusBar reads scan + notifications). One store +
 * `useShallow` for multi-property reads keeps the API uniform.
 *
 * WHY no Provider: Zustand stores are module-level singletons.
 *
 * WHY no `persist` middleware: Tauri restarts intentionally reset UI
 * state. DB persists scanned files; UI selections are ephemeral.
 */
import { create, type StateCreator } from "zustand";
import { coreErrorMessage } from "../lib/coreError";
import type {
  BatchId,
  CoreError,
  FileUuid,
  FullHashOutcome,
  ScanReport,
  TranscriptionError,
} from "../bindings";

type ViewMode = "table" | "grid";

interface ViewSlice {
  viewMode: ViewMode;
  selectedTagId: string | null;
  setViewMode: (mode: ViewMode) => void;
  setSelectedTagId: (id: string | null) => void;
}

interface SearchSlice {
  searchQuery: string;
  debouncedQuery: string;
  setSearchQuery: (q: string) => void;
  setDebouncedQuery: (q: string) => void;
}

type ScanStatus = "idle" | "scanning" | "done";
interface ScanSlice {
  scan: { status: ScanStatus; lastReport: ScanReport | null };
  setScanStatus: (status: ScanStatus) => void;
  setLastScanReport: (report: ScanReport) => void;
}

type NotificationKind = "info" | "error";
export interface Notification {
  id: string;
  kind: NotificationKind;
  message: string;
}
interface NotificationsSlice {
  notifications: Notification[];
  notify: (kind: NotificationKind, message: string) => void;
  notifyError: (err: CoreError) => void;
  dismiss: (id: string) => void;
}

/**
 * Per-batch full-hash compute progress, pushed via `AppEvent::VerifyProgress`.
 *
 * WHY nullable top-level: when no batch is running the field is `null` so
 * consumers can quickly gate on "batch active" without checking inner fields.
 * Task 13 (`/dedup` route) subscribes to this slice to drive a progress bar.
 */
interface VerifyBatchSlice {
  verifyBatch: {
    batchId: BatchId;
    filesDone: number;
    filesTotal: number;
    latestOutcome: FullHashOutcome | null;
  } | null;
  setVerifyBatchProgress: (
    batchId: BatchId,
    filesDone: number,
    filesTotal: number,
    latestOutcome: FullHashOutcome,
  ) => void;
  clearVerifyBatch: () => void;
}

/**
 * File selection state — drives the `FileSidebar` panel.
 *
 * WHY nullable (not string | undefined): null means "no file selected"
 * and is the Zustand-idiomatic sentinel for optional singular selection.
 * Toggle-to-deselect: clicking the same row sets it back to null.
 */
interface SelectionSlice {
  selectedFileUuid: FileUuid | null;
  setSelectedFileUuid: (uuid: FileUuid | null) => void;
}

/**
 * Discriminated-union status of a single in-flight transcription job.
 *
 * Wire shape mirrors `AppEvent::Transcription*` payloads (see bindings.ts):
 * each event arm folds into one status variant via `useDomainEvents`.
 *
 * WHY discriminated union (not flat fields): `progressPercent: number | null`
 * + `error: TranscriptionError | null` (the spec's flat shape) loses the
 * compile-time guarantee that "completed" jobs have a `transcript_id` and
 * "failed" jobs have an `error`. The discriminated union lets components
 * pattern-match without nullable-field-juggling.
 */
export type TranscriptionJobStatus =
  | { kind: "queued"; queue_position: number }
  | { kind: "running"; processed_ms: number; total_ms: number | null }
  | {
      kind: "completed";
      transcript_id: string;
      segment_count: number;
      language: string | null;
    }
  | { kind: "cancelled" }
  | { kind: "failed"; error: TranscriptionError };

/**
 * One in-flight (or recently-terminated) transcription job.
 *
 * Lifecycle: created on `AppEvent::TranscriptionStarted`; mutated on every
 * `Transcription{Progress,Completed,Cancelled,Failed}` for the same
 * `request_uuid`; auto-removed by `useDomainEvents` after a terminal-state
 * grace period (5s for Completed, 3s for Cancelled). `failed` jobs persist
 * until the user dismisses them via `removeJob`.
 */
export interface TranscriptionJob {
  /** Per-request UUIDv7 (lowercase-hex simple form). The map key. */
  request_uuid: string;
  /** Stable file surrogate (FileUuid) the job is transcribing. */
  file_uuid: string;
  /** Display name (basename or curated label) for UI surfacing. */
  file_name: string;
  /** Current discriminated-union status. */
  status: TranscriptionJobStatus;
  /** `Date.now()` captured at `startJob` time; used for relative timestamps. */
  started_at_ms: number;
}

/**
 * In-flight transcription jobs, keyed by `request_uuid`.
 *
 * WHY a `Record` (not an array): `useDomainEvents` looks up by request_uuid
 * on every Progress / Completed / Cancelled / Failed event; an array would
 * force an O(n) `find` per event. The map keeps mutation O(1) and matches
 * the natural identity of a job (its UUIDv7).
 *
 * WHY no Zustand `persist`: terminal jobs are intentionally lost on app
 * restart — they're ephemeral UX, not server state. The DB owns durable
 * `transcript` rows; this slice is the in-flight progress mirror.
 *
 * WHY `updateJob` and `removeJob` are no-ops on missing keys: the
 * auto-remove timer in `useDomainEvents` (5s post-Completed, 3s post-
 * Cancelled) can race with a user-initiated `removeJob` from the
 * `<TranscriptionPill>` popover. Defensive no-ops collapse the race
 * without requiring lookup-then-update at every call site.
 */
export interface TranscriptionSlice {
  /** Map of in-flight + recently-terminated jobs. */
  transcription: {
    jobs: Record<string, TranscriptionJob>;
    startJob: (job: TranscriptionJob) => void;
    updateJob: (request_uuid: string, status: TranscriptionJobStatus) => void;
    removeJob: (request_uuid: string) => void;
  };
}

export type UiStore = ViewSlice
  & SearchSlice
  & ScanSlice
  & NotificationsSlice
  & VerifyBatchSlice
  & SelectionSlice
  & TranscriptionSlice;

const createViewSlice: StateCreator<UiStore, [], [], ViewSlice> = (set) => ({
  viewMode: "table",
  selectedTagId: null,
  setViewMode: (mode) => { set({ viewMode: mode }); },
  setSelectedTagId: (id) => { set({ selectedTagId: id }); },
});

const createSearchSlice: StateCreator<UiStore, [], [], SearchSlice> = (set) => ({
  searchQuery: "",
  debouncedQuery: "",
  setSearchQuery: (q) => { set({ searchQuery: q }); },
  setDebouncedQuery: (q) => { set({ debouncedQuery: q }); },
});

const createScanSlice: StateCreator<UiStore, [], [], ScanSlice> = (set) => ({
  scan: { status: "idle", lastReport: null },
  setScanStatus: (status) => {
    set((s) => ({ scan: { ...s.scan, status } }));
  },
  setLastScanReport: (report) => {
    set((s) => ({ scan: { ...s.scan, lastReport: report } }));
  },
});

let notificationIdCounter = 0;
const nextId = () => `n${++notificationIdCounter}`;

const createNotificationsSlice: StateCreator<UiStore, [], [], NotificationsSlice> = (set) => ({
  notifications: [],
  notify: (kind, message) => {
    const id = nextId();
    set((s) => ({ notifications: [...s.notifications, { id, kind, message }] }));
  },
  notifyError: (err) => {
    const id = nextId();
    // WHY use lib/coreError::coreErrorMessage: single source of truth for
    // CoreError → display string. Verified no circular import — coreError.ts
    // only imports `type CoreError from ../bindings` (type-only); stores/ui.ts
    // does the same. Chain is one-way.
    const message = `[${err.kind}] ${coreErrorMessage(err)}`;
    set((s) => ({ notifications: [...s.notifications, { id, kind: "error", message }] }));
  },
  dismiss: (id) => {
    set((s) => ({ notifications: s.notifications.filter((n) => n.id !== id) }));
  },
});

const createVerifyBatchSlice: StateCreator<UiStore, [], [], VerifyBatchSlice> = (set) => ({
  verifyBatch: null,
  setVerifyBatchProgress: (batchId, filesDone, filesTotal, latestOutcome) => {
    set({ verifyBatch: { batchId, filesDone, filesTotal, latestOutcome } });
  },
  clearVerifyBatch: () => {
    set({ verifyBatch: null });
  },
});

const createSelectionSlice: StateCreator<UiStore, [], [], SelectionSlice> = (set) => ({
  selectedFileUuid: null,
  setSelectedFileUuid: (uuid) => { set({ selectedFileUuid: uuid }); },
});

const createTranscriptionSlice: StateCreator<UiStore, [], [], TranscriptionSlice> = (set) => ({
  transcription: {
    jobs: {},
    startJob: (job) => {
      set((s) => ({
        transcription: {
          ...s.transcription,
          jobs: { ...s.transcription.jobs, [job.request_uuid]: job },
        },
      }));
    },
    updateJob: (request_uuid, status) => {
      set((s) => {
        const existing = s.transcription.jobs[request_uuid];
        // WHY no-op on missing key: see TranscriptionSlice doc — auto-remove
        // timers in `useDomainEvents` race with user-initiated removeJob.
        if (!existing) return s;
        return {
          transcription: {
            ...s.transcription,
            jobs: { ...s.transcription.jobs, [request_uuid]: { ...existing, status } },
          },
        };
      });
    },
    removeJob: (request_uuid) => {
      set((s) => {
        if (!(request_uuid in s.transcription.jobs)) return s;
        // WHY destructure-and-discard: produces a shallow copy without the
        // removed key; immutable update keeps Zustand subscribers happy.
        const { [request_uuid]: _removed, ...rest } = s.transcription.jobs;
        return { transcription: { ...s.transcription, jobs: rest } };
      });
    },
  },
});

export const useUiStore = create<UiStore>()((...a) => ({
  ...createViewSlice(...a),
  ...createSearchSlice(...a),
  ...createScanSlice(...a),
  ...createNotificationsSlice(...a),
  ...createVerifyBatchSlice(...a),
  ...createSelectionSlice(...a),
  ...createTranscriptionSlice(...a),
}));
