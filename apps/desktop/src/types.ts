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
