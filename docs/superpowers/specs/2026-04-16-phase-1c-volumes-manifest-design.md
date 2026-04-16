# Phase 1c — Volumes, manifest, `perima volumes`

**Status:** draft awaiting reviewer
**Date:** 2026-04-16
**Parent:** meta-plan, phase 1 entry (split 1a/1b/1c).
**Prior:** `phase-1b-complete` tag.

---

## Goal

Replace the sentinel `VolumeId(Uuid::nil())` with real volume
detection using `sysinfo`. Implement `VolumeRepository` in
`crates/db`. Write `.perima/manifest.db` at each volume root during
scan. Ship `perima volumes` command. Retroactively fix sentinel
rows from 1b scans. Tag `phase-1-complete` when done.

## Non-goals

- Manifest *recovery* from `.perima/manifest.db` (phase 9).
- File watching, EventBus (phase 3).
- Thumbnails, tags, FTS5 (phases 4-5).

---

## Architecture

### New modules

```
crates/fs/
└── src/
    └── volumes.rs          # volume detection via sysinfo

crates/db/
└── src/
    ├── volume_repo.rs      # SqliteVolumeRepository impls VolumeRepository
    └── manifest.rs         # per-drive .perima/manifest.db writer
```

### Modified modules

```
crates/cli/
└── src/
    ├── main.rs             # add Volumes command, wire volume detection
    ├── cmd/
    │   ├── mod.rs          # add volumes module
    │   ├── scan.rs         # accept VolumeRepository, real volume detection
    │   └── volumes.rs      # new — perima volumes command
```

---

## Volume detection (`crates/fs/src/volumes.rs`)

Uses `sysinfo::Disks` to enumerate mounted volumes, then matches
the scan root to a volume by longest mount-point prefix.

```rust
/// Detect which volume contains `path` by finding the disk whose
/// mount point is the longest prefix of `path`.
pub fn detect_volume(path: &Path) -> Result<DetectedVolume, CoreError>

pub struct DetectedVolume {
    pub identifiers: VolumeIdentifiers,
    pub mount_point: PathBuf,
}
```

### Drive-identifier priority chain

When `VolumeRepository::find_or_create` looks for a matching row,
it uses this priority:

1. **GPT partition GUID** — most reliable cross-platform ID.
   Available on Linux via `blkid`, sysinfo may not expose it
   directly. For v1, we extract what `sysinfo` provides.
2. **Filesystem UUID** (`fs_uuid`) — second most reliable. `sysinfo`
   exposes this on most platforms.
3. **Volume label** — fallback. Can be user-changed; least stable.

**Conflict resolution:** if GUID matches volume X but label matches
volume Y, **GUID wins** (higher priority). The match algorithm:
try GUID first; if no GUID match found, try fs_uuid; if none,
try label. First match wins. If no match at all, create new.

On platforms where `sysinfo` returns empty identifiers (e.g., some
macOS APFS volumes), fall back to `(capacity_bytes, mount_point)`
as a heuristic — imperfect but better than creating a new volume
row each scan.

**v1 honest assessment:** `sysinfo` 0.38 does NOT expose GPT GUID
or filesystem UUID on any platform. The priority chain (GUID →
fs_uuid → label) is a design-for-the-future skeleton. **Actual v1
matching is label + capacity on all three platforms.** On Linux,
`blkid` could provide fs_uuid but requires root on many distros
(or reading `/run/blkid/blkid.tab` which may not exist); this is
**best-effort, not guaranteed**. The code structure supports
plugging in richer identifiers later without a refactor — that is
the value of the priority chain even when it falls through to
label+capacity today.

### What `sysinfo` provides per disk

- `name()` → device name (e.g., `/dev/sda1`).
- `mount_point()` → `&Path`.
- `file_system()` → `OsStr` (e.g., "ext4", "apfs").
- `kind()` → `DiskKind` (HDD, SSD, Unknown).
- `is_removable()` → `bool`.
- `total_space()` → `u64`.
- `available_space()` → `u64`.

`sysinfo` does NOT directly expose GPT GUID or filesystem UUID as
of 0.38. We set `gpt_partition_guid = None` and attempt to derive
`fs_uuid` from the device name on Linux (parse from `blkid` cache
at `/run/blkid/blkid.tab` or run `blkid -s UUID -o value <dev>`).
On macOS/Windows, `fs_uuid = None` for v1. This means v1 matching
is label + capacity on macOS/Windows — acceptable for a desktop-
only single-machine tool.

---

## `SqliteVolumeRepository` (`crates/db/src/volume_repo.rs`)

