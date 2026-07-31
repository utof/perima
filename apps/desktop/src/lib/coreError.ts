import type {
  BackupFailureReason,
  CoreError,
  FullHashUnavailableReason,
  TranscriptionError,
} from "../bindings";

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
  if (e.kind === "Transcription") {
    return transcriptionErrorMessage(e.data);
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

/**
 * Returns a user-friendly string for the inner {@link TranscriptionError}
 * carried by `CoreError::Transcription`. Exhaustive `switch (e.kind)` with
 * a TypeScript `never` default — adding a new variant in
 * `crates/core/src/transcription.rs` without extending this switch becomes
 * a compile-time error after `just bindings` regenerates `bindings.ts`.
 *
 * WHY string-form messages over raw `JSON.stringify(data)`: the discriminant
 * payloads (e.g. `RateLimited.retry_after_secs`, `FileTooLarge.limit_bytes`)
 * carry information the user can act on; a generic stringification would bury
 * the actionable bit in object syntax.
 */
function transcriptionErrorMessage(e: TranscriptionError): string {
  switch (e.kind) {
    case "Network":
      return `Network error reaching transcription provider: ${e.data}`;
    case "Auth":
      return "Authentication failed — provider rejected the API key. Check the Settings modal.";
    case "RateLimited":
      return e.data.retry_after_secs !== null
        ? `Rate-limited by transcription provider; retry in ${String(e.data.retry_after_secs)}s.`
        : "Rate-limited by transcription provider; retry shortly.";
    case "QuotaExceeded":
      return "Provider quota or billing exhausted. Check the provider's dashboard.";
    case "ModelNotFound":
      return `Model "${e.data.model}" not available at backend "${e.data.backend}".`;
    case "AudioDecode":
      return `Could not decode audio from source: ${e.data}`;
    case "FileTooLarge": {
      // WHY MiB rounded to whole bytes: provider limits are documented in MB
      // (Groq/OpenAI: 25 MB), not bytes; users compare against that scale.
      const limitMib = Math.round(e.data.limit_bytes / (1024 * 1024));
      return `File too large for provider (limit ${String(limitMib)} MiB).`;
    }
    case "Cancelled":
      return "Transcription cancelled.";
    case "BackendUnavailable":
      return `Transcription backend unavailable: ${e.data.reason}`;
    case "QueueFull":
      return `Transcription queue is full (${String(e.data.queued)} jobs queued); try again shortly.`;
    case "Internal":
      return `Internal transcription error: ${e.data}`;
    default: {
      const _exhaustive: never = e;
      return String(_exhaustive);
    }
  }
}
