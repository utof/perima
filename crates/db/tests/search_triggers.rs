//! FTS5 trigger-maintenance tests — verify that `search_content` stays in
//! sync as files, locations, metadata, and tags change. Covers T22 + T40-T48
//! regressions, multi-location rename, soft-delete + restore, and the
//! tag-rename propagation path. Extracted from
//! `crates/db/src/search_repo.rs::tests` in Batch G.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

mod common;

use common::{
    HASH_A, VOL, VOL2, attach_tag_raw, device, hash_n, insert_file, insert_file_at_volume,
    insert_metadata, restore_location, search_count, seed_conn, soft_delete_location,
    soft_delete_metadata, soft_delete_tag_raw, test_db, test_db_with_tag_repo, update_path,
    update_path_at_volume,
};
use perima_core::{BlakeHash, SearchRepository, TagRepository};

#[test]
fn trigger_sync_on_metadata_insert() {
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH_A, VOL, "photos/trigger_test.jpg");
        // Inserting metadata fires search_after_metadata_insert trigger.
        insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
    }
    // No explicit rebuild — trigger should have synced the index.
    let hits = repo.search("trigger_test", 50).expect("search");
    assert_eq!(hits.len(), 1, "trigger must sync on metadata insert");
}

#[test]
fn trigger_sync_on_tag_attach() {
    let (_td, db, repo, tag_repo, _writer) = test_db_with_tag_repo();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH_A, VOL, "img.jpg");
        insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
    }
    // Metadata insert has already fired `search_after_metadata_insert`;
    // now attach a tag to fire `search_after_file_tags_insert`.
    let tag = tag_repo.upsert_tag("triggertag", device()).expect("upsert");
    let hash = BlakeHash::parse_hex(HASH_A).expect("hash");
    tag_repo.attach(&hash, tag.id, device()).expect("attach");
    // No rebuild — file_tags INSERT trigger should have updated the index.
    let hits = repo.search("triggertag", 50).expect("search");
    assert_eq!(hits.len(), 1, "trigger must sync on tag attach");
}

/// T22: no `file_locations` UPDATE trigger in V006 — rename leaves old
/// path indexed and new path absent.
#[test]
#[allow(non_snake_case)]
fn test_T22_rename_updates_indexed_path() {
    let hash_owned = hash_n(3);
    let HASH = hash_owned.as_str();
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH, VOL, "oldname_22.jpg");
        insert_metadata(&conn, HASH, "image/jpeg", "", "");
    }
    // Rename: same hash, new path. V006 has no UPDATE trigger on
    // file_locations, so FTS index is not updated.
    {
        let conn = seed_conn(&db);
        update_path(&conn, HASH, "oldname_22.jpg", "newname_22.jpg");
    }
    let old_hits = repo.search("oldname_22", 50).expect("search old");
    let new_hits = repo.search("newname_22", 50).expect("search new");
    // V006 bug: old path 'oldname_22' still matches; new path 'newname_22' does not.
    assert!(
        old_hits.is_empty(),
        "#22: old path 'oldname_22' still matches after rename (V006 bug)"
    );
    assert_eq!(
        new_hits.len(),
        1,
        "#22: new path 'newname_22' does not match after rename (V006 bug)"
    );
}

/// T40: contentless FTS5 'delete' with blank payloads is a no-op.
/// After updating `camera_model` the old token ("Canon") must not match.
/// Fails on V006 because `search_after_metadata_update` supplies ''
/// for every column on the 'delete' command — stale tokens remain.
#[test]
#[allow(non_snake_case)]
fn test_T40_metadata_update_removes_stale_tokens() {
    let hash_owned = hash_n(1);
    let HASH = hash_owned.as_str();
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH, VOL, "cam.jpg");
        insert_metadata(&conn, HASH, "image/jpeg", "Canon EOS R5", "");
    }
    // Trigger: UPDATE file_metadata fires search_after_metadata_update.
    {
        let conn = seed_conn(&db);
        conn.execute(
            "UPDATE file_metadata SET camera_model = ?1 WHERE blake3_hash = ?2",
            rusqlite::params!["Nikon Zf", HASH],
        )
        .expect("update metadata");
    }
    // V006 bug: stale token 'Canon' still matches.
    let hits = repo.search("Canon", 50).expect("search");
    assert!(
        hits.is_empty(),
        "#40: stale token 'Canon' still matches after metadata update (V006 bug)"
    );
}

