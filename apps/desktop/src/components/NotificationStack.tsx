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

  const bg = kind === "error" ? "bg-red-700" : "bg-blue-700";
  return (
    <div
      role={kind === "error" ? "alert" : "status"}
      className={`${bg} text-white text-sm px-4 py-2 rounded shadow flex items-start gap-3 max-w-md`}
    >
      <span className="flex-1">{message}</span>
      <button
        type="button"
        className="text-white/80 hover:text-white focus:outline-none"
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
    <div className="fixed bottom-4 right-4 flex flex-col gap-2 z-50">
      {notifications.map((n) => (
        <NotificationItem key={n.id} notification={n} />
      ))}
    </div>
  );
}
