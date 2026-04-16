import type { Tag } from "../types";

/** Props for {@link TagSidebar}. */
interface TagSidebarProps {
  /** Tags to render. */
  tags: Tag[];
  /** Map from tag id to attachment count (for display only). */
  counts: Record<string, number>;
  /** Total unfiltered file count, shown next to "All". */
  totalCount: number;
  /** Selected tag id, or null for "All". */
  selectedTagId: string | null;
  /** Called when the user selects a tag or "All". */
  onSelect: (tagId: string | null) => void;
}

/**
 * Sidebar lists all tags plus an "All" selector. Clicking a row
 * calls `onSelect` with the tag id (or null for "All").
 */
export default function TagSidebar({
  tags,
  counts,
  totalCount,
  selectedTagId,
  onSelect,
}: TagSidebarProps) {
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
      {tags.map((t) => (
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
  const countCls = active ? "text-xs text-blue-200 ml-2" : "text-xs text-gray-400 ml-2";
  return (
    <button
      type="button"
      className={`${base} ${active ? activeCls : inactiveCls}`}
      onClick={onClick}
      aria-pressed={active}
    >
      <span className="truncate">{label}</span>
      {count !== undefined && (
        <span className={countCls}>{count}</span>
      )}
    </button>
  );
}
