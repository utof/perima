/**
 * Global UI state — Zustand 5, slice pattern.
 *
 * 4 slices:
 *   - ViewSlice         — viewMode, selectedTagId
 *   - SearchSlice       — searchQuery (raw input), debouncedQuery (drives useSearch)
 *   - ScanSlice         — status, lastReport (StatusBar reads these)
 *   - NotificationsSlice — id-keyed toast queue
 *
 * WHY single store (not 4 separate stores): components read across
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
import type { CoreError, ScanReport } from "../bindings";

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

export type UiStore = ViewSlice & SearchSlice & ScanSlice & NotificationsSlice;

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

export const useUiStore = create<UiStore>()((...a) => ({
  ...createViewSlice(...a),
  ...createSearchSlice(...a),
  ...createScanSlice(...a),
  ...createNotificationsSlice(...a),
}));
