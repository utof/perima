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
      <div className="p-6 text-center text-muted-foreground" role="status">
        Loading...
      </div>
    );
  }
  if (files.length === 0) {
    return (
      <div className="p-6 text-center text-muted-foreground">No files indexed yet</div>
    );
  }
  return (
    <div
      className="grid grid-cols-[repeat(auto-fill,minmax(200px,1fr))] gap-4 p-6"
      role="list"
    >
      {files.map((f) => (
        // WHY key={f.file_uuid} (Task 11): stable across the
        // pending → full-hash transition; matches FileTable.tsx's choice.
        <FileGridTile key={f.file_uuid} file={f} />
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
      className="bg-card rounded-md shadow-e1 hover:shadow-e2 hover:bg-popover transition-colors duration-micro ease-perima cursor-pointer overflow-hidden flex flex-col"
      role="listitem"
    >
      <div className="aspect-square bg-muted flex items-center justify-center">
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
      <div className="px-3 py-2 text-sm text-foreground truncate" title={filename}>
        {filename}
      </div>
      {file.tags.length > 0 && (
        <div className="px-3 pb-2 flex flex-wrap gap-1">
          {file.tags.slice(0, 3).map((t) => (
            <TagChip key={t.id} tag={t} />
          ))}
          {file.tags.length > 3 && (
            <span className="text-xs text-muted-foreground ml-1">
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
      className="text-muted-foreground text-2xl"
      role="img"
      aria-label={label}
      data-testid={`placeholder-${status ?? "unknown"}`}
    >
      {glyph}
    </div>
  );
}
