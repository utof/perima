/**
 * Scan-folder button. After Batch H, owns its own mutation; dispatches
 * results into the Zustand scan slice (StatusBar reads from there).
 */
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { open } from "@tauri-apps/plugin-dialog";
import { DownloadSimpleIcon } from "@phosphor-icons/react";
import * as api from "../api";
import { filesKeys } from "../queries/files";
import { tagsKeys } from "../queries/tags";
import { useUiStore } from "../stores/ui";
import type { CoreError, ScanReport } from "../bindings";

export default function ScanButton() {
  const queryClient = useQueryClient();
  const notify = useUiStore((s) => s.notify);
  const notifyError = useUiStore((s) => s.notifyError);
  const setScanStatus = useUiStore((s) => s.setScanStatus);
  const setLastScanReport = useUiStore((s) => s.setLastScanReport);
  const status = useUiStore((s) => s.scan.status);

  const scanMutation = useMutation<ScanReport, CoreError, { path: string; dryRun: boolean }>({
    mutationKey: ["scan"],
    mutationFn: ({ path, dryRun }) =>
      api.scan(path, dryRun).match(
        (report) => report,
        // WHY eslint-disable: TanStack Query mutationFn accepts any thrown value;
        // CoreError is the registered defaultError type so the typed discriminant
        // reaches onError without wrapping.
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
    onMutate: () => { setScanStatus("scanning"); },
    onSuccess: (report, { path }) => {
      setLastScanReport(report);
      setScanStatus("done");
      notify("info", `Scanned ${report.files_seen} files`);
      void queryClient.invalidateQueries({ queryKey: filesKeys.all });
      void queryClient.invalidateQueries({ queryKey: tagsKeys.all });
      // WHY void: startWatch is fire-and-forget; errors surface via notifyError,
      // not by blocking the onSuccess flow.
      void api.startWatch(path).match(
        () => undefined,
        (err) => { notifyError(err); },
      );
    },
    onError: (err) => {
      setScanStatus("idle");
      notifyError(err);
    },
  });

  const onClick = async () => {
    // WHY @tauri-apps/plugin-dialog open: native OS folder picker, not
    // <input type="file"> which cannot return arbitrary directory paths in
    // the Tauri WebView.
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected !== "string") return;
    scanMutation.mutate({ path: selected, dryRun: false });
  };

  // WHY both conditions: status may be "scanning" from a previous render
  // cycle (set by onMutate) before isPending becomes true, and isPending
  // covers the mutation lifecycle between mutate() and onMutate settling.
  const busy = status === "scanning" || scanMutation.isPending;
  return (
    <button
      type="button"
      onClick={() => { void onClick(); }}
      disabled={busy}
      className="inline-flex items-center gap-2 rounded-full px-5 py-2 text-sm font-medium
                 bg-primary text-primary-foreground hover:bg-primary/90
                 transition-colors duration-micro ease-perima
                 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring
                 focus-visible:ring-offset-2 focus-visible:ring-offset-background
                 disabled:opacity-40 disabled:pointer-events-none"
    >
      <DownloadSimpleIcon size={18} weight="regular" />
      {busy ? "Scanning…" : "Scan folder"}
    </button>
  );
}
