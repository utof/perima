#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

//! Property test: random sequences of `file_locations` ops (insert /
//! `update_hash` / `update_path` / `soft_delete` / restore) interleaved
//! with tag attach / detach preserve FTS5-trigger ↔ ground-truth
//! equivalence.
//!
//! Closes V008 #2 (hash-change retire/seed split — `_retire`/`_seed`
//! triggers must agree on which hash owns which `search_content` row)
//! and V008 #3b (`file_locations` restore must recreate the FTS doc
//! after a soft-delete).
//!
//! Complements the two `search_repo.rs` proptests: tag-churn focus
//! (`fts_consistent_under_tag_churn`, 64 cases) and the soft-delete
//! ground-truth oracle (`fts_matches_ground_truth_under_soft_delete_churn`,
//! 32 cases). This one specifically rotates a slot's hash through a
//! shadow-pair (`h_i_a` ↔ `h_i_b`) to flush hash-change retire/seed
//! triggers densely.
//!
//! Capped at 32 cases per GH #124 (per-case writer-spawn cost).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use perima_core::EventBus;
use perima_db::{ReadPool, SqliteSearchRepository, SqliteWriter, test_utils::NoopBus};
use proptest::prelude::*;
use rusqlite::Connection;

const DEV: &str = "dev";
const TS: &str = "2026-01-01T00:00:00Z";
const VOL: &str = "00000000-0000-0000-0000-0000000000bb";

// WHY small alphabet: 3 slots × 2 shadow hashes × 3 paths × 3 tags keeps
// collisions dense, so a 20-op sequence is very likely to hit each
// trigger body multiple times. Larger alphabets push the test toward
// "no collisions, no interesting cases". `SLOTS` is referenced from
// the strategy + Model + ground-truth projection — bumping it requires
// matching changes there AND verifying `shadow_hash`'s slot encoding
// still fits in a single byte (slot < 128).
const SLOTS: usize = 3;
const PATHS: &[&str] = &["pathzero.jpg", "pathone.jpg", "pathtwo.jpg"];
const TAGS: &[&str] = &["tagone", "tagtwo", "tagthree"];

/// Build the 6-element shadow hash table: slot `i` ∈ {0,1,2} has two
/// shadow hashes `h_{i}_a` and `h_{i}_b`. `UpdateHash` toggles between
/// them so the hash-change trigger fires without merging slots.
fn shadow_hash(slot: usize, is_b: bool) -> String {
    // 64-hex chars: first byte encodes (slot, is_b), rest are '0'.
    debug_assert!(slot < 128, "shadow_hash slot encoding overflows past 127");
    let high = u8::try_from(slot).unwrap() * 2 + u8::from(is_b);
    format!("{high:02x}{}", "0".repeat(62))
}

/// Open a direct raw connection for seeding. Mirrors `common::seed_conn`.
///
/// WHY raw connection: each op below is a single autocommit `UPDATE` /
/// `INSERT` that exercises an FTS trigger in isolation. The writer
/// actor is idle (blocked on its `flume` channel) while we seed, so a
/// second WAL connection does not contend. Per GH #131 the upstream
/// `unixClose` lock-order inversion was fixed in `SQLite` 3.51.2+ (we
/// ship 3.51.3 via rusqlite 0.39); the longer-term writer-routed
/// rewrite is tracked under #124.
#[allow(clippy::disallowed_methods)] // WHY: see fn doc.
fn seed_conn(db_path: &Path) -> Connection {
    Connection::open(db_path).expect("seed conn open")
}

/// Pre-create both shadow `files` rows for every slot so `UpdateHash`
/// never trips an FK (none enforced today, but keeps semantics close to
/// production where `files` rows always pre-exist their `file_locations`).
fn seed_files_table(conn: &Connection) {
    for slot in 0..SLOTS {
        for is_b in [false, true] {
            let h = shadow_hash(slot, is_b);
            conn.execute(
                "INSERT OR IGNORE INTO files
                     (blake3_hash, file_size, first_seen, updated_at, device_id)
                 VALUES (?1, 1024, ?2, ?2, ?3)",
                rusqlite::params![h, TS, DEV],
            )
            .expect("insert files");
        }
    }
}

/// Insert a single `file_locations` row for `(hash, path)`. Idempotent
/// via `INSERT OR IGNORE`.
fn insert_location(conn: &Connection, hash: &str, path: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO file_locations
             (id, blake3_hash, volume_id, relative_path, status,
              first_seen, updated_at, device_id)
         VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, ?6)",
        rusqlite::params![uuid::Uuid::now_v7().to_string(), hash, VOL, path, TS, DEV],
    )
    .expect("insert file_location");
}

