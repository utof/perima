import type { CoreError, ScanReport } from "../bindings";

/** Props for {@link StatusBar}. */
interface StatusBarProps {
  /** Most recent scan report, or null if no scan has run. */
  scanResult: ScanReport | null;
  /**
   * Current error, or null if none.
   * WHY CoreError not string: api.ts now surfaces typed errors from
   * the backend discriminated union. Task 11 will add per-variant UI.
   */
  error: CoreError | null;
}

/**
 * Thin status strip at the bottom of the layout.
 *
 * Shows the last scan summary, an error message, or a "No scans yet" hint.
 */
export default function StatusBar({ scanResult, error }: StatusBarProps) {
  if (error) {
    // WHY inline stringification: Task 11 adds a proper switch(error.kind) here;
    // for now surface the data payload so the error is still readable.
    const errMsg =
      typeof error.data === "string" ? error.data : JSON.stringify(error.data);
    return (
      <div className="px-4 py-2 bg-gray-800 border-t border-gray-700 text-sm text-red-400">
        {errMsg}
      </div>
    );
  }

  if (scanResult) {
    return (
      <div className="px-4 py-2 bg-gray-800 border-t border-gray-700 text-sm text-gray-300">
        {`scanned ${scanResult.files_seen} files (${scanResult.files_new} new, ${scanResult.files_updated} updated, ${scanResult.files_errored} errors)`}
      </div>
    );
  }

  return (
    <div className="px-4 py-2 bg-gray-800 border-t border-gray-700 text-sm text-gray-500">
      No scans yet
    </div>
  );
}
