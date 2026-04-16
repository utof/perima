/**
 * Thin `neverthrow` wrappers around Tauri IPC commands.
 *
 * WHY: Components use `.match()` instead of try/catch, making error paths
 * explicit and type-checked. `ResultAsync<T, string>` is the contract.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ResultAsync } from "neverthrow";
import type { FileEntry, FileEvent, ScanResult, VolumeEntry } from "./types";

/**
 * Wraps a Tauri `invoke` call in a `ResultAsync`, mapping thrown errors to
 * their string message.
 */
function fromInvoke<T>(
  cmd: string,
  args: Record<string, unknown>,
): ResultAsync<T, string> {
  return ResultAsync.fromPromise(
    invoke<T>(cmd, args),
    (e) => (e instanceof Error ? e.message : String(e)),
  );
}

/**
 * Scan a directory tree, hashing and indexing every file found.
 *
 * @param path - Absolute path to the root folder to scan.
 * @param dryRun - When true, compute hashes but do not write to the database.
 */
export function scan(
  path: string,
  dryRun: boolean,
): ResultAsync<ScanResult, string> {
  // WHY: Tauri v2 auto-converts camelCase args to snake_case on the Rust side.
  return fromInvoke("scan", { path, dryRun });
}

/**
 * List the most-recently-seen files up to `limit` rows.
 *
 * @param limit - Maximum number of rows to return.
 * @param volume - Optional volume UUID to filter by.
 */
export function listFiles(
  limit: number,
  volume?: string,
): ResultAsync<FileEntry[], string> {
  return fromInvoke("list_files", { limit, volume: volume ?? null });
}

/**
 * List all volumes known to the database.
 */
export function listVolumes(): ResultAsync<VolumeEntry[], string> {
  return fromInvoke("list_volumes", {});
}

/**
 * Start watching the given folder for filesystem changes.
 *
 * Cancels any currently active watcher. Events are emitted via the
 * Tauri `file-event` channel; subscribe with {@link subscribeToFileEvents}.
 */
export function startWatch(path: string): ResultAsync<void, string> {
  return fromInvoke("start_watch", { path });
}

/** Stop the active watcher, if any. No-op when nothing is watched. */
export function stopWatch(): ResultAsync<void, string> {
  return fromInvoke("stop_watch", {});
}

/** Query whether a watcher is currently active. */
export function isWatching(): ResultAsync<boolean, string> {
  return fromInvoke("is_watching", {});
}

/** Returned by {@link subscribeToFileEvents}; call to stop listening. */
export type UnsubscribeFn = () => void;

/**
 * Subscribe to `file-event` notifications emitted by the backend watcher.
 *
 * Resolves to an unsubscribe function. Consumers MUST call it on cleanup
 * to avoid leaks (e.g., from `useEffect` return).
 *
 * WHY wrap `listen`: the raw `@tauri-apps/api/event` listener passes a
 * `{ payload, event, id, ... }` object to the callback; we unwrap the
 * payload so consumers only deal with the typed `FileEvent`.
 */
export async function subscribeToFileEvents(
  callback: (event: FileEvent) => void,
): Promise<UnsubscribeFn> {
  return listen<FileEvent>("file-event", (tauriEvent) => {
    callback(tauriEvent.payload);
  });
}