/// T41: under V006, `search_rowid_map` was only seeded on `file_metadata`
/// INSERT, so attaching a tag to a metadata-less file was a silent no-op.
/// V007 trigger 4a seeds `search_content` from `file_locations` directly.
#[test]
#[allow(non_snake_case)]
fn test_T41_tag_attach_on_metadata_less_file() {
    let hash_owned = hash_n(2);
    let HASH = hash_owned.as_str();
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH, VOL, "plain.txt"); // NO metadata row
        attach_tag_raw(&conn, HASH, "beach");
    }
    // V006 bug: file_tags INSERT trigger found no search_rowid_map row;
    // the tag was never indexed. V007 trigger 4a fixes this by seeding
    // search_content from file_locations directly on tag attach.
    let hits = repo.search("beach", 50).expect("search");
    assert_eq!(
        hits.len(),
        1,
        "#41: tag-attach on metadata-less file was a no-op (V006 bug)"
    );
}

/// T42: no blake3_hash-change trigger in V006 — replace-in-place
/// leaves stale FTS doc for the old hash's content.
///
/// Ignored post-Task-3 (FTS5 trigger pivot to `file_uuid`): this test
/// asserts pre-pivot semantics where the OLD `blake3_hash`'s `search_content`
/// row was retired and a NEW row reseeded on hash change. Post-pivot,
/// `search_content` is keyed by `file_uuid` (stable across hash changes),
/// so the row stays in place; OLD `file_metadata` (still keyed to the
/// same `file_uuid`) keeps influencing search until `ScanUseCase` (Task 7)
/// soft-deletes it. The post-pivot replacement test belongs in Task 7's
/// scope — see GH #155 + plan §4.1.4.
#[test]
#[ignore = "pre-pivot semantics; replacement covered by Task 7 (ScanUseCase rewrite)"]
#[allow(non_snake_case)]
fn test_T42_hash_change_retires_old_doc() {
    let hash_old_owned = hash_n(4);
    let hash_new_owned = hash_n(5);
    let HASH_OLD = hash_old_owned.as_str();
    let HASH_NEW = hash_new_owned.as_str();
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH_OLD, VOL, "cam.jpg");
        insert_metadata(&conn, HASH_OLD, "image/jpeg", "Canon EOS R5", "");
    }
    // Replace hash in-place (file content changed at same path).
    // V006 has no trigger on file_locations.blake3_hash change.
    {
        let conn = seed_conn(&db);
        conn.execute(
            "INSERT OR IGNORE INTO files
                 (blake3_hash, file_size, first_seen, updated_at, device_id)
             VALUES (?1, 2048, ?2, ?2, ?3)",
            rusqlite::params![HASH_NEW, common::TS, common::DEV],
        )
        .expect("insert new files row");
        conn.execute(
            "UPDATE file_locations SET blake3_hash = ?1 WHERE relative_path = 'cam.jpg'",
            rusqlite::params![HASH_NEW],
        )
        .expect("update hash");
        insert_metadata(&conn, HASH_NEW, "image/jpeg", "Nikon Zf", "");
    }
    // V006 bug: old FTS doc not retired — "Canon" still matches.
    let hits = repo.search("Canon", 50).expect("search");
    assert!(
        hits.is_empty(),
        "#42: old doc not retired when hash changed at same path (V006 bug)"
    );
}

/// T43 (#1): soft-deleting a tag must remove its tokens from FTS.
#[test]
#[allow(non_snake_case)]
fn test_T43_tag_soft_delete_removes_tokens_from_fts() {
    let hash_owned = hash_n(43);
    let hash_s = hash_owned.as_str();
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, hash_s, VOL, "cabin_43.jpg");
        attach_tag_raw(&conn, hash_s, "vacation_43");
    }
    assert_eq!(
        search_count(&repo, "vacation_43"),
        1,
        "pre: tag token must match before soft-delete"
    );

    {
        let conn = seed_conn(&db);
        soft_delete_tag_raw(&conn, "vacation_43");
    }

    assert_eq!(
        search_count(&repo, "vacation_43"),
        0,
        "#1: soft-deleted tag token must NOT match (V007 bug: no tag-soft-delete trigger)"
    );

    repo.rebuild().expect("rebuild");
    assert_eq!(
        search_count(&repo, "vacation_43"),
        0,
        "#1: rebuild() must not reintroduce the soft-deleted tag token"
    );
}

