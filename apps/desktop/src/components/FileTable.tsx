import { useState } from "react";
import type { FileEntry } from "../types";

/** Props for {@link FileTable}. */
interface FileTableProps {
  /** Rows to display. */
  files: FileEntry[];
  /** When true, show a loading indicator instead of the table body. */
  loading: boolean;
}

type SortColumn = "hash" | "size" | "volume_id" | "relative_path" | "status";
type SortDir = "asc" | "desc";

/** Human-readable byte size (e.g. "1.5 MB"). */
function humanSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/**
 * Sortable table that renders a list of {@link FileEntry} rows.
 *
 * Columns: HASH (8-char prefix), SIZE, VOLUME (8-char UUID prefix), PATH, STATUS.
 * Clicking a column header toggles ascending/descending sort on that column.
 */
export default function FileTable({ files, loading }: FileTableProps) {
  const [sortBy, setSortBy] = useState<SortColumn>("relative_path");
  const [sortDir, setSortDir] = useState<SortDir>("asc");

  function handleSort(col: SortColumn) {
    if (sortBy === col) {
      setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortBy(col);
      setSortDir("asc");
    }
  }

  const sorted = [...files].sort((a, b) => {
    let cmp = 0;
    if (sortBy === "size") {
      cmp = a.size - b.size;
    } else {
      cmp = a[sortBy].localeCompare(b[sortBy]);
    }
    return sortDir === "asc" ? cmp : -cmp;
  });

  const thCls =
    "px-3 py-2 text-left text-xs font-semibold uppercase tracking-wider text-gray-400 cursor-pointer select-none hover:text-gray-100";

  function arrow(col: SortColumn) {
    if (sortBy !== col) return null;
    return sortDir === "asc" ? " ▲" : " ▼";
  }

  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm text-gray-200">
        <thead className="bg-gray-800">
          <tr>
            <th className={thCls} onClick={() => handleSort("hash")}>
              HASH{arrow("hash")}
            </th>
            <th className={thCls} onClick={() => handleSort("size")}>
              SIZE{arrow("size")}
            </th>
            <th className={thCls} onClick={() => handleSort("volume_id")}>
              VOLUME{arrow("volume_id")}
            </th>
            <th className={thCls} onClick={() => handleSort("relative_path")}>
              PATH{arrow("relative_path")}
            </th>
            <th className={thCls} onClick={() => handleSort("status")}>
              STATUS{arrow("status")}
            </th>
          </tr>
        </thead>
        <tbody>
          {loading ? (
            <tr>
              <td colSpan={5} className="px-3 py-6 text-center text-gray-400">
                Loading...
              </td>
            </tr>
          ) : sorted.length === 0 ? (
            <tr>
              <td colSpan={5} className="px-3 py-6 text-center text-gray-500">
                No files indexed yet
              </td>
            </tr>
          ) : (
            sorted.map((f) => (
              <tr
                key={`${f.hash}-${f.volume_id}-${f.relative_path}`}
                className="border-t border-gray-700 odd:bg-gray-900 even:bg-gray-800 hover:bg-gray-700"
              >
                <td className="px-3 py-2 font-mono text-xs">{f.hash.slice(0, 8)}</td>
                <td className="px-3 py-2">{humanSize(f.size)}</td>
                <td className="px-3 py-2 font-mono text-xs">{f.volume_id.slice(0, 8)}</td>
                <td className="px-3 py-2 font-mono text-xs max-w-xs truncate">{f.relative_path}</td>
                <td className="px-3 py-2">{f.status}</td>
              </tr>
            ))
          )}
        </tbody>
      </table>
    </div>
  );
}
