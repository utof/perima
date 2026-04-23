import type { CoreError } from "../bindings";

/**
 * Returns a human-readable string from a {@link CoreError} data payload.
 *
 * WHY helper: three call sites (App.tsx, ScanButton.tsx, StatusBar.tsx) all
 * need to stringify the `data` field, which is either a plain string or the
 * structured `{ kind, message }` object carried by Io errors. Centralising the
 * logic eliminates duplicated inline ternaries and adds cyclic-object safety
 * (JSON.stringify throws on cyclic inputs — guarded by try/catch).
 */
export function coreErrorMessage(e: CoreError): string {
  if (typeof e.data === "string") {
    return e.data;
  }
  // WHY try/catch: JSON.stringify throws on cyclic objects. In practice
  // CoreError payloads are plain structs from Rust, but we guard defensively.
  try {
    return JSON.stringify(e.data);
  } catch {
    return "[unserializable error data]";
  }
}
