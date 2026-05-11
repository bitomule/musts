//! Ledger of accepted evidence, persisted in the `evidence_records`
//! table per `docs/PLAN.md` §4.7.

use rusqlite::{params, OptionalExtension};

use crate::error::Result;
use crate::state::Db;

/// One row to insert into `evidence_records`.
#[derive(Debug)]
pub struct EvidenceRow<'a> {
    pub task_id: &'a str,
    pub submission_id: &'a str,
    pub check_id: &'a str,
    pub scope_hash: &'a str,
    pub accepted: bool,
    pub summary: Option<&'a str>,
    pub submission_json: &'a str,
    pub result_json: &'a str,
    pub submitted_at_unix: i64,
}

/// Is `check_id` currently green for `scope_hash`?
pub fn is_green(db: &Db, check_id: &str, scope_hash: &str) -> Result<bool> {
    let mut stmt = db.conn().prepare(
        "SELECT 1 FROM evidence_records WHERE check_id = ?1 AND scope_hash = ?2 AND accepted = 1 LIMIT 1",
    )?;
    let exists = stmt.exists(params![check_id, scope_hash])?;
    Ok(exists)
}

/// Insert one or more rows in a single transaction. Per PLAN.md §4.2,
/// every multi-check accept is atomic.
pub fn insert_atomic(db: &mut Db, rows: &[EvidenceRow<'_>]) -> Result<()> {
    let tx = db.conn_mut().transaction()?;
    for r in rows {
        tx.execute(
            r#"
            INSERT INTO evidence_records
                (task_id, submission_id, check_id, scope_hash, accepted,
                 summary, submission_json, result_json, submitted_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                r.task_id,
                r.submission_id,
                r.check_id,
                r.scope_hash,
                r.accepted as i64,
                r.summary,
                r.submission_json,
                r.result_json,
                r.submitted_at_unix,
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Persisted form of one row from the `tasks` table.
#[derive(Debug, Clone)]
pub struct StoredTask {
    pub satisfies_json: String,
    pub scope_hashes_json: String,
    pub task_snapshot_hash: String,
    pub payload_json: String,
    pub capability: String,
}

/// Look up the stored task by id. Returns `None` if the task is not in
/// the table — this is the "task no longer applies" signal for
/// `harness evidence` after a `validate` re-issues a different set.
pub fn fetch_task(db: &Db, task_id: &str) -> Result<Option<StoredTask>> {
    let row = db
        .conn()
        .query_row(
            "SELECT satisfies_json, scope_hashes, task_snapshot_hash, payload_json, capability \
             FROM tasks WHERE task_id = ?1",
            params![task_id],
            |r| {
                Ok(StoredTask {
                    satisfies_json: r.get(0)?,
                    scope_hashes_json: r.get(1)?,
                    task_snapshot_hash: r.get(2)?,
                    payload_json: r.get(3)?,
                    capability: r.get(4)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::open as open_db;
    use tempfile::TempDir;

    fn fresh_db() -> (TempDir, Db) {
        let dir = TempDir::new().unwrap();
        let db = open_db(&dir.path().join("state.sqlite")).unwrap();
        (dir, db)
    }

    #[test]
    fn is_green_returns_true_after_insert() {
        let (_dir, mut db) = fresh_db();
        assert!(!is_green(&db, "App/Login/login-build", "abc").unwrap());
        insert_atomic(
            &mut db,
            &[EvidenceRow {
                task_id: "t1",
                submission_id: "submission-001",
                check_id: "App/Login/login-build",
                scope_hash: "abc",
                accepted: true,
                summary: Some("ok"),
                submission_json: "{}",
                result_json: "{}",
                submitted_at_unix: 1,
            }],
        )
        .unwrap();
        assert!(is_green(&db, "App/Login/login-build", "abc").unwrap());
    }

    #[test]
    fn is_green_is_scope_hash_specific() {
        let (_dir, mut db) = fresh_db();
        insert_atomic(
            &mut db,
            &[EvidenceRow {
                task_id: "t1",
                submission_id: "submission-001",
                check_id: "c",
                scope_hash: "old",
                accepted: true,
                summary: None,
                submission_json: "{}",
                result_json: "{}",
                submitted_at_unix: 1,
            }],
        )
        .unwrap();
        assert!(is_green(&db, "c", "old").unwrap());
        assert!(!is_green(&db, "c", "new").unwrap());
    }

    #[test]
    fn rejected_rows_do_not_count_as_green() {
        let (_dir, mut db) = fresh_db();
        insert_atomic(
            &mut db,
            &[EvidenceRow {
                task_id: "t",
                submission_id: "submission-001",
                check_id: "c",
                scope_hash: "h",
                accepted: false,
                summary: None,
                submission_json: "{}",
                result_json: "{}",
                submitted_at_unix: 1,
            }],
        )
        .unwrap();
        assert!(!is_green(&db, "c", "h").unwrap());
    }

    #[test]
    fn fetch_task_returns_none_when_absent() {
        let (_dir, db) = fresh_db();
        assert!(fetch_task(&db, "missing").unwrap().is_none());
    }
}
