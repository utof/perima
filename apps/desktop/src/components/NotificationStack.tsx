/**
 * Toast/banner UX — replaces ad-hoc error + watcherError rendering
 * deferred from Batch D (per Batch D D-11 note).
 *
 * WHY in-store queue (not portal): Zustand store is single source of
 * truth; components dispatch `notify(kind, msg)` from anywhere
 * (mutations, hooks, async callbacks). `info` auto-dismiss after 5s;
 * `error` persists until user dismisses.
 */
import { useEffect } from "react";
import { useUiStore } from "../stores/ui";
import type { Notification } from "../stores/ui";

const INFO_AUTODISMISS_MS = 5000;

function NotificationItem({ notification }: { notification: Notification }) {
  const dismiss = useUiStore((s) => s.dismiss);
  const { id, kind, message } = notification;

  useEffect(() => {
    if (kind !== "info") return;
    const timer = setTimeout(() => { dismiss(id); }, INFO_AUTODISMISS_MS);
    return () => { clearTimeout(timer); };
  }, [id, kind, dismiss]);

  const variantClasses =
    kind === "error"
      ? "border-destructive bg-destructive/10 text-destructive-foreground"
      : "border-info bg-popover text-popover-foreground";
  return (
    <div
      role={kind === "error" ? "alert" : "status"}
      className={`pointer-events-auto rounded-md border ${variantClasses} shadow-e2 px-4 py-3 max-w-sm flex items-start gap-3`}
    >
      <span className="flex-1 text-sm">{message}</span>
      <button
        type="button"
        className="inline-flex items-center justify-center rounded-md p-1 text-muted-foreground hover:text-foreground hover:bg-muted transition-colors duration-micro ease-perima focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        aria-label="Dismiss notification"
        onClick={() => { dismiss(id); }}
      >
        ×
      </button>
    </div>
  );
}

export default function NotificationStack() {
  const notifications = useUiStore((s) => s.notifications);
  if (notifications.length === 0) return null;
  return (
    <div className="fixed top-4 right-4 z-50 flex flex-col gap-2 pointer-events-none">
      {notifications.map((n) => (
        <NotificationItem key={n.id} notification={n} />
      ))}
    </div>
  );
}
