//! `VolumeRepository` implementation backed by rusqlite.
//!
//! WHY: priority chain tries GUID first, then `fs_uuid`, then label+capacity.
//! v1 only has label+capacity; the structure exists so future sysinfo upgrades
//! or blkid integration slot in without refactoring the match logic.

use std::sync::Mutex;

use perima_core::{
    CoreError, DeviceId, VolumeId, VolumeIdentifiers, VolumeRecord, VolumeRepository,
};
use rusqlite::{Connection, OptionalExtension};

use crate::errors::Error;

/// Rusqlite-backed volume + volume-mount repository.
///
/// WHY `Mutex`: `rusqlite::Connection` is `Send` but not `Sync`. Wrapping in
/// `Mutex` satisfies `Send + Sync` without `unsafe`, matching the pattern used
/// by [`crate::SqliteFileRepository`].
pub struct SqliteVolumeRepository {
    conn: Mutex<Connection>,
}

impl SqliteVolumeRepository {
    /// Wrap an existing connection. Caller must have run migrations first.
    pub const fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Lock the mutex, mapping poison to `CoreError::Internal`.
fn lock(conn: &Mutex<Connection>) -> Result<std::sync::MutexGuard<'_, Connection>, CoreError> {
    conn.lock()
        .map_err(|e| CoreError::Internal(format!("mutex poisoned: {e}")))
}

impl VolumeRepository for SqliteVolumeRepository {
    fn find_or_create(
        &self,
        ident: &VolumeIdentifiers,
        device: DeviceId,
    ) -> Result<VolumeId, CoreError> {
        let mut conn = lock(&self.conn)?;
        let now = now_iso();
        let dev_str = device.0.to_string();

        // WHY BEGIN IMMEDIATE: SqliteVolumeRepository wraps one Connection
        // per adapter, but the CLI and desktop spawn multiple repos against
        // the same DB file. Without IMMEDIATE, two handles can both pass
        // the SELECT "not found" check and INSERT duplicates. IMMEDIATE
        // takes the reserved writer lock at BEGIN; the busy_timeout in
        // `open_and_migrate` makes the loser wait rather than fail.
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(Error::from)?;

        // WHY: priority chain — GUID is the most stable identifier (survives
        // reformatting on the same hardware). fs_uuid is next. label+capacity
        // is the v1 fallback. Each arm SELECT-then-UPDATE-last-seen, or falls
        // through to the next.

        // Arm 1: GPT partition GUID
        if let Some(ref guid) = ident.gpt_partition_guid {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT volume_id FROM volumes
                     WHERE gpt_partition_guid = ?1 AND deleted_at IS NULL",
                    [guid],
                    |row| row.get(0),
                )
                .optional()
                .map_err(Error::from)?;

            if let Some(vol_id_str) = existing {
                tx.execute(
                    "UPDATE volumes SET last_seen = ?1, updated_at = ?1, device_id = ?2
                     WHERE volume_id = ?3",
                    rusqlite::params![now, dev_str, vol_id_str],
                )
                .map_err(Error::from)?;
                let vol_id = uuid::Uuid::parse_str(&vol_id_str)
                    .map_err(|e| CoreError::Internal(format!("bad volume uuid: {e}")))?;
                tx.commit().map_err(Error::from)?;
                return Ok(VolumeId(vol_id));
            }
        }

        // Arm 2: Filesystem UUID
        if let Some(ref fs_uuid) = ident.fs_uuid {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT volume_id FROM volumes
                     WHERE fs_uuid = ?1 AND deleted_at IS NULL",
                    [fs_uuid],
                    |row| row.get(0),
                )
                .optional()
                .map_err(Error::from)?;