/// Toggle a slot's hash from `from_hash` → `to_hash` on its existing
/// `file_locations` row. Only one row matches in our model.
fn update_location_hash(conn: &Connection, from_hash: &str, to_hash: &str) {
    conn.execute(
        "UPDATE file_locations SET blake3_hash = ?1, updated_at = ?2
         WHERE blake3_hash = ?3",
        rusqlite::params![to_hash, TS, from_hash],
    )
    .expect("update_location_hash");
}

/// Rename the location row for `hash` to `new_path`.
fn update_location_path(conn: &Connection, hash: &str, new_path: &str) {
    conn.execute(
        "UPDATE file_locations SET relative_path = ?1, updated_at = ?2
         WHERE blake3_hash = ?3",
        rusqlite::params![new_path, TS, hash],
    )
    .expect("update_location_path");
}

/// Soft-delete the location row for `hash`.
fn soft_delete_location(conn: &Connection, hash: &str) {
    conn.execute(
        "UPDATE file_locations SET deleted_at = ?1, updated_at = ?1
         WHERE blake3_hash = ?2 AND deleted_at IS NULL",
        rusqlite::params![TS, hash],
    )
    .expect("soft_delete_location");
}

/// Restore (clear `deleted_at` on) the location row for `hash`.
fn restore_location(conn: &Connection, hash: &str) {
    conn.execute(
        "UPDATE file_locations SET deleted_at = NULL, updated_at = ?1
         WHERE blake3_hash = ?2",
        rusqlite::params![TS, hash],
    )
    .expect("restore_location");
}

/// Attach `tag_name` to `hash` (creating the `tags` row if needed).
/// Idempotent via `INSERT OR IGNORE`.
fn attach_tag_raw(conn: &Connection, hash: &str, tag_name: &str) {
    conn.execute(
        "INSERT OR IGNORE INTO tags
             (id, name, first_seen, updated_at, device_id)
         VALUES (?1, ?2, ?3, ?3, ?4)",
        rusqlite::params![uuid::Uuid::now_v7().to_string(), tag_name, TS, DEV],
    )
    .expect("insert tag");
    let tag_id: String = conn
        .query_row(
            "SELECT id FROM tags WHERE name = ?1",
            rusqlite::params![tag_name],
            |r| r.get(0),
        )
        .expect("get tag id");
    // Use INSERT OR IGNORE on (blake3_hash, tag_id) — but file_tags has
    // its own UUID PK, so we instead UPSERT-by-soft-delete: if a row
    // already exists we restore it; else insert fresh. Idempotency
    // mirrors the model's `tags_by_hash[hash].insert(tag_idx)` (a
    // BTreeSet insert that no-ops on existing membership), which is
    // load-bearing for the proptest invariant.
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM file_tags
             WHERE blake3_hash = ?1 AND tag_id = ?2",
            rusqlite::params![hash, tag_id],
            |r| r.get(0),
        )
        .ok();
    if let Some(id) = existing {
        conn.execute(
            "UPDATE file_tags SET deleted_at = NULL, updated_at = ?1
             WHERE id = ?2",
            rusqlite::params![TS, id],
        )
        .expect("restore file_tag");
    } else {
        conn.execute(
            "INSERT INTO file_tags
                 (id, blake3_hash, tag_id, first_seen, updated_at, device_id)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            rusqlite::params![uuid::Uuid::now_v7().to_string(), hash, tag_id, TS, DEV],
        )
        .expect("insert file_tag");
    }
}

/// Soft-delete the `(hash, tag_name)` `file_tags` link.
fn detach_tag_raw(conn: &Connection, hash: &str, tag_name: &str) {
    conn.execute(
        "UPDATE file_tags SET deleted_at = ?1, updated_at = ?1, device_id = ?2
         WHERE blake3_hash = ?3
           AND tag_id = (SELECT id FROM tags WHERE name = ?4 AND deleted_at IS NULL)
           AND deleted_at IS NULL",
        rusqlite::params![TS, DEV, hash, tag_name],
    )
    .expect("detach_tag_raw");
}

/// Build a tempfile-on-disk DB + writer + read pool + search repo.
/// Mirrors `common::test_db`.
fn test_db() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    SqliteSearchRepository,
    perima_db::SqliteWriterHandle,
) {
    let td = tempfile::tempdir().expect("tempdir");
    let db_path = td.path().join("test.db");
    let bus: Arc<dyn EventBus> = Arc::new(NoopBus);
    let writer = SqliteWriter::start(&db_path, bus).expect("writer start");
    let reads = ReadPool::open(&db_path).expect("pool open");
    let repo = SqliteSearchRepository::new(writer.sender(), reads);
    (td, db_path, repo, writer)
}

