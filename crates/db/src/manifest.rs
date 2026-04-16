//! Per-volume manifest writer.
//!
//! WHY: manifest uses `INSERT OR REPLACE` (not the main DB's app-level
//! uniqueness pattern) because the manifest is a local recovery dump, not a
//! CRDT-replicated table. Simplicity wins here.

use std::path::Path;

use perima_core::{CoreError, HashedFile, VolumeId};

/// Write (or update) `.perima/manifest.db` at `volume_root`.
///
/// Creates the `.perima/` directory if missing, then opens (or creates) the
/// manifest database and upserts all metadata + file rows.  If the write fails
/// (e.g. read-only volume), a warning is logged and `Ok(())` is returned —
/// the manifest is a convenience, not a hard requirement.
///
/// # Errors
///
/// This function only returns `Err` on internal logic failures unrelated to
/// filesystem permission or space issues; those are swallowed as warnings.
pub fn write_manifest(
    volume_root: &Path,
    volume_id: VolumeId,
    files: &[HashedFile],
) -> Result<(), CoreError> {
    let perima_dir = volume_root.join(".perima");
    if let Err(e) = std::fs::create_dir_all(&perima_dir) {
        tracing::warn!(
            path = %perima_dir.display(),
            error = %e,
            "cannot create .perima dir; skipping manifest write"
        );
        return Ok(());
    }

    let db_path = perima_dir.join("manifest.db");
    let conn = match rusqlite::Connection::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                path = %db_path.display(),
                error = %e,
                "cannot open manifest.db; skipping manifest write"
            );
            return Ok(());
        }
    };

    let result = write_manifest_inner(&conn, volume_id, files);
    if let Err(e) = result {
        tracing::warn!(
            path = %db_path.display(),
            error = %e,
            "manifest write failed; continuing without manifest"
        );
    }
    Ok(())
}

fn write_manifest_inner(
    conn: &rusqlite::Connection,
    volume_id: VolumeId,
    files: &[HashedFile],
) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS manifest_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS manifest_files (
            blake3_hash   TEXT PRIMARY KEY,
            file_size     INTEGER NOT NULL,
            relative_path TEXT NOT NULL,
            first_seen    TEXT NOT NULL,
            updated_at    TEXT NOT NULL
         );",
    )?;

    let now = chrono::Utc::now().to_rfc3339();
    let vol_id_str = volume_id.0.to_string();

    // Upsert meta rows.
    conn.execute(
        "INSERT OR REPLACE INTO manifest_meta (key, value) VALUES ('volume_id', ?1)",
        [&vol_id_str],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO manifest_meta (key, value) VALUES ('manifest_version', '1')",
        [],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO manifest_meta (key, value) VALUES ('created_at', ?1)",
        [&now],
    )?;

    // Upsert file rows.
    for file in files {
        let hash_hex = file.hash.to_hex();
        let size = i64::try_from(file.discovered.size.0).unwrap_or(i64::MAX);
        let rel_path = file.discovered.relative_path.as_str();
        conn.execute(
            "INSERT OR REPLACE INTO manifest_files
             (blake3_hash, file_size, relative_path, first_seen, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            rusqlite::params![hash_hex, size, rel_path, now],
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use perima_core::{BlakeHash, DiscoveredFile, FileSize, MediaPath};

    fn sample_file(content: &[u8], path: &str) -> HashedFile {
        HashedFile {
            discovered: DiscoveredFile {
                absolute_path: PathBuf::from("/tmp/fake"),
                relative_path: MediaPath::new(path),
                size: FileSize(content.len() as u64),
            },
            hash: BlakeHash::from_bytes(*blake3::hash(content).as_bytes()),
        }
    }

    #[test]
    fn write_manifest_creates_db() {
        let td = tempfile::tempdir().expect("tempdir");
        let vol_id = VolumeId::new();
        write_manifest(td.path(), vol_id, &[]).expect("write_manifest");

        let db_path = td.path().join(".perima/manifest.db");
        assert!(db_path.exists(), "manifest.db must be created");

        let conn = rusqlite::Connection::open(&db_path).expect("open");
        let vol_str: String = conn
            .query_row(
                "SELECT value FROM manifest_meta WHERE key = 'volume_id'",
                [],
                |row| row.get(0),
            )
            .expect("volume_id meta row");
        assert_eq!(vol_str, vol_id.0.to_string());

        let ver: String = conn
            .query_row(
                "SELECT value FROM manifest_meta WHERE key = 'manifest_version'",
                [],
                |row| row.get(0),
            )
            .expect("manifest_version meta row");
        assert_eq!(ver, "1");
    }

    #[test]
    fn write_manifest_writes_files() {
        let td = tempfile::tempdir().expect("tempdir");
        let vol_id = VolumeId::new();
        let files = vec![
            sample_file(b"alpha", "a.txt"),
            sample_file(b"beta", "b.txt"),
            sample_file(b"gamma", "c.txt"),
        ];
        write_manifest(td.path(), vol_id, &files).expect("write_manifest");

        let db_path = td.path().join(".perima/manifest.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM manifest_files", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 3);
    }

    #[test]
    fn write_manifest_updates_on_rerun() {
        let td = tempfile::tempdir().expect("tempdir");
        let vol_id = VolumeId::new();
        let file1 = sample_file(b"hello", "original.txt");

        write_manifest(td.path(), vol_id, std::slice::from_ref(&file1)).expect("first write");

        // Change the relative_path for the same hash (contrived but tests
        // INSERT OR REPLACE semantics).
        let mut file2 = file1.clone();
        file2.discovered.relative_path = MediaPath::new("renamed.txt");
        write_manifest(td.path(), vol_id, &[file2]).expect("second write");

        let db_path = td.path().join(".perima/manifest.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open");

        let path: String = conn
            .query_row(
                "SELECT relative_path FROM manifest_files WHERE blake3_hash = ?1",
                [file1.hash.to_hex()],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(path, "renamed.txt", "row must be updated to new path");
    }
}
