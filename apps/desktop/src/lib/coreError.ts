import type { BackupFailureReason, CoreError, FullHashUnavailableReason } from "../bindings";

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
  if (e.kind === "FullHashUnavailable") {
    return fullHashUnavailableMessage(e.data.reason);
  }
  if (e.kind === "BackupFailed") {
    return backupFailureMessage(e.data.reason);
  }
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

function fullHashUnavailableMessage(reason: FullHashUnavailableReason): string {
  switch (reason.kind) {
    case "NotMounted":
      return `Full hash unavailable: volume not mounted (${reason.volume_id}).`;
    case "NotComputed":
      return "Full hash has not been computed for this file yet.";
    case "IoError":
      return `Full hash unavailable: I/O error (${reason.message}).`;
  }
}

function backupFailureMessage(reason: BackupFailureReason): string {
  switch (reason.kind) {
    case "TargetExists":
      return `Backup file already exists at ${reason.data.path}. Pass --force to overwrite, or pick a different path.`;
    case "TargetUnwritable":
      return `Cannot write backup at ${reason.data.path}: ${reason.data.message}`;
    case "DiskFull":
      return `Disk is full; cannot write backup to ${reason.data.path}.`;
    case "AlreadyInProgress":
      return "A backup is already running. Try again when it finishes.";
    case "Internal":
      return `Internal backup error: ${reason.data}`;
    default: {
      const _exhaustive: never = reason;
      return _exhaustive;
    }
  }
}
