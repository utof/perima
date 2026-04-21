//! `VolumeUseCase` — orchestrates volume list and mount-recording.
//!
//! This is the `crates/app` port of the volume orchestration that previously
//! lived in `crates/cli/src/cmd/volumes.rs` (54 LOC) and
//! `crates/desktop/src/commands.rs::{list_volumes_inner, scan}`
//! (`record_mount` call sites).
//!
//! Zero generics: dependency ports are carried as `Arc<dyn Port>` fields;
//! a single `async fn execute(&self, cmd: VolumeCommand) ->
//! Result<VolumeOutput, CoreError>` exposes both operations.
//!
//! # Why two commands (not one)
//!
//! `List` (read-only, no side-effects) and `RecordMount` (write, CRDT-relevant)
//! have different callers: `list_volumes` is a Tauri query and CLI display;
//! `RecordMount` is called during scan startup when a volume is detected.
//! Folding them into one trait would conflate read + write concerns. Two
//! commands in one `UseCase` is the right grain — same repository, distinct
//! intent.
//!
//! # `DeviceId` on `RecordMount`
//!
//! `VolumeRepository::record_mount` takes a `DeviceId` (the "machine" that
//! observed the mount). The CLAUDE.md CRDT-prep rule (`updated_at +
//! device_id` on every mutable row) requires the caller to supply the
//! originating device. Carrying it per-command (not as a constructor field)
//! is consistent with `TagCommand::Attach` + `Detach` — callers know the
//! device context at call site, not at construction time.
//!
//! See [`VolumeUseCase::execute`] for the workflow body.

use std::{path::PathBuf, sync::Arc};

use perima_core::{CoreError, DeviceId, EventBus, VolumeId, VolumeRecord, VolumeRepository};

/// Inputs to [`VolumeUseCase::execute`].
#[derive(Debug, Clone)]
pub enum VolumeCommand {
    /// List all volumes seen on `device`, including their current mount paths.
    List {
        /// Device (machine) whose mount records should be included.
        ///
        /// WHY per-command (not constructor): the same `VolumeUseCase`
        /// instance may be called from contexts that differ by device (e.g.
        /// multi-device CLI tooling). Keeping device here avoids a mutable
        /// state change on the struct and matches `RecordMount`'s device
        /// semantics.
        device: DeviceId,
    },

    /// Record that `volume_id` was mounted at `path` on `device`.
    ///
    /// Idempotent: re-recording the same `(volume_id, device, path)` triple
    /// is a no-op at the DB level.
    RecordMount {
        /// The volume being mounted.
        volume_id: VolumeId,
        /// Absolute path to the volume's mount point on the local machine.
        path: PathBuf,
        /// Device (machine) that observed this mount.
        ///
        /// WHY required: `volume_mounts` rows carry `device_id` for CRDT
        /// scope isolation — each machine maintains its own mount-path
        /// history independently of other machines in the library.
        device: DeviceId,
    },
}

/// Output of a successful volume operation.
#[derive(Debug, Clone)]
pub enum VolumeOutput {
    /// Response to [`VolumeCommand::List`] — all volumes for the device.
    Volumes(Vec<VolumeRecord>),

    /// Response to [`VolumeCommand::RecordMount`] — the id of the volume
    /// whose mount was recorded.
    Recorded(VolumeId),
}

/// Orchestrator: volume list and mount-recording.
///
/// Dependencies are carried as `Arc<dyn Port>` fields; there are zero
/// generic parameters on the struct itself. See [`VolumeUseCase::execute`]
/// for the workflow body.
pub struct VolumeUseCase {
    volumes: Arc<dyn VolumeRepository>,
    // WHY `events` is held but unused in the orchestration body today:
    // Batch E will emit `VolumeEvent::MountRecorded` from here once the
    // async-broadcast bus lands. Holding the handle at construction makes
    // the Batch-E diff a single-file addition rather than a signature churn
    // across every caller. The field is silenced below with a
    // `_ = &self.events` one-liner (preferred zero-cost form — no refcount
    // increment on each call, unlike `Arc::clone`).
    events: Arc<dyn EventBus>,
}

impl std::fmt::Debug for VolumeUseCase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VolumeUseCase").finish_non_exhaustive()
    }
}

impl VolumeUseCase {
    /// Construct a `VolumeUseCase` with the given dependency ports.
    ///
    /// The container (Task 7) calls this once and shares the resulting
    /// `Arc<VolumeUseCase>` across surfaces.
    #[must_use]
    pub fn new(volumes: Arc<dyn VolumeRepository>, events: Arc<dyn EventBus>) -> Self {
        Self { volumes, events }
    }

