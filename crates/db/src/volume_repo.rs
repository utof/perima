//! `VolumeRepository` adapter — writer-actor + read-pool backed.
//!
//! Post-Batch-C Task 2. The struct holds two cheap-to-clone handles:
//! a [`flume::Sender<WriteCmd>`] connected to the single writer actor
//! (spec §3.1) and a [`ReadPool`] of read-only `r2d2_sqlite`
//! connections (spec §3.4). Writes build a `WriteCmd` variant with a
//! `flume::bounded(1)` reply channel and block on the reply. Reads run
//! SQL directly against a pooled connection.
//!
//! No `Mutex<Connection>`. The legacy `::new(conn)` constructor is
//! deleted; every caller now supplies `(writer_sender, read_pool)`.

use std::path::Path;

use flume::Sender;
use perima_core::{
    CoreError, DeviceId, VolumeId, VolumeIdentifiers, VolumeRecord, VolumeRepository,
};

use crate::cmd::{VolumeWriteCmd, WriteCmd};
use crate::errors::Error;
use crate::pool::ReadPool;

/// Writer-actor + read-pool backed volume + volume-mount repository.
///
/// Cheap to [`Clone`]: both fields (`flume::Sender`, `ReadPool`) are
/// internally refcounted.
#[derive(Clone)]
pub struct SqliteVolumeRepository {
    writer: Sender<WriteCmd>,
    reads: ReadPool,
}

impl std::fmt::Debug for SqliteVolumeRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteVolumeRepository")
            .finish_non_exhaustive()
    }
}

impl SqliteVolumeRepository {
    /// Construct an adapter from a writer-command sender + a read pool.
    ///
    /// WHY no migration run here: migrations happen exactly once inside
    /// [`crate::SqliteWriter::start`] BEFORE the writer thread spawns
    /// (spec §3.6). The read pool is opened after migrations complete.
    #[must_use]
    pub const fn new(writer: Sender<WriteCmd>, reads: ReadPool) -> Self {
        Self { writer, reads }
    }
}

impl VolumeRepository for SqliteVolumeRepository {
    fn find_or_create(
        &self,
        ident: &VolumeIdentifiers,
        device: DeviceId,
    ) -> Result<VolumeId, CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<VolumeId, CoreError>>(1);
        self.writer
            .send(WriteCmd::Volume(VolumeWriteCmd::FindOrCreate {
                identifiers: ident.clone(),
                device,
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
    }

    fn record_mount(
        &self,
        volume: VolumeId,
        machine: DeviceId,
        mount: &Path,
    ) -> Result<(), CoreError> {
        let (reply_tx, reply_rx) = flume::bounded::<Result<(), CoreError>>(1);
        self.writer
            .send(WriteCmd::Volume(VolumeWriteCmd::RecordMount {
                volume,
                device: machine,
                mount: mount.to_path_buf(),
                reply: reply_tx,
            }))
            .map_err(|e| CoreError::Internal(format!("writer send: {e}")))?;
        reply_rx
            .recv()
            .map_err(|e| CoreError::Internal(format!("writer recv: {e}")))?
    }

    fn list(&self, machine: DeviceId) -> Result<Vec<VolumeRecord>, CoreError> {
        // WHY a pool checkout here (no writer hop): `list` is a pure
        // SELECT. Reads go directly through the `r2d2_sqlite` pool
        // (spec §3.5). `PooledConnection` derefs to `rusqlite::Connection`,
        // so the SQL body is lifted verbatim from the pre-Batch-C impl.
        let conn = self.reads.get()?;
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

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.
mod tests {
    use std::sync::Arc;

    use perima_core::{EventBus, FileEvent};
    use tempfile::TempDir;

    use super::*;
    use crate::pool::ReadPool;
    use crate::writer::{SqliteWriter, SqliteWriterHandle};

    /// No-op event bus used by writer-backed test fixtures.
    struct NoopBus;
    impl EventBus for NoopBus {
        fn emit(&self, _: &FileEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }

    /// Test harness: tempdir-backed DB, writer actor, read pool, repo.
    ///
    /// WHY tempfile-on-disk (not in-memory): the writer actor opens its
    /// connection via `Connection::open_in_memory()` in the test-only
    /// `start_in_memory` helper, which is PER-CONNECTION private memory.
    /// A separate read pool can't see that memory DB. A tempfile-backed
    /// DB lets writer + pool share the same file; WAL mode keeps both
    /// sides cheap.
    fn test_db() -> (TempDir, SqliteVolumeRepository, SqliteWriterHandle) {
        let td = tempfile::tempdir().expect("tempdir");
        let db_path = td.path().join("test.db");
        let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
        let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
        let reads = ReadPool::open(&db_path).expect("pool open");
        let repo = SqliteVolumeRepository::new(writer.sender(), reads);
        (td, repo, writer)
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
        let (_td, repo, _writer) = test_db();
        let ident = label_cap_ident("MY_DRIVE", 1_000_000);
        let vol_id = repo
            .find_or_create(&ident, device())
            .expect("find_or_create");
        // UUIDv7 — just verify it is not nil.
        assert_ne!(vol_id.0, uuid::Uuid::nil());
    }

    #[test]
    fn find_or_create_matches_on_label_capacity() {
        let (_td, repo, _writer) = test_db();
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
        let (_td, repo, _writer) = test_db();
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
        let (_td, repo, _writer) = test_db();
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
        let (_td, repo, _writer) = test_db();
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
    fn record_mount_retires_superseded_path() {
        // WHY: remount on a new path for the same (volume, machine) must
        // soft-delete the prior row rather than leaving two active mount
        // rows for one device. `list` reads only active mounts, so the
        // observable contract is: after remount only the new path surfaces.
        let (_td, repo, _writer) = test_db();
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
        // churn the row and update updated_at.
        let (_td, repo, _writer) = test_db();
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
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;

        let (_td, repo, _writer) = test_db();
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
        let (_td, repo, _writer) = test_db();
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

    #[test]
    fn find_or_create_concurrent_unique() {
        // WHY: two concurrent adapter HANDLES (same writer+pool, cloned)
        // calling find_or_create with identical label+capacity must
        // settle on ONE active volume row. Under the writer actor this
        // is guaranteed by single-threaded serialization — the test
        // still covers the observable behaviour contract.
        use std::sync::{Arc as ArcStd, Barrier};
        use std::thread;

        let (_td, repo, _writer) = test_db();
        let dev = device();

        let repo = ArcStd::new(repo);
        let barrier = ArcStd::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let repo = ArcStd::clone(&repo);
            let barrier = ArcStd::clone(&barrier);
            handles.push(thread::spawn(move || -> VolumeId {
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

        // Cross-check: exactly one active row for label+capacity.
        let records = repo.list(dev).expect("list");
        let matching = records
            .iter()
            .filter(|r| r.label.as_deref() == Some("RACE_VOL") && r.capacity_bytes == 42_000)
            .count();
        assert_eq!(
            matching, 1,
            "exactly one active volume row must exist after concurrent inserts"
        );
    }
}
