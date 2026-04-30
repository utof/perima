import { useState } from "react";
import type { FileWithTagsPayload, Tag } from "../bindings";
import TagChip from "./TagChip";
import {
  useAttachTag,
  useAttachTagByUuid,
  useDetachTag,
  useDetachTagByUuid,
} from "../queries/tags";
import { useUiStore } from "../stores/ui";

/**
 * Per-row tag input + chip strip with detach buttons.
 *
 * WHY a sub-component (not inline in FileTable's row map): each row owns
 * its own draft text state. Hoisting it into FileTable's render would
 * collapse all rows into one shared input — every keystroke would re-render
 * every row. A small per-row component keeps state local.
 *
 * WHY (Task 11): `hash` may be `null` for pending files (no `full_hash`
 * computed yet). Pending files use `file_uuid` to attach/detach via the
 * `*_by_uuid` IPC endpoints. The aria-label falls back to `file_uuid` when
 * `hash` is null so screen readers still get a stable identifier.
 */
function RowTagsCell({
  fileUuid,
  hash,
  tags,
}: {
  fileUuid: string;
  hash: string | null;
  tags: Tag[];
}) {
  const [draft, setDraft] = useState("");
  const attach = useAttachTag();
  const detach = useDetachTag();
  const attachByUuid = useAttachTagByUuid();
  const detachByUuid = useDetachTagByUuid();

  function submit() {
    const name = draft.trim();
    if (name === "") return;
    if (hash !== null) {
      attach.mutate({ hash, tagName: name });
    } else {
      attachByUuid.mutate({ fileUuid, tagName: name });
    }
    setDraft("");
  }

  function onRemove(tagId: string) {
    if (hash !== null) {
      detach.mutate({ hash, tagId });
    } else {
      detachByUuid.mutate({ fileUuid, tagId });
    }
  }

  const labelKey = hash ?? fileUuid;
  const isPending = attach.isPending || attachByUuid.isPending;

  return (
    <div className="flex flex-wrap items-center gap-1">
      {tags.map((t) => (
        <TagChip
          key={t.id}
          tag={t}
          onRemove={() => { onRemove(t.id); }}
        />
      ))}
      <input
        type="text"
        value={draft}
        onChange={(e) => { setDraft(e.target.value); }}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            submit();
          }
        }}
        placeholder="+ tag"
        aria-label={`Add tag to file ${labelKey.slice(0, 8)}`}
        disabled={isPending}
        className="w-20 bg-gray-700 text-white text-xs rounded px-1.5 py-0.5 placeholder:text-gray-400 focus:outline-none focus:ring-1 focus:ring-blue-500"
      />
    </div>
  );
}

/** Props for {@link FileTable}. */
interface FileTableProps {
  /** Rows to display. */
  files: FileWithTagsPayload[];
  /** When true, show a loading indicator instead of the table body. */
  loading: boolean;
}

type SortColumn = "hash" | "size" | "volume_id" | "relative_path" | "status";
type SortDir = "asc" | "desc";

/** Human-readable byte size (e.g. "1.5 MB"). */
function humanSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/**
 * Sortable table that renders a list of {@link FileWithTags} rows.
 *
 * Columns: HASH (8-char prefix), SIZE, VOLUME (8-char UUID prefix), PATH,
 * STATUS, TAGS (up to 3 chips + overflow badge).
 * Clicking a column header toggles ascending/descending sort on that column.
 * Clicking a row selects it (sets `selectedFileUuid` in the UI store);
 * clicking the same row again deselects (sets to null).
 */
