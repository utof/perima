import { GridFourIcon, ListIcon } from "@phosphor-icons/react";
import { useUiStore } from "../stores/ui";

/**
 * Segmented toggle between table and grid views.
 * Reads + dispatches viewMode via the Zustand store.
 *
 * WHY segmented control (not a single button that flips): two explicit
 * options make the inactive choice discoverable at a glance and match
 * desktop convention (Finder/Files-style switchers).
 */
export default function ViewModeToggle() {
  const viewMode = useUiStore((s) => s.viewMode);
  const setViewMode = useUiStore((s) => s.setViewMode);
  return (
    <div
      className="inline-flex items-center rounded-full bg-secondary p-0.5"
      role="group"
      aria-label="View mode"
    >
      <button
        type="button"
        onClick={() => { setViewMode("grid"); }}
        aria-pressed={viewMode === "grid"}
        aria-label="Grid view"
        className={`inline-flex items-center justify-center rounded-full px-3 py-1
                    transition-colors duration-micro ease-perima
                    ${viewMode === "grid"
                      ? "bg-accent text-accent-foreground"
                      : "text-muted-foreground hover:text-foreground"}`}
      >
        <GridFourIcon size={16} weight="regular" />
      </button>
      <button
        type="button"
        onClick={() => { setViewMode("table"); }}
        aria-pressed={viewMode === "table"}
        aria-label="Table view"
        className={`inline-flex items-center justify-center rounded-full px-3 py-1
                    transition-colors duration-micro ease-perima
                    ${viewMode === "table"
                      ? "bg-accent text-accent-foreground"
                      : "text-muted-foreground hover:text-foreground"}`}
      >
        <ListIcon size={16} weight="regular" />
      </button>
    </div>
  );
}
