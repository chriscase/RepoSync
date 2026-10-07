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

impl Database {
    /// Repo ids consulted for personal-mode receipt and journal reads.
    pub fn personal_repo_ids_for_read(&self) -> Result<Vec<&'static str>, DatabaseError> {
        personal_repo_ids_for_read(self)
    }

    /// Active personal SVN→Git journal, preferring the scope key then legacy.
    pub fn active_personal_git_push_operation(
        &self,
    ) -> Result<Option<GitPushOperation>, DatabaseError> {
        for repo_id in personal_repo_ids_for_read(self)? {
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
        for repo_id in personal_repo_ids_for_read(self)? {
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
        let exists: bool = if read_ids.len() == 1 {
            conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM commit_map
                    WHERE svn_rev = ?1
                      AND direction = 'svn_to_git'
                      AND (repo_id = ?2 OR repo_id IS NULL)
                )",
                params![svn_rev, PERSONAL_SCOPE_KEY],
                |row| row.get(0),
            )?
        } else {
            conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM commit_map
                    WHERE svn_rev = ?1
                      AND direction = 'svn_to_git'
                      AND (repo_id IN (?2, ?3) OR repo_id IS NULL)
                )",
                params![svn_rev, PERSONAL_SCOPE_KEY, LEGACY_PERSONAL_REPO_ID],
                |row| row.get(0),
            )?
        };
        Ok(exists)
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
        let exists: bool = if read_ids.len() == 1 {
            conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM commit_map
                    WHERE git_sha = ?1
                      AND direction = 'git_to_svn'
                      AND (repo_id = ?2 OR repo_id IS NULL)
                )",
                params![git_sha, PERSONAL_SCOPE_KEY],
                |row| row.get(0),
            )?
        } else {
            conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM commit_map
                    WHERE git_sha = ?1
                      AND direction = 'git_to_svn'
                      AND (repo_id IN (?2, ?3) OR repo_id IS NULL)
                )",
                params![git_sha, PERSONAL_SCOPE_KEY, LEGACY_PERSONAL_REPO_ID],
                |row| row.get(0),
            )?
        };
        Ok(exists)
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
}