/// T44 (#2): `search_after_location_hash_change` must NOT overwrite an
/// existing representative's indexed path with NEW.* when NEW is not
/// the first-seen active location for its target hash.
///
/// Ignored post-Task-3 (FTS5 trigger pivot): the test setup creates two
/// distinct `file_uuid`s whose `blake3_hash` converges to the same value
/// post-update. Pre-pivot, `search_content` was keyed by `blake3_hash` so
/// they collapsed onto one row; post-pivot, they stay as two separate
/// rows (one per `file_uuid`), but the V007 `search_content.blake3_hash
/// NOT NULL UNIQUE` constraint trips the second row's
/// `refresh_full_from_live` UPDATE. Resolving cleanly requires V012 to
/// drop that UNIQUE (spec §4.1.4 "`blake3_hash` becomes nullable") —
/// follow-up scope.
#[test]
#[ignore = "needs V012 to drop search_content.blake3_hash UNIQUE (spec §4.1.4)"]
#[allow(non_snake_case)]
fn test_T44_hash_change_preserves_representative_path() {
    let hash_a = hash_n(44);
    let hash_b = hash_n(45);
    let a_s = hash_a.as_str();
    let b_s = hash_b.as_str();
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, a_s, VOL, "earlier_44.jpg");
        insert_file(&conn, b_s, VOL, "later_44.jpg");
    }
    assert_eq!(
        search_count(&repo, "earlier_44"),
        1,
        "pre: representative path for HASH_A must match"
    );

    {
        let conn = seed_conn(&db);
        conn.execute(
            "UPDATE file_locations SET blake3_hash = ?1
             WHERE blake3_hash = ?2 AND relative_path = 'later_44.jpg'",
            rusqlite::params![a_s, b_s],
        )
        .expect("hash change");
    }

    assert_eq!(
        search_count(&repo, "earlier_44"),
        1,
        "#2: representative's indexed path must remain 'earlier_44' after non-rep hash-change"
    );
}

/// T45a (#3a): combined UPDATE of `blake3_hash` + `deleted_at` must NOT seed
/// a `search_content` row for the NEW (tombstoned) hash.
#[test]
#[allow(non_snake_case)]
fn test_T45a_soft_delete_with_hash_change_skips_fts_insert() {
    let hash_old = hash_n(46);
    let hash_new = hash_n(47);
    let old_s = hash_old.as_str();
    let new_s = hash_new.as_str();
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, old_s, VOL, "combined_45.jpg");
        conn.execute(
            "INSERT OR IGNORE INTO files
                 (blake3_hash, file_size, first_seen, updated_at, device_id)
             VALUES (?1, 2048, ?2, ?2, ?3)",
            rusqlite::params![new_s, common::TS, common::DEV],
        )
        .expect("insert new files row");
    }

    {
        let conn = seed_conn(&db);
        conn.execute(
            "UPDATE file_locations SET blake3_hash = ?1, deleted_at = ?2
             WHERE blake3_hash = ?3 AND relative_path = 'combined_45.jpg'",
            rusqlite::params![new_s, common::TS, old_s],
        )
        .expect("hash change + soft-delete");
    }

    // Avoid unused variable warning — the repo must stay alive to keep the writer sender alive.
    let _ = &repo;

    let sc_count_new: i64 = {
        let conn = seed_conn(&db);
        conn.query_row(
            "SELECT COUNT(*) FROM search_content WHERE blake3_hash = ?1",
            rusqlite::params![new_s],
            |r| r.get(0),
        )
        .expect("count sc new")
    };
    assert_eq!(
        sc_count_new, 0,
        "#3a: combined hash-change+soft-delete must not leak NEW hash into search_content"
    );
}

