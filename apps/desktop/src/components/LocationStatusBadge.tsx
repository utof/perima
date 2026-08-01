/**
 * Visual treatment for `file_locations.status`.
 *
 * WHY this exists as more than a string render: the catalogue tracks
 * whether a file is still on disk, and the table used to print that
 * verdict as plain lowercase text indistinguishable from every other
 * column. A user scanning the list had no way to tell that a row's file
 * was gone — the information was present and invisible.
 *
 * `active` deliberately renders muted rather than green: the common case
 * is every row being active, and colouring all of them would drown the
 * one row that is not. Only the abnormal states draw the eye.
 */

/** Status values written by the backend (`crates/db` `status_to_str`). */
export type LocationStatus = "active" | "missing" | "moved" | "stale" | (string & {});

interface Style {
  readonly label: string;
  readonly className: string;
  /** Screen-reader text; the colour alone must never carry the meaning. */
  readonly srText: string;
}

const STYLES: Record<string, Style> = {
  active: {
    label: "active",
    className: "text-muted-foreground",
    srText: "file is present on disk",
  },
  missing: {
    label: "missing",
    className:
      "font-semibold text-destructive-foreground bg-destructive/15 " +
      "ring-1 ring-destructive/40 rounded px-1.5 py-0.5",
    srText: "file is missing from disk",
  },
  moved: {
    label: "moved",
    className:
      "font-medium text-amber-700 dark:text-amber-400 bg-amber-500/10 " +
      "ring-1 ring-amber-500/30 rounded px-1.5 py-0.5",
    srText: "file moved elsewhere on the same volume",
  },
  stale: {
    label: "stale",
    className:
      "font-medium text-amber-700 dark:text-amber-400 bg-amber-500/10 " +
      "ring-1 ring-amber-500/30 rounded px-1.5 py-0.5",
    srText: "file changed on disk since it was indexed",
  },
};

/** True when the status means the file is not usable right now. */
export function isUnavailable(status: string): boolean {
  return status === "missing";
}

/**
 * Render a location status as a badge.
 *
 * Unknown values fall through to a neutral rendering rather than
 * throwing — the column is a plain text field in SQLite and a future
 * backend variant must not blank the table.
 */
export function LocationStatusBadge({ status }: { status: string }) {
  const style = STYLES[status] ?? {
    label: status,
    className: "text-muted-foreground",
    srText: status,
  };
  return (
    <span className={`inline-flex items-center text-xs ${style.className}`}>
      {style.label}
      <span className="sr-only"> — {style.srText}</span>
    </span>
  );
}