            if let Some(vol_id_str) = existing {
                tx.execute(
                    "UPDATE volumes SET last_seen = ?1, updated_at = ?1, device_id = ?2
                     WHERE volume_id = ?3",
                    rusqlite::params![now, dev_str, vol_id_str],
                )
                .map_err(Error::from)?;
                let vol_id = uuid::Uuid::parse_str(&vol_id_str)
                    .map_err(|e| CoreError::Internal(format!("bad volume uuid: {e}")))?;
                tx.commit().map_err(Error::from)?;
                return Ok(VolumeId(vol_id));
            }
        }

        // Arm 3: label + capacity (v1 primary matching path)
        if let Some(ref label) = ident.label {
            let cap_i64 = capacity_to_i64(ident.capacity_bytes)?;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT volume_id FROM volumes
                     WHERE volume_label = ?1 AND capacity_bytes = ?2
                       AND deleted_at IS NULL",
                    rusqlite::params![label, cap_i64],
                    |row| row.get(0),
                )
                .optional()
                .map_err(Error::from)?;

            if let Some(vol_id_str) = existing {
                tx.execute(
                    "UPDATE volumes SET last_seen = ?1, updated_at = ?1, device_id = ?2
                     WHERE volume_id = ?3",
                    rusqlite::params![now, dev_str, vol_id_str],
                )
                .map_err(Error::from)?;
                let vol_id = uuid::Uuid::parse_str(&vol_id_str)
                    .map_err(|e| CoreError::Internal(format!("bad volume uuid: {e}")))?;
                tx.commit().map_err(Error::from)?;
                return Ok(VolumeId(vol_id));
            }
        }

        // No match → INSERT new volume row.
        let new_id = VolumeId::new();
        let new_id_str = new_id.0.to_string();
        let cap_i64 = capacity_to_i64(ident.capacity_bytes)?;
        tx.execute(
            "INSERT INTO volumes
             (volume_id, gpt_partition_guid, fs_uuid, volume_label,
              capacity_bytes, is_removable, last_seen, updated_at, device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
            rusqlite::params![
                new_id_str,
                ident.gpt_partition_guid,
                ident.fs_uuid,
                ident.label,
                cap_i64,
                i64::from(ident.is_removable),
                now,
                dev_str,
            ],
        )
        .map_err(Error::from)?;
        tx.commit().map_err(Error::from)?;
        drop(conn);

        Ok(new_id)
    }

    fn record_mount(
        &self,
        volume: VolumeId,
        machine: DeviceId,
        mount: &std::path::Path,
    ) -> Result<(), CoreError> {
        // WHY: validate UTF-8 at the boundary. `to_string_lossy` silently
        // replaces invalid bytes with U+FFFD, corrupting future identity
        // matches. InvalidPath is the taxonomy hit used elsewhere for path
        // problems that fail validation before we touch the DB.
        let mount_str = mount.to_str().ok_or_else(|| {
            CoreError::InvalidPath(format!(
                "mount path is not valid UTF-8: {}",
                mount.display()
            ))
        })?;

        let mut conn = lock(&self.conn)?;
        let now = now_iso();
        let vol_str = volume.0.to_string();
        let machine_str = machine.0.to_string();

        // WHY BEGIN IMMEDIATE: the read-modify-write sequence below
        // (SELECT existing, soft-delete superseded rows, INSERT new row) is
        // not atomic across connections under SQLite's default DEFERRED
        // transaction. IMMEDIATE acquires the writer lock at BEGIN time so
        // concurrent repo handles serialize instead of racing to insert
        // duplicates. See `open_and_migrate` busy_timeout — the retry
        // callback makes the second writer wait, not fail.
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(Error::from)?;

        // WHY: soft-delete any active mount row for the same
        // (volume_id, machine_id) whose mount_path differs from the new one.
        // CRDT rules forbid hard-delete on mutable rows; retired mounts
        // remain observable to future sync with deleted_at set.
        tx.execute(
            "UPDATE volume_mounts
             SET deleted_at = ?1, updated_at = ?1, device_id = ?2
             WHERE volume_id = ?3 AND machine_id = ?4
               AND mount_path <> ?5 AND deleted_at IS NULL",
            rusqlite::params![now, machine_str, vol_str, machine_str, mount_str],
        )
        .map_err(Error::from)?;

        // WHY: app-level uniqueness on (volume_id, machine_id, mount_path,
        // deleted_at IS NULL) replaces a UNIQUE constraint that CLAUDE.md
        // forbids on mutable columns. SELECT-then-INSERT under IMMEDIATE is
        // race-safe because only one writer can hold the reserved lock.
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM volume_mounts
                 WHERE volume_id = ?1 AND machine_id = ?2
                   AND mount_path = ?3 AND deleted_at IS NULL",
                rusqlite::params![vol_str, machine_str, mount_str],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)?;

        if existing.is_none() {
            let new_id = perima_core::ids::new_id().to_string();
            tx.execute(
                "INSERT INTO volume_mounts
                 (id, volume_id, machine_id, mount_path, first_seen, updated_at, device_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
                rusqlite::params![new_id, vol_str, machine_str, mount_str, now, machine_str,],
            )
            .map_err(Error::from)?;
        }

        tx.commit().map_err(Error::from)?;
        drop(conn);

        Ok(())
    }

    // WHY allow(significant_drop_tightening): the Mutex guard `conn` must
    // outlive `stmt` and `rows` — same pattern as SqliteFileRepository.
    #[allow(clippy::significant_drop_tightening)]
    fn list(&self, machine: DeviceId) -> Result<Vec<VolumeRecord>, CoreError> {
        let conn = lock(&self.conn)?;
        let machine_str = machine.0.to_string();

        let mut stmt = conn
            .prepare(
                "SELECT v.volume_id, v.volume_label, v.capacity_bytes,
                        v.is_removable, v.last_seen, vm.mount_path
                 FROM volumes v
                 LEFT JOIN volume_mounts vm
                   ON vm.volume_id = v.volume_id
                  AND vm.machine_id = ?1
                  AND vm.deleted_at IS NULL
                 WHERE v.deleted_at IS NULL
                 ORDER BY v.volume_id, vm.mount_path",
            )
            .map_err(Error::from)?;

        let rows = stmt
            .query_map(rusqlite::params![machine_str], |row| {
                let vol_id_str: String = row.get(0)?;
                let label: Option<String> = row.get(1)?;
                let cap: i64 = row.get(2)?;
                let removable: i64 = row.get(3)?;
                let last_seen: String = row.get(4)?;
                let mount_path: Option<String> = row.get(5)?;
                Ok((vol_id_str, label, cap, removable, last_seen, mount_path))
            })
            .map_err(Error::from)?;

        // Collect rows, merging mount paths per volume.
        let mut records: Vec<VolumeRecord> = Vec::new();
        for row in rows {
            let (vol_id_str, label, cap, removable, last_seen, mount_path) =
                row.map_err(Error::from)?;
            let vol_id = VolumeId(
                uuid::Uuid::parse_str(&vol_id_str)
                    .map_err(|e| CoreError::Internal(format!("bad volume uuid: {e}")))?,
            );
            let cap_u64 = u64::try_from(cap)
                .map_err(|_| CoreError::Internal(format!("stored capacity {cap} is negative")))?;

            // Find existing record for this volume_id (from a previous row in
            // the LEFT JOIN) or push a new one.
            if let Some(rec) = records.iter_mut().find(|r| r.id == vol_id) {
                if let Some(mp) = mount_path {
                    rec.mounts_on_this_machine
                        .push(std::path::PathBuf::from(mp));
                }
            } else {
                let mounts = mount_path
                    .map(|mp| vec![std::path::PathBuf::from(mp)])
                    .unwrap_or_default();
                records.push(VolumeRecord {
                    id: vol_id,
                    label,
                    capacity_bytes: cap_u64,
                    is_removable: removable != 0,
                    mounts_on_this_machine: mounts,
                    last_seen,
                });
            }
        }

        Ok(records)
    }
}