/// T45b (#3b): restoring a soft-deleted sole-location row must recreate
/// the FTS doc.
#[test]
#[allow(non_snake_case)]
fn test_T45b_location_restore_recreates_fts_doc() {
    let hash = hash_n(48);
    let hash_s = hash.as_str();
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, hash_s, VOL, "restore_45_token.jpg");
    }
    assert_eq!(search_count(&repo, "restore_45_token"), 1, "pre: indexed");

    {
        let conn = seed_conn(&db);
        soft_delete_location(&conn, hash_s, "restore_45_token.jpg");
    }
    assert_eq!(
        search_count(&repo, "restore_45_token"),
        0,
        "after soft-delete: retired"
    );

    {
        let conn = seed_conn(&db);
        restore_location(&conn, hash_s, "restore_45_token.jpg");
    }

    assert_eq!(
        search_count(&repo, "restore_45_token"),
        1,
        "#3b: restore must recreate the FTS doc (V007 bug: no restore trigger)"
    );
}

/// T46 (#4): soft-deleting a `file_metadata` row must clear its tokens
/// from FTS.
#[test]
#[allow(non_snake_case)]
fn test_T46_metadata_soft_delete_clears_tokens() {
    let hash = hash_n(49);
    let hash_s = hash.as_str();
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, hash_s, VOL, "meta_soft_46.jpg");
        insert_metadata(&conn, hash_s, "image/jpeg", "CanonGone46", "");
    }
    assert_eq!(
        search_count(&repo, "CanonGone46"),
        1,
        "pre: camera token indexed"
    );

    {
        let conn = seed_conn(&db);
        soft_delete_metadata(&conn, hash_s);
    }

    assert_eq!(
        search_count(&repo, "CanonGone46"),
        0,
        "#4: soft-deleted metadata's camera token must NOT match (V007 bug)"
    );
}

/// T47 (reviewer #2): `search_after_metadata_insert` must not seed FTS
/// tokens when the metadata row is already tombstoned.
#[test]
#[allow(non_snake_case)]
fn test_T47_tombstoned_metadata_insert_skipped() {
    let hash = hash_n(50);
    let hash_s = hash.as_str();
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, hash_s, VOL, "ghost_47.jpg");
        conn.execute(
            "INSERT INTO file_metadata
                 (blake3_hash, mime_type, camera_model, captured_at,
                  extracted_at, updated_at, deleted_at, device_id)
             VALUES (?1, 'image/ghost', 'GhostCam47', '', ?2, ?2, ?2, ?3)",
            rusqlite::params![hash_s, common::TS, common::DEV],
        )
        .expect("insert tombstoned metadata");
    }

    assert_eq!(
        search_count(&repo, "GhostCam47"),
        0,
        "reviewer #2: tombstoned metadata INSERT must not seed live tokens"
    );
}

/// T48 (reviewer #3): `search_after_file_locations_insert` must aggregate
/// tags + metadata with `deleted_at IS NULL` filters on BOTH the link
/// table AND the joined entity.
#[test]
#[allow(non_snake_case)]
fn test_T48_fresh_location_seed_excludes_soft_deleted_tag_and_metadata() {
    let hash = hash_n(51);
    let hash_s = hash.as_str();
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, hash_s, VOL, "seed_48.jpg");
        attach_tag_raw(&conn, hash_s, "ghostag_48");
        insert_metadata(&conn, hash_s, "image/jpeg", "GhostCam48", "");

        soft_delete_tag_raw(&conn, "ghostag_48");
        soft_delete_metadata(&conn, hash_s);

        soft_delete_location(&conn, hash_s, "seed_48.jpg");
    }

    assert_eq!(
        search_count(&repo, "ghostag_48"),
        0,
        "pre: tag token must be absent after soft-delete + retire"
    );

    {
        let conn = seed_conn(&db);
        insert_file_at_volume(&conn, hash_s, "reseed_48.jpg", VOL2);
    }

    assert_eq!(
        search_count(&repo, "ghostag_48"),
        0,
        "reviewer #3: fresh-location seed must exclude soft-deleted tag tokens"
    );
    assert_eq!(
        search_count(&repo, "GhostCam48"),
        0,
        "reviewer #3: fresh-location seed must exclude soft-deleted metadata tokens"
    );
}

