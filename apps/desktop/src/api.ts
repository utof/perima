/**
 * Thin `neverthrow` wrappers around Tauri IPC commands.
 *
 * WHY: Components use `.match()` instead of try/catch, making error paths
 * explicit and type-checked. `ResultAsync<T, string>` is the contract.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ResultAsync } from "neverthrow";
import type {
  FileEntry,
  FileEvent,
  FileWithMetadata,
  FileWithTags,
  ScanResult,
  Tag,
  VolumeEntry,
} from "./types";

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
 * List files joined with any extracted media metadata, up to `limit`
 * rows.
 *
 * Metadata fields are independently nullable — a location without an
 * extracted `file_metadata` row surfaces with every metadata column as
 * `null` and should be treated by the UI as "pending extraction".
 *
 * @param limit - Maximum number of rows to return.
 * @param volume - Optional volume UUID to filter by.
 */
export function listFilesWithMetadata(
  limit: number,
  volume?: string,
): ResultAsync<FileWithMetadata[], string> {
  return fromInvoke("list_files_with_metadata", {
    limit,
    volume: volume ?? null,
  });
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

/** List all active tags. */
export function listTags(): ResultAsync<Tag[], string> {
  return fromInvoke("list_tags", {});
}

/** Attach a tag to a file by content hash. Returns the tag. */
export function attachTag(
  hash: string,
  tagName: string,
): ResultAsync<Tag, string> {
  return fromInvoke("attach_tag", { hash, tagName });
}

/** Remove a tag from a file. */
export function detachTag(
  hash: string,
  tagId: string,
): ResultAsync<void, string> {
  return fromInvoke("detach_tag", { hash, tagId });
}

/** List files with metadata and tags. */
export function listFilesWithTags(
  limit: number,
  volume?: string,
): ResultAsync<FileWithTags[], string> {
  return fromInvoke("list_files_with_tags", { limit, volume: volume ?? null });
}
