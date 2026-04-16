/** Result returned by a scan command. */
export interface ScanResult {
  /** Total files processed. */
  total: number;
  /** Files added for the first time. */
  new: number;
  /** Files already known to the database. */
  existing: number;
  /** Files that produced errors during scanning. */
  errors: number;
}

/**
 * A file location record — the joined view of `files` + `file_locations`.
 */
export interface FileEntry {
  /** 64-char lowercase hex SHA-256 hash. */
  hash: string;
  /** File size in bytes. */
  size: number;
  /** UUID of the volume that contains this file. */
  volume_id: string;
  /** Path relative to the volume root. */
  relative_path: string;
  /** Lifecycle status: "active" | "missing" | "moved". */
  status: string;
  /** ISO 8601 timestamp of first indexing. */
  first_seen: string;
}

/**
 * A file location joined with any extracted media metadata.
 *
 * Metadata fields are all independently nullable — a location without a
 * matching `file_metadata` row surfaces with every metadata column as
 * `null`, which the UI should treat as "pending extraction", not
 * "no metadata exists".
 */
export interface FileWithMetadata {
  /** 64-char lowercase hex BLAKE3 hash. */
  hash: string;
  /** File size in bytes. */
  size: number;
  /** UUID of the volume that contains this file. */
  volume_id: string;
  /** Path relative to the volume root. */
  relative_path: string;
  /** Lifecycle status: "active" | "missing" | "moved" | "stale". */
  status: string;
  /** ISO 8601 timestamp of first indexing. */
  first_seen: string;
  /** Pixel width (images / video). */
  width: number | null;
  /** Pixel height (images / video). */
  height: number | null;
  /** Duration in milliseconds (video / audio). */
  duration_ms: number | null;
  /** ISO 8601 UTC capture timestamp. */
  captured_at: string | null;
  /** Camera manufacturer (EXIF `Make`). */
  camera_make: string | null;
  /** Camera model (EXIF `Model`). */
  camera_model: string | null;
  /** Codec identifier (e.g. "avc1", "hevc"). */
  codec: string | null;
  /** Overall bitrate in bits per second. */
  bitrate_bps: number | null;
  /** MIME type as detected at extraction time. */
  mime_type: string | null;
}

/** A storage volume known to perima. */
export interface VolumeEntry {
  /** UUIDv7 primary key. */
  id: string;
  /** User-visible label, if any. */
  label: string | null;
  /** Total capacity in bytes. */
  capacity_bytes: number;
  /** Whether the volume is removable (USB, etc.). */
  is_removable: boolean;
  /** Mount-point paths seen on this machine. */
  mounts_on_this_machine: string[];
  /** ISO 8601 timestamp of last observation. */
  last_seen: string;
}

/**
 * Filesystem event emitted by the backend watcher.
 *
 * WHY discriminated union with literal `type` tag: matches the Rust
 * `FileEventPayload` which uses `#[serde(tag = "type")]`. TypeScript
 * can narrow by `switch (e.type)` the same way Rust matches the enum.
 */
export type FileEvent =
  | { type: "Created"; path: string; volume: string }
  | { type: "Modified"; path: string; volume: string }
  | { type: "Deleted"; path: string; volume: string }
  | { type: "Renamed"; from: string; to: string; volume: string };
