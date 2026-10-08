//! Backfill `conflicts.repo_id` for legacy NULL rows when ownership is certain.
//!
//! Rows that remain NULL still block every scoped engine; callers surface their
//! ids in apply-gate errors so operators can dismiss or attribute them manually.

use rusqlite::{params, Connection};

use crate::errors::DatabaseError;

const BLOCKING_STATUS_SQL: &str = "status NOT IN ('resolved', 'dismissed')";

/// Run safe attribution for NULL `repo_id` conflict rows.
///
/// Rules (each row is considered only while `repo_id IS NULL` and blocking):
/// 1. Exactly one row in `repositories` → assign that id.
/// 2. Exactly one distinct non-NULL `repo_id` among other conflicts with the same
///    `file_path` → assign that id.
///
/// Ambiguous or unknown rows are left NULL and continue to block all scoped engines.
pub fn attribute_null_conflict_repo_ids(conn: &Connection) -> Result<usize, DatabaseError> {
    let repo_count: i64 = conn.query_row("SELECT COUNT(*) FROM repositories", [], |r| r.get(0))?;
    let mut attributed = 0usize;
    if repo_count == 1 {
        let sole: String =
            conn.query_row("SELECT id FROM repositories LIMIT 1", [], |r| r.get(0))?;
        let changed = conn.execute(
            &format!(
                "UPDATE conflicts SET repo_id = ?1
                 WHERE repo_id IS NULL AND {BLOCKING_STATUS_SQL}"
            ),
            params![sole],
        )?;
        attributed += changed;
        if changed > 0 {
            return Ok(attributed);
        }
    }

    let null_ids: Vec<String> = conn
        .prepare(&format!(
            "SELECT id FROM conflicts
                 WHERE repo_id IS NULL AND {BLOCKING_STATUS_SQL}
                 ORDER BY created_at ASC"
        ))?
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    for id in null_ids {
        let file_path: String = conn.query_row(
            "SELECT file_path FROM conflicts WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        let owners: Vec<String> = conn
            .prepare(&format!(
                "SELECT DISTINCT repo_id FROM conflicts
                     WHERE file_path = ?1 AND repo_id IS NOT NULL AND {BLOCKING_STATUS_SQL}"
            ))?
            .query_map(params![file_path], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        if owners.len() == 1 {
            let changed = conn.execute(
                "UPDATE conflicts SET repo_id = ?1 WHERE id = ?2 AND repo_id IS NULL",
                params![owners[0], id],
            )?;
            attributed += changed;
        }
    }

    Ok(attributed)
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
    fn path_pair_attribution_when_unique_scoped_sibling() {
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
        assert_eq!(db.attribute_null_conflict_repo_ids().unwrap(), 1);
        let repo_id: Option<String> = db
            .conn()
            .query_row(
                "SELECT repo_id FROM conflicts WHERE id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repo_id.as_deref(), Some("one"));
        assert_eq!(
            db.count_conflicts_blocking_apply_for_repo("two").unwrap(),
            0
        );
        assert_eq!(
            db.count_conflicts_blocking_apply_for_repo("one").unwrap(),
            2
        );
    }
}
