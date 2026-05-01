/**
 * `useBackupDatabase` — TanStack Query mutation hook for `backup_database` IPC.
 *
 * WHY no `invalidateQueries` call: backup is a read-only-from-the-frontend
 * operation — it produces a side-effect file but no frontend cache observes
 * DB rows differently after a backup. If a future slice (e.g. a "list of
 * recent backups" panel) introduces a queryKey that mirrors filesystem
 * state under `<data_dir>/backups/`, invalidate that key here.
 */

import { useMutation } from "@tanstack/react-query";

import * as api from "../api";
import type { CoreError } from "../bindings";
import { coreErrorMessage } from "../lib/coreError";
import { useUiStore } from "../stores/ui";

export const backupKeys = { all: ["backup"] as const };

interface BackupVars {
  target?: string;
  force?: boolean;
}

export function useBackupDatabase() {
  const notify = useUiStore((s) => s.notify);
  return useMutation({
    mutationKey: backupKeys.all,
    mutationFn: async (vars: BackupVars) =>
      api.backupDatabase(vars.target, vars.force ?? false).match(
        (out) => out,
        (err) => {
          // eslint-disable-next-line @typescript-eslint/only-throw-error
          throw err;
        },
      ),
    onSuccess: (out) => {
      const mb = (out.size_bytes / (1024 * 1024)).toFixed(1);
      notify("info", `Backup saved to ${out.absolute_path} (${mb} MB)`);
    },
    onError: (err: CoreError) => {
      notify("error", `Backup failed: ${coreErrorMessage(err)}`);
    },
  });
}
