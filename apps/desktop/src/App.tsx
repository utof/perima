import { useEffect, useState } from "react";
import * as api from "./api";
import type { UnsubscribeFn } from "./api";
import FileGrid from "./components/FileGrid";
import FileTable from "./components/FileTable";
import ScanButton from "./components/ScanButton";
import StatusBar from "./components/StatusBar";
import TagSidebar from "./components/TagSidebar";
import WatcherBanner from "./components/WatcherBanner";
import type { FileWithTags, ScanResult, Tag } from "./types";

/**
 * Which rendering mode the main file list uses.
 *
 * WHY default `"table"`: v0.3.x / v0.4.0 shipped only the table view.
 * Keeping table as the startup mode preserves UX continuity; the grid
 * opts in on user demand.
 */
type ViewMode = "table" | "grid";

/**
 * Root application shell.
 *
 * Manages global state and composes the three main UI components.
 * WHY: Single top-level state owner keeps data flow simple for the current
 * feature set; introduce a state library (zustand / jotai) when the number of
 * consumers grows beyond 2–3 components.
 */
export default function App() {
  const [files, setFiles] = useState<FileWithTags[]>([]);
  const [tags, setTags] = useState<Tag[]>([]);
  // WHY string | null (not Set<string>): spec models multi-select as Set<string>
  // but v0.5.1 ships single-select only. Using null for "All" is simpler and
  // avoids converting Set → serializable state. Upgrade to Set when multi-select lands.
  const [selectedTagId, setSelectedTagId] = useState<string | null>(null);
  const [scanResult, setScanResult] = useState<ScanResult | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // WHY: Watcher failures are non-blocking (the table is still accurate,
  // just not live-updating). Surface them via a dismissible banner rather
  // than the scan `error` state so they don't mask the StatusBar output.
  const [watcherError, setWatcherError] = useState<string | null>(null);
  // WHY single fetch for both views: `listFilesWithTags` returns a
  // strict superset of `listFiles`, so the table reads the same rows it
  // used to and the grid gets its thumbnail fields "for free" — no need
  // to double-fetch on toggle.
  const [viewMode, setViewMode] = useState<ViewMode>("table");

  useEffect(() => {
    // WHY: Populate the list on mount so existing indexed files are visible
    // immediately without requiring the user to trigger a scan.
    api.listFilesWithTags(100).match(
      (result) => {
        setFiles(result);
        setLoading(false);
      },
      (err) => {
        setError(err);
        setLoading(false);
      },
    );
    api.listTags().match(
      (result) => setTags(result),
      () => {
        // WHY: tag fetch failure is non-fatal; the file list still renders.
      },
    );
  }, []);

  useEffect(() => {
    // WHY: Coalesce filesystem event bursts (e.g., saving a file triggers
    // several low-level events) into a single refresh. 300 ms was chosen
    // in the spec — short enough to feel live, long enough to absorb
    // typical editor save storms.
    let timer: ReturnType<typeof setTimeout> | null = null;
    let unsubscribe: UnsubscribeFn | null = null;
    // WHY: Guard against setState after unmount when the subscribe promise
    // or the debounced refresh resolves post-cleanup.
    let active = true;

    api
      .subscribeToFileEvents(() => {
        if (timer) clearTimeout(timer);
        timer = setTimeout(() => {
          // WHY no listTags() here: the file watcher fires on filesystem
          // events (file created/deleted/modified). Tags are only mutated
          // via explicit Tauri commands from within this app — no external
          // process can change file_tags without going through Tauri. A
          // tag re-fetch on every file event would be wasteful and
          // incorrect; tags refresh after scan (handleScanComplete) where
          // new tags may actually have been created.
          api.listFilesWithTags(100).match(
            (refreshed) => {
              if (active) setFiles(refreshed);
            },
            (err) => {
              if (active) setError(err);
            },
          );
        }, 300);
      })
      .then((fn) => {
        if (active) {
          unsubscribe = fn;
        } else {
          // Already unmounted before the listener was registered; tear down.
          fn();
        }
      })
      .catch((err) => {
        const msg = err instanceof Error ? err.message : String(err);
        if (active) {
          setWatcherError(`Failed to subscribe to watcher events: ${msg}`);
        }
      });

    return () => {
      active = false;
      if (timer) clearTimeout(timer);
      if (unsubscribe) unsubscribe();
    };
  }, []);

  function handleScanStart() {
    setScanning(true);
    setError(null);
  }

  function handleScanComplete(result: ScanResult, path: string) {
    setScanResult(result);
    setScanning(false);
    // Refresh file list and tags after a successful scan.
    api.listFilesWithTags(100).match(
      (refreshed) => setFiles(refreshed),
      (err) => setError(err),
    );
    api.listTags().match(
      (refreshed) => setTags(refreshed),
      () => {
        // WHY: tag fetch failure is non-fatal after scan.
      },
    );
    // WHY: Auto-start the watcher on the folder we just scanned so live
    // updates flow without an extra user gesture. Non-blocking: failures
    // are logged but must not prevent the scan from being reported as
    // complete.
    api.startWatch(path).match(
      () => setWatcherError(null),
      (err) => setWatcherError(`Failed to start watcher: ${err}`),
    );
  }

  // Compute per-tag file counts from the full unfiltered list.
  // WHY client-side from the 100-row cap: listFilesWithTags(100) returns at
  // most 100 rows, so counts may be understated for large libraries. Acceptable
  // for v0.5.1; a dedicated count_files_for_tag Tauri command lands with FTS5.
  const counts: Record<string, number> = {};
  for (const f of files) {
    for (const t of f.tags) {
      counts[t.id] = (counts[t.id] ?? 0) + 1;
    }
  }

  // Filter the visible file list by the selected tag (client-side).
  // WHY single-select: Set<string> multi-select deferred until post-v1.
  const visibleFiles =
    selectedTagId === null
      ? files
      : files.filter((f) => f.tags.some((t) => t.id === selectedTagId));

  return (
    <div className="bg-gray-900 text-gray-100 min-h-screen flex flex-col">
      <header className="flex items-center justify-between px-6 py-4 bg-gray-800 border-b border-gray-700">
        <h1 className="text-xl font-bold tracking-wide">perima</h1>
        <div className="flex items-center gap-3">
          <ViewModeToggle mode={viewMode} onChange={setViewMode} />
          <ScanButton
            onScanComplete={handleScanComplete}
            onScanStart={handleScanStart}
            scanning={scanning}
          />
        </div>
      </header>

      <WatcherBanner
        message={watcherError}
        onDismiss={() => setWatcherError(null)}
      />

      <div className="flex-1 flex overflow-hidden">
        {tags.length > 0 && (
          <TagSidebar
            tags={tags}
            counts={counts}
            totalCount={files.length}
            selectedTagId={selectedTagId}
            onSelect={setSelectedTagId}
          />
        )}
        <main className="flex-1 overflow-auto p-4">
          {viewMode === "table" ? (
            <FileTable files={visibleFiles} loading={loading} />
          ) : (
            <FileGrid files={visibleFiles} loading={loading} />
          )}
        </main>
      </div>

      <footer>
        <StatusBar scanResult={scanResult} error={error} />
      </footer>
    </div>
  );
}

/**
 * Segmented toggle between the table and grid views.
 *
 * WHY segmented control (not a single button that flips): two explicit
 * labels make the inactive option discoverable at a glance and match
 * desktop convention (Finder/Files-style switchers).
 */
function ViewModeToggle({
  mode,
  onChange,
}: {
  mode: ViewMode;
  onChange: (next: ViewMode) => void;
}) {
  const base =
    "px-3 py-1.5 text-sm font-medium rounded transition-colors focus:outline-none focus:ring-2 focus:ring-blue-500";
  const active = "bg-blue-600 text-white";
  const inactive = "bg-gray-700 text-gray-200 hover:bg-gray-600";
  return (
    <div
      className="inline-flex items-center gap-1 bg-gray-900 rounded p-0.5"
      role="group"
      aria-label="View mode"
    >
      <button
        type="button"
        className={`${base} ${mode === "table" ? active : inactive}`}
        aria-pressed={mode === "table"}
        onClick={() => onChange("table")}
      >
        Table
      </button>
      <button
        type="button"
        className={`${base} ${mode === "grid" ? active : inactive}`}
        aria-pressed={mode === "grid"}
        onClick={() => onChange("grid")}
      >
        Grid
      </button>
    </div>
  );
}