/// One row of `search_content` reduced to the (hash, path, tags) triple
/// the FTS search exposes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GroundRow {
    blake3_hash: String,
    relative_path: String,
    tags: BTreeSet<String>,
}

/// Read live `search_content` and project to `GroundRow`s keyed by hash.
fn read_search_content(conn: &Connection) -> BTreeMap<String, GroundRow> {
    let mut stmt = conn
        .prepare("SELECT blake3_hash, relative_path, tags FROM search_content")
        .expect("prepare sc");
    stmt.query_map([], |r| {
        let hash: String = r.get(0)?;
        let path: String = r.get(1)?;
        let tags_raw: String = r.get(2)?;
        let tags: BTreeSet<String> = tags_raw.split_whitespace().map(str::to_owned).collect();
        Ok((
            hash.clone(),
            GroundRow {
                blake3_hash: hash,
                relative_path: path,
                tags,
            },
        ))
    })
    .expect("query sc")
    .filter_map(Result::ok)
    .collect()
}

/// Per-slot location state. Tracks which shadow hash the slot currently
/// owns + the path + soft-delete flag. Tags are tracked SEPARATELY by
/// hash (see `Model::tags_by_hash`) because `UpdateHash` only renames
/// the `file_locations.blake3_hash` column — it does NOT migrate the
/// `file_tags` rows, which remain keyed to the OLD hash. Carrying tags
/// across a hash-change in the ground truth is the bug that surfaced
/// the first time we ran this proptest.
#[derive(Debug, Clone, Default)]
struct SlotState {
    inserted: bool,
    current_is_b: bool,
    path_idx: usize,
    deleted: bool,
}

impl SlotState {
    fn current_hash(&self, slot: usize) -> String {
        shadow_hash(slot, self.current_is_b)
    }

    const fn live(&self) -> bool {
        self.inserted && !self.deleted
    }
}

/// Full Rust-side ground-truth model.
#[derive(Debug, Clone, Default)]
struct Model {
    slots: [SlotState; SLOTS],
    /// `hash → set of attached (non-soft-deleted) tag indices`. Keyed by
    /// the concrete shadow hash (NOT the slot) so `UpdateHash` produces
    /// the correct "new hash has zero tags until re-attached" outcome.
    tags_by_hash: BTreeMap<String, BTreeSet<usize>>,
}

/// Compute the expected `search_content` rowset from the Rust-side model.
fn expected_rows(model: &Model) -> BTreeMap<String, GroundRow> {
    let mut out = BTreeMap::new();
    for (i, s) in model.slots.iter().enumerate() {
        if s.live() {
            let hash = s.current_hash(i);
            let path = PATHS[s.path_idx].to_owned();
            let tags: BTreeSet<String> = model
                .tags_by_hash
                .get(&hash)
                .map(|set| set.iter().map(|&t| TAGS[t].to_owned()).collect())
                .unwrap_or_default();
            out.insert(
                hash.clone(),
                GroundRow {
                    blake3_hash: hash,
                    relative_path: path,
                    tags,
                },
            );
        }
    }
    out
}

#[derive(Debug, Clone)]
enum LocOp {
    Insert { slot: usize, path_idx: usize },
    UpdateHash { slot: usize },
    UpdatePath { slot: usize, new_path_idx: usize },
    SoftDelete { slot: usize },
    Restore { slot: usize },
    AttachTag { slot: usize, tag_idx: usize },
    DetachTag { slot: usize, tag_idx: usize },
}

fn loc_op_strategy() -> impl Strategy<Value = LocOp> {
    prop_oneof![
        (0..SLOTS, 0..PATHS.len()).prop_map(|(slot, path_idx)| LocOp::Insert { slot, path_idx }),
        (0..SLOTS).prop_map(|slot| LocOp::UpdateHash { slot }),
        (0..SLOTS, 0..PATHS.len())
            .prop_map(|(slot, new_path_idx)| LocOp::UpdatePath { slot, new_path_idx }),
        (0..SLOTS).prop_map(|slot| LocOp::SoftDelete { slot }),
        (0..SLOTS).prop_map(|slot| LocOp::Restore { slot }),
        (0..SLOTS, 0..TAGS.len()).prop_map(|(slot, tag_idx)| LocOp::AttachTag { slot, tag_idx }),
        (0..SLOTS, 0..TAGS.len()).prop_map(|(slot, tag_idx)| LocOp::DetachTag { slot, tag_idx }),
    ]
}

