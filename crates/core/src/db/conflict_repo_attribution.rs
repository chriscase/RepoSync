//! Backfill `conflicts.repo_id` for legacy NULL rows when ownership is certain.
//!
//! Rows that remain NULL still block every scoped engine; callers surface their
//! ids and paths in apply-gate errors so operators can dismiss or attribute them.

use rusqlite::{params, Connection};

use crate::errors::DatabaseError;

const BLOCKING_STATUS_SQL: &str = "status NOT IN ('resolved', 'dismissed')";

/// Run safe attribution for NULL `repo_id` conflict rows.
///
/// The only certain case is exactly one row in `repositories`: all blocking NULL
/// rows receive that id. Multi-repo databases never infer ownership from
/// `file_path` or sibling rows (branch pairs share paths; deleted pairs leave
/// orphan scoped siblings).
///
/// Idempotent: safe to call on every init and before each apply gate.
pub fn attribute_null_conflict_repo_ids(conn: &Connection) -> Result<usize, DatabaseError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        let repo_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))?;
        if repo_count != 1 {
            return Ok(0usize);
        }
        let sole: String =
            conn.query_row("SELECT id FROM repositories LIMIT 1", [], |row| row.get(0))?;
        let changed = conn.execute(
            &format!(
                "UPDATE conflicts SET repo_id = ?1
                 WHERE repo_id IS NULL AND {BLOCKING_STATUS_SQL}"
            ),
            params![sole],
        )?;
        Ok(changed)
    })();
    match result {
        Ok(changed) => {
            conn.execute_batch("COMMIT")?;
            Ok(changed)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Blocking conflict row ids with `repo_id IS NULL` (legacy, unattributed).
pub fn unattributed_null_conflict_ids(conn: &Connection) -> Result<Vec<String>, DatabaseError> {
    let ids: Vec<String> = conn
        .prepare(&format!(
            "SELECT id FROM conflicts
                 WHERE repo_id IS NULL AND {BLOCKING_STATUS_SQL}
                 ORDER BY created_at ASC"
        ))?
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(ids)
}

/// `(id, file_path)` for each unattributed blocking NULL row.
pub fn unattributed_null_conflict_rows(
    conn: &Connection,
) -> Result<Vec<(String, String)>, DatabaseError> {
    let rows: Vec<(String, String)> = conn
        .prepare(&format!(
            "SELECT id, file_path FROM conflicts
                 WHERE repo_id IS NULL AND {BLOCKING_STATUS_SQL}
                 ORDER BY created_at ASC"
        ))?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn setup_db() -> Database {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db
    }

    fn insert_repo(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO repositories (id, name, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, sync_mode, poll_interval_secs, lfs_threshold_mb, auto_merge, enabled, created_at, updated_at, last_svn_rev, last_git_sha, sync_status, total_syncs, total_errors, consecutive_errors)
             VALUES (?1, ?1, 'file:///x', '', '', 'local', '', '', 'main', 'team', 5, 0, 0, 1, 't', 't', 0, '', 'idle', 0, 0, 0)",
            [id],
        )
        .unwrap();
    }

    #[test]
    fn single_repository_backfills_null_repo_id() {
        let db = setup_db();
        {
            let conn = db.conn();
            insert_repo(&conn, "only");
            conn.execute(
                "INSERT INTO conflicts (id, file_path, conflict_type, status, created_at, repo_id)
                 VALUES ('legacy', 'orphan.txt', 'content', 'detected', '2020-01-01T00:00:00Z', NULL)",
                [],
            )
            .unwrap();
        }
        assert_eq!(db.attribute_null_conflict_repo_ids().unwrap(), 1);
        assert_eq!(db.attribute_null_conflict_repo_ids().unwrap(), 0);
        let repo_id: Option<String> = db
            .conn()
            .query_row(
                "SELECT repo_id FROM conflicts WHERE id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repo_id.as_deref(), Some("only"));
        assert!(db
            .unattributed_null_conflict_ids_blocking_apply()
            .unwrap()
            .is_empty());
        assert_eq!(
            db.count_conflicts_blocking_apply_for_repo("other").unwrap(),
            0
        );
        assert_eq!(
            db.count_conflicts_blocking_apply_for_repo("only").unwrap(),
            1
        );
    }

    #[test]
    fn ambiguous_multi_repo_leaves_null_and_lists_ids() {
        let db = setup_db();
        {
            let conn = db.conn();
            insert_repo(&conn, "one");
            insert_repo(&conn, "two");
            conn.execute(
                "INSERT INTO conflicts (id, file_path, conflict_type, status, created_at, repo_id)
                 VALUES ('legacy-null', 'orphan.txt', 'content', 'detected', '2020-01-01T00:00:00Z', NULL)",
                [],
            )
            .unwrap();
        }
        assert_eq!(db.attribute_null_conflict_repo_ids().unwrap(), 0);
        let ids = db.unattributed_null_conflict_ids_blocking_apply().unwrap();
        assert_eq!(ids, vec!["legacy-null".to_string()]);
        assert_eq!(
            db.count_conflicts_blocking_apply_for_repo("one").unwrap(),
            1
        );
        assert_eq!(
            db.count_conflicts_blocking_apply_for_repo("two").unwrap(),
            1
        );
    }

    #[test]
    fn null_row_with_same_path_scoped_sibling_still_blocks_other_repos() {
        let db = setup_db();
        {
            let conn = db.conn();
            insert_repo(&conn, "one");
            insert_repo(&conn, "two");
            conn.execute(
                "INSERT INTO conflicts (id, file_path, conflict_type, status, created_at, repo_id)
                 VALUES ('scoped', 'shared.txt', 'content', 'detected', '2020-01-01T00:00:00Z', 'one'),
                        ('legacy', 'shared.txt', 'content', 'detected', '2020-01-01T00:00:00Z', NULL)",
                [],
            )
            .unwrap();
        }
        assert_eq!(db.attribute_null_conflict_repo_ids().unwrap(), 0);
        let repo_id: Option<String> = db
            .conn()
            .query_row(
                "SELECT repo_id FROM conflicts WHERE id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(repo_id.is_none());
        assert_eq!(
            db.count_conflicts_blocking_apply_for_repo("two").unwrap(),
            1
        );
        assert_eq!(
            db.count_conflicts_blocking_apply_for_repo("one").unwrap(),
            2
        );
    }

    #[test]
    fn null_row_with_orphan_sibling_from_deleted_repo_still_blocks_other_repos() {
        let db = setup_db();
        {
            let conn = db.conn();
            insert_repo(&conn, "one");
            insert_repo(&conn, "two");
            insert_repo(&conn, "removed");
            conn.execute(
                "INSERT INTO conflicts (id, file_path, conflict_type, status, created_at, repo_id)
                 VALUES ('orphan-scoped', 'shared.txt', 'content', 'detected', '2020-01-01T00:00:00Z', 'removed'),
                        ('legacy', 'shared.txt', 'content', 'detected', '2020-01-01T00:00:00Z', NULL)",
                [],
            )
            .unwrap();
        }
        db.delete_repository("removed").unwrap();
        assert_eq!(db.attribute_null_conflict_repo_ids().unwrap(), 0);
        let repo_id: Option<String> = db
            .conn()
            .query_row(
                "SELECT repo_id FROM conflicts WHERE id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(repo_id.is_none());
        assert_eq!(
            db.count_conflicts_blocking_apply_for_repo("one").unwrap(),
            1
        );
        assert_eq!(
            db.count_conflicts_blocking_apply_for_repo("two").unwrap(),
            1
        );
    }
}
