//! Durable v12 operation journal for #67 late-pair publish (SVN copy + replay).
//! Documents live in `kv_state`; schema stays v12.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Database;
use crate::errors::DatabaseError;

const PREFIX: &str = "late_pair_publish_v1:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatePairPublishState {
    Queued,
    /// SVN copy intent persisted; copy may be in flight or complete but not yet journaled.
    SvnCopyPending,
    SvnCopied,
    ChildRegistered,
    ReplayInProgress,
    Completed,
    Failed,
    ReconciliationRequired,
}

impl LatePairPublishState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::ReconciliationRequired
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatePairPublishOperation {
    pub version: u8,
    pub id: String,
    /// Parent repository that owns the pair request.
    pub parent_repo_id: String,
    pub child_repo_id: Option<String>,
    pub operation_type: String,
    pub initiator_id: String,
    pub request_id: String,
    pub target_fingerprint: String,
    pub created_at: String,
    pub updated_at: String,
    pub state: LatePairPublishState,
    pub policy_version: String,
    pub git_branch: String,
    pub svn_branch: String,
    pub pinned_git_tip: String,
    pub baseline_git_sha: String,
    pub baseline_svn_rev: i64,
    pub svn_copy_source_rev: i64,
    #[serde(default)]
    pub svn_copy_source_path: Option<String>,
    #[serde(default)]
    pub svn_branch_head_rev: Option<i64>,
    #[serde(default)]
    pub replayed_git_shas: Vec<String>,
    pub outcome_detail: Option<String>,
}

fn key(kind: &str, id: &str) -> String {
    format!("{PREFIX}{kind}:{id}")
}

fn read_value(tx: &Connection, name: &str) -> Result<Option<String>, DatabaseError> {
    Ok(tx
        .query_row("SELECT value FROM kv_state WHERE key=?1", [name], |r| {
            r.get(0)
        })
        .optional()?)
}

