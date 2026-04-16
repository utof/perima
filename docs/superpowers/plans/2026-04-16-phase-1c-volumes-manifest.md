# Phase 1c — Volumes, Manifest, `perima volumes` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the sentinel `VolumeId(Uuid::nil())` with real volume detection via `sysinfo`. Implement `VolumeRepository` in `crates/db`. Write `.perima/manifest.db` at each volume root during scan. Ship `perima volumes`. Fix sentinel rows from 1b. Tag `phase-1-complete`.

**Architecture:** `crates/fs/volumes.rs` detects volumes via `sysinfo::Disks` (longest mount-prefix match). `crates/db/volume_repo.rs` impls `VolumeRepository` (priority-chain match: label+capacity for v1). `crates/db/manifest.rs` creates `.perima/manifest.db` per volume. `crates/cli` wires volume detection into scan, adds `perima volumes` command, migrates sentinel rows per-file.

**Tech Stack:** sysinfo 0.38, rusqlite 0.38 (bundled), chrono 0.4, uuid v7. Existing: blake3, walkdir, clap 4, rayon, ctrlc, tempfile.

**Spec:** `docs/superpowers/specs/2026-04-16-phase-1c-volumes-manifest-design.md`

**Execution rule:** All work on `main`. Per-commit: execute → `just ci` green → reviewer → commit. No branches/force.

---

## File Structure

```
crates/core/src/ports/volume_repo.rs    # modify — add machine param to list()
crates/fs/
├── Cargo.toml                          # modify — add sysinfo dep
└── src/
    ├── lib.rs                          # modify — add volumes module
    └── volumes.rs                      # new — volume detection

crates/db/src/
├── lib.rs                              # modify — add volume_repo + manifest
├── volume_repo.rs                      # new — SqliteVolumeRepository
└── manifest.rs                         # new — per-drive manifest writer

crates/cli/src/
├── main.rs                             # modify — wire volume detection, add Volumes cmd
└── cmd/
    ├── mod.rs                          # modify — add volumes module
    ├── scan.rs                         # modify — accept VolumeRepository, sentinel migration
    └── volumes.rs                      # new — perima volumes command

crates/cli/tests/
├── scan_with_volumes.rs                # new
├── volumes_output.rs                   # new
└── manifest_created.rs                 # new
```

Three reviewer-gated commits: (1) fs volumes + db volume_repo + manifest, (2) CLI wiring, (3) integration tests + push + tag.

---

## Task 1: Fix `VolumeRepository::list` trait signature

**Files:**
- Modify: `crates/core/src/ports/volume_repo.rs`

- [ ] **Step 1: Add `DeviceId` parameter to `list`**

Change the `list` method signature from:

```rust
fn list(&self) -> Result<Vec<VolumeRecord>, CoreError>;
```

to:

```rust
/// Enumerate all known volumes with their current mounts for
/// the given `machine`.
///
/// # Errors
/// `CoreError::Internal` on adapter failure.
fn list(&self, machine: DeviceId) -> Result<Vec<VolumeRecord>, CoreError>;
```

- [ ] **Step 2: Verify workspace builds**

Run: `cargo build --workspace`
Expected: exit 0 (no code implements this trait yet with the wrong sig).

---

## Task 2: Volume detection (`crates/fs/src/volumes.rs`)

**Files:**
- Modify: `crates/fs/Cargo.toml` — add `sysinfo.workspace = true`
- Create: `crates/fs/src/volumes.rs`
- Modify: `crates/fs/src/lib.rs`

- [ ] **Step 1: Add sysinfo dep**

Add to `crates/fs/Cargo.toml` `[dependencies]`:

```toml
sysinfo.workspace = true
```

- [ ] **Step 2: Write `volumes.rs` with tests**

The implementer should create `crates/fs/src/volumes.rs`. The module should:

1. Define `DetectedVolume { identifiers: VolumeIdentifiers, mount_point: PathBuf }`.
2. Implement `pub fn detect_volume(path: &Path) -> Result<DetectedVolume, CoreError>` that:
   - Canonicalizes `path` via `dunce::canonicalize`.
   - Uses `sysinfo::Disks::new_with_refreshed_list()` to enumerate disks.
   - Finds the disk whose `mount_point()` is the longest prefix of the canonicalized path.
   - Populates `VolumeIdentifiers` from the matched disk: `gpt_partition_guid = None`, `fs_uuid = None` (v1 honest assessment — sysinfo doesn't expose these), `label` from `disk.name().to_string_lossy()`, `capacity_bytes` from `disk.total_space()`, `is_removable` from `disk.is_removable()`.
   - Returns `CoreError::Internal("no volume found for path: ...")` if no disk matches.
3. Include a `// WHY: v1 matching is label+capacity only; sysinfo 0.38 does not expose GPT GUID or fs_uuid on any platform. The priority-chain structure supports plugging in richer identifiers later.` comment.
4. Include tests:
   - `detect_volume_on_cwd` — `detect_volume(std::env::current_dir())` returns `Ok` with non-zero capacity (smoke test).
   - `detect_volume_nonexistent_path` — `detect_volume(Path::new("/definitely/does/not/exist"))` returns `Err`.

- [ ] **Step 3: Update `crates/fs/src/lib.rs`**

Add `pub mod volumes;` and `pub use volumes::{DetectedVolume, detect_volume};`.

- [ ] **Step 4: Run gates**

Run: `cargo test -p perima-fs` → existing 6 + 2 new = 8 tests pass.
Run: `just ci` → exit 0.

---

## Task 3: `SqliteVolumeRepository` (`crates/db/src/volume_repo.rs`) — TDD

**Files:**
- Create: `crates/db/src/volume_repo.rs`
- Modify: `crates/db/src/lib.rs`

The implementer should create `crates/db/src/volume_repo.rs`. The module should:

1. Define `pub struct SqliteVolumeRepository` wrapping `Mutex<Connection>` (same pattern as `SqliteFileRepository`).
2. Implement `VolumeRepository` for it with the three methods:
   - `find_or_create`: priority-chain SELECT (label+capacity for v1, GUID/fs_uuid arms for future). If matched, UPDATE `last_seen` + `updated_at`. If not, INSERT new row with `VolumeId::new()`.
   - `record_mount`: app-level uniqueness on `(volume_id, machine_id, deleted_at IS NULL)`. SELECT-then-INSERT/UPDATE.
   - `list(machine)`: LEFT JOIN volumes + volume_mounts filtered by `machine_id`, grouped by `volume_id`. Return `Vec<VolumeRecord>`.
3. Include a `// WHY: priority chain tries GUID first, then fs_uuid, then label+capacity. v1 only has label+capacity; the structure exists so future sysinfo upgrades or blkid integration slot in without refactoring the match logic.` comment.
4. Include **6 tests** (TDD — write tests first, verify compile fail, then implement):
   - `find_or_create_inserts_new` → returns a `VolumeId`.
   - `find_or_create_matches_on_label_capacity` → same `VolumeId` when label+capacity match.
   - `find_or_create_guid_trumps_label` — insert vol A with GUID "X" + label "A"; insert vol B with label "B"; then `find_or_create` with GUID "X" + label "B" should return vol A's id (GUID wins).
   - `record_mount_inserts_new` → inserts.
   - `record_mount_unchanged_on_repeat` → no error, unchanged.
   - `list_returns_volumes_with_mounts` → after find_or_create + record_mount, list returns 1 volume with 1 mount path.

- [ ] **Step 1: Write tests first, verify compile fail**
- [ ] **Step 2: Implement `SqliteVolumeRepository`**
- [ ] **Step 3: Update `crates/db/src/lib.rs`** — add `pub mod volume_repo;` + `pub use volume_repo::SqliteVolumeRepository;`
- [ ] **Step 4: Run gates**

Run: `cargo test -p perima-db` → 11 existing + 6 new = 17 tests pass.
Run: `just ci` → exit 0.

---

## Task 4: Manifest writer (`crates/db/src/manifest.rs`)

**Files:**
- Create: `crates/db/src/manifest.rs`
- Modify: `crates/db/src/lib.rs`

The implementer should create `crates/db/src/manifest.rs`. The module should:

1. Define `pub fn write_manifest(volume_root: &Path, volume_id: VolumeId, files: &[HashedFile]) -> Result<(), CoreError>`.
2. Implementation:
   - Create `<volume_root>/.perima/` directory if missing.
   - Open (or create) `<volume_root>/.perima/manifest.db` via `rusqlite::Connection::open`.
   - Run `CREATE TABLE IF NOT EXISTS manifest_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)`.
   - Run `CREATE TABLE IF NOT EXISTS manifest_files (blake3_hash TEXT PRIMARY KEY, file_size INTEGER NOT NULL, relative_path TEXT NOT NULL, first_seen TEXT NOT NULL, updated_at TEXT NOT NULL)`.
   - Upsert `manifest_meta` rows: `volume_id`, `manifest_version` = "1", `created_at` (INSERT OR REPLACE).
   - For each file: `INSERT OR REPLACE INTO manifest_files` with hash, size, relative_path, first_seen=now, updated_at=now.
   - Include `// WHY: manifest uses INSERT OR REPLACE (not the main DB's app-level uniqueness pattern) because the manifest is a local recovery dump, not a CRDT-replicated table. Simplicity wins here.`
3. If the write fails (e.g., read-only volume), log a warning via `tracing::warn!` and return `Ok(())` — the manifest is a convenience, not a hard requirement.
4. Include **3 tests**:
   - `write_manifest_creates_db` — creates `.perima/manifest.db` with correct `manifest_meta` rows.
   - `write_manifest_writes_files` — 3 files → 3 `manifest_files` rows.
   - `write_manifest_updates_on_rerun` — change a file's relative_path, re-run, verify updated row.

- [ ] **Step 1: Write tests first, verify compile fail**
- [ ] **Step 2: Implement manifest writer**
- [ ] **Step 3: Update `crates/db/src/lib.rs`** — add `pub mod manifest;`
- [ ] **Step 4: Run gates**

Run: `cargo test -p perima-db` → 17 + 3 = 20 tests pass.
Run: `just ci` → exit 0.

- [ ] **Step 5: Dispatch reviewer (checkpoint #1 — fs volumes + db volume_repo + manifest)**

Reviewer checklist:
- [ ] `detect_volume` uses longest mount-prefix match, returns `DetectedVolume`.
- [ ] WHY comment on v1 label+capacity reality.
- [ ] `SqliteVolumeRepository`: priority chain structure with label+capacity v1 path, GUID arm exists but tests show it works.
- [ ] `record_mount`: app-level uniqueness.
- [ ] `list(machine)`: LEFT JOIN, grouped by volume_id.
- [ ] Manifest: separate DB, INSERT OR REPLACE, creates `.perima/` dir, graceful failure on read-only.
- [ ] 20 db tests + 8 fs tests pass. `just ci` green.
- [ ] WHY comments present.

- [ ] **Step 6: Commit (after APPROVED)**

```bash
git add crates/core/src/ports/volume_repo.rs crates/fs/ crates/db/ Cargo.lock
git commit -m "$(cat <<'EOF'
feat(phase-1c): volume detection, VolumeRepository, manifest writer

crates/fs/volumes.rs: detect_volume via sysinfo longest mount-prefix
match. v1 matching is label+capacity only (sysinfo 0.38 does not
expose GPT GUID or fs_uuid); priority-chain structure supports future
richer identifiers.

crates/db/volume_repo.rs: SqliteVolumeRepository impls VolumeRepository.
Priority-chain find_or_create (GUID -> fs_uuid -> label+capacity);
record_mount with app-level uniqueness; list(machine) with LEFT JOIN.

crates/db/manifest.rs: write_manifest creates .perima/manifest.db at
volume root with manifest_meta + manifest_files tables. Uses INSERT OR
REPLACE (non-CRDT local recovery dump). Graceful failure on read-only
volumes (warn + continue).

VolumeRepository::list trait signature updated to accept DeviceId for
machine-scoped mount filtering.

Refs: docs/superpowers/specs/2026-04-16-phase-1c-volumes-manifest-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: CLI — update `scan.rs` for volume detection + sentinel migration

**Files:**
- Modify: `crates/cli/src/cmd/scan.rs`

The implementer should modify `scan.rs`:

1. Change `run` signature to accept an optional `VolumeRepository`:
   ```rust
   pub fn run<S, H, FR, VR>(
       scanner: &S,
       hasher: &H,
       mut file_repo: Option<&mut FR>,
       mut volume_repo: Option<&mut VR>,
       device: DeviceId,
       cancel: &Cancellation,
       args: &ScanArgs,
   ) -> Result<(ExitCode, ScanStats), CoreError>
   where
       S: Scanner + ?Sized,
       H: HashService + ?Sized,
       FR: FileRepository + ?Sized,
       VR: VolumeRepository + ?Sized,
   ```
   Note: `volume: VolumeId` parameter is REMOVED — the function detects it internally when not dry-run.

2. When `dry_run=false`:
   - Call `perima_fs::detect_volume(&canonical_root)` to get `DetectedVolume`.
   - Call `volume_repo.find_or_create(&detected.identifiers, device)` to get real `VolumeId`.
   - Call `volume_repo.record_mount(volume_id, device, &detected.mount_point)`.
   - Use this real `VolumeId` for all `upsert_location` calls.

3. Sentinel migration: inside the persist loop, after `upsert_location`, add:
   ```rust
   // Migrate sentinel rows from 1b for this specific path.
   if let Some(ref mut fr) = file_repo {
       // Use a raw SQL approach via a new method or direct conn access.
       // For simplicity, add a migrate_sentinel method to FileRepository.
   }
   ```
   
   Actually, the sentinel migration should be a standalone function in `crates/db` rather than polluting the `FileRepository` trait. Add a public function:
   ```rust
   // crates/db/src/file_repo.rs (or a new sentinel.rs)
   pub fn migrate_sentinel_for_path(
       conn: &Mutex<Connection>,
       relative_path: &str,
       real_volume_id: &str,
       device_id: &str,
   ) -> Result<u64, Error>
   ```
   
   But since `SqliteFileRepository` owns the `Mutex<Connection>`, expose it via a method:
   ```rust
   impl SqliteFileRepository {
       pub fn migrate_sentinel_row(
           &self,
           relative_path: &MediaPath,
           real_volume: VolumeId,
           device: DeviceId,
       ) -> Result<u64, CoreError>
   }
   ```
   This runs the scoped UPDATE from the spec.

4. When `dry_run=true`: pass `None` for both repos, use a placeholder `VolumeId` (doesn't matter — no DB).

5. Update the summary line: when not dry-run, print `"scanned N files on volume <label|short-id> (...)"` where label comes from the detected volume.

- [ ] **Step 1: Add `migrate_sentinel_row` to `SqliteFileRepository` (in crates/db)**
- [ ] **Step 2: Rewrite `scan.rs` with the new signature + volume detection + sentinel migration**
- [ ] **Step 3: Run gates** — expect compile fail until main.rs updated. Continue.

---

## Task 6: CLI — `cmd/volumes.rs`

**Files:**
- Create: `crates/cli/src/cmd/volumes.rs`
- Modify: `crates/cli/src/cmd/mod.rs`

The implementer should create `volumes.rs`:

1. `pub fn run<VR: VolumeRepository>(repo: &VR, machine: DeviceId) -> Result<(), CoreError>`.
2. Calls `repo.list(machine)`, prints a table:
   ```
   VOLUME ID   LABEL        REMOVABLE  CAPACITY   MOUNT PATHS
   f0e9a1b2…   BACKUP_SSD   yes        2.0 TB     /mnt/backup
   ```
3. Re-use the `format_size` function from `ls.rs` (move it to a shared `cmd/format.rs` or just duplicate — plan author's choice; I recommend a small `cmd/format.rs` to avoid duplication).

- [ ] **Step 1: Create `cmd/format.rs`** with the shared `format_size` function (move from ls.rs).
- [ ] **Step 2: Update `cmd/ls.rs`** to use `cmd::format::format_size`.
- [ ] **Step 3: Create `cmd/volumes.rs`**.
- [ ] **Step 4: Update `cmd/mod.rs`** — add `pub mod format;` and `pub mod volumes;`.

---

## Task 7: CLI — update `main.rs`

**Files:**
- Modify: `crates/cli/src/main.rs`

The implementer should:

1. Add `Volumes` variant to `Command` enum (no args).
2. Update `dispatch_scan`:
   - Remove the sentinel `VolumeId(Uuid::nil())` line.
   - For non-dry-run: open DB, create both `SqliteFileRepository` and `SqliteVolumeRepository` from the same connection... **Wait — they each wrap `Mutex<Connection>`, so they each take ownership.** This means two repos can't share one connection.
   
   **Solution:** open two connections (one for file_repo, one for volume_repo). Under WAL mode this is safe — multiple readers/one writer, and scan's writes go through file_repo while volume_repo writes happen before the scan loop. Alternatively, pass the same `Connection` to both via `Arc<Mutex<Connection>>`. The simplest approach for v1: **open the DB twice** (`open_and_migrate` is idempotent; the second call is instant because migrations already ran).
   
3. For dry-run: pass `None` for both repos (turbofish needs two type params now).
4. Add `dispatch_volumes` function.
5. Import `perima_db::SqliteVolumeRepository` and `perima_fs::detect_volume`.

- [ ] **Step 1: Rewrite main.rs**
- [ ] **Step 2: Run gates**

Run: `just ci` → exit 0.
Run: `cargo run -p perima -- --help` → shows `scan`, `ls`, `volumes`.
Run: `cargo run -p perima -- volumes` → shows at least 1 volume.
Run: `cargo run -p perima -- scan <tmpdir>` → uses real volume, summary shows volume label.

- [ ] **Step 3: Dispatch reviewer (checkpoint #2 — CLI wiring)**

Reviewer checklist:
- [ ] `scan.rs` calls `detect_volume`, `find_or_create`, `record_mount`.
- [ ] Sentinel migration runs per-file via `migrate_sentinel_row`.
- [ ] `volumes.rs` prints table.
- [ ] `main.rs` wires both repos (two DB connections under WAL).
- [ ] `just ci` green. All existing tests pass.
- [ ] WHY comments on sentinel migration scoping.

- [ ] **Step 4: Commit (after APPROVED)**

```bash
git add crates/cli/ crates/db/src/file_repo.rs Cargo.lock
git commit -m "$(cat <<'EOF'
feat(phase-1c): scan uses real volume detection, perima volumes

scan.rs: detect_volume via sysinfo, find_or_create + record_mount
via VolumeRepository, sentinel migration per-file (scoped by
relative_path). Summary shows volume label.

volumes.rs: new command listing detected volumes with mount paths.

main.rs: wires SqliteVolumeRepository (second DB connection under
WAL). format.rs extracted from ls.rs for shared format_size.

migrate_sentinel_row on SqliteFileRepository: scoped UPDATE of
sentinel VolumeId rows by relative_path.

Refs: docs/superpowers/specs/2026-04-16-phase-1c-volumes-manifest-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Integration tests

**Files:**
- Create: `crates/cli/tests/scan_with_volumes.rs`
- Create: `crates/cli/tests/volumes_output.rs`
- Create: `crates/cli/tests/manifest_created.rs`

The implementer should create 3 integration test files:

### `scan_with_volumes.rs` (2 tests):
1. `scan_uses_real_volume` — scan 3 fixtures, open DB with rusqlite, assert:
   - `volumes` table has 1 row.
   - `volume_mounts` table has 1 row.
   - All `file_locations.volume_id` are NOT `'00000000-0000-0000-0000-000000000000'`.
2. `sentinel_rows_migrated` — pre-populate the DB with a sentinel file_location row (using raw rusqlite), then run scan on the same fixture. Assert the sentinel row's `volume_id` is now the real one (not all-zeros).

### `volumes_output.rs` (1 test):
1. `volumes_shows_one_volume` — scan a tmpdir, then `perima volumes`, assert stdout has header + at least 1 data line.

### `manifest_created.rs` (1 test):
1. `manifest_db_created_after_scan` — scan a tmpdir, assert `<tmpdir>/.perima/manifest.db` exists, open it with rusqlite, assert `manifest_files` has 3 rows and `manifest_meta` has `volume_id` key.

All tests use `env!("CARGO_BIN_EXE_perima")`, tmpdir fixtures, `PERIMA_CONFIG_DIR`/`PERIMA_DATA_DIR` env overrides.

- [ ] **Step 1: Write `scan_with_volumes.rs`**
- [ ] **Step 2: Write `volumes_output.rs`**
- [ ] **Step 3: Write `manifest_created.rs`**
- [ ] **Step 4: Run gates**

Run: `cargo test --workspace` → all tests pass.
Run: `just ci` → exit 0.

---

## Task 9: Final sweep + push + tag

- [ ] **Step 1: WHY-comment check**

Run: `grep -rE '^\s*//\s*WHY:' crates/ | wc -l`
Expected: ≥ 3 new (priority chain, sentinel migration scoping, manifest non-CRDT).

- [ ] **Step 2: Clean build sweep**

Run: `cargo clean && just ci`
Expected: exit 0.

- [ ] **Step 3: `.md` hygiene**

Run: `git ls-files '*.md'`
Expected: empty.

- [ ] **Step 4: Dispatch final reviewer (checkpoint #3)**

Reviewer checklist:
- [ ] All exit criteria C1–C11 from the spec met.
- [ ] All 42+ previous tests pass.
- [ ] New integration tests cover volumes, sentinel migration, manifest.
- [ ] `just ci` green.
- [ ] WHY comments present.

- [ ] **Step 5: Commit integration tests**

```bash
git add crates/cli/tests/ Cargo.lock
git commit -m "$(cat <<'EOF'
test(phase-1c): integration tests for volumes + manifest

scan_with_volumes.rs: scan uses real volume (non-sentinel); volumes +
volume_mounts tables populated. Sentinel migration verified.

volumes_output.rs: perima volumes shows header + data lines.

manifest_created.rs: .perima/manifest.db created with manifest_meta +
manifest_files rows.

Refs: docs/superpowers/specs/2026-04-16-phase-1c-volumes-manifest-design.md

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 6: Push + wait CI green**

```bash
git push origin main
gh run watch --exit-status "$(gh run list --workflow=ci.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
```

- [ ] **Step 7: Tag (ONLY after CI green)**

Note: tag is `phase-1-complete` (not `phase-1c-complete`) per the meta-plan — this completes all of phase 1.

```bash
git tag -a phase-1-complete -m "Phase 1: indexing core + CLI (scan, ls, volumes)"
git push origin phase-1-complete
```

---

## Self-review

**Spec coverage:**
- Volume detection (detect_volume) → Task 2 ✓
- Priority chain (label+capacity v1, GUID/fs_uuid future) → Task 3 ✓
- VolumeRepository impl (find_or_create, record_mount, list) → Task 3 ✓
- Trait signature fix (list + DeviceId) → Task 1 ✓
- Sentinel migration (per-file scoped) → Task 5 ✓
- Manifest writer (.perima/manifest.db) → Task 4 ✓
- scan.rs wiring → Task 5 ✓
- perima volumes command → Task 6 ✓
- main.rs wiring → Task 7 ✓
- Integration tests → Task 8 ✓
- Exit criteria C1–C11 → Task 9 ✓
- ScanContext debt noted → spec only, no action in 1c ✓

**Placeholder scan:** no TBD/TODO. Tasks 2-4 say "the implementer should" with specific requirements rather than verbatim code — this is intentional for these tasks because clippy pedantic + the `Mutex<Connection>` pattern requires adaptations that prior dispatches have shown can't be predicted byte-for-byte. The requirements are specific enough for a skilled implementer.

**Type consistency:** `DetectedVolume` used consistently between fs/volumes.rs and cli/scan.rs. `SqliteVolumeRepository` matches between db/volume_repo.rs and main.rs. `migrate_sentinel_row` on `SqliteFileRepository` consistent between db and scan.rs. `format_size` extracted to `cmd/format.rs` used by both ls.rs and volumes.rs.

**Commit discipline:** three reviewer-gated commits + push/tag at final checkpoint.
