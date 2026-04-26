//! FTS5 ground-truth proptests — randomized tag churn + soft-delete
//! churn against a ground-truth oracle. Validates that `search_content`
//! stays consistent under sequences of (attach/detach/delete/restore)
//! operations. Cases capped per GH #124. Extracted from
//! `crates/db/src/search_repo.rs::tests` in Batch G.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

mod common;

use common::{
    attach_tag_raw, compute_ground_truth, detach_tag_raw, insert_file, read_search_content,
    restore_location, restore_metadata_raw, restore_tag_raw, seed_conn, set_metadata_variant,
    soft_delete_location, soft_delete_metadata, soft_delete_tag_raw, test_db,
};
use perima_core::SearchRepository;

// ---------------------------------------------------------------------------
// Proptest-private constants: tag-churn proptest universe
// ---------------------------------------------------------------------------

/// Small universe: 3 files × 3 tags.
const PROP_FILES: &[&str] = &[
    "7100000000000000000000000000000000000000000000000000000000000000",
    "7200000000000000000000000000000000000000000000000000000000000000",
    "7300000000000000000000000000000000000000000000000000000000000000",
];
const PROP_TAGS: &[&str] = &["alpha", "beta", "gamma"];
const PROP_VOL: &str = "00000000-0000-0000-0000-000000000099";

// ---------------------------------------------------------------------------
// Proptest-private action enums
// ---------------------------------------------------------------------------

/// Operations exercised by the property: Attach or Detach a (file, tag) pair.
#[derive(Debug, Clone)]
enum TagOp {
    Attach(usize, usize),
    Detach(usize, usize),
}

#[derive(Debug, Clone)]
enum SoftOp {
    AttachTag(usize, usize),
    DetachTag(usize, usize),
    SoftDeleteTag(usize),
    RestoreTag(usize),
    SetMetadata(usize, u8),
    SoftDeleteMetadata(usize),
    RestoreMetadata(usize),
    SoftDeleteLocation(usize),
    RestoreLocation(usize),
}

// ---------------------------------------------------------------------------
// Proptest 1: tag-churn invariant
// ---------------------------------------------------------------------------

