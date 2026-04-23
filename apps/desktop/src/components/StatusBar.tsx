import type { CoreError, ScanReport } from "../bindings";

/**
 * Props for {@link StatusBar}.
 *
 * WHY all-optional with `null` defaults (Batch H Task 7 interim): App.tsx
 * (post-Task-7 root-route shell) calls `<StatusBar />` prop-less while
 * waiting for Task 9's store-driven rewrite. Keeping props optional
 * silences the call-site TS error without disturbing existing tests,
 * which still pass them explicitly.
 */
interface StatusBarProps {
  /** Most recent scan report, or null if no scan has run. */
  scanResult?: ScanReport | null;
  /**
   * Current error, or null if none.
   * WHY CoreError not string: api.ts surfaces typed errors from the backend
   * discriminated union; the switch(error.kind) below renders distinct UX
   * per variant (Task 11).
   */
  error?: CoreError | null;
}

/**
 * Renders a human-readable label for a {@link CoreError} discriminated union.
 *
 * WHY distinct NotFound branch: "No results found." is user-facing vocabulary
 * for a search miss — not a system error. All other variants share the generic
 * "Something went wrong" phrasing that conveys unexpected failure. The
 * TypeScript `never` default forces a compile error if a new CoreError variant
 * is added without updating this switch.
 */
function renderError(error: CoreError): React.ReactNode {
  switch (error.kind) {
    case "NotFound":
      return "No results found.";
    case "Internal":
    case "Io":
    case "Duplicate":
    case "InvalidPath":
    case "InvalidHash":
    case "InvalidTag":
    case "Unsupported": {
      // WHY: Io carries { kind, message }; all others carry a plain string.
      const detail =
        error.data instanceof Object
          ? (error.data as { message: string }).message
          : error.data;
      return `Something went wrong: ${detail}`;
    }
    default: {
      // WHY exhaustive default with `never`: TypeScript verifies every
      // CoreError variant is handled at compile time — adding a new variant
      // without updating this switch is a compile error.
      const _exhaustive: never = error;
      return `Unknown error (${(_exhaustive as CoreError).kind})`;
    }
  }
}

/**
 * Thin status strip at the bottom of the layout.
 *
 * Shows the last scan summary, an error message, or a "No scans yet" hint.
 */
export default function StatusBar({ scanResult = null, error = null }: StatusBarProps) {
  if (error) {
    return (
      <div className="px-4 py-2 bg-gray-800 border-t border-gray-700 text-sm text-red-400">
        {renderError(error)}
      </div>
    );
  }

  if (scanResult) {
    return (
      <div className="px-4 py-2 bg-gray-800 border-t border-gray-700 text-sm text-gray-300">
        {`scanned ${scanResult.files_seen} files (${scanResult.files_new} new, ${scanResult.files_updated} updated, ${scanResult.files_errored} errors)`}
      </div>
    );
  }

  return (
    <div className="px-4 py-2 bg-gray-800 border-t border-gray-700 text-sm text-gray-500">
      No scans yet
    </div>
  );
}
