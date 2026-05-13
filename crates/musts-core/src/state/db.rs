//! SQLite handle for `.musts/state.sqlite`.
//!
//! Opens the database with WAL journaling and synchronous=NORMAL, applies
//! the latest migration idempotently, and exposes typed helpers as the
//! library grows.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::Result;
use crate::snapshot::fingerprint::HashDigest;

use super::schema::{CURRENT_VERSION, MIGRATION_V1};

/// Thin wrapper around a SQLite connection so the rest of the crate can
/// keep behaviour-specific helpers organised here.
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Underlying [`Connection`] (for tests and modules that need raw SQL).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Mutable handle — needed for transactions.
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Upsert a manifest index row. `now_unix_seconds` is the wall-clock
    /// timestamp recorded as `last_seen_at`.
    pub fn upsert_manifest_index(
        &mut self,
        manifest_path: &str,
        scope_path: &str,
        mtime_ns: i128,
        size_bytes: u64,
        content_hash: &HashDigest,
        now_unix_seconds: i64,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO manifest_index
                (manifest_path, scope_path, mtime_ns, size_bytes, content_hash, last_seen_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(manifest_path) DO UPDATE SET
                scope_path   = excluded.scope_path,
                mtime_ns     = excluded.mtime_ns,
                size_bytes   = excluded.size_bytes,
                content_hash = excluded.content_hash,
                last_seen_at = excluded.last_seen_at
            "#,
            params![
                manifest_path,
                scope_path,
                mtime_ns as i64,
                size_bytes as i64,
                content_hash,
                now_unix_seconds,
            ],
        )?;
        Ok(())
    }

    /// Look up a cached fingerprint for a file. Returns `(mtime_ns, size, hash)` if present.
    pub fn fingerprint_for(&self, rel_path: &str) -> Result<Option<(i128, u64, HashDigest)>> {
        let row = self
            .conn
            .query_row(
                "SELECT mtime_ns, size_bytes, content_hash FROM file_fingerprints WHERE rel_path = ?1",
                params![rel_path],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as i128,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Upsert a file fingerprint row.
    pub fn upsert_fingerprint(
        &mut self,
        rel_path: &str,
        mtime_ns: i128,
        size_bytes: u64,
        content_hash: &HashDigest,
        now_unix_seconds: i64,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO file_fingerprints
                (rel_path, mtime_ns, size_bytes, content_hash, last_seen_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(rel_path) DO UPDATE SET
                mtime_ns     = excluded.mtime_ns,
                size_bytes   = excluded.size_bytes,
                content_hash = excluded.content_hash,
                last_seen_at = excluded.last_seen_at
            "#,
            params![
                rel_path,
                mtime_ns as i64,
                size_bytes as i64,
                content_hash,
                now_unix_seconds,
            ],
        )?;
        Ok(())
    }
}

/// Open or create `.musts/state.sqlite` at `db_path`. Applies the latest
/// migration if needed.
pub fn open(db_path: &Path) -> Result<Db> {
    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    let mut db = Db { conn };
    apply_migrations(&mut db)?;
    Ok(db)
}

fn apply_migrations(db: &mut Db) -> Result<()> {
    // Run v1 unconditionally — every statement uses IF NOT EXISTS, so it's
    // safe to re-run on an already-migrated database.
    db.conn.execute_batch(MIGRATION_V1)?;
    let current: Option<i32> = db
        .conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .optional()?;
    if current.is_none() {
        db.conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            params![CURRENT_VERSION],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_in_tmp() -> (TempDir, Db) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.sqlite");
        let db = open(&path).unwrap();
        (dir, db)
    }

    #[test]
    fn open_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.sqlite");
        let _ = open(&path).unwrap();
        // Second open should also succeed and leave schema_version with one row.
        let db = open(&path).unwrap();
        let count: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let version: i32 = db
            .conn()
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_VERSION);
    }

    #[test]
    fn manifest_index_upserts_and_overwrites() {
        let (_dir, mut db) = open_in_tmp();
        db.upsert_manifest_index("MUSTS.yml", "root", 100, 32, &"abc".into(), 1)
            .unwrap();
        db.upsert_manifest_index("MUSTS.yml", "root", 200, 64, &"def".into(), 2)
            .unwrap();
        let (mtime, size, hash): (i64, i64, String) = db
            .conn()
            .query_row(
                "SELECT mtime_ns, size_bytes, content_hash FROM manifest_index WHERE manifest_path = 'MUSTS.yml'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(mtime, 200);
        assert_eq!(size, 64);
        assert_eq!(hash, "def");
    }

    #[test]
    fn fingerprint_round_trip() {
        let (_dir, mut db) = open_in_tmp();
        assert!(db.fingerprint_for("a.swift").unwrap().is_none());

        db.upsert_fingerprint("a.swift", 1234, 99, &"hash1".into(), 10)
            .unwrap();
        let found = db.fingerprint_for("a.swift").unwrap().expect("present");
        assert_eq!(found, (1234, 99, "hash1".into()));

        db.upsert_fingerprint("a.swift", 1235, 100, &"hash2".into(), 20)
            .unwrap();
        let updated = db.fingerprint_for("a.swift").unwrap().unwrap();
        assert_eq!(updated, (1235, 100, "hash2".into()));
    }

    #[test]
    fn all_tables_are_created() {
        let (_dir, db) = open_in_tmp();
        let expected = [
            "schema_version",
            "manifest_index",
            "file_fingerprints",
            "scope_snapshots",
            "tasks",
            "resolve_notes",
            "evidence_records",
        ];
        for table in expected {
            let exists: Option<i32> = db
                .conn()
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?1",
                    params![table],
                    |r| r.get(0),
                )
                .optional()
                .unwrap();
            assert!(exists.is_some(), "missing table: {table}");
        }
    }
}
