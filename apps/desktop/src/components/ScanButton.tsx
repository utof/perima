import { open as openDialog } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import { coreErrorMessage } from "../lib/coreError";
import type { ScanReport } from "../bindings";

/** Props for {@link ScanButton}. */
interface ScanButtonProps {
  /**
   * Called when a scan completes successfully with the result summary and
   * the absolute path that was scanned.
   *
   * WHY path is passed: the parent needs it to auto-start the filesystem
   * watcher on the folder that was just scanned (phase 3b).
   */
  onScanComplete: (result: ScanReport, path: string) => void;
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
    void api.scan(selected, false).match(
      (result) => { onScanComplete(result, selected); },
      // WHY coreErrorMessage: helper centralises the data-payload stringification
      // (plain string vs Io's { kind, message } struct) with cyclic-object safety.
      (err) => { window.alert(`Scan failed [${err.kind}]: ${coreErrorMessage(err)}`); },
    );
  }

  return (
    <button
      onClick={() => { void handleClick(); }}
      disabled={scanning}
      className="px-4 py-2 rounded bg-blue-600 text-white font-semibold hover:bg-blue-500 disabled:opacity-50 disabled:cursor-not-allowed"
    >
      {scanning ? "Scanning..." : "Scan Folder"}
    </button>
  );
}
