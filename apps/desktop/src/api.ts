/**
 * Thin `neverthrow` wrappers around Tauri IPC commands.
 *
 * WHY: Components use `.match()` instead of try/catch, making error paths
 * explicit and type-checked. `ResultAsync<T, CoreError>` is the contract.
 * WHY CoreError not string: the backend now returns a typed discriminated
 * union `{ kind, data }` so the frontend can branch on recoverable vs not
 * (e.g. "NotFound" → soft refresh, "Io.kind=PermissionDenied" → modal).
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ResultAsync } from "neverthrow";
import type {
  AppEvent,
  CoreError,
  FileLocationRecord,
  FileWithMetadataPayload,
  FileWithTagsPayload,
  ScanReport,
  SearchHit,
  Tag,
  VolumeRecord,
} from "./bindings";

// ── Error parsing ─────────────────────────────────────────────────────

/**
 * All discriminant strings recognised in `CoreError`.
 * Kept in sync with `crates/core/src/errors.rs` variants.
 * WHY a Set: `KNOWN_KINDS.has(x)` is O(1) and type-narrows cleanly.
 */
const KNOWN_KINDS: ReadonlySet<CoreError["kind"]> = new Set([
  "NotFound",
  "Duplicate",
  "InvalidPath",
  "InvalidHash",
  "InvalidTag",
  "Io",
  "Unsupported",
  "Internal",
  "FullHashUnavailable",
]);

/**
 * Convert an unknown Tauri rejection value into a `CoreError`.
 *
 * Tauri rejects with the JSON-serialised `Result::Err` payload when a
 * command handler returns `Err(CoreError::…)`. The payload is already
 * `{ kind: "…", data: … }` on the wire, so we just validate and
 * pass it through.
 *
 * The fallback path covers:
 * - Tauri runtime errors that reject before the command runs (e.g.
 *   command-not-registered, IPC serialisation failures).
 * - Future `CoreError` variants the frontend hasn't picked up yet
 *   (graceful degradation rather than crash).
 */
export function parseCoreError(raw: unknown): CoreError {
  if (typeof raw === "object" && raw !== null && "kind" in raw) {
    const r = raw as { kind: unknown; data?: unknown };
    if (
      typeof r.kind === "string" &&
      KNOWN_KINDS.has(r.kind as CoreError["kind"])
    ) {
      return r as CoreError;
    }
  }
  return {
    kind: "Internal",
    data: typeof raw === "string" ? raw : JSON.stringify(raw),
  };
}

// ── IPC wrapper ───────────────────────────────────────────────────────

/**
 * Wraps a Tauri `invoke` call in a `ResultAsync`, mapping rejected
 * values to typed `CoreError` via `parseCoreError`.
 */
function fromInvoke<T>(
  cmd: string,
  args: Record<string, unknown>,
): ResultAsync<T, CoreError> {
  return ResultAsync.fromPromise(invoke<T>(cmd, args), parseCoreError);
}

// ── Command wrappers ──────────────────────────────────────────────────

/**
 * Scan a directory tree, hashing and indexing every file found.
 *
 * @param path - Absolute path to the root folder to scan.
 * @param dryRun - When true, compute hashes but do not write to the database.
 */
export function scan(
  path: string,
  dryRun: boolean,
): ResultAsync<ScanReport, CoreError> {
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
): ResultAsync<FileLocationRecord[], CoreError> {
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
): ResultAsync<FileWithMetadataPayload[], CoreError> {
  return fromInvoke("list_files_with_metadata", {
    limit,
    volume: volume ?? null,
  });
}

/**
 * List all volumes known to the database.
 */
export function listVolumes(): ResultAsync<VolumeRecord[], CoreError> {
  return fromInvoke("list_volumes", {});
}

/**
 * Start watching the given folder for filesystem changes.
 *
 * Cancels any currently active watcher. Events are emitted via the
 * Tauri `app-event` channel; subscribe with {@link subscribeToAppEvents}.
 */
export function startWatch(path: string): ResultAsync<void, CoreError> {
  return fromInvoke("start_watch", { path });
}

/** Stop the active watcher, if any. No-op when nothing is watched. */
export function stopWatch(): ResultAsync<void, CoreError> {
  return fromInvoke("stop_watch", {});
}

/** Query whether a watcher is currently active. */
export function isWatching(): ResultAsync<boolean, CoreError> {
  return fromInvoke("is_watching", {});
}

/** Returned by {@link subscribeToAppEvents}; call to stop listening. */
export type UnsubscribeFn = () => void;

/**
 * Subscribe to `app-event` notifications emitted by the backend bus.
 *
 * Resolves to an unsubscribe function. Consumers MUST call it on cleanup
 * to avoid leaked listeners (e.g., from `useEffect` return).
 *
 * Channel renamed from `"file-event"` to `"app-event"` in Batch E — the
 * single channel now carries the full `AppEvent` envelope (`File`,
 * `ScanCompleted`, `IndexInvalidated`).
 *
 * WHY wrap `listen`: the raw `@tauri-apps/api/event` listener passes a
 * `{ payload, event, id, ... }` object to the callback; we unwrap the
 * payload so consumers only deal with the typed `AppEvent`.
 */
export async function subscribeToAppEvents(
  callback: (event: AppEvent) => void,
): Promise<UnsubscribeFn> {
  return listen<AppEvent>("app-event", (tauriEvent) => {
    callback(tauriEvent.payload);
  });
}

/** List all active tags. */
export function listTags(): ResultAsync<Tag[], CoreError> {
  return fromInvoke("list_tags", {});
}

/** Attach a tag to a file by content hash. Returns the tag. */
export function attachTag(
  hash: string,
  tagName: string,
): ResultAsync<Tag, CoreError> {
  return fromInvoke("attach_tag", { hash, tagName });
}

/** Remove a tag from a file. */
export function detachTag(
  hash: string,
  tagId: string,
): ResultAsync<void, CoreError> {
  return fromInvoke("detach_tag", { hash, tagId });
}

/** List files with metadata and tags. */
export function listFilesWithTags(
  limit: number,
  volume?: string,
): ResultAsync<FileWithTagsPayload[], CoreError> {
  return fromInvoke("list_files_with_tags", { limit, volume: volume ?? null });
}

/**
 * Run a full-text search query against the FTS5 index.
 *
 * @param query - FTS5 MATCH expression (e.g. `"vacation"`, `"Canon*"`).
 * @param limit - Maximum number of ranked results (default 50).
 */
export function search(
  query: string,
  limit = 50,
): ResultAsync<SearchHit[], CoreError> {
  return fromInvoke("search", { query, limit });
}

/** Wipe and rebuild the FTS5 search index from the current DB state. */
export function searchRebuild(): ResultAsync<void, CoreError> {
  return fromInvoke("search_rebuild", {});
}
