//! Personal-mode repository scope keys and scoped persistence queries.
//!
//! Personal mode journals and receipts use a collision-proof scope key so a
//! managed repository whose id is literally `personal` cannot share active
//! journals or sync records with personal mode. Reads fall back to the legacy
//! `personal` id on upgrade unless a managed repository already owns that id.

use rusqlite::params;

use super::git_push_operations::GitPushOperation;
use super::svn_commit_operations::SvnCommitOperation;
use super::Database;
use crate::errors::DatabaseError;

/// Collision-proof scope key for new personal-mode journals and receipts.
pub const PERSONAL_SCOPE_KEY: &str = "__reposync_personal__";

/// Legacy repo id used before personal scope was namespaced.
pub const LEGACY_PERSONAL_REPO_ID: &str = "personal";

/// Scope key used for all new personal-mode writes.
pub fn personal_scope_key() -> &'static str {
    PERSONAL_SCOPE_KEY
}

/// History-inspection scope id for new durable blocks.
pub fn personal_history_scope_id() -> &'static str {
    PERSONAL_SCOPE_KEY
}

/// History block keys checked for durable personal rewrite blocks.
pub fn personal_history_block_keys() -> [String; 2] {
    [PERSONAL_SCOPE_KEY, LEGACY_PERSONAL_REPO_ID]
        .iter()
        .map(|id| crate::history_inspect::history_block_key(Some(id)))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

fn personal_repo_ids_for_read(db: &Database) -> Result<Vec<&'static str>, DatabaseError> {
    let legacy_blocked = db.get_repository(LEGACY_PERSONAL_REPO_ID)?.is_some();
    if legacy_blocked {
        Ok(vec![PERSONAL_SCOPE_KEY])
    } else {
        Ok(vec![PERSONAL_SCOPE_KEY, LEGACY_PERSONAL_REPO_ID])
    }
}

/// Repo ids consulted for unfinalized personal journals.
///
/// Legacy journals keyed under `personal` must keep blocking replay even when a
/// managed repository already owns that id and legacy receipt reads are off.
fn personal_repo_ids_for_journal_read() -> [&'static str; 2] {
    [PERSONAL_SCOPE_KEY, LEGACY_PERSONAL_REPO_ID]
}

fn has_explicit_personal_commit_map_svn_to_git(
    conn: &rusqlite::Connection,
    svn_rev: i64,
    read_ids: &[&str],
) -> Result<bool, DatabaseError> {
    let exists: bool = if read_ids.len() == 1 {
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM commit_map
                WHERE svn_rev = ?1
                  AND direction = 'svn_to_git'
                  AND repo_id = ?2
            )",
            params![svn_rev, read_ids[0]],
            |row| row.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM commit_map
                WHERE svn_rev = ?1
                  AND direction = 'svn_to_git'
                  AND repo_id IN (?2, ?3)
            )",
            params![svn_rev, read_ids[0], read_ids[1]],
            |row| row.get(0),
        )?
    };
    Ok(exists)
}

fn null_commit_map_svn_to_git_has_personal_evidence(
    conn: &rusqlite::Connection,
    svn_rev: i64,
) -> Result<bool, DatabaseError> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM commit_map cm
            WHERE cm.svn_rev = ?1
              AND cm.direction = 'svn_to_git'
              AND cm.repo_id IS NULL
              AND NOT EXISTS (
                  SELECT 1 FROM commit_map foreign_cm
                  WHERE foreign_cm.svn_rev = cm.svn_rev
                    AND foreign_cm.direction = cm.direction
                    AND foreign_cm.repo_id IS NOT NULL
                    AND foreign_cm.repo_id NOT IN (?2, ?3)
              )
              AND NOT EXISTS (
                  SELECT 1 FROM sync_records foreign_sr
                  WHERE foreign_sr.svn_rev = cm.svn_rev
                    AND foreign_sr.direction = cm.direction
                    AND foreign_sr.status = 'applied'
                    AND foreign_sr.repo_id IS NOT NULL
                    AND foreign_sr.repo_id NOT IN (?2, ?3)
              )
        )",
        params![svn_rev, PERSONAL_SCOPE_KEY, LEGACY_PERSONAL_REPO_ID],
        |row| row.get(0),
    )?;
    Ok(exists)
}

