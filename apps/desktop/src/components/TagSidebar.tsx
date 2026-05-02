import type { Tag } from "../bindings";
import { useUiStore } from "../stores/ui";

interface TagSidebarProps {
  /** Full tag list (all known tags). */
  tags: Tag[];
  /** Tag id → file count within the current visible set. */
  counts: Record<string, number>;
  /** Total visible file count (displayed on the "All" row). */
  totalCount: number;
  /**
   * Rendering mode:
   * - "all": show every tag in `tags` (no search active).
   * - "facets": show only tags with counts \> 0 (search active; the
   *   sidebar becomes a facet panel over the current results).
   */
  mode: "all" | "facets";
  // selectedTagId + onSelect REMOVED — store-driven now.
}

/**
 * Left-column filter: "All" + per-tag rows with attachment counts and
 * aria-pressed toggle state. Single-select for v0.5.x; multi-select
 * tracked as post-v1 per issue #32.
 *
 * WHY store-driven selectedTagId (not props): Batch H Task 9 — removing
 * the prop threading from IndexRoute keeps IndexRoute clean and TagSidebar
 * self-contained.
 */
export default function TagSidebar({
  tags,
  counts,
  totalCount,
  mode,
}: TagSidebarProps) {
  const selectedTagId = useUiStore((s) => s.selectedTagId);
  const setSelectedTagId = useUiStore((s) => s.setSelectedTagId);

  const visibleTags =
    mode === "facets"
      ? tags.filter((t) => (counts[t.id] ?? 0) > 0)
      : tags;

  return (
    <nav
      className="w-64 bg-card border-r border-border p-4 flex flex-col gap-1 overflow-y-auto"
      aria-label="Tag filter"
    >
      <SidebarRow
        label="All"
        count={totalCount}
        active={selectedTagId === null}
        onClick={() => { setSelectedTagId(null); }}
      />
      {mode === "facets" && visibleTags.length === 0 && (
        <p className="px-3 py-1.5 caption text-muted-foreground italic">
          No tags in current results
        </p>
      )}
      {visibleTags.map((t) => (
        <SidebarRow
          key={t.id}
          label={t.name}
          count={counts[t.id] ?? 0}
          active={selectedTagId === t.id}
          onClick={() => { setSelectedTagId(t.id); }}
        />
      ))}
    </nav>
  );
}

function SidebarRow({
  label,
  count,
  active,
  onClick,
}: {
  label: string;
  count?: number;
  active: boolean;
  onClick: () => void;
}) {
  const base =
    "flex items-center justify-between w-full rounded-md px-3 py-1.5 text-sm cursor-pointer transition-colors duration-micro ease-perima focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
  const activeCls = "bg-accent text-accent-foreground";
  const inactiveCls = "text-foreground hover:bg-muted";
  const countCls = active ? "text-accent-foreground" : "text-muted-foreground";
  return (
    <button
      type="button"
      className={`${base} ${active ? activeCls : inactiveCls}`}
      onClick={onClick}
      aria-pressed={active}
    >
      <span className="truncate">{label}</span>
      {count !== undefined && (
        <span className={`text-xs ml-2 tabular-nums ${countCls}`}>{count}</span>
      )}
    </button>
  );
}
