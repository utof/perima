import { useEffect, useState } from "react";
import * as api from "./api";
import FileTable from "./components/FileTable";
import ScanButton from "./components/ScanButton";
import StatusBar from "./components/StatusBar";
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

  function handleScanStart() {
    setScanning(true);
    setError(null);
  }

  function handleScanComplete(result: ScanResult) {
    setScanResult(result);
    setScanning(false);
    // Refresh file list after a successful scan.
    api.listFiles(100).match(
      (refreshed) => setFiles(refreshed),
      (err) => setError(err),
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

      <main className="flex-1 overflow-auto p-4">
        <FileTable files={files} loading={loading} />
      </main>

      <footer>
        <StatusBar scanResult={scanResult} error={error} />
      </footer>
    </div>
  );
}