fn has_explicit_personal_commit_map_git_to_svn(
    conn: &rusqlite::Connection,
    git_sha: &str,
    read_ids: &[&str],
) -> Result<bool, DatabaseError> {
    let exists: bool = if read_ids.len() == 1 {
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM commit_map
                WHERE git_sha = ?1
                  AND direction = 'git_to_svn'
                  AND repo_id = ?2
            )",
            params![git_sha, read_ids[0]],
            |row| row.get(0),
        )?
    } else {
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM commit_map
                WHERE git_sha = ?1
                  AND direction = 'git_to_svn'
                  AND repo_id IN (?2, ?3)
            )",
            params![git_sha, read_ids[0], read_ids[1]],
            |row| row.get(0),
        )?
    };
    Ok(exists)
}

fn null_commit_map_git_to_svn_has_personal_evidence(
    conn: &rusqlite::Connection,
    git_sha: &str,
) -> Result<bool, DatabaseError> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM commit_map cm
            WHERE cm.git_sha = ?1
              AND cm.direction = 'git_to_svn'
              AND cm.repo_id IS NULL
              AND NOT EXISTS (
                  SELECT 1 FROM commit_map foreign_cm
                  WHERE foreign_cm.git_sha = cm.git_sha
                    AND foreign_cm.direction = cm.direction
                    AND foreign_cm.repo_id IS NOT NULL
                    AND foreign_cm.repo_id NOT IN (?2, ?3)
              )
              AND NOT EXISTS (
                  SELECT 1 FROM sync_records foreign_sr
                  WHERE foreign_sr.git_sha = cm.git_sha
                    AND foreign_sr.direction = cm.direction
                    AND foreign_sr.status = 'applied'
                    AND foreign_sr.repo_id IS NOT NULL
                    AND foreign_sr.repo_id NOT IN (?2, ?3)
              )
        )",
        params![git_sha, PERSONAL_SCOPE_KEY, LEGACY_PERSONAL_REPO_ID],
        |row| row.get(0),
    )?;
    Ok(exists)
}