Implements `VolumeRepository`. Uses same `Mutex<Connection>` pattern
as `SqliteFileRepository`.

### `find_or_create`

Priority-chain match:

```sql
-- Step 1: try GPT GUID (if provided)
SELECT volume_id FROM volumes
WHERE gpt_partition_guid = ?1 AND deleted_at IS NULL;

-- Step 2: try fs_uuid (if step 1 returned nothing)
SELECT volume_id FROM volumes
WHERE fs_uuid = ?1 AND deleted_at IS NULL;

-- Step 3: try label + capacity (if step 2 returned nothing)
SELECT volume_id FROM volumes
WHERE volume_label = ?1 AND capacity_bytes = ?2 AND deleted_at IS NULL;

-- Step 4: insert new
INSERT INTO volumes (volume_id, gpt_partition_guid, fs_uuid,
    volume_label, capacity_bytes, is_removable, last_seen,
    updated_at, device_id)
VALUES (...);
```

Each step is a SELECT; first non-empty result wins. If a match is
found, UPDATE `last_seen` + `updated_at`. If no match, INSERT.

### `record_mount`

App-level uniqueness on `(volume_id, machine_id, deleted_at IS
NULL)`. SELECT-then-INSERT/UPDATE like `upsert_location`.

### `list`

**Trait signature fix required:** the current trait at
`crates/core/src/ports/volume_repo.rs` defines `fn list(&self)` with
no `machine` parameter, but the SQL needs `machine_id` to filter
mounts. Update the trait to `fn list(&self, machine: DeviceId)`.

```sql
SELECT v.*, vm.mount_path
FROM volumes v
LEFT JOIN volume_mounts vm
    ON v.volume_id = vm.volume_id
    AND vm.machine_id = ?1
    AND vm.deleted_at IS NULL
WHERE v.deleted_at IS NULL
ORDER BY v.volume_label;
```

Group rows by `volume_id` to build `VolumeRecord` with
`mounts_on_this_machine: Vec<PathBuf>`.

---

## Sentinel migration

Phase 1b wrote `file_locations` rows with `volume_id =
'00000000-0000-0000-0000-000000000000'`. The sentinel migration
runs **per-file inside the walk loop**, not as a blanket UPDATE,
because a user who ran 1b scans on multiple directories may have
sentinel rows belonging to different volumes.

Algorithm: during the scan persist loop, for each `(relative_path,
volume_id)` that was just upserted with the real volume ID, also
check if a sentinel row exists for the same `relative_path`:

```sql
UPDATE file_locations
SET volume_id = ?1, updated_at = ?2, device_id = ?3
WHERE volume_id = '00000000-0000-0000-0000-000000000000'
  AND relative_path = ?4
  AND deleted_at IS NULL;
```

This scopes the fix to paths actually observed on the current
volume. Sentinel rows from other volumes' 1b scans remain
untouched until those volumes are scanned in 1c. After all volumes
have been scanned once in 1c, no sentinel rows should remain.

The migration is idempotent (UPDATE WHERE already-real is a no-op)
and has zero cost on fresh installs (no sentinel rows exist).

---

## Per-drive manifest (`crates/db/src/manifest.rs`)

### Schema (`.perima/manifest.db`)

Created at each volume's mount root. Uses a **separate**
`open_manifest(path)` that does NOT share the main DB connection.

```sql
CREATE TABLE IF NOT EXISTS manifest_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS manifest_files (
    blake3_hash     TEXT PRIMARY KEY,
    file_size       INTEGER NOT NULL,
    relative_path   TEXT NOT NULL,
    first_seen      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
```

`manifest_meta` seeded on creation with: `volume_id`,
`manifest_version` (= `"1"`), `created_at`.

### Write strategy

After the scan walk+hash+persist loop, a second pass writes
(or updates) the manifest. For each `HashedFile` that was
successfully persisted to the main DB:

```rust
pub fn write_manifest(
    volume_root: &Path,
    volume_id: VolumeId,
    files: &[HashedFile],
) -> Result<(), CoreError>
```

Uses `INSERT OR REPLACE` into `manifest_files` (the manifest
doesn't follow the main DB's "no UNIQUE on mutable" rule because
the manifest is local + non-CRDT; it's a recovery dump, not a
replicated table).

---

## CLI changes

### `perima scan <path>` (updated)

