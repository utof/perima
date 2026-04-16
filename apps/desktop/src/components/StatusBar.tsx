import type { ScanResult } from "../types";

/** Props for {@link StatusBar}. */
interface StatusBarProps {
  /** Most recent scan result, or null if no scan has run. */
  scanResult: ScanResult | null;
  /** Current error message, or null if none. */
  error: string | null;
}

/**
 * Thin status strip at the bottom of the layout.
 *
 * Shows the last scan summary, an error message, or a "No scans yet" hint.
 */
export default function StatusBar({ scanResult, error }: StatusBarProps) {
  if (error) {
    return (
      <div className="px-4 py-2 bg-gray-800 border-t border-gray-700 text-sm text-red-400">
        {error}
      </div>
    );
  }

  if (scanResult) {
    return (
      <div className="px-4 py-2 bg-gray-800 border-t border-gray-700 text-sm text-gray-300">
        {`scanned ${scanResult.total} files (${scanResult.new} new, ${scanResult.existing} existing, ${scanResult.errors} errors)`}
      </div>
    );
  }

  return (
    <div className="px-4 py-2 bg-gray-800 border-t border-gray-700 text-sm text-gray-500">
      No scans yet
    </div>
  );
}