impl Database {
    /// Repo ids consulted for personal-mode receipt and journal reads.
    pub fn personal_repo_ids_for_read(&self) -> Result<Vec<&'static str>, DatabaseError> {
        personal_repo_ids_for_read(self)
    }

    /// Active personal SVN→Git journal, preferring the scope key then legacy.
    pub fn active_personal_git_push_operation(
        &self,
    ) -> Result<Option<GitPushOperation>, DatabaseError> {
        for repo_id in personal_repo_ids_for_journal_read() {
            if let Some(op) = self.active_git_push_operation(repo_id)? {
                return Ok(Some(op));
            }
        }
        Ok(None)
    }

    /// Active personal Git→SVN journal, preferring the scope key then legacy.
    pub fn active_personal_svn_commit_operation(
        &self,
    ) -> Result<Option<SvnCommitOperation>, DatabaseError> {
        for repo_id in personal_repo_ids_for_journal_read() {
            if let Some(op) = self.active_svn_commit_operation(repo_id)? {
                return Ok(Some(op));
            }
        }
        Ok(None)
    }

    /// True when personal mode already has an applied SVN→Git receipt for this revision.
    pub fn has_personal_svn_to_git_receipt_scoped(
        &self,
        svn_rev: i64,
    ) -> Result<bool, DatabaseError> {
        for repo_id in personal_repo_ids_for_read(self)? {
            if self.has_personal_svn_to_git_receipt(repo_id, svn_rev)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// True when personal mode already synced this SVN revision (receipts only).
    pub fn is_personal_svn_rev_synced(&self, svn_rev: i64) -> Result<bool, DatabaseError> {
        if self.has_personal_svn_to_git_receipt_scoped(svn_rev)? {
            return Ok(true);
        }
        let read_ids = personal_repo_ids_for_read(self)?;
        let conn = self.conn();
        if has_explicit_personal_commit_map_svn_to_git(&conn, svn_rev, &read_ids)? {
            return Ok(true);
        }
        if read_ids.len() == 1 {
            return Ok(false);
        }
        null_commit_map_svn_to_git_has_personal_evidence(&conn, svn_rev)
    }

    /// True when personal mode already synced this Git SHA (receipts only).
    pub fn is_personal_git_sha_synced(&self, git_sha: &str) -> Result<bool, DatabaseError> {
        for repo_id in personal_repo_ids_for_read(self)? {
            let conn = self.conn();
            let exists: bool = conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sync_records
                    WHERE repo_id = ?1 AND git_sha = ?2
                      AND direction = 'git_to_svn' AND status = 'applied'
                )",
                params![repo_id, git_sha],
                |row| row.get(0),
            )?;
            if exists {
                return Ok(true);
            }
        }
        let read_ids = personal_repo_ids_for_read(self)?;
        let conn = self.conn();
        if has_explicit_personal_commit_map_git_to_svn(&conn, git_sha, &read_ids)? {
            return Ok(true);
        }
        if read_ids.len() == 1 {
            return Ok(false);
        }
        null_commit_map_git_to_svn_has_personal_evidence(&conn, git_sha)
    }

    /// True when personal mode has completed syncing this PR merge SHA.
    pub fn is_personal_pr_synced(&self, merge_sha: &str) -> Result<bool, DatabaseError> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pr_sync_log
             WHERE merge_sha = ?1 AND status = 'completed'",
            params![merge_sha],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Remove a pending PR sync row left by a defer bail so the PR can retry.
    pub fn abandon_pending_pr_sync(&self, id: i64) -> Result<(), DatabaseError> {
        let conn = self.conn();
        let changed = conn.execute(
            "DELETE FROM pr_sync_log WHERE id = ?1 AND status = 'pending'",
            params![id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "pr_sync_log".into(),
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// True when this repository has an applied Git→SVN receipt for the emitted SVN revision.
    pub fn has_personal_emitted_svn_revision(&self, svn_rev: i64) -> Result<bool, DatabaseError> {
        for repo_id in personal_repo_ids_for_read(self)? {
            if self.has_repo_emitted_svn_revision(repo_id, svn_rev)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// True when this repository has an applied SVN→Git receipt for the emitted Git SHA.
    pub fn has_personal_emitted_git_commit(&self, git_sha: &str) -> Result<bool, DatabaseError> {
        for repo_id in personal_repo_ids_for_read(self)? {
            if self.has_repo_emitted_git_commit(repo_id, git_sha)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{SyncDirection, SyncRecord, SyncRecordStatus};

    fn setup_db() -> Database {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db
    }

    #[test]
    fn legacy_null_commit_map_counts_when_no_foreign_attribution() {
        let db = setup_db();
        db.insert_commit_map(
            1,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "svn_to_git",
            "dev",
            "Dev",
        )
        .unwrap();
        assert!(db.is_personal_svn_rev_synced(1).unwrap());
    }

    #[test]
    fn null_commit_map_with_foreign_sync_record_does_not_count_as_personal() {
        let db = setup_db();
        db.insert_commit_map(
            1,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "svn_to_git",
            "team",
            "Team",
        )
        .unwrap();
        let now = chrono::Utc::now();
        db.insert_sync_record(&SyncRecord {
            id: "foreign-svn-to-git".into(),
            repo_id: Some("foreign-team".into()),
            svn_revision: Some(1),
            git_hash: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            direction: SyncDirection::SvnToGit,
            author: "team".into(),
            message: "team import".into(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        })
        .unwrap();
        assert!(!db.is_personal_svn_rev_synced(1).unwrap());
    }

    #[test]
    fn null_commit_map_with_foreign_sync_record_does_not_count_as_personal_git_sha() {
        let db = setup_db();
        let git_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        db.insert_commit_map(1, git_sha, "git_to_svn", "team", "Team")
            .unwrap();
        let now = chrono::Utc::now();
        db.insert_sync_record(&SyncRecord {
            id: "foreign-git-to-svn".into(),
            repo_id: Some("foreign-team".into()),
            svn_revision: Some(1),
            git_hash: Some(git_sha.into()),
            direction: SyncDirection::GitToSvn,
            author: "team".into(),
            message: "team replay".into(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        })
        .unwrap();
        assert!(!db.is_personal_git_sha_synced(git_sha).unwrap());
    }

    #[test]
    fn foreign_commit_map_does_not_count_as_personal_svn_rev_synced() {
        let db = setup_db();
        db.insert_commit_map(
            2,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "svn_to_git",
            "a",
            "A",
        )
        .unwrap();
        db.conn()
            .execute(
                "UPDATE commit_map SET repo_id = 'foreign-team' WHERE svn_rev = 2",
                [],
            )
            .unwrap();
        assert!(!db.is_personal_svn_rev_synced(2).unwrap());
    }

    #[test]
    fn legacy_personal_receipt_counts_as_personal_svn_rev_synced() {
        let db = setup_db();
        let now = chrono::Utc::now();
        db.insert_sync_record(&SyncRecord {
            id: "legacy-svn-to-git".into(),
            repo_id: Some(LEGACY_PERSONAL_REPO_ID.to_string()),
            svn_revision: Some(3),
            git_hash: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
            direction: SyncDirection::SvnToGit,
            author: "dev".into(),
            message: "legacy".into(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        })
        .unwrap();
        assert!(db.is_personal_svn_rev_synced(3).unwrap());
    }

    #[test]
    fn pending_pr_sync_does_not_block_personal_retry() {
        let db = setup_db();
        let id = db
            .insert_pr_sync(
                1,
                "t",
                "b",
                "cccccccccccccccccccccccccccccccccccccccc",
                "squash",
                1,
            )
            .unwrap();
        assert!(!db
            .is_personal_pr_synced("cccccccccccccccccccccccccccccccccccccccc")
            .unwrap());
        db.abandon_pending_pr_sync(id).unwrap();
        assert_eq!(
            db.conn()
                .query_row("SELECT COUNT(*) FROM pr_sync_log", [], |row| row
                    .get::<_, i64>(0),)
                .unwrap(),
            0
        );
    }

    #[test]
    fn legacy_git_to_svn_receipt_still_suppresses_personal_echo() {
        let db = setup_db();
        let now = chrono::Utc::now();
        db.insert_sync_record(&SyncRecord {
            id: "legacy-git-to-svn".into(),
            repo_id: Some(LEGACY_PERSONAL_REPO_ID.to_string()),
            svn_revision: Some(5),
            git_hash: Some("dddddddddddddddddddddddddddddddddddddddd".into()),
            direction: SyncDirection::GitToSvn,
            author: "dev".into(),
            message: "legacy".into(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        })
        .unwrap();
        assert!(db.has_personal_emitted_svn_revision(5).unwrap());
        assert!(!db
            .has_repo_emitted_svn_revision(PERSONAL_SCOPE_KEY, 5)
            .unwrap());
    }

    #[test]
    fn managed_personal_repo_id_disables_legacy_receipt_reads() {
        let db = setup_db();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
                 VALUES ('personal','Managed','file:///x','','','local','','r','main','team',5,0,0,1,'t','t',1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','idle',0,0)",
                [],
            )
            .unwrap();
        let now = chrono::Utc::now();
        db.insert_sync_record(&SyncRecord {
            id: "team-owned-receipt".into(),
            repo_id: Some(LEGACY_PERSONAL_REPO_ID.to_string()),
            svn_revision: Some(1),
            git_hash: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
            direction: SyncDirection::SvnToGit,
            author: "team".into(),
            message: "managed".into(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        })
        .unwrap();
        assert_eq!(
            db.personal_repo_ids_for_read().unwrap(),
            vec![PERSONAL_SCOPE_KEY]
        );
        assert!(!db.has_personal_svn_to_git_receipt_scoped(1).unwrap());
    }

    #[test]
    fn legacy_unfinalized_journal_visible_when_managed_personal_repo_exists() {
        use crate::db::git_push_operations::{git_push_target_fingerprint, GitPushIntent};

        let db = setup_db();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
                 VALUES ('personal','Managed','file:///x','','','local','','r','main','team',5,0,0,1,'t','t',1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','idle',0,0)",
                [],
            )
            .unwrap();
        let fingerprint = git_push_target_fingerprint(LEGACY_PERSONAL_REPO_ID, "origin", "main");
        db.begin_svn_to_git_push(GitPushIntent {
            repo_id: LEGACY_PERSONAL_REPO_ID,
            initiator_id: "worker",
            request_id: "legacy-hold",
            target_fingerprint: &fingerprint,
            source_svn_rev: 2,
            source_svn_author: "dev",
            source_svn_message: "pre-upgrade journal",
            pre_push_git_remote: "origin",
            pre_push_git_branch: "main",
            pre_push_git_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            pre_push_git_tree: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            intended_local_git_sha: "cccccccccccccccccccccccccccccccccccccccc",
            intended_local_git_parent: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            intended_local_git_tree: "dddddddddddddddddddddddddddddddddddddddd",
        })
        .unwrap();
        assert!(
            db.active_personal_git_push_operation().unwrap().is_some(),
            "legacy journal under personal must still hold when managed repo owns that id"
        );
        assert!(db
            .active_git_push_operation(PERSONAL_SCOPE_KEY)
            .unwrap()
            .is_none());
    }
}