fn write_value(tx: &Connection, name: &str, value: &str) -> Result<(), DatabaseError> {
    tx.execute(
        "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        params![name, value, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn parse(raw: &str) -> Result<LatePairPublishOperation, DatabaseError> {
    let op: LatePairPublishOperation = serde_json::from_str(raw)
        .map_err(|e| DatabaseError::Other(format!("invalid late-pair publish operation: {e}")))?;
    if op.version != 1 {
        return Err(DatabaseError::Other(
            "unsupported late-pair publish operation version".into(),
        ));
    }
    if op.operation_type != "late_pair_publish" {
        return Err(DatabaseError::Other(
            "unsupported late-pair publish operation type".into(),
        ));
    }
    Ok(op)
}

fn write_op(tx: &Connection, op: &LatePairPublishOperation) -> Result<(), DatabaseError> {
    write_value(
        tx,
        &key("document", &op.id),
        &serde_json::to_string(op).map_err(|e| DatabaseError::Other(e.to_string()))?,
    )
}

pub fn late_pair_publish_fingerprint(
    parent_id: &str,
    git_branch: &str,
    svn_branch: &str,
    git_tip: &str,
    baseline_git_sha: &str,
    baseline_svn_rev: i64,
) -> String {
    let source = serde_json::json!({
        "parent_repo_id": parent_id,
        "git_branch": git_branch,
        "svn_branch": svn_branch,
        "pinned_git_tip": git_tip,
        "baseline_git_sha": baseline_git_sha,
        "baseline_svn_rev": baseline_svn_rev,
    })
    .to_string();
    hex::encode(Sha256::digest(source.as_bytes()))
}

impl Database {
    #[allow(clippy::too_many_arguments)]
    pub fn create_late_pair_publish_operation(
        &self,
        parent_repo_id: &str,
        initiator_id: &str,
        request_id: &str,
        fingerprint: &str,
        git_branch: &str,
        svn_branch: &str,
        pinned_git_tip: &str,
        baseline_git_sha: &str,
        baseline_svn_rev: i64,
        svn_copy_source_rev: i64,
        policy_version: &str,
    ) -> Result<LatePairPublishOperation, DatabaseError> {
        self.transaction(|tx| {
            crate::writer_fence::require_current(tx)?;
            if super::managed_remove::new_work_blocked(tx, parent_repo_id)? {
                return Err(DatabaseError::Other(
                    "repository removal blocks a new late-pair publish".into(),
                ));
            }
            let active = key("active", parent_repo_id);
            if read_value(tx, &active)?.is_some() {
                return Err(DatabaseError::Other(
                    "parent repository has an active late-pair publish operation".into(),
                ));
            }
            let now = Utc::now().to_rfc3339();
            let op = LatePairPublishOperation {
                version: 1,
                id: Uuid::new_v4().to_string(),
                parent_repo_id: parent_repo_id.into(),
                child_repo_id: None,
                operation_type: "late_pair_publish".into(),
                initiator_id: initiator_id.into(),
                request_id: request_id.into(),
                target_fingerprint: fingerprint.into(),
                created_at: now.clone(),
                updated_at: now,
                state: LatePairPublishState::Queued,
                policy_version: policy_version.into(),
                git_branch: git_branch.into(),
                svn_branch: svn_branch.into(),
                pinned_git_tip: pinned_git_tip.into(),
                baseline_git_sha: baseline_git_sha.into(),
                baseline_svn_rev,
                svn_copy_source_rev,
                svn_copy_source_path: None,
                svn_branch_head_rev: None,
                replayed_git_shas: Vec::new(),
                outcome_detail: None,
            };
            write_op(tx, &op)?;
            write_value(tx, &active, &op.id)?;
            write_value(tx, &key("latest", parent_repo_id), &op.id)?;
            Ok(op)
        })
    }

    pub fn get_late_pair_publish_operation(
        &self,
        parent_repo_id: &str,
        op_id: &str,
    ) -> Result<Option<LatePairPublishOperation>, DatabaseError> {
        let conn = self.conn();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM kv_state WHERE key=?1",
                [key("document", op_id)],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|v| parse(&v)).transpose().map(|op| {
            if op
                .as_ref()
                .is_some_and(|o| o.parent_repo_id != parent_repo_id)
            {
                None
            } else {
                op
            }
        })
    }

    pub fn latest_late_pair_publish_operation(
        &self,
        parent_repo_id: &str,
    ) -> Result<Option<LatePairPublishOperation>, DatabaseError> {
        let op_id: Option<String> = {
            let conn = self.conn();
            conn.query_row(
                "SELECT value FROM kv_state WHERE key=?1",
                [key("latest", parent_repo_id)],
                |r| r.get(0),
            )
            .optional()?
        };
        match op_id {
            Some(id) => self.get_late_pair_publish_operation(parent_repo_id, &id),
            None => Ok(None),
        }
    }

    pub fn update_late_pair_publish_operation(
        &self,
        op: LatePairPublishOperation,
    ) -> Result<LatePairPublishOperation, DatabaseError> {
        self.transaction(|tx| {
            crate::writer_fence::require_current(tx)?;
            let mut op = op;
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            Ok(op)
        })
    }

    pub fn finalize_late_pair_publish_operation(
        &self,
        parent_repo_id: &str,
        op_id: &str,
    ) -> Result<LatePairPublishOperation, DatabaseError> {
        self.transaction(|tx| {
            crate::writer_fence::require_current(tx)?;
            finalize_late_pair_publish_op_in_tx(tx, parent_repo_id, op_id, None)
        })
    }

    /// Atomically mark the journal completed and enable the child (crash-safe publish finish).
    pub fn finalize_late_pair_publish_enabling_child(
        &self,
        parent_repo_id: &str,
        op_id: &str,
        child_repo_id: &str,
        git_tip: &str,
    ) -> Result<LatePairPublishOperation, DatabaseError> {
        self.transaction(|tx| {
            crate::writer_fence::require_current(tx)?;
            finalize_late_pair_publish_op_in_tx(
                tx,
                parent_repo_id,
                op_id,
                Some((child_repo_id, git_tip)),
            )
        })
    }
}

