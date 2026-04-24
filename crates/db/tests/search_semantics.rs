//! Search behavior tests — query semantics with no trigger-side assertion.
//! Extracted from `crates/db/src/search_repo.rs::tests` in Batch G.

#![allow(clippy::unwrap_used)] // WHY: integration test; unwrap panics signal bugs.

mod common;

use common::{
    HASH_A, VOL, device, hash_n, insert_file, insert_metadata, seed_conn, test_db,
    test_db_with_tag_repo,
};
use perima_core::{BlakeHash, SearchRepository, TagRepository};

#[test]
fn search_empty_index_returns_empty() {
    let (_td, _db, repo, _writer) = test_db();
    let hits = repo.search("vacation", 50).expect("search");
    assert!(hits.is_empty());
}

#[test]
fn search_finds_by_filename() {
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH_A, VOL, "photos/sunset.jpg");
        insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
    }
    repo.rebuild().expect("rebuild");
    let hits = repo.search("sunset", 50).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].blake3_hash, HASH_A);
}

#[test]
fn search_finds_by_mime_type() {
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH_A, VOL, "doc.pdf");
        insert_metadata(&conn, HASH_A, "application/pdf", "", "");
    }
    repo.rebuild().expect("rebuild");
    // FTS5 exact phrase search.
    let hits = repo.search("\"application/pdf\"", 50).expect("search");
    assert_eq!(hits.len(), 1);
}

#[test]
fn search_finds_by_camera_model() {
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH_A, VOL, "img.jpg");
        insert_metadata(&conn, HASH_A, "image/jpeg", "Canon EOS R5", "");
    }
    repo.rebuild().expect("rebuild");
    let hits = repo.search("Canon", 50).expect("search");
    assert_eq!(hits.len(), 1);
}

#[test]
fn search_finds_by_tag() {
    let (_td, db, repo, tag_repo, _writer) = test_db_with_tag_repo();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH_A, VOL, "beach.jpg");
        insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
    }
    let tag = tag_repo
        .upsert_tag("beachlife", device())
        .expect("upsert tag");
    let hash = BlakeHash::parse_hex(HASH_A).expect("hash");
    tag_repo.attach(&hash, tag.id, device()).expect("attach");
    repo.rebuild().expect("rebuild");
    let hits = repo.search("beachlife", 50).expect("search");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].blake3_hash, HASH_A);
}

#[test]
fn search_limit_is_respected() {
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        for i in 0..5u8 {
            let hash = format!("{:0<64}", format!("{i:x}"));
            insert_file(&conn, &hash, VOL, &format!("file{i}.jpg"));
            insert_metadata(&conn, &hash, "image/jpeg", "", "");
        }
    }
    repo.rebuild().expect("rebuild");
    // All 5 files have "jpeg" — limit 2 should return exactly 2.
    let hits = repo.search("jpeg", 2).expect("search");
    assert_eq!(hits.len(), 2);
}

#[test]
fn search_no_results_for_unknown_term() {
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH_A, VOL, "alpha.txt");
        insert_metadata(&conn, HASH_A, "text/plain", "", "");
    }
    repo.rebuild().expect("rebuild");
    let hits = repo
        .search("xyzzy_nonexistent_term_42", 50)
        .expect("search");
    assert!(hits.is_empty());
}

#[test]
fn search_rank_orders_better_match_first() {
    // WHY: plan Task 1 Step 2 required this test — the whole point of
    // FTS5 over LIKE is BM25 ranking. Two files both contain "vacation"
    // in their filename; only one also has the matching TAG attached.
    // BM25 weights multi-field matches higher, so the tagged hit must
    // rank before the filename-only hit. In FTS5 lower rank = better
    // match (SQLite convention; default `rank` returns negative BM25
    // score, smaller = better).
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let (_td, db, repo, tag_repo, _writer) = test_db_with_tag_repo();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH_A, VOL, "vacation_tagged.jpg");
        insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
        insert_file(&conn, HASH_B, VOL, "vacation_only.jpg");
        insert_metadata(&conn, HASH_B, "image/jpeg", "", "");
    }
    // Attach the matching tag only to HASH_A so the BM25 signal is
    // stronger for that row.
    let tag = tag_repo.upsert_tag("vacation", device()).expect("upsert");
    let hash_a = BlakeHash::parse_hex(HASH_A).expect("hash A");
    tag_repo.attach(&hash_a, tag.id, device()).expect("attach");

    repo.rebuild().expect("rebuild");
    let hits = repo.search("vacation", 50).expect("search");
    assert_eq!(hits.len(), 2, "both files should hit on 'vacation'");
    assert_eq!(
        hits[0].blake3_hash,
        HASH_A,
        "tagged file must rank above filename-only file (got order: {:?})",
        hits.iter().map(|h| &h.blake3_hash).collect::<Vec<_>>()
    );
    assert!(
        hits[0].rank <= hits[1].rank,
        "FTS5 BM25 rank must be non-increasing (lower = better); \
         got [0]={}, [1]={}",
        hits[0].rank,
        hits[1].rank
    );
}

#[test]
fn filename_without_slash_is_indexed_correctly() {
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        // Root-level file: no '/' in path.
        insert_file(&conn, HASH_A, VOL, "rootfile.jpg");
        insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
    }
    repo.rebuild().expect("rebuild");
    let hits = repo.search("rootfile", 50).expect("search");
    assert_eq!(hits.len(), 1);
}

#[test]
fn rebuild_is_idempotent() {
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        insert_file(&conn, HASH_A, VOL, "a.jpg");
        insert_metadata(&conn, HASH_A, "image/jpeg", "", "");
    }
    repo.rebuild().expect("rebuild 1");
    repo.rebuild().expect("rebuild 2");
    let hits = repo.search("a", 50).expect("search");
    // "a.jpg" filename contains "a".
    assert!(!hits.is_empty());
    // Exactly one doc (idempotent — no duplicates from double rebuild).
    let count: i64 = {
        let conn = seed_conn(&db);
        conn.query_row("SELECT COUNT(*) FROM search_content", [], |r| r.get(0))
            .expect("count")
    };
    assert_eq!(count, 1);
}

/// I5: calling `SearchRepository::rebuild()` twice produces an identical
/// result set; no row-count drift in `search_content`.
#[test]
fn test_rebuild_idempotence_post_v007() {
    let (_td, db, repo, _writer) = test_db();
    {
        let conn = seed_conn(&db);
        for i in 20u8..23u8 {
            let h = hash_n(i);
            insert_file(&conn, &h, VOL, &format!("idempotent_{i}.jpg"));
            insert_metadata(&conn, &h, "image/jpeg", "", "");
        }
    }
    repo.rebuild().expect("rebuild 1");
    let count_after_first: i64 = {
        let conn = seed_conn(&db);
        conn.query_row("SELECT COUNT(*) FROM search_content", [], |r| r.get(0))
            .expect("count after first rebuild")
    };

    repo.rebuild().expect("rebuild 2");
    let count_after_second: i64 = {
        let conn = seed_conn(&db);
        conn.query_row("SELECT COUNT(*) FROM search_content", [], |r| r.get(0))
            .expect("count after second rebuild")
    };

    assert_eq!(
        count_after_first, count_after_second,
        "I5: search_content row count must be stable across two rebuilds (no drift)"
    );
    assert_eq!(
        count_after_first, 3,
        "I5: exactly 3 rows expected (one per file)"
    );

    let hits_first = repo
        .search("idempotent", 50)
        .expect("search after rebuild 2");
    assert_eq!(
        hits_first.len(),
        3,
        "I5: all 3 files must be discoverable after double rebuild"
    );
}