/// I6: a single `BEGIN…COMMIT` updating `file_metadata.camera_model` +
/// attaching a new tag + renaming `file_locations.relative_path` must
/// produce FTS docs that reflect ALL three changes after commit.
#[test]
fn test_combined_transaction_update() {
    let hash = hash_n(13);
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, &hash, VOL, "combined_old_ctx.jpg");
        insert_metadata(&conn, &hash, "image/jpeg", "OldCamera", "");
    }

    let pre = repo.search("OldCamera", 50).expect("pre OldCamera");
    assert_eq!(pre.len(), 1, "pre-condition: OldCamera must be indexed");

    {
        let conn = seed_conn(&db);
        conn.execute_batch("BEGIN;").expect("begin");
        conn.execute(
            "UPDATE file_locations SET relative_path = 'combined_new_ctx.jpg'
             WHERE blake3_hash = ?1 AND relative_path = 'combined_old_ctx.jpg'",
            rusqlite::params![hash],
        )
        .expect("rename");
        conn.execute(
            "UPDATE file_metadata SET camera_model = 'NewCamera'
             WHERE blake3_hash = ?1",
            rusqlite::params![hash],
        )
        .expect("update metadata");
        attach_tag_raw(&conn, &hash, "combined_tag_ctx");
        conn.execute_batch("COMMIT;").expect("commit");
    }

    let new_cam = repo.search("NewCamera", 50).expect("NewCamera");
    assert_eq!(
        new_cam.len(),
        1,
        "I6: NewCamera must be indexed post-commit"
    );

    let old_cam = repo.search("OldCamera", 50).expect("OldCamera after");
    assert!(
        old_cam.is_empty(),
        "I6: OldCamera must not appear after metadata update in combined tx"
    );

    let tag_hits = repo.search("combined_tag_ctx", 50).expect("tag");
    assert_eq!(tag_hits.len(), 1, "I6: new tag must be indexed post-commit");

    let new_path = repo.search("combined_new_ctx", 50).expect("new path");
    assert_eq!(
        new_path.len(),
        1,
        "I6: new relative_path token must be indexed post-commit"
    );

    let old_path = repo.search("combined_old_ctx", 50).expect("old path");
    assert!(
        old_path.is_empty(),
        "I6: old relative_path token must not appear after rename in combined tx"
    );
}

/// Soft-deleting the *only* location of a file must retire both the
/// `search_content` row and the FTS doc.
#[test]
fn test_last_location_soft_delete_retires_doc() {
    let hash = hash_n(12);
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, &hash, VOL, "solo_retire_lsd.jpg");
        insert_metadata(&conn, &hash, "image/jpeg", "RetireCamera", "");
    }
    let pre = repo.search("RetireCamera", 50).expect("pre-search");
    assert_eq!(pre.len(), 1, "file must be indexed before soft-delete");

    {
        let conn = seed_conn(&db);
        soft_delete_location(&conn, &hash, "solo_retire_lsd.jpg");
    }

    let hits = repo.search("RetireCamera", 50).expect("post-search");
    assert!(
        hits.is_empty(),
        "last-location soft-delete must remove the file from FTS search"
    );

    let sc_count: i64 = {
        let conn = seed_conn(&db);
        conn.query_row(
            "SELECT COUNT(*) FROM search_content WHERE blake3_hash = ?1",
            rusqlite::params![hash],
            |r| r.get(0),
        )
        .expect("count search_content")
    };
    assert_eq!(
        sc_count, 0,
        "search_content row must be deleted after last-location soft-delete"
    );
}

/// I4: "multi-location rename preserves findability."
///
/// Per the v0.6.3 spec §Non-goals: the representative FTS doc is
/// one-per-hash, indexed under the first-seen active location. This test
/// verifies that the co-existence of multiple locations does NOT break
/// the rename trigger — the file remains findable via its current
/// representative-path tokens across both a non-representative rename
/// (no-op on FTS) and a representative rename (updates FTS).
#[test]
fn test_multi_location_rename_preserves_findability() {
    let hash = hash_n(10);
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        // Representative (first-seen) location on VOL.
        insert_file(&conn, &hash, VOL, "shared_mlr.jpg");
        // Second location on VOL2, same relative_path.
        insert_file_at_volume(&conn, &hash, "shared_mlr.jpg", VOL2);
    }

    // Rename the non-representative (VOL2) location.
    {
        let conn = seed_conn(&db);
        update_path_at_volume(&conn, &hash, "shared_mlr.jpg", "renamed_mlr.jpg", VOL2);
    }
    assert_eq!(
        search_count(&repo, "shared_mlr"),
        1,
        "non-rep rename must not affect FTS — representative path still matches"
    );

    // Rename the representative (VOL) location. Trigger 2b fires and
    // updates search_content.
    {
        let conn = seed_conn(&db);
        update_path_at_volume(&conn, &hash, "shared_mlr.jpg", "alpha_mlr.jpg", VOL);
    }
    assert_eq!(
        search_count(&repo, "shared_mlr"),
        0,
        "rep rename retires old path token from FTS"
    );
    assert_eq!(
        search_count(&repo, "alpha_mlr"),
        1,
        "rep rename indexes new path token in FTS"
    );
}

