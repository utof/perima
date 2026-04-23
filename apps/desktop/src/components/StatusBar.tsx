/**
 * Status footer. Reads scan slice from useUiStore.
 *
 * WHY useShallow for scan slice: it returns an object with status + lastReport;
 * inline destructuring without useShallow would crash with
 * "Maximum update depth exceeded" in Zustand v5.
 */
import { useShallow } from "zustand/shallow";
import { useUiStore } from "../stores/ui";

export default function StatusBar() {
  const { status, lastReport } = useUiStore(
    useShallow((s) => ({ status: s.scan.status, lastReport: s.scan.lastReport })),
  );

  let summary: string;
  if (status === "scanning") {
    summary = "Scanning…";
  } else if (lastReport !== null) {
    summary = `Last scan: ${lastReport.files_seen} files`;
  } else {
    summary = "Ready";
  }

  return (
    <div className="px-6 py-2 bg-gray-800 text-xs text-gray-400 border-t border-gray-700">
      {summary}
    </div>
  );
}
