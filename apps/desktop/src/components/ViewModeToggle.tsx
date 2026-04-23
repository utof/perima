import { useUiStore } from "../stores/ui";

/**
 * Segmented toggle between table and grid views.
 * Reads + dispatches viewMode via the Zustand store.
 *
 * WHY segmented control (not a single button that flips): two explicit
 * labels make the inactive option discoverable at a glance and match
 * desktop convention (Finder/Files-style switchers).
 */
export default function ViewModeToggle() {
  const viewMode = useUiStore((s) => s.viewMode);
  const setViewMode = useUiStore((s) => s.setViewMode);
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
        className={`${base} ${viewMode === "table" ? active : inactive}`}
        aria-pressed={viewMode === "table"}
        onClick={() => { setViewMode("table"); }}
      >
        Table
      </button>
      <button
        type="button"
        className={`${base} ${viewMode === "grid" ? active : inactive}`}
        aria-pressed={viewMode === "grid"}
        onClick={() => { setViewMode("grid"); }}
      >
        Grid
      </button>
    </div>
  );
}
