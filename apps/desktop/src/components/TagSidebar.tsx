import type { Tag } from "../types";

interface TagSidebarProps {
  /** Full tag list (all known tags). */
  tags: Tag[];
  /** Tag id → file count within the current visible set. */
  counts: Record<string, number>;
  /** Total visible file count (displayed on the "All" row). */
  totalCount: number;
  selectedTagId: string | null;
  onSelect: (tagId: string | null) => void;
  /**
   * Rendering mode (optional; defaults to "all" so existing callers
   * that don't pass this prop continue to work):
   * - "all": show every tag in `tags` (no search active).
   * - "facets": show only tags with counts \> 0 (search active; the
   *   sidebar becomes a facet panel over the current results).
   */
  mode?: "all" | "facets";
}

/**
 * Left-column filter: "All" + per-tag rows with attachment counts and
 * aria-pressed toggle state. Single-select for v0.5.x; multi-select
 * tracked as post-v1 per issue #32.
 */
export default function TagSidebar({
  tags,
  counts,
  totalCount,
  selectedTagId,
  onSelect,
  mode = "all",
}: TagSidebarProps) {
  const visibleTags =
    mode === "facets"
      ? tags.filter((t) => (counts[t.id] ?? 0) > 0)
      : tags;

  return (
    <nav
      className="w-48 bg-gray-800 border-r border-gray-700 p-2 flex flex-col gap-1 overflow-y-auto"
      aria-label="Tag filter"
    >
      <SidebarRow
        label="All"
        count={totalCount}
        active={selectedTagId === null}
        onClick={() => onSelect(null)}
      />
      {mode === "facets" && visibleTags.length === 0 && (
        <p className="px-2 py-1.5 text-xs text-gray-500 italic">
          No tags in current results
        </p>
      )}
      {visibleTags.map((t) => (
        <SidebarRow
          key={t.id}
          label={t.name}
          count={counts[t.id] ?? 0}
          active={selectedTagId === t.id}
          onClick={() => onSelect(t.id)}
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
    "flex items-center justify-between px-2 py-1.5 text-sm rounded cursor-pointer transition-colors";
  const activeCls = "bg-blue-600 text-white";
  const inactiveCls = "text-gray-300 hover:bg-gray-700";
  const countCls = active ? "text-blue-200" : "text-gray-400";
  return (
    <button
      type="button"
      className={`${base} ${active ? activeCls : inactiveCls}`}
      onClick={onClick}
      aria-pressed={active}
    >
      <span className="truncate">{label}</span>
      {count !== undefined && (
        <span className={`text-xs ml-2 ${countCls}`}>{count}</span>
      )}
    </button>
  );
}
