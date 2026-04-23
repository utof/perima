import { convertFileSrc } from "@tauri-apps/api/core";
import type { FileWithTagsPayload } from "../bindings";
import TagChip from "./TagChip";

/** Props for {@link FileGrid}. */
interface FileGridProps {
  /** Rows to render as grid tiles. */
  files: FileWithTagsPayload[];
  /** When true, show a loading indicator instead of tiles. */
  loading?: boolean;
}

/**
 * Grid view of indexed files with thumbnails.
 *
 * Renders 200px auto-fill tiles. Each tile shows the WebP thumbnail via
 * Tauri's `convertFileSrc` asset protocol when the thumbnail status is
 * `"ready"`, otherwise a placeholder glyph keyed off the lifecycle status.
 *
 * WHY naive CSS grid (no virtualization): file counts stay under 10k in
 * v0.4.1 test corpora. Virtualization (react-window) lands in v0.5 once
 * real libraries stress the naive render.
 */
export default function FileGrid({ files, loading = false }: FileGridProps) {
  if (loading) {
    return (
      <div className="p-6 text-center text-gray-400" role="status">
        Loading...
      </div>
    );
  }
  if (files.length === 0) {
    return (
      <div className="p-6 text-center text-gray-500">No files indexed yet</div>
    );
  }
  return (
    <div
      className="grid grid-cols-[repeat(auto-fill,minmax(200px,1fr))] gap-2 p-2"
      role="list"
    >
      {files.map((f) => (
        <FileGridTile
          key={`${f.hash}-${f.volume_id}-${f.relative_path}`}
          file={f}
        />
      ))}
    </div>
  );
}

/** Single grid tile rendering either a thumbnail or a placeholder. */
function FileGridTile({ file }: { file: FileWithTagsPayload }) {
  const ready =
    file.thumbnail_status === "ready" && file.thumbnail_path !== null;
  const filename = file.relative_path.split("/").pop() ?? file.relative_path;
  return (
    <div
      className="aspect-square bg-gray-800 rounded overflow-hidden flex flex-col"
      role="listitem"
    >
      <div className="flex-1 flex items-center justify-center min-h-0">
        {ready ? (
          // WHY convertFileSrc: Tauri's asset protocol maps absolute
          // disk paths to a custom URL scheme the WebView can load
          // without violating CSP. Raw `file://` URLs are blocked.
          <img
            src={convertFileSrc(file.thumbnail_path as string)}
            alt={filename}
            className="w-full h-full object-contain"
          />
        ) : (
          <PlaceholderIcon status={file.thumbnail_status} />
        )}
      </div>
      <div className="p-1 text-xs truncate text-gray-200" title={filename}>
        {filename}
      </div>
      {file.tags.length > 0 && (
        <div className="px-1 pb-1 flex flex-wrap gap-0.5">
          {file.tags.slice(0, 3).map((t) => (
            <TagChip key={t.id} tag={t} />
          ))}
          {file.tags.length > 3 && (
            <span className="text-xs text-gray-400 ml-1">
              +{file.tags.length - 3}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * Placeholder glyph for tiles without a ready thumbnail.
 *
 * `"pending"` → ellipsis (generation enqueued); `"failed"` → warning
 * triangle; anything else (including `null`) → question mark for
 * "unknown / not applicable" media types.
 */
function PlaceholderIcon({ status }: { status: string | null | undefined }) {
  const glyph =
    status === "failed" ? "\u26A0" : status === "pending" ? "\u2026" : "?";
  const label =
    status === "failed"
      ? "thumbnail failed"
      : status === "pending"
        ? "thumbnail pending"
        : "no thumbnail";
  return (
    <div
      className="text-gray-400 text-2xl"
      role="img"
      aria-label={label}
      data-testid={`placeholder-${status ?? "unknown"}`}
    >
      {glyph}
    </div>
  );
}