fn finalize_late_pair_publish_op_in_tx(
    tx: &Connection,
    parent_repo_id: &str,
    op_id: &str,
    enable_child: Option<(&str, &str)>,
) -> Result<LatePairPublishOperation, DatabaseError> {
    let raw = read_value(tx, &key("document", op_id))?
        .ok_or_else(|| DatabaseError::Other("late-pair publish operation not found".into()))?;
    let mut op = parse(&raw)?;
    if op.parent_repo_id != parent_repo_id {
        return Err(DatabaseError::Other(
            "late-pair publish operation parent mismatch".into(),
        ));
    }
    if let Some((child_id, git_tip)) = enable_child {
        if super::managed_remove::new_work_blocked(tx, child_id)? {
            op.state = LatePairPublishState::ReconciliationRequired;
            op.outcome_detail =
                Some("child repository removal blocks late-pair publish finalize".into());
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            tx.execute(
                "DELETE FROM kv_state WHERE key=?1 AND value=?2",
                params![key("active", parent_repo_id), op_id],
            )?;
            return Ok(op);
        }
        let changed = tx.execute(
            "UPDATE repositories SET enabled = 1, sync_status = 'idle', last_git_sha = ?1, updated_at = ?2 WHERE id = ?3",
            params![git_tip, Utc::now().to_rfc3339(), child_id],
        )?;
        if changed != 1 {
            op.state = LatePairPublishState::ReconciliationRequired;
            op.outcome_detail = Some(
                "child repository row missing or unchanged during late-pair publish finalize"
                    .into(),
            );
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            tx.execute(
                "DELETE FROM kv_state WHERE key=?1 AND value=?2",
                params![key("active", parent_repo_id), op_id],
            )?;
            return Ok(op);
        }
    }
    op.state = LatePairPublishState::Completed;
    op.updated_at = Utc::now().to_rfc3339();
    write_op(tx, &op)?;
    tx.execute(
        "DELETE FROM kv_state WHERE key=?1 AND value=?2",
        params![key("active", parent_repo_id), op_id],
    )?;
    Ok(op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn insert_parent_child(db: &Database, child_id: &str, enabled: bool) {
        let now = Utc::now().to_rfc3339();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors,parent_id)
                 VALUES ('parent','p','file:///svn','trunk','','local','','r','main','team',5,0,0,1,?1,?1,2,'base','idle',0,0,NULL),
                        (?2,'c','file:///svn','branches/f','','local','','r','feature','team',5,0,0,?3,?1,?1,2,'base','idle',0,0,'parent')",
                params![now, child_id, enabled as i32],
            )
            .unwrap();
    }

    fn replay_in_progress_op(child_id: &str) -> LatePairPublishOperation {
        let now = Utc::now().to_rfc3339();
        LatePairPublishOperation {
            version: 1,
            id: "op-finalize".into(),
            parent_repo_id: "parent".into(),
            child_repo_id: Some(child_id.into()),
            operation_type: "late_pair_publish".into(),
            initiator_id: "i".into(),
            request_id: "r".into(),
            target_fingerprint: "fp".into(),
            created_at: now.clone(),
            updated_at: now,
            state: LatePairPublishState::ReplayInProgress,
            policy_version: "late_pair_publish_v1".into(),
            git_branch: "feature".into(),
            svn_branch: "branches/f".into(),
            pinned_git_tip: "tipsha".into(),
            baseline_git_sha: "base".into(),
            baseline_svn_rev: 2,
            svn_copy_source_rev: 2,
            svn_copy_source_path: Some("trunk".into()),
            svn_branch_head_rev: Some(3),
            replayed_git_shas: vec!["sha1".into()],
            outcome_detail: None,
        }
    }

    fn seed_active_op(db: &Database, op: &LatePairPublishOperation) {
        let conn = db.conn();
        write_op(&conn, op).unwrap();
        write_value(&conn, &key("active", "parent"), &op.id).unwrap();
        write_value(&conn, &key("latest", "parent"), &op.id).unwrap();
    }

    fn child_enabled(db: &Database, child_id: &str) -> bool {
        db.conn()
            .query_row(
                "SELECT enabled FROM repositories WHERE id=?1",
                [child_id],
                |r| r.get::<_, i32>(0),
            )
            .unwrap()
            != 0
    }

    fn tombstone_child(db: &Database, child_id: &str) {
        let conn = db.conn();
        write_value(
            &conn,
            &format!("managed_remove_v1:tombstone:{child_id}"),
            "{}",
        )
        .unwrap();
    }

    #[test]
    fn finalize_enables_child_and_completes_journal() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let child_id = "child-ok";
        insert_parent_child(&db, child_id, false);
        let op = replay_in_progress_op(child_id);
        seed_active_op(&db, &op);

        let done = db
            .finalize_late_pair_publish_enabling_child("parent", &op.id, child_id, "tipsha")
            .unwrap();
        assert_eq!(done.state, LatePairPublishState::Completed);
        assert!(child_enabled(&db, child_id));
        assert!(db
            .latest_late_pair_publish_operation("parent")
            .unwrap()
            .is_some());
    }

    #[test]
    fn finalize_refuses_tombstoned_child_without_enabling() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let child_id = "child-tomb";
        insert_parent_child(&db, child_id, false);
        tombstone_child(&db, child_id);
        let op = replay_in_progress_op(child_id);
        seed_active_op(&db, &op);

        let held = db
            .finalize_late_pair_publish_enabling_child("parent", &op.id, child_id, "tipsha")
            .unwrap();
        assert_eq!(held.state, LatePairPublishState::ReconciliationRequired);
        assert!(!child_enabled(&db, child_id));
        let loaded = db
            .get_late_pair_publish_operation("parent", &op.id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.state, LatePairPublishState::ReconciliationRequired);
        assert_ne!(loaded.state, LatePairPublishState::Completed);
    }

    #[test]
    fn finalize_refuses_when_child_row_deleted() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let child_id = "child-gone";
        insert_parent_child(&db, child_id, false);
        let op = replay_in_progress_op(child_id);
        seed_active_op(&db, &op);
        db.conn()
            .execute("DELETE FROM repositories WHERE id=?1", [child_id])
            .unwrap();

        let held = db
            .finalize_late_pair_publish_enabling_child("parent", &op.id, child_id, "tipsha")
            .unwrap();
        assert_eq!(held.state, LatePairPublishState::ReconciliationRequired);
        let loaded = db
            .get_late_pair_publish_operation("parent", &op.id)
            .unwrap()
            .unwrap();
        assert_ne!(loaded.state, LatePairPublishState::Completed);
    }
}