export default function FileTable({ files, loading }: FileTableProps) {
  const [sortBy, setSortBy] = useState<SortColumn>("relative_path");
  const [sortDir, setSortDir] = useState<SortDir>("asc");
  const selectedFileUuid = useUiStore((s) => s.selectedFileUuid);
  const setSelectedFileUuid = useUiStore((s) => s.setSelectedFileUuid);

  function handleSort(col: SortColumn) {
    if (sortBy === col) {
      setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortBy(col);
      setSortDir("asc");
    }
  }

  // WHY (Task 11): `hash` is `string | null` post-spec-§4.8 sweep. Sorting on
  // hash uses an empty string for null so pending rows cluster predictably
  // (alphabetic-ascending puts them first, matching their "not-yet-computed"
  // status semantically). Other string columns are unchanged.
  const sorted = [...files].sort((a, b) => {
    let cmp = 0;
    if (sortBy === "size") {
      cmp = a.size - b.size;
    } else if (sortBy === "hash") {
      cmp = (a.hash ?? "").localeCompare(b.hash ?? "");
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
            <th className={thCls} onClick={() => { handleSort("hash"); }}>
              HASH{arrow("hash")}
            </th>
            <th className={thCls} onClick={() => { handleSort("size"); }}>
              SIZE{arrow("size")}
            </th>
            <th className={thCls} onClick={() => { handleSort("volume_id"); }}>
              VOLUME{arrow("volume_id")}
            </th>
            <th className={thCls} onClick={() => { handleSort("relative_path"); }}>
              PATH{arrow("relative_path")}
            </th>
            <th className={thCls} onClick={() => { handleSort("status"); }}>
              STATUS{arrow("status")}
            </th>
            <th className={thCls}>TAGS</th>
          </tr>
        </thead>
        <tbody>
          {loading ? (
            <tr>
              <td colSpan={6} className="px-3 py-6 text-center text-gray-400">
                Loading...
              </td>
            </tr>
          ) : sorted.length === 0 ? (
            <tr>
              <td colSpan={6} className="px-3 py-6 text-center text-gray-500">
                No files indexed yet
              </td>
            </tr>
          ) : (
            sorted.map((f) => (
              // WHY key={f.file_uuid} (Task 11, spec §4.8): `file_uuid` is the
              // stable surrogate present on every row from V011 on. `hash` is
              // nullable for pending files, so a `f.hash`-derived key would
              // collide across pending rows and force React to re-mount when
              // `full_hash` materialises later.
              // WHY onClick toggles: clicking an already-selected row deselects
              // (returns sidebar to hidden state). Matching the row to the
              // selected UUID uses string equality; no deep comparison needed.
              <tr
                key={f.file_uuid}
                onClick={() => {
                  setSelectedFileUuid(
                    selectedFileUuid === f.file_uuid ? null : f.file_uuid,
                  );
                }}
                className={`border-t border-gray-700 cursor-pointer ${
                  selectedFileUuid === f.file_uuid
                    ? "bg-blue-900"
                    : "odd:bg-gray-900 even:bg-gray-800 hover:bg-gray-700"
                }`}
                aria-selected={selectedFileUuid === f.file_uuid}
              >
                <td className="px-3 py-2 font-mono text-xs">
                  {/* WHY isPlaceholder check: pre-V012 blake3_hash is NOT NULL —
                    * quick_hash is stored there until compute_full_hash promotes
                    * the real hash. f.hash === null never fires today; equality
                    * with quick_hash is the reliable placeholder signal. */}
                  {f.hash === null || (f.quick_hash !== null && f.hash === f.quick_hash)
                    ? "pending"
                    : f.hash.slice(0, 8)}
                </td>
                <td className="px-3 py-2">{humanSize(f.size)}</td>
                <td className="px-3 py-2 font-mono text-xs">
                  {f.volume_id.slice(0, 8)}
                </td>
                <td className="px-3 py-2 font-mono text-xs max-w-xs truncate">
                  {f.relative_path}
                </td>
                <td className="px-3 py-2">{f.status}</td>
                <td className="px-3 py-2">
                  <RowTagsCell
                    fileUuid={f.file_uuid}
                    hash={f.hash}
                    tags={f.tags}
                  />
                </td>
              </tr>
            ))
          )}
        </tbody>
      </table>
    </div>
  );
}