proptest! {
    // WHY cases=32: each case spawns a writer thread + r2d2 read pool +
    // one seed connection on a fresh tempdir DB (#124, same accounting
    // as the soft-delete proptest in `search_repo.rs`). 32 cases × up
    // to 20 ops = ~640 ops, dense enough to flush every trigger body
    // via the small-alphabet collision rate.
    #![proptest_config(ProptestConfig {
        cases: 32,
        ..ProptestConfig::default()
    })]

    /// **Invariant:** after every op, the live `search_content` rowset
    /// (incrementally maintained by FTS5 triggers) equals the ground
    /// truth derived from the Rust-side `SlotState` model.
    #[test]
    fn fts_consistent_under_hash_change_and_restore(
        ops in proptest::collection::vec(loc_op_strategy(), 1..20),
    ) {
        let (_td, db, _repo, _writer) = test_db();

        // Single seed connection per case (matches the canonical
        // proptest's GH #124 cost-amortization pattern).
        let conn = seed_conn(&db);
        seed_files_table(&conn);

        let mut model = Model::default();

        for op in &ops {
            apply_op(&conn, &mut model, op);

            let actual = read_search_content(&conn);
            let expected = expected_rows(&model);
            proptest::prop_assert_eq!(
                &actual,
                &expected,
                "search_content drifted from ground truth after op {:?} \
                 in sequence {:?}",
                op, ops
            );
        }
    }
}

/// Apply one op to BOTH the `SQLite` DB (via the relevant trigger-firing
/// SQL) AND the Rust-side ground-truth `Model`. Skipped ops (no-ops
/// against current state) are skipped on both sides identically — this
/// keeps the model in lock-step with the trigger-maintained index.
fn apply_op(conn: &Connection, model: &mut Model, op: &LocOp) {
    match *op {
        LocOp::Insert { slot, path_idx } => {
            if model.slots[slot].inserted {
                // Already inserted — pure no-op (ground truth + DB
                // INSERT OR IGNORE both skip).
                return;
            }
            let hash = shadow_hash(slot, false);
            insert_location(conn, &hash, PATHS[path_idx]);
            model.slots[slot] = SlotState {
                inserted: true,
                current_is_b: false,
                path_idx,
                deleted: false,
            };
        }
        LocOp::UpdateHash { slot } => {
            if !model.slots[slot].inserted {
                return;
            }
            let from = model.slots[slot].current_hash(slot);
            let to = shadow_hash(slot, !model.slots[slot].current_is_b);
            update_location_hash(conn, &from, &to);
            model.slots[slot].current_is_b = !model.slots[slot].current_is_b;
            // WHY no tag rebind: triggers only see file_locations changes;
            // file_tags rows still reference the old hash (= now have no
            // matching live file_locations row), so the new hash's
            // search_content row gets zero tags until re-attached. That
            // matches the by-hash tag map, no manual rebind needed.
        }
        LocOp::UpdatePath { slot, new_path_idx } => {
            if !model.slots[slot].inserted {
                return;
            }
            let hash = model.slots[slot].current_hash(slot);
            update_location_path(conn, &hash, PATHS[new_path_idx]);
            model.slots[slot].path_idx = new_path_idx;
        }
        LocOp::SoftDelete { slot } => {
            if !model.slots[slot].inserted || model.slots[slot].deleted {
                return;
            }
            let hash = model.slots[slot].current_hash(slot);
            soft_delete_location(conn, &hash);
            model.slots[slot].deleted = true;
        }
        LocOp::Restore { slot } => {
            if !model.slots[slot].inserted || !model.slots[slot].deleted {
                return;
            }
            let hash = model.slots[slot].current_hash(slot);
            restore_location(conn, &hash);
            model.slots[slot].deleted = false;
        }
        LocOp::AttachTag { slot, tag_idx } => {
            if !model.slots[slot].inserted {
                return;
            }
            let hash = model.slots[slot].current_hash(slot);
            attach_tag_raw(conn, &hash, TAGS[tag_idx]);
            model.tags_by_hash.entry(hash).or_default().insert(tag_idx);
        }
        LocOp::DetachTag { slot, tag_idx } => {
            if !model.slots[slot].inserted {
                return;
            }
            let hash = model.slots[slot].current_hash(slot);
            let attached = model
                .tags_by_hash
                .get(&hash)
                .is_some_and(|set| set.contains(&tag_idx));
            if !attached {
                return;
            }
            detach_tag_raw(conn, &hash, TAGS[tag_idx]);
            if let Some(set) = model.tags_by_hash.get_mut(&hash) {
                set.remove(&tag_idx);
            }
        }
    }
}
