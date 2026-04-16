/** Props for {@link WatcherBanner}. */
interface WatcherBannerProps {
  /** Error message, or null to hide the banner. */
  message: string | null;
  /** Called when the user dismisses the banner. */
  onDismiss: () => void;
}

/**
 * Non-blocking banner that surfaces watcher subscribe / startWatch
 * failures without obscuring the file table.
 *
 * WHY a separate banner rather than reusing the scan `error` state:
 * scan errors are blocking — the user needs to know the scan failed.
 * Watcher errors are degraded-mode — the table is still accurate, just
 * not live-refreshing. Different severity, different UI treatment.
 */
export default function WatcherBanner({ message, onDismiss }: WatcherBannerProps) {
  if (!message) return null;
  return (
    <div
      role="alert"
      className="flex items-center justify-between px-4 py-2 bg-yellow-900 text-yellow-100 border-b border-yellow-700"
    >
      <span className="text-sm">
        <strong>Watcher:</strong> {message}
      </span>
      <button
        onClick={onDismiss}
        className="px-2 py-1 text-xs rounded bg-yellow-800 hover:bg-yellow-700"
      >
        Dismiss
      </button>
    </div>
  );
}