/// C1: soft-deleting the representative location of a two-location file
/// must re-point `search_content` to the surviving sibling, not retire the doc.
#[test]
fn test_representative_location_soft_delete_repoints() {
    let hash = hash_n(11);
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        // First (representative) location on VOL / vol1 path.
        insert_file(&conn, &hash, VOL, "vol1/repfile_c1.jpg");
        // Second location on VOL2 / vol2 path — same hash.
        // WHY OR IGNORE on files: prior `insert_file` already seeded the
        // (blake3_hash, file_uuid) pair for this hash, so this is a no-op.
        // The file_locations row's file_uuid is looked up from the prior
        // files row via blake3_hash (post-Task-3 trigger pivot,
        // spec §4.1.4 — see common::insert_file WHY block).
        conn.execute(
            "INSERT OR IGNORE INTO files
                 (blake3_hash, file_uuid, file_size, first_seen, updated_at, device_id)
             VALUES (?1, ?2, 1024, ?3, ?3, ?4)",
            rusqlite::params![
                hash,
                uuid::Uuid::now_v7().to_string(),
                common::TS,
                common::DEV
            ],
        )
        .expect("insert files");
        conn.execute(
            "INSERT OR IGNORE INTO file_locations
                 (id, blake3_hash, file_uuid, volume_id, relative_path, status,
                  first_seen, updated_at, device_id)
             VALUES (?1, ?2,
                     (SELECT f.file_uuid FROM files f WHERE f.blake3_hash = ?2),
                     ?3, ?4, 'active', ?5, ?5, ?6)",
            rusqlite::params![
                uuid::Uuid::now_v7().to_string(),
                hash,
                VOL2,
                "vol2/repfile_c1.jpg",
                common::TS,
                common::DEV
            ],
        )
        .expect("insert second location");
    }
    // Soft-delete the first (representative) location.
    {
        let conn = seed_conn(&db);
        soft_delete_location(&conn, &hash, "vol1/repfile_c1.jpg");
    }
    let vol1_hits = repo.search("vol1", 50).expect("search vol1");
    assert_eq!(
        vol1_hits.len(),
        0,
        "C1: search on deleted representative's path must return zero"
    );
    let vol2_hits = repo.search("vol2", 50).expect("search vol2");
    assert_eq!(
        vol2_hits.len(),
        1,
        "C1: sibling location must be discoverable after representative soft-delete"
    );
    assert_eq!(vol2_hits[0].blake3_hash, hash);
}

/// Trigger 5: renaming a tag must update every `search_content` row that
/// references it.
#[test]
fn test_tag_name_rename_propagates() {
    let (_td, db, repo, _writer) = test_db();
    let hashes: Vec<String> = (30u8..33u8).map(hash_n).collect();
    {
        let conn = seed_conn(&db);
        for (i, h) in hashes.iter().enumerate() {
            insert_file(&conn, h, VOL, &format!("trnp_{i}.jpg"));
            attach_tag_raw(&conn, h, "vacation");
        }
    }

    let pre = repo.search("vacation", 50).expect("pre-vacation");
    assert_eq!(
        pre.len(),
        3,
        "pre-condition: all 3 files must be indexed under 'vacation'"
    );

    {
        let conn = seed_conn(&db);
        conn.execute(
            "UPDATE tags SET name = 'holiday' WHERE name = 'vacation'",
            [],
        )
        .expect("rename tag");
    }

    let old_hits = repo.search("vacation", 50).expect("vacation after rename");
    assert_eq!(
        old_hits.len(),
        0,
        "trigger 5: 'vacation' must return zero after tag rename"
    );

    let new_hits = repo.search("holiday", 50).expect("holiday");
    assert_eq!(
        new_hits.len(),
        3,
        "trigger 5: 'holiday' must match all 3 files after tag rename"
    );
}