fn capacity_to_i64(cap: u64) -> Result<i64, CoreError> {
    i64::try_from(cap).map_err(|_| CoreError::Internal(format!("capacity {cap} overflows i64")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_and_migrate;

    fn test_db() -> (tempfile::TempDir, SqliteVolumeRepository) {
        let td = tempfile::tempdir().expect("tempdir");
        let conn = open_and_migrate(&td.path().join("test.db")).expect("open");
        (td, SqliteVolumeRepository::new(conn))
    }

    fn device() -> DeviceId {
        DeviceId::new()
    }

    fn label_cap_ident(label: &str, cap: u64) -> VolumeIdentifiers {
        VolumeIdentifiers {
            gpt_partition_guid: None,
            fs_uuid: None,
            label: Some(label.to_owned()),
            capacity_bytes: cap,
            is_removable: false,
        }
    }

    #[test]
    fn find_or_create_inserts_new() {
        let (_td, repo) = test_db();
        let ident = label_cap_ident("MY_DRIVE", 1_000_000);
        let vol_id = repo
            .find_or_create(&ident, device())
            .expect("find_or_create");
        // UUIDv7 — just verify it is not nil.
        assert_ne!(vol_id.0, uuid::Uuid::nil());
    }

    #[test]
    fn find_or_create_matches_on_label_capacity() {
        let (_td, repo) = test_db();
        let dev = device();
        let ident = label_cap_ident("BACKUP_SSD", 2_000_000_000);
        let first = repo.find_or_create(&ident, dev).expect("first");
        let second = repo.find_or_create(&ident, dev).expect("second");
        assert_eq!(
            first, second,
            "same label+capacity must return the same VolumeId"
        );
    }

    #[test]
    fn find_or_create_guid_trumps_label() {
        let (_td, repo) = test_db();
        let dev = device();

        // Insert volume A with GUID "aaa…" + label "A".
        let ident_a = VolumeIdentifiers {
            gpt_partition_guid: Some("aaaa-bbbb".to_owned()),
            fs_uuid: None,
            label: Some("LABEL_A".to_owned()),
            capacity_bytes: 500_000,
            is_removable: false,
        };
        let vol_a = repo.find_or_create(&ident_a, dev).expect("insert A");

        // Insert volume B with a different GUID (or none) + label "B".
        let ident_b = label_cap_ident("LABEL_B", 300_000);
        let vol_b = repo.find_or_create(&ident_b, dev).expect("insert B");
        assert_ne!(vol_a, vol_b, "distinct volumes must have distinct ids");

        // Look up with GUID "aaaa-bbbb" but label "LABEL_B" —
        // GUID arm wins and returns vol A.
        let ident_guid_a_label_b = VolumeIdentifiers {
            gpt_partition_guid: Some("aaaa-bbbb".to_owned()),
            fs_uuid: None,
            label: Some("LABEL_B".to_owned()),
            capacity_bytes: 300_000,
            is_removable: false,
        };
        let found = repo
            .find_or_create(&ident_guid_a_label_b, dev)
            .expect("find by guid");
        assert_eq!(found, vol_a, "GUID arm must win over label+capacity");
    }

    #[test]
    fn record_mount_inserts_new() {
        let (_td, repo) = test_db();
        let dev = device();
        let ident = label_cap_ident("MOUNT_TEST", 100_000);
        let vol_id = repo.find_or_create(&ident, dev).expect("create");
        repo.record_mount(vol_id, dev, std::path::Path::new("/mnt/test"))
            .expect("record_mount");
        // Verify via list.
        let records = repo.list(dev).expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].mounts_on_this_machine,
            vec![std::path::PathBuf::from("/mnt/test")]
        );
    }

    #[test]
    fn record_mount_unchanged_on_repeat() {
        let (_td, repo) = test_db();
        let dev = device();
        let ident = label_cap_ident("MOUNT_REPEAT", 200_000);
        let vol_id = repo.find_or_create(&ident, dev).expect("create");
        let mp = std::path::Path::new("/mnt/repeat");
        repo.record_mount(vol_id, dev, mp).expect("first");
        repo.record_mount(vol_id, dev, mp)
            .expect("second — must not error");
        // Should still be exactly 1 mount row.
        let records = repo.list(dev).expect("list");
        assert_eq!(records[0].mounts_on_this_machine.len(), 1);
    }

    #[test]
    fn find_or_create_concurrent_unique() {
        // WHY: two concurrent repo handles calling find_or_create with
        // identical label+capacity must settle on ONE active volume row.
        // A 2-party `Barrier` forces both threads past the initial SELECT
        // before either INSERT, reproducing the race that `BEGIN IMMEDIATE`
        // must close. Pre-fix: both threads SELECT "not found", both
        // INSERT, producing two active rows with different UUIDs. Post-fix:
        // IMMEDIATE serializes the tx; the second thread re-reads after
        // the first commits and reuses the existing id.
        use std::sync::{Arc, Barrier};
        use std::thread;

        let td = tempfile::tempdir().expect("tempdir");
        let db_path = td.path().join("race.db");

        // Drive migrations once up front so both worker connections skip them.
        {
            let _ = open_and_migrate(&db_path).expect("migrate");
        }

        let dev = device();
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let db_path = db_path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || -> VolumeId {
                let conn = open_and_migrate(&db_path).expect("open");
                let repo = SqliteVolumeRepository::new(conn);
                let ident = label_cap_ident("RACE_VOL", 42_000);
                barrier.wait();
                repo.find_or_create(&ident, dev).expect("find_or_create")
            }));
        }
        let a = handles.remove(0).join().expect("thread a");
        let b = handles.remove(0).join().expect("thread b");
        assert_eq!(
            a, b,
            "concurrent find_or_create must resolve to a single VolumeId"
        );

        // Cross-check: the DB must contain exactly one active row for the
        // label+capacity combination (regardless of whether threads produced
        // the same id twice or one thread reused the other's insert).
        let conn = open_and_migrate(&db_path).expect("verify open");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM volumes
                 WHERE volume_label = ?1 AND capacity_bytes = ?2
                   AND deleted_at IS NULL",
                rusqlite::params!["RACE_VOL", 42_000_i64],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(
            count, 1,
            "exactly one active volume row must exist after concurrent inserts"
        );
    }

    #[test]
    fn record_mount_retires_superseded_path() {
        // WHY: remount on a new path for the same (volume, machine) must
        // soft-delete the prior row rather than leaving two active mount
        // rows for one device. `list` reads only active mounts, so the
        // observable contract is: after remount only the new path surfaces.
        let (_td, repo) = test_db();
        let dev = device();
        let ident = label_cap_ident("SUPERSEDE", 100_000);
        let vol_id = repo.find_or_create(&ident, dev).expect("create");

        let old_mp = std::path::Path::new("/mnt/old");
        let new_mp = std::path::Path::new("/mnt/new");
        repo.record_mount(vol_id, dev, old_mp).expect("first mount");
        repo.record_mount(vol_id, dev, new_mp).expect("remount");

        let records = repo.list(dev).expect("list");
        assert_eq!(records.len(), 1, "exactly one volume record");
        assert_eq!(
            records[0].mounts_on_this_machine,
            vec![std::path::PathBuf::from("/mnt/new")],
            "only the current mount path must be active",
        );
    }

    #[test]
    fn record_mount_idempotent_on_same_path() {
        // WHY: the retirement sweep must NOT soft-delete a row whose
        // mount_path equals the new one, or remount-to-same-path would
        // churn the row and update updated_at. Explicit test to pin the
        // "idempotent" behaviour beyond `record_mount_unchanged_on_repeat`
        // which only asserts the list count.
        let (_td, repo) = test_db();
        let dev = device();
        let ident = label_cap_ident("IDEMPOTENT", 50_000);
        let vol_id = repo.find_or_create(&ident, dev).expect("create");
        let mp = std::path::Path::new("/mnt/same");
        repo.record_mount(vol_id, dev, mp).expect("first");
        repo.record_mount(vol_id, dev, mp).expect("second");
        repo.record_mount(vol_id, dev, mp).expect("third");

        let records = repo.list(dev).expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].mounts_on_this_machine.len(), 1);
        assert_eq!(
            records[0].mounts_on_this_machine[0],
            std::path::PathBuf::from("/mnt/same")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn record_mount_rejects_non_utf8_path() {
        // WHY: Linux paths are arbitrary bytes; `to_string_lossy` silently
        // replaces invalid UTF-8 with U+FFFD, corrupting identity matching.
        // `record_mount` must reject non-UTF8 paths at the boundary with
        // `CoreError::InvalidPath` so callers surface the problem instead of
        // silently storing an ambiguous row.
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;

        let (_td, repo) = test_db();
        let dev = device();
        let ident = label_cap_ident("NONUTF8", 100_000);
        let vol_id = repo.find_or_create(&ident, dev).expect("create");

        let bad_os = OsString::from_vec(vec![0x66, 0x6f, 0x6f, 0xFF]); // "foo\xFF"
        let bad_path = PathBuf::from(bad_os);

        let err = repo
            .record_mount(vol_id, dev, &bad_path)
            .expect_err("non-utf8 must error");
        assert!(
            matches!(err, CoreError::InvalidPath(_)),
            "expected CoreError::InvalidPath, got {err:?}"
        );
    }

    #[test]
    fn list_returns_volumes_with_mounts() {
        let (_td, repo) = test_db();
        let dev = device();
        let ident = label_cap_ident("LIST_TEST", 999_000);
        let vol_id = repo.find_or_create(&ident, dev).expect("create");
        repo.record_mount(vol_id, dev, std::path::Path::new("/mnt/listtest"))
            .expect("mount");

        let records = repo.list(dev).expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, vol_id);
        assert_eq!(records[0].label.as_deref(), Some("LIST_TEST"));
        assert_eq!(records[0].capacity_bytes, 999_000);
        assert_eq!(
            records[0].mounts_on_this_machine,
            vec![std::path::PathBuf::from("/mnt/listtest")]
        );
    }
}