    /// Execute the volume command.
    ///
    /// # Errors
    /// - [`CoreError::Internal`] on `SQLite` failures from the repository.
    // WHY allow unused_async: `VolumeRepository` methods are synchronous
    // today; the `async fn` signature is mandated by the UseCase contract so
    // the Batch-C connection-actor swap (async write channel) can evolve the
    // impl without touching callers. Removing `async` now would force a
    // caller-side churn when the trait gains async variants.
    #[allow(clippy::unused_async)]
    pub async fn execute(&self, cmd: VolumeCommand) -> Result<VolumeOutput, CoreError> {
        // WHY touch self.events: held for the Batch-E event-emit path;
        // reference the field so `unused` lints don't fire before Batch E
        // wires the emissions.
        let _ = &self.events;

        match cmd {
            VolumeCommand::List { device } => {
                let records = self.volumes.list(device)?;
                Ok(VolumeOutput::Volumes(records))
            }

            VolumeCommand::RecordMount {
                volume_id,
                path,
                device,
            } => {
                self.volumes.record_mount(volume_id, device, &path)?;
                Ok(VolumeOutput::Recorded(volume_id))
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code; unwrap panics signal bugs.
mod tests {
    use perima_core::{DeviceId, FileEvent, VolumeIdentifiers};
    use perima_db::{SqliteVolumeRepository, open_and_migrate};
    use tempfile::TempDir;

    use super::*;

    /// No-op event bus for tests that don't care about emissions.
    struct NullBus;
    impl EventBus for NullBus {
        fn emit(&self, _event: &FileEvent) -> Result<(), CoreError> {
            Ok(())
        }
    }

    /// Build a [`VolumeUseCase`] backed by a real `SQLite` DB in a tempdir.
    ///
    /// WHY single harness: every test uses this helper so setup is
    /// consistent and the `TempDir` lifetime is managed uniformly.
    fn harness() -> (VolumeUseCase, TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("perima.db");
        let conn = open_and_migrate(&db_path).unwrap();
        let volumes: Arc<dyn VolumeRepository> = Arc::new(SqliteVolumeRepository::new(conn));
        let events: Arc<dyn EventBus> = Arc::new(NullBus);
        (VolumeUseCase::new(volumes, events), tmp)
    }

    fn device() -> DeviceId {
        DeviceId::new()
    }

    /// A minimal `VolumeIdentifiers` suitable for seeding test volumes.
    fn test_ident(label: &str) -> VolumeIdentifiers {
        VolumeIdentifiers {
            gpt_partition_guid: None,
            fs_uuid: Some(format!("test-uuid-{label}")),
            label: Some(label.to_owned()),
            capacity_bytes: 1_000_000,
            is_removable: false,
        }
    }

    // -----------------------------------------------------------------------
    // List
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn list_returns_volumes_after_mount() {
        let (uc, tmp) = harness();
        let dev = device();

        // Seed: find_or_create a volume then record a mount for it.
        // WHY use the repo directly for seeding: `VolumeCommand` doesn't
        // expose `find_or_create` (scan startup concern), so we seed via the
        // underlying repository as other integration tests do.
        let db_path = tmp.path().join("perima.db");
        let seed_conn = open_and_migrate(&db_path).unwrap();
        let seed_repo = SqliteVolumeRepository::new(seed_conn);
        let ident = test_ident("TestVol");
        let vol_id = seed_repo.find_or_create(&ident, dev).unwrap();
        seed_repo
            .record_mount(vol_id, dev, std::path::Path::new("/mnt/test"))
            .unwrap();

        let out = uc
            .execute(VolumeCommand::List { device: dev })
            .await
            .unwrap();
        let VolumeOutput::Volumes(records) = out else {
            panic!("expected VolumeOutput::Volumes");
        };
        assert!(
            !records.is_empty(),
            "expected at least one volume after mount"
        );
        assert_eq!(records[0].id, vol_id);
    }

    // -----------------------------------------------------------------------
    // RecordMount idempotency
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn record_mount_is_idempotent() {
        let (uc, tmp) = harness();
        let dev = device();
        let mount_path = PathBuf::from("/mnt/idempotent");

        // Seed volume via raw repo (find_or_create is a scan startup concern).
        let db_path = tmp.path().join("perima.db");
        let seed_conn = open_and_migrate(&db_path).unwrap();
        let seed_repo = SqliteVolumeRepository::new(seed_conn);
        let ident = test_ident("IdempotentVol");
        let vol_id = seed_repo.find_or_create(&ident, dev).unwrap();

        // Record the same mount twice via the UseCase.
        let out1 = uc
            .execute(VolumeCommand::RecordMount {
                volume_id: vol_id,
                path: mount_path.clone(),
                device: dev,
            })
            .await
            .unwrap();
        let out2 = uc
            .execute(VolumeCommand::RecordMount {
                volume_id: vol_id,
                path: mount_path.clone(),
                device: dev,
            })
            .await
            .unwrap();

        // Both calls must return Recorded with the same volume_id.
        assert!(
            matches!(out1, VolumeOutput::Recorded(id) if id == vol_id),
            "first RecordMount should return Recorded(vol_id)"
        );
        assert!(
            matches!(out2, VolumeOutput::Recorded(id) if id == vol_id),
            "second RecordMount should return Recorded(vol_id)"
        );

        // After two record_mount calls, list should still return exactly 1 volume.
        let list_out = uc
            .execute(VolumeCommand::List { device: dev })
            .await
            .unwrap();
        let VolumeOutput::Volumes(records) = list_out else {
            panic!("expected VolumeOutput::Volumes");
        };
        assert_eq!(
            records.len(),
            1,
            "idempotent record_mount should not create duplicate volume rows"
        );
        // The single volume should have exactly one mount path entry.
        assert_eq!(
            records[0].mounts_on_this_machine.len(),
            1,
            "idempotent record_mount should not duplicate mount paths"
        );
    }
}
