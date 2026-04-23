import { useCallback, useEffect, useState } from "react";
import * as api from "./api";
import type { UnsubscribeFn } from "./api";
import FileGrid from "./components/FileGrid";
import FileTable from "./components/FileTable";
import ScanButton from "./components/ScanButton";
import SearchBar from "./components/SearchBar";
import StatusBar from "./components/StatusBar";
import TagSidebar from "./components/TagSidebar";
import WatcherBanner from "./components/WatcherBanner";
import { composeVisible, computeFacets, sortByRank } from "./lib/search";
import { coreErrorMessage } from "./lib/coreError";
import type { AppEvent, CoreError, FileWithTagsPayload, ScanReport, SearchHit, Tag } from "./bindings";

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
  const [files, setFiles] = useState<FileWithTagsPayload[]>([]);
  const [tags, setTags] = useState<Tag[]>([]);
  // WHY string | null (not Set<string>): spec models multi-select as Set<string>
  // but v0.5.1 ships single-select only. Using null for "All" is simpler and
  // avoids converting Set → serializable state. Upgrade to Set when multi-select lands.
  const [selectedTagId, setSelectedTagId] = useState<string | null>(null);
  const [scanResult, setScanResult] = useState<ScanReport | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<CoreError | null>(null);
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
  const [searchHits, setSearchHits] = useState<Set<string> | null>(null);
  const [hitRanks, setHitRanks] = useState<Map<string, number>>(new Map());
  // WHY: stored for future status-line / search-persistence use (Task 8+).
  // Not yet consumed in render; ESLint-silenced rather than dropped per spec
  // section "State (owned by App.tsx)".
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const [searchQuery, setSearchQuery] = useState("");

  useEffect(() => {
    // WHY: Populate the list on mount so existing indexed files are visible
    // immediately without requiring the user to trigger a scan.
    void api.listFilesWithTags(100).match(
      (result) => {
        setFiles(result);
        setLoading(false);
      },
      (err) => {
        setError(err);
        setLoading(false);
      },
    );
    void api.listTags().match(
      (result) => { setTags(result); },
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

    /** Shared refetch helper — called by multiple AppEvent branches. */
    const refetch = () => {
      // WHY no listTags() here: the file watcher fires on filesystem
      // events (file created/deleted/modified). Tags are only mutated
      // via explicit Tauri commands from within this app — no external
      // process can change file_tags without going through Tauri. A
      // tag re-fetch on every file event would be wasteful and
      // incorrect; tags refresh after scan (handleScanComplete) where
      // new tags may actually have been created.
      void api.listFilesWithTags(100).match(
        (refreshed) => {
          if (active) setFiles(refreshed);
        },
        (err) => {
          if (active) setError(err);
        },
      );
    };

    api
      .subscribeToAppEvents((event: AppEvent) => {
        switch (event.kind) {
          case "File":
            // WHY 300ms debounce: a watcher burst (e.g., file-copy of 100
            // files) shouldn't trigger 100 list_files_with_tags refetches.
            if (timer) clearTimeout(timer);
            timer = setTimeout(refetch, 300);
            break;
          case "ScanCompleted":
            // WHY immediate (no debounce): scan-end is rare + intentional;
            // the user is waiting for their scanned files to appear.
            if (timer) clearTimeout(timer);
            refetch();
            break;
          case "IndexInvalidated":
            // TODO Batch H: split per event.data (TagsChanged / FilesChanged
            // / MetadataChanged / SearchIndexRebuilt) for surgical TanStack
            // invalidation. Currently coarse → debounced refetch matches
            // the File-event behavior.
            if (timer) clearTimeout(timer);
            timer = setTimeout(refetch, 300);
            break;
          default: {
            // WHY exhaustiveness check: ensures the switch stays complete
            // as new AppEvent variants are added (matches StatusBar.tsx
            // pattern from Batch D).
            const _exhaustive: never = event;
            throw new Error(`Unhandled AppEvent kind: ${JSON.stringify(_exhaustive)}`);
          }
        }
      })
      .then((fn) => {
        if (active) {
          unsubscribe = fn;
        } else {
          // Already unmounted before the listener was registered; tear down.
          fn();
        }
      })
      .catch((err: unknown) => {
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

  function handleScanComplete(result: ScanReport, path: string) {
    setScanResult(result);
    setScanning(false);
    // Refresh file list and tags after a successful scan.
    void api.listFilesWithTags(100).match(
      (refreshed) => { setFiles(refreshed); },
      (err) => { setError(err); },
    );
    void api.listTags().match(
      (refreshed) => { setTags(refreshed); },
      () => {
        // WHY: tag fetch failure is non-fatal after scan.
      },
    );
    // WHY: Auto-start the watcher on the folder we just scanned so live
    // updates flow without an extra user gesture. Non-blocking: failures
    // are logged but must not prevent the scan from being reported as
    // complete.
    void api.startWatch(path).match(
      () => { setWatcherError(null); },
      (err) => {
        setWatcherError(`Failed to start watcher [${err.kind}]: ${coreErrorMessage(err)}`);
      },
    );
  }

  /**
   * Receives debounced (query, hits) from SearchBar. Lifts into App state
   * so the visible-file composition can re-run.
   *
   * WHY Set + Map instead of the raw SearchHit[]: composeVisible does an
   * O(1) membership check per file; sortByRank does an O(1) rank lookup.
   * Storing the raw array would mean O(n*m) filtering per render.
   *
   * WHY useCallback with empty deps: SearchBar's useEffect lists
   * onQueryChange in its dependency array. Without memoisation, every
   * App re-render (triggered by setSearchHits / setHitRanks below) would
   * produce a new handler identity, re-run the effect, re-arm the 300 ms
   * timer, and re-fire api.search — an infinite feedback loop. React
   * guarantees that state-setter identities (setSearchHits etc.) are
   * stable across renders, so the empty deps array is correct.
   */
  const handleSearchChange = useCallback(
    (query: string, hits: SearchHit[] | null) => {
      setSearchQuery(query);
      if (hits === null) {
        setSearchHits(null);
        setHitRanks(new Map());
      } else {
        setSearchHits(new Set(hits.map((h) => h.blake3_hash)));
        setHitRanks(new Map(hits.map((h) => [h.blake3_hash, h.rank])));
      }
    },
    [],
  );

  const searchActive = searchHits !== null;
  const baseVisible = composeVisible(files, selectedTagId, searchHits);
  const visibleFiles = searchActive ? sortByRank(baseVisible, hitRanks) : baseVisible;
  const facetCounts = computeFacets(visibleFiles);
  const sidebarMode: "all" | "facets" = searchActive ? "facets" : "all";
  const sidebarTotalCount = searchActive ? visibleFiles.length : files.length;

  return (
    <div className="bg-gray-900 text-gray-100 min-h-screen flex flex-col">
      <header className="flex items-center justify-between px-6 py-4 bg-gray-800 border-b border-gray-700">
        <h1 className="text-xl font-bold tracking-wide">perima</h1>
        <div className="flex items-center gap-3">
          <SearchBar onQueryChange={handleSearchChange} />
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
        onDismiss={() => { setWatcherError(null); }}
      />

      <div className="flex-1 flex overflow-hidden">
        {tags.length > 0 && (
          <TagSidebar
            tags={tags}
            counts={facetCounts}
            totalCount={sidebarTotalCount}
            selectedTagId={selectedTagId}
            onSelect={(id) => { setSelectedTagId(id); }}
            mode={sidebarMode}
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
        onClick={() => { onChange("table"); }}
      >
        Table
      </button>
      <button
        type="button"
        className={`${base} ${mode === "grid" ? active : inactive}`}
        aria-pressed={mode === "grid"}
        onClick={() => { onChange("grid"); }}
      >
        Grid
      </button>
    </div>
  );
}
