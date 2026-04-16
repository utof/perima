import { open as openDialog } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import type { ScanResult } from "../types";

/** Props for {@link ScanButton}. */
interface ScanButtonProps {
  /** Called when a scan completes successfully with the result summary. */
  onScanComplete: (result: ScanResult) => void;
  /** Called immediately before the scan starts (use to set loading state). */
  onScanStart: () => void;
  /** When true, show the disabled "Scanning..." state. */
  scanning: boolean;
}

/**
 * Button that opens a native OS folder picker and triggers a perima scan.
 *
 * WHY: The dialog is delegated to the Tauri plugin so the OS-native picker is
 * used rather than a web `<input type="file">`, which can't return arbitrary
 * directory paths in the Tauri WebView.
 */
export default function ScanButton({
  onScanComplete,
  onScanStart,
  scanning,
}: ScanButtonProps) {
  async function handleClick() {
    const selected = await openDialog({ directory: true, multiple: false });
    if (!selected || typeof selected !== "string") return;

    onScanStart();
    api.scan(selected, false).match(
      (result) => onScanComplete(result),
      (err) => window.alert(`Scan failed: ${err}`),
    );
  }

  return (
    <button
      onClick={handleClick}
      disabled={scanning}
      className="px-4 py-2 rounded bg-blue-600 text-white font-semibold hover:bg-blue-500 disabled:opacity-50 disabled:cursor-not-allowed"
    >
      {scanning ? "Scanning..." : "Scan Folder"}
    </button>
  );
}