- Detect volume via `detect_volume(canonical_root)`.
- `find_or_create` the volume in the DB.
- `record_mount` for this machine.
- Run sentinel migration (UPDATE sentinel rows).
- Walk + hash + persist (existing loop, now with real VolumeId).
- Write manifest to `<volume_root>/.perima/manifest.db`.
- Summary: `scanned <N> files on volume <label|short-id> (...)`.

`scan::run` gains:
```rust
pub fn run<S, H, FR, VR>(
    scanner: &S,
    hasher: &H,
    file_repo: Option<&mut FR>,
    volume_repo: Option<&mut VR>,
    // ...
)
```

When `dry_run`, both repos are `None`.

Note: the parameter count is now 8+. A `ScanContext` struct would
be the preferred refactor but is deferred to avoid scope creep in
1c. Acknowledged as tech debt for phase 2.

### `perima volumes`

No options. Output:

```
VOLUME ID   LABEL        REMOVABLE  CAPACITY   MOUNT PATHS
f0e9a1b2…   BACKUP_SSD   yes        2.0 TB     /mnt/backup
```

Implementation is trivial — calls `VolumeRepository::list` and
prints a table.

---

## Test strategy

### Unit tests (`crates/db/src/volume_repo.rs`)

- `find_or_create` inserts new → `VolumeId` returned.
- `find_or_create` matches on `fs_uuid` → same `VolumeId`.
- `find_or_create` matches on label+capacity → same `VolumeId`.
- `find_or_create` GUID match trumps label match (conflict test).
- `record_mount` insert + unchanged on repeat.
- `list` returns volumes with mount paths.

### Unit tests (`crates/fs/src/volumes.rs`)

- `detect_volume` on the current system returns a `DetectedVolume`
  with non-zero capacity (smoke test — can't predict identifiers).
- `detect_volume` on a non-existent path returns `Err`.

### Unit tests (`crates/db/src/manifest.rs`)

- `write_manifest` creates `.perima/manifest.db` with correct
  `manifest_meta` rows.
- `write_manifest` with 3 files → 3 `manifest_files` rows.
- Re-calling `write_manifest` with a changed file → row updated.

### Integration tests (`crates/cli/tests/`)

- `scan_with_volumes.rs`: scan a tmpdir, verify `volumes` table
  has 1 row, `volume_mounts` has 1 row, `file_locations.volume_id`
  is NOT the sentinel.
- `volumes_output.rs`: after scan, `perima volumes` shows 1 volume
  with correct mount path.
- `manifest_created.rs`: after scan, `.perima/manifest.db` exists
  at the tmpdir root with 3 `manifest_files` rows.

### Existing tests

All 42 phase-1b tests must remain green.

---

## Dependencies

Add to `crates/fs/Cargo.toml`:

```toml
sysinfo.workspace = true
```

No new workspace deps needed (sysinfo already in workspace).

---

## Exit criteria (phase 1c, autonomously verifiable)

C1. After scan, `volumes` has 1 row; `volume_mounts` has 1 row
    matching the current machine. Verified via integration test.

C2. `perima volumes` output matches expected format (1 volume row).

C3. `<fixture>/.perima/manifest.db` exists with 3 rows in
    `manifest_files` and volume identity in `manifest_meta`.

C4. Volume-ID priority chain: GUID match trumps label match
    (unit test with injected identifiers).

C5. Sentinel migration: after 1c scan, zero rows have
    `volume_id = '00000000-...'` in `file_locations`.

C6. All 42 phase-1b tests still pass.

C7. `just ci` green.

C8. `cargo doc --workspace --no-deps` exit 0.

C9. WHY-comments on: priority chain algorithm, sentinel migration,
    manifest not following CRDT rules.

C10. CI green on all 3 platforms after push.

C11. `phase-1-complete` tag (note: not `phase-1c-complete`; this
     completes all of phase 1 per the meta-plan).

---

## Risks

- **`sysinfo` platform variance.** GPT GUID and fs_uuid may not be
  available on all platforms. Mitigation: fallback to label+capacity;
  smoke test guards against silent `None` on CI runners.
- **Sentinel migration on large DBs.** Runs per-file inside the walk
  loop (one UPDATE per relative_path). On a 100k-file scan this is
  100k small UPDATEs — fast under WAL. No concern.
- **`.perima/manifest.db` permissions.** On read-only volumes the
  write will fail. Mitigation: catch the error, log a warning,
  continue (the manifest is a convenience, not a hard requirement
  for v1 correctness).
- **Manifest on Windows drive roots.** `C:\` → `.perima/manifest.db`
  at `C:\.perima\manifest.db`. Possible admin-permission issue.
  Mitigation: same — catch and warn.