proptest::proptest! {
    // WHY cases=64 (down from the 256 default): post-Batch-C each proptest
    // case creates a writer-actor thread + `r2d2` read pool + a single
    // `seed_conn` on a fresh tempdir DB — ~5x the per-case cost of the
    // pre-Task-7 single-`Mutex<Connection>` fixture (#124). The seed
    // connection is already hoisted below to case scope so per-op
    // `Connection::open` churn is gone; the residual per-case cost is
    // the writer-thread + pool init itself. At 256 cases the cumulative
    // overhead exceeds the 80s terminate-after window on VM filesystems
    // even though no individual case contends for the write lock. 64
    // cases × up to 30 ops = ~1 920 ops per proptest, still strong
    // combinatorial coverage for FTS trigger invariants.
    #![proptest_config(proptest::test_runner::Config {
        cases: 64,
        ..proptest::test_runner::Config::default()
    })]

    /// **Invariant:** after every Attach / Detach operation, for every
    /// `(file, tag)` pair, `MATCH tag_name` returns the file iff
    /// `file_tags.deleted_at IS NULL` for that pair.
    #[test]
    fn fts_consistent_under_tag_churn(
        ops in proptest::collection::vec(
            proptest::prop_oneof![
                proptest::strategy::Strategy::prop_map(
                    (0..PROP_FILES.len(), 0..PROP_TAGS.len()),
                    |(f, t)| TagOp::Attach(f, t),
                ),
                proptest::strategy::Strategy::prop_map(
                    (0..PROP_FILES.len(), 0..PROP_TAGS.len()),
                    |(f, t)| TagOp::Detach(f, t),
                ),
            ],
            0..30,
        ),
    ) {
        // Fresh DB per proptest case — each case is independent.
        let (_td, db, repo, _writer) = test_db();

        // WHY single seed_conn hoisted to case scope: each `Connection::open`
        // on a WAL file does several syscalls (open, SHARED lock, -shm/-wal
        // handshake, header read). With 30 ops × 256 default cases × 2
        // proptests = ~15k opens, that cost compounded to >80s on VM
        // filesystems (#124). Reusing one connection per case keeps all
        // writes as auto-commit statements — no transaction state crosses
        // ops, so test semantics are identical to the per-op-scope version.
        let conn = seed_conn(&db);

        // Seed all three files.
        for (i, hash) in PROP_FILES.iter().enumerate() {
            insert_file(
                &conn,
                hash,
                PROP_VOL,
                &format!("prop_file_{i}.jpg"),
            );
        }

        let mut attached: std::collections::HashMap<(usize, usize), bool> =
            std::collections::HashMap::new();

        for op in &ops {
            match *op {
                TagOp::Attach(f, t) => {
                    attach_tag_raw(&conn, PROP_FILES[f], PROP_TAGS[t]);
                    attached.insert((f, t), true);
                }
                TagOp::Detach(f, t) => {
                    if *attached.get(&(f, t)).unwrap_or(&false) {
                        detach_tag_raw(&conn, PROP_FILES[f], PROP_TAGS[t]);
                        attached.insert((f, t), false);
                    }
                }
            }

            for (f_idx, &file_hash) in PROP_FILES.iter().enumerate() {
                for (t_idx, &tag_name) in PROP_TAGS.iter().enumerate() {
                    let is_attached =
                        *attached.get(&(f_idx, t_idx)).unwrap_or(&false);
                    let hits = repo
                        .search(tag_name, 50)
                        .expect("proptest search");
                    let found = hits
                        .iter()
                        .any(|h| h.blake3_hash.as_deref() == Some(file_hash));
                    proptest::prop_assert_eq!(
                        found,
                        is_attached,
                        "FTS invariant violated: file={} tag={} \
                         attached={} found={}",
                        file_hash,
                        tag_name,
                        is_attached,
                        found
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Proptest-private constants: soft-delete proptest universe
// ---------------------------------------------------------------------------

const SOFT_FILES: &[&str] = &[
    "a100000000000000000000000000000000000000000000000000000000000000",
    "a200000000000000000000000000000000000000000000000000000000000000",
];
const SOFT_TAGS: &[&str] = &["alpha", "beta"];
const SOFT_VOL: &str = "00000000-0000-0000-0000-0000000000aa";

// ---------------------------------------------------------------------------
// Proptest 2: ground-truth invariant over full soft-delete op universe
// ---------------------------------------------------------------------------

proptest::proptest! {
    // See `fts_consistent_under_tag_churn` for the cases-reduction
    // rationale (#124). This proptest is set to cases=32 (half the other
    // one) because each op runs `compute_ground_truth` — up to 8
    // per-hash SELECTs plus a `read_search_content` scan — on top of the
    // mutation. With 25 ops × 9 queries ≈ 225 DB ops per case, the
    // per-case cost is ~2x the tag-churn proptest.
    #![proptest_config(proptest::test_runner::Config {
        cases: 32,
        ..proptest::test_runner::Config::default()
    })]

    /// **Invariant:** after EVERY op, search_content rows (incrementally
    /// maintained by triggers) must equal the ground-truth rows computed
    /// directly from joined live state via independent per-field subqueries.
    #[test]
    fn fts_matches_ground_truth_under_soft_delete_churn(
        ops in proptest::collection::vec(
            proptest::prop_oneof![
                proptest::strategy::Strategy::prop_map(
                    (0..SOFT_FILES.len(), 0..SOFT_TAGS.len()),
                    |(f, t)| SoftOp::AttachTag(f, t),
                ),
                proptest::strategy::Strategy::prop_map(
                    (0..SOFT_FILES.len(), 0..SOFT_TAGS.len()),
                    |(f, t)| SoftOp::DetachTag(f, t),
                ),
                proptest::strategy::Strategy::prop_map(
                    0..SOFT_TAGS.len(),
                    SoftOp::SoftDeleteTag,
                ),
                proptest::strategy::Strategy::prop_map(
                    0..SOFT_TAGS.len(),
                    SoftOp::RestoreTag,
                ),
                proptest::strategy::Strategy::prop_map(
                    (0..SOFT_FILES.len(), 0u8..4u8),
                    |(f, v)| SoftOp::SetMetadata(f, v),
                ),
                proptest::strategy::Strategy::prop_map(
                    0..SOFT_FILES.len(),
                    SoftOp::SoftDeleteMetadata,
                ),
                proptest::strategy::Strategy::prop_map(
                    0..SOFT_FILES.len(),
                    SoftOp::RestoreMetadata,
                ),
                proptest::strategy::Strategy::prop_map(
                    0..SOFT_FILES.len(),
                    SoftOp::SoftDeleteLocation,
                ),
                proptest::strategy::Strategy::prop_map(
                    0..SOFT_FILES.len(),
                    SoftOp::RestoreLocation,
                ),
            ],
            0..25,
        ),
    ) {
        let (_td, db, _repo, _writer) = test_db();

        // See `fts_consistent_under_tag_churn` for the rationale — one
        // seed_conn per case instead of per-op avoids ~15k extra
        // `Connection::open` calls on the WAL-mode DB file (#124).
        let conn = seed_conn(&db);
        for (i, h) in SOFT_FILES.iter().enumerate() {
            insert_file(&conn, h, SOFT_VOL, &format!("soft_{i}.jpg"));
        }

        for op in &ops {
            match *op {
                SoftOp::AttachTag(f, t) => {
                    attach_tag_raw(&conn, SOFT_FILES[f], SOFT_TAGS[t]);
                }
                SoftOp::DetachTag(f, t) => {
                    detach_tag_raw(&conn, SOFT_FILES[f], SOFT_TAGS[t]);
                }
                SoftOp::SoftDeleteTag(t) => {
                    soft_delete_tag_raw(&conn, SOFT_TAGS[t]);
                }
                SoftOp::RestoreTag(t) => {
                    restore_tag_raw(&conn, SOFT_TAGS[t]);
                }
                SoftOp::SetMetadata(f, v) => {
                    set_metadata_variant(&conn, SOFT_FILES[f], v);
                }
                SoftOp::SoftDeleteMetadata(f) => {
                    soft_delete_metadata(&conn, SOFT_FILES[f]);
                }
                SoftOp::RestoreMetadata(f) => {
                    restore_metadata_raw(&conn, SOFT_FILES[f]);
                }
                SoftOp::SoftDeleteLocation(f) => {
                    soft_delete_location(
                        &conn,
                        SOFT_FILES[f],
                        &format!("soft_{f}.jpg"),
                    );
                }
                SoftOp::RestoreLocation(f) => {
                    restore_location(
                        &conn,
                        SOFT_FILES[f],
                        &format!("soft_{f}.jpg"),
                    );
                }
            }

            let (actual, expected) = (
                read_search_content(&conn),
                compute_ground_truth(&conn),
            );
            proptest::prop_assert_eq!(
                actual,
                expected,
                "search_content drifted from ground truth after op {:?} in sequence {:?}",
                op, ops
            );
        }
    }
}
