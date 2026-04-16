import { useEffect, useState } from "react";
import * as api from "./api";
import type { UnsubscribeFn } from "./api";
import FileTable from "./components/FileTable";
import ScanButton from "./components/ScanButton";
import StatusBar from "./components/StatusBar";
import WatcherBanner from "./components/WatcherBanner";
import type { FileEntry, ScanResult } from "./types";

/**
 * Root application shell.
 *
 * Manages global state and composes the three main UI components.
 * WHY: Single top-level state owner keeps data flow simple for the current
 * feature set; introduce a state library (zustand / jotai) when the number of
 * consumers grows beyond 2–3 components.
 */
export default function App() {
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [scanResult, setScanResult] = useState<ScanResult | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // WHY: Watcher failures are non-blocking (the table is still accurate,
  // just not live-updating). Surface them via a dismissible banner rather
  // than the scan `error` state so they don't mask the StatusBar output.
  const [watcherError, setWatcherError] = useState<string | null>(null);

  useEffect(() => {
    // WHY: Populate the table on mount so existing indexed files are visible
    // immediately without requiring the user to trigger a scan.
    api.listFiles(100).match(
      (result) => {
        setFiles(result);
        setLoading(false);
      },
      (err) => {
        setError(err);
        setLoading(false);
      },
    );
  }, []);

  useEffect(() => {
    // WHY: Coalesce filesystem event bursts (e.g., saving a file triggers
    // several low-level events) into a single `list_files` refresh. 300 ms
    // was chosen in the spec — short enough to feel live, long enough to
    // absorb typical editor save storms.
    let timer: ReturnType<typeof setTimeout> | null = null;
    let unsubscribe: UnsubscribeFn | null = null;
    // WHY: Guard against setState after unmount when the subscribe promise
    // or the debounced refresh resolves post-cleanup.
    let active = true;

    api
      .subscribeToFileEvents(() => {
        if (timer) clearTimeout(timer);
        timer = setTimeout(() => {
          api.listFiles(100).match(
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
    // Refresh file list after a successful scan.
    api.listFiles(100).match(
      (refreshed) => setFiles(refreshed),
      (err) => setError(err),
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

  return (
    <div className="bg-gray-900 text-gray-100 min-h-screen flex flex-col">
      <header className="flex items-center justify-between px-6 py-4 bg-gray-800 border-b border-gray-700">
        <h1 className="text-xl font-bold tracking-wide">perima</h1>
        <ScanButton
          onScanComplete={handleScanComplete}
          onScanStart={handleScanStart}
          scanning={scanning}
        />
      </header>

      <WatcherBanner
        message={watcherError}
        onDismiss={() => setWatcherError(null)}
      />

      <main className="flex-1 overflow-auto p-4">
        <FileTable files={files} loading={loading} />
      </main>

      <footer>
        <StatusBar scanResult={scanResult} error={error} />
      </footer>
    </div>
  );
}
