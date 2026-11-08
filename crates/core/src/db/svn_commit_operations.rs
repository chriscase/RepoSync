//! Bounded v12 operation journal for one team-engine Git→SVN commit.
//! Documents live in `kv_state`; ordinary schema stays v12.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Database;
use crate::errors::DatabaseError;
use crate::models::{SyncDirection, SyncRecord, SyncRecordStatus};

const PREFIX: &str = "git_to_svn_commit_v1:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SvnCommitOperationState {
    Queued,
    Running,
    Completed,
    Failed,
    ReconciliationRequired,
}

impl SvnCommitOperationState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::ReconciliationRequired
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntendedPath {
    pub action: String,
    pub path: String,
    pub content_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SvnCommitOperation {
    pub version: u8,
    pub id: String,
    pub repo_id: String,
    pub operation_type: String,
    pub initiator_id: String,
    pub request_id: String,
    pub target_fingerprint: String,
    pub created_at: String,
    pub updated_at: String,
    pub state: SvnCommitOperationState,
    pub source_git_sha: String,
    pub source_git_parent: Option<String>,
    pub source_git_tree: String,
    pub target_svn_uuid: String,
    pub target_svn_path: String,
    #[serde(default)]
    pub target_svn_root_url: String,
    #[serde(default)]
    pub target_svn_branch_path: String,
    pub pre_write_svn_rev: i64,
    pub pre_write_svn_tree: String,
    pub projection: String,
    pub intended_changed_paths: Vec<IntendedPath>,
    pub intended_svn_tree: String,
    pub author: String,
    pub source_message: String,
    pub last_confirmed_svn_rev: Option<i64>,
    pub last_confirmed_svn_tree: Option<String>,
    #[serde(default)]
    pub resume_authorized: bool,
    pub outcome_detail: Option<String>,
}

pub struct ReconciledSvnCommit {
    pub operation: SvnCommitOperation,
    pub finalized: bool,
    pub resume_authorized: bool,
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

fn parse(raw: &str) -> Result<SvnCommitOperation, DatabaseError> {
    let op: SvnCommitOperation = serde_json::from_str(raw)
        .map_err(|e| DatabaseError::Other(format!("invalid git-to-svn commit operation: {e}")))?;
    if op.version != 1 {
        return Err(DatabaseError::Other(
            "unsupported git-to-svn commit operation version".into(),
        ));
    }
    if op.operation_type != "git_to_svn_commit" {
        return Err(DatabaseError::Other(
            "unsupported git-to-svn commit operation type".into(),
        ));
    }
    Ok(op)
}

pub fn svn_commit_target_fingerprint(
    repo_id: &str,
    svn_uuid: &str,
    svn_url: &str,
    svn_path: &str,
    projection: &str,
) -> String {
    let source = serde_json::json!({
        "repo_id": repo_id,
        "svn_uuid": svn_uuid,
        "svn_url": svn_url,
        "svn_path": svn_path,
        "projection": projection,
    })
    .to_string();
    hex::encode(Sha256::digest(source.as_bytes()))
}

fn write_op(tx: &Connection, op: &SvnCommitOperation) -> Result<(), DatabaseError> {
    write_value(
        tx,
        &key("document", &op.id),
        &serde_json::to_string(op).map_err(|e| DatabaseError::Other(e.to_string()))?,
    )
}

fn clear_active(tx: &Connection, repo_id: &str, op_id: &str) -> Result<(), DatabaseError> {
    tx.execute(
        "DELETE FROM kv_state WHERE key=?1 AND value=?2",
        params![key("active", repo_id), op_id],
    )?;
    Ok(())
}

fn mapping_exists(
    tx: &Connection,
    repo_id: &str,
    git_sha: &str,
    svn_rev: i64,
) -> Result<bool, DatabaseError> {
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM sync_records
         WHERE repo_id=?1 AND git_sha=?2 AND svn_rev=?3 AND direction='git_to_svn'",
        params![repo_id, git_sha, svn_rev],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn finalize_tx(
    tx: &Connection,
    mut op: SvnCommitOperation,
    svn_rev: i64,
    svn_tree: &str,
) -> Result<SvnCommitOperation, DatabaseError> {
    if op.source_git_sha.len() != 40 || !op.source_git_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DatabaseError::Other(
            "git-to-svn commit lacks a full source SHA".into(),
        ));
    }
    if svn_rev <= op.pre_write_svn_rev {
        return Err(DatabaseError::Other(
            "observed SVN revision is not after the pre-write revision".into(),
        ));
    }
    if !mapping_exists(tx, &op.repo_id, &op.source_git_sha, svn_rev)? {
        let now = Utc::now();
        let record = SyncRecord {
            id: Uuid::new_v4().to_string(),
            repo_id: Some(op.repo_id.clone()),
            svn_revision: Some(svn_rev),
            git_hash: Some(op.source_git_sha.clone()),
            direction: SyncDirection::GitToSvn,
            author: op.author.clone(),
            message: op.source_message.clone(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        };
        tx.execute(
            "INSERT INTO sync_records (id, repo_id, svn_rev, git_sha, direction, author, message, timestamp, synced_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.id,
                record.repo_id,
                record.svn_revision,
                record.git_hash,
                record.direction.to_string(),
                record.author,
                record.message,
                record.timestamp.to_rfc3339(),
                record.synced_at.to_rfc3339(),
                record.status.to_string(),
            ],
        )?;
    }
    let removed = super::managed_remove::tombstone_present(tx, &op.repo_id)?;
    let now = Utc::now().to_rfc3339();
    if !removed {
        let updated_sha = tx.execute(
            "UPDATE repositories SET last_git_sha=?1 WHERE id=?2",
            params![op.source_git_sha, op.repo_id],
        )?;
        let updated_syncs = tx.execute(
            "UPDATE repositories SET total_syncs = total_syncs + 1 WHERE id=?1",
            params![op.repo_id],
        )?;
        if updated_sha != 1 || updated_syncs != 1 {
            return Err(DatabaseError::Other(
                "git-to-svn commit lost its repository registration".into(),
            ));
        }
        crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
            tx,
            &op.repo_id,
            &op.source_git_sha,
            &now,
        )?;
        if Database::repo_writes_global_git_watermark(tx, &op.repo_id)? {
            write_value(tx, "last_git_hash", &op.source_git_sha)?;
        }
    }
    if !removed {
        crate::db::lineage_checkpoints::try_persist_directional_checkpoint_tx(
            tx,
            &op.repo_id,
            crate::db::lineage_checkpoints::MappingWrite {
                direction: crate::db::lineage_checkpoints::CheckpointDirection::GitToSvn,
                source_key: &crate::db::lineage_checkpoints::git_source_key(&op.source_git_sha),
                outcome: crate::db::lineage_checkpoints::MappingOutcome::AppliedAndVerified,
                target_git_sha: None,
                target_svn_rev: Some(svn_rev),
                reason: None,
                policy_snapshot: None,
            },
        )?;
    }
    if removed {
        op.outcome_detail =
            Some("recorded after repository removal; registration was not restored".into());
    }
    op.state = SvnCommitOperationState::Completed;
    op.last_confirmed_svn_rev = Some(svn_rev);
    op.last_confirmed_svn_tree = Some(svn_tree.into());
    op.resume_authorized = false;
    op.updated_at = now;
    if !removed {
        op.outcome_detail = Some("Git-to-SVN commit verified and recorded".into());
    }
    write_op(tx, &op)?;
    clear_active(tx, &op.repo_id, &op.id)?;
    Ok(op)
}

fn personal_finalize_tx(
    tx: &Connection,
    mut op: SvnCommitOperation,
    svn_rev: i64,
    svn_tree: &str,
    git_author: &str,
) -> Result<SvnCommitOperation, DatabaseError> {
    if op.source_git_sha.len() != 40 || !op.source_git_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DatabaseError::Other(
            "git-to-svn commit lacks a full source SHA".into(),
        ));
    }
    if svn_rev <= op.pre_write_svn_rev {
        return Err(DatabaseError::Other(
            "observed SVN revision is not after the pre-write revision".into(),
        ));
    }
    if !mapping_exists(tx, &op.repo_id, &op.source_git_sha, svn_rev)? {
        let now = Utc::now();
        let record = SyncRecord {
            id: Uuid::new_v4().to_string(),
            repo_id: Some(op.repo_id.clone()),
            svn_revision: Some(svn_rev),
            git_hash: Some(op.source_git_sha.clone()),
            direction: SyncDirection::GitToSvn,
            author: op.author.clone(),
            message: op.source_message.clone(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        };
        tx.execute(
            "INSERT INTO sync_records (id, repo_id, svn_rev, git_sha, direction, author, message, timestamp, synced_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.id,
                record.repo_id,
                record.svn_revision,
                record.git_hash,
                record.direction.to_string(),
                record.author,
                record.message,
                record.timestamp.to_rfc3339(),
                record.synced_at.to_rfc3339(),
                record.status.to_string(),
            ],
        )?;
    }
    let exists: bool = tx.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM commit_map
            WHERE git_sha = ?1 AND direction = 'git_to_svn' AND repo_id = ?2
        )",
        params![op.source_git_sha, op.repo_id],
        |row| row.get(0),
    )?;
    if !exists {
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
             VALUES (?1, ?2, 'git_to_svn', ?3, ?4, ?5, ?6)",
            params![
                svn_rev,
                op.source_git_sha,
                now,
                op.author,
                git_author,
                op.repo_id
            ],
        )?;
    }
    let now = Utc::now().to_rfc3339();
    op.state = SvnCommitOperationState::Completed;
    op.last_confirmed_svn_rev = Some(svn_rev);
    op.last_confirmed_svn_tree = Some(svn_tree.into());
    op.resume_authorized = false;
    op.updated_at = now;
    op.outcome_detail = Some("personal Git-to-SVN commit verified and recorded".into());
    write_op(tx, &op)?;
    clear_active(tx, &op.repo_id, &op.id)?;
    Ok(op)
}

impl Database {
    pub fn get_svn_commit_operation(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> Result<Option<SvnCommitOperation>, DatabaseError> {
        let conn = self.conn();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM kv_state WHERE key=?1",
                [key("document", op_id)],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|v| parse(&v)).transpose().map(|op| {
            if op.as_ref().is_some_and(|o| o.repo_id != repo_id) {
                None
            } else {
                op
            }
        })
    }

    pub fn latest_svn_commit_operation(
        &self,
        repo_id: &str,
    ) -> Result<Option<SvnCommitOperation>, DatabaseError> {
        let id = self.get_state(&key("latest", repo_id))?;
        id.map(|id| self.get_svn_commit_operation(repo_id, &id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn active_svn_commit_operation(
        &self,
        repo_id: &str,
    ) -> Result<Option<SvnCommitOperation>, DatabaseError> {
        let id = self.get_state(&key("active", repo_id))?;
        id.map(|id| self.get_svn_commit_operation(repo_id, &id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn has_any_blocking_svn_commit_hold(&self) -> Result<bool, DatabaseError> {
        let conn = self.conn();
        let mut query = conn.prepare(
            "SELECT key,value FROM kv_state WHERE key LIKE 'git_to_svn_commit_v1:active:%'",
        )?;
        let rows = query
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(query);
        drop(conn);
        for (active_key, id) in rows {
            let repo_id = active_key
                .strip_prefix(&key("active", ""))
                .ok_or_else(|| DatabaseError::Other("invalid active git-to-svn key".into()))?;
            if let Some(op) = self.get_svn_commit_operation(repo_id, &id)? {
                if op.state == SvnCommitOperationState::ReconciliationRequired
                    && !op.resume_authorized
                {
                    return Ok(true);
                }
                if !op.state.is_terminal() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub fn begin_git_to_svn_commit(
        &self,
        intent: SvnCommitIntent<'_>,
    ) -> Result<SvnCommitOperation, DatabaseError> {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(
                tx,
                intent.repo_id,
            )?;
            crate::writer_fence::require_current(tx)?;
            let active = key("active", intent.repo_id);
            let active_existing = read_value(tx, &active)?;
            if active_existing.is_none()
                && super::managed_remove::new_work_blocked(tx, intent.repo_id)?
            {
                return Err(DatabaseError::Other(
                    "repository removal blocks a new Git-to-SVN commit".into(),
                ));
            }
            if let Some(existing_id) = active_existing {
                let mut existing = parse(
                    &read_value(tx, &key("document", &existing_id))?.ok_or_else(|| {
                        DatabaseError::Other("missing git-to-svn commit document".into())
                    })?,
                )?;
                if existing.repo_id != intent.repo_id {
                    return Err(DatabaseError::Other(
                        "git-to-svn commit repository mismatch".into(),
                    ));
                }
                let same_intent = existing.source_git_sha == intent.source_git_sha
                    && existing.source_git_parent == intent.source_git_parent.map(str::to_owned)
                    && existing.source_git_tree == intent.source_git_tree
                    && existing.target_svn_uuid == intent.target_svn_uuid
                    && existing.target_svn_path == intent.target_svn_path
                    && existing.target_svn_root_url == intent.target_svn_root_url
                    && existing.target_svn_branch_path == intent.target_svn_branch_path
                    && existing.pre_write_svn_rev == intent.pre_write_svn_rev
                    && existing.pre_write_svn_tree == intent.pre_write_svn_tree
                    && existing.intended_svn_tree == intent.intended_svn_tree
                    && existing.intended_changed_paths == intent.intended_changed_paths
                    && existing.target_fingerprint == intent.target_fingerprint;
                if existing.resume_authorized
                    && existing.state == SvnCommitOperationState::ReconciliationRequired
                    && same_intent
                {
                    existing.state = SvnCommitOperationState::Running;
                    existing.resume_authorized = false;
                    existing.updated_at = Utc::now().to_rfc3339();
                    existing.outcome_detail = Some("resuming the one planned write".into());
                    write_op(tx, &existing)?;
                    return Ok(existing);
                }
                return Err(DatabaseError::Other(
                    "repository has an active or held git-to-svn commit".into(),
                ));
            }
            let now = Utc::now().to_rfc3339();
            let op = SvnCommitOperation {
                version: 1,
                id: Uuid::new_v4().to_string(),
                repo_id: intent.repo_id.into(),
                operation_type: "git_to_svn_commit".into(),
                initiator_id: intent.initiator_id.into(),
                request_id: intent.request_id.into(),
                target_fingerprint: intent.target_fingerprint.into(),
                created_at: now.clone(),
                updated_at: now,
                state: SvnCommitOperationState::Running,
                source_git_sha: intent.source_git_sha.into(),
                source_git_parent: intent.source_git_parent.map(str::to_owned),
                source_git_tree: intent.source_git_tree.into(),
                target_svn_uuid: intent.target_svn_uuid.into(),
                target_svn_path: intent.target_svn_path.into(),
                target_svn_root_url: intent.target_svn_root_url.into(),
                target_svn_branch_path: intent.target_svn_branch_path.into(),
                pre_write_svn_rev: intent.pre_write_svn_rev,
                pre_write_svn_tree: intent.pre_write_svn_tree.into(),
                projection: intent.projection.into(),
                intended_changed_paths: intent.intended_changed_paths,
                intended_svn_tree: intent.intended_svn_tree.into(),
                author: intent.author.into(),
                source_message: intent.source_message.into(),
                last_confirmed_svn_rev: None,
                last_confirmed_svn_tree: None,
                resume_authorized: false,
                outcome_detail: None,
            };
            write_op(tx, &op)?;
            write_value(tx, &active, &op.id)?;
            write_value(tx, &key("latest", intent.repo_id), &op.id)?;
            Ok(op)
        })
    }

    pub fn hold_git_to_svn_reconciliation(
        &self,
        repo_id: &str,
        op_id: &str,
        detail: &str,
    ) -> Result<SvnCommitOperation, DatabaseError> {
        self.update_svn_commit_operation(repo_id, op_id, |op| {
            if op.state == SvnCommitOperationState::Completed {
                return Err(DatabaseError::Other(
                    "terminal git-to-svn outcome cannot be rewritten".into(),
                ));
            }
            if op.state == SvnCommitOperationState::ReconciliationRequired {
                op.outcome_detail = Some(detail.into());
                return Ok(());
            }
            op.state = SvnCommitOperationState::ReconciliationRequired;
            op.resume_authorized = false;
            op.outcome_detail = Some(detail.into());
            Ok(())
        })
    }

    pub fn note_svn_commit_reconciliation_reason(
        &self,
        repo_id: &str,
        op_id: &str,
        reason: &str,
    ) -> Result<SvnCommitOperation, DatabaseError> {
        self.update_svn_commit_operation(repo_id, op_id, |op| {
            if op.state != SvnCommitOperationState::ReconciliationRequired {
                return Err(DatabaseError::Other(
                    "operation is not an active git-to-svn reconciliation hold".into(),
                ));
            }
            op.outcome_detail = Some(reason.into());
            op.resume_authorized = false;
            Ok(())
        })
    }

    pub fn authorize_git_to_svn_resume(
        &self,
        repo_id: &str,
        op_id: &str,
        reason: &str,
    ) -> Result<SvnCommitOperation, DatabaseError> {
        self.update_svn_commit_operation(repo_id, op_id, |op| {
            if op.state != SvnCommitOperationState::ReconciliationRequired {
                return Err(DatabaseError::Other(
                    "operation is not an active git-to-svn reconciliation hold".into(),
                ));
            }
            if op.last_confirmed_svn_rev.is_some() {
                return Err(DatabaseError::Other(
                    "confirmed git-to-svn commit cannot be resumed".into(),
                ));
            }
            op.resume_authorized = true;
            op.outcome_detail = Some(reason.into());
            Ok(())
        })
    }

    pub fn finalize_verified_svn_commit(
        &self,
        repo_id: &str,
        op_id: &str,
        svn_rev: i64,
        svn_tree: &str,
        fingerprint: &str,
    ) -> Result<ReconciledSvnCommit, DatabaseError> {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, repo_id)?;
            crate::writer_fence::require_current(tx)?;
            if read_value(tx, &key("active", repo_id))?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive git-to-svn commit operation".into(),
                ));
            }
            let mut op = parse(&read_value(tx, &key("document", op_id))?.ok_or_else(|| {
                DatabaseError::Other("missing git-to-svn commit document".into())
            })?)?;
            if op.repo_id != repo_id
                || op.operation_type != "git_to_svn_commit"
                || op.state != SvnCommitOperationState::ReconciliationRequired
            {
                return Err(DatabaseError::Other(
                    "operation is not an active git-to-svn reconciliation hold".into(),
                ));
            }
            if op.target_fingerprint.is_empty() || op.target_fingerprint != fingerprint {
                return Err(DatabaseError::Other(
                    "git-to-svn target fingerprint changed".into(),
                ));
            }
            if op.last_confirmed_svn_rev == Some(svn_rev)
                && op.last_confirmed_svn_tree.as_deref() == Some(svn_tree)
            {
                let operation = finalize_tx(tx, op, svn_rev, svn_tree)?;
                return Ok(ReconciledSvnCommit {
                    operation,
                    finalized: true,
                    resume_authorized: false,
                });
            }
            if op.last_confirmed_svn_rev.is_some() {
                return Err(DatabaseError::Other(
                    "confirmed git-to-svn revision differs from observed evidence".into(),
                ));
            }
            op = finalize_tx(tx, op, svn_rev, svn_tree)?;
            Ok(ReconciledSvnCommit {
                operation: op,
                finalized: true,
                resume_authorized: false,
            })
        })
    }

    pub fn confirm_git_to_svn_commit(
        &self,
        repo_id: &str,
        op_id: &str,
        svn_rev: i64,
        svn_tree: &str,
    ) -> Result<SvnCommitOperation, DatabaseError> {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, repo_id)?;
            crate::writer_fence::require_current(tx)?;
            if read_value(tx, &key("active", repo_id))?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive git-to-svn commit operation".into(),
                ));
            }
            let op = parse(&read_value(tx, &key("document", op_id))?.ok_or_else(|| {
                DatabaseError::Other("missing git-to-svn commit document".into())
            })?)?;
            if op.repo_id != repo_id || op.state != SvnCommitOperationState::Running {
                return Err(DatabaseError::Other(
                    "git-to-svn commit is not running".into(),
                ));
            }
            finalize_tx(tx, op, svn_rev, svn_tree)
        })
    }

    /// Finalize a held personal Git→SVN journal after remote inspection proves
    /// the one planned SVN revision is ours.
    pub fn finalize_personal_verified_git_to_svn_commit(
        &self,
        repo_id: &str,
        op_id: &str,
        svn_rev: i64,
        svn_tree: &str,
        git_author: &str,
    ) -> Result<SvnCommitOperation, DatabaseError> {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, repo_id)?;
            crate::writer_fence::require_current(tx)?;
            if read_value(tx, &key("active", repo_id))?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive git-to-svn commit operation".into(),
                ));
            }
            let op = parse(&read_value(tx, &key("document", op_id))?.ok_or_else(|| {
                DatabaseError::Other("missing git-to-svn commit document".into())
            })?)?;
            if op.repo_id != repo_id
                || op.operation_type != "git_to_svn_commit"
                || op.state != SvnCommitOperationState::ReconciliationRequired
            {
                return Err(DatabaseError::Other(
                    "operation is not an active git-to-svn reconciliation hold".into(),
                ));
            }
            personal_finalize_tx(tx, op, svn_rev, svn_tree, git_author)
        })
    }

    /// Personal-mode checkpoint: journal completion plus `commit_map`.
    pub fn confirm_personal_git_to_svn_commit(
        &self,
        repo_id: &str,
        op_id: &str,
        svn_rev: i64,
        svn_tree: &str,
        git_author: &str,
    ) -> Result<SvnCommitOperation, DatabaseError> {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, repo_id)?;
            crate::writer_fence::require_current(tx)?;
            if read_value(tx, &key("active", repo_id))?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive git-to-svn commit operation".into(),
                ));
            }
            let op = parse(&read_value(tx, &key("document", op_id))?.ok_or_else(|| {
                DatabaseError::Other("missing git-to-svn commit document".into())
            })?)?;
            if op.repo_id != repo_id || op.state != SvnCommitOperationState::Running {
                return Err(DatabaseError::Other(
                    "git-to-svn commit is not running".into(),
                ));
            }
            personal_finalize_tx(tx, op, svn_rev, svn_tree, git_author)
        })
    }

    fn update_svn_commit_operation<F>(
        &self,
        repo_id: &str,
        op_id: &str,
        edit: F,
    ) -> Result<SvnCommitOperation, DatabaseError>
    where
        F: FnOnce(&mut SvnCommitOperation) -> Result<(), DatabaseError>,
    {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, repo_id)?;
            crate::writer_fence::require_current(tx)?;
            let active = key("active", repo_id);
            if read_value(tx, &active)?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive git-to-svn commit operation".into(),
                ));
            }
            let document = key("document", op_id);
            let mut op = parse(&read_value(tx, &document)?.ok_or_else(|| {
                DatabaseError::Other("missing git-to-svn commit document".into())
            })?)?;
            if op.repo_id != repo_id {
                return Err(DatabaseError::Other(
                    "git-to-svn commit repository mismatch".into(),
                ));
            }
            let original = op.clone();
            edit(&mut op)?;
            if op == original {
                return Ok(op);
            }
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            if op.state == SvnCommitOperationState::Completed {
                clear_active(tx, repo_id, op_id)?;
            }
            Ok(op)
        })
    }

    /// Unfinished Git→SVN writes cannot be mistaken for completed work after restart.
    pub fn hold_interrupted_svn_commits(&self) -> Result<(), DatabaseError> {
        let active = {
            let conn = self.conn();
            let mut query = conn.prepare(
                "SELECT key,value FROM kv_state WHERE key LIKE 'git_to_svn_commit_v1:active:%'",
            )?;
            let rows = query
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for (active_key, id) in active {
            let repo_id = active_key
                .strip_prefix(&key("active", ""))
                .ok_or_else(|| DatabaseError::Other("invalid active git-to-svn key".into()))?;
            let op = self
                .get_svn_commit_operation(repo_id, &id)?
                .ok_or_else(|| {
                    DatabaseError::Other("active git-to-svn document missing or misowned".into())
                })?;
            if !op.state.is_terminal() {
                self.update_svn_commit_operation(repo_id, &op.id, |o| {
                    o.state = SvnCommitOperationState::ReconciliationRequired;
                    o.resume_authorized = false;
                    o.outcome_detail = Some(
                        "worker stopped before a verified Git-to-SVN result; inspect the exact SVN target"
                            .into(),
                    );
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
}

pub struct SvnCommitIntent<'a> {
    pub repo_id: &'a str,
    pub initiator_id: &'a str,
    pub request_id: &'a str,
    pub target_fingerprint: &'a str,
    pub source_git_sha: &'a str,
    pub source_git_parent: Option<&'a str>,
    pub source_git_tree: &'a str,
    pub target_svn_uuid: &'a str,
    pub target_svn_path: &'a str,
    pub target_svn_root_url: &'a str,
    pub target_svn_branch_path: &'a str,
    pub pre_write_svn_rev: i64,
    pub pre_write_svn_tree: &'a str,
    pub projection: &'a str,
    pub intended_changed_paths: Vec<IntendedPath>,
    pub intended_svn_tree: &'a str,
    pub author: &'a str,
    pub source_message: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_intent<'a>(
        paths: Vec<IntendedPath>,
        tree: &'a str,
    ) -> (SvnCommitIntent<'a>, Vec<IntendedPath>, &'a str) {
        (
            SvnCommitIntent {
                repo_id: "pair",
                initiator_id: "worker",
                request_id: "req-1",
                target_fingerprint: "fp",
                source_git_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                source_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                source_git_tree: "cccccccccccccccccccccccccccccccccccccccc",
                target_svn_uuid: "uuid",
                target_svn_path: "file:///svn",
                target_svn_root_url: "file:///svn",
                target_svn_branch_path: "",
                pre_write_svn_rev: 2,
                pre_write_svn_tree: "pre-tree",
                projection: "{}",
                intended_changed_paths: paths.clone(),
                intended_svn_tree: tree,
                author: "dev",
                source_message: "add feature",
            },
            paths,
            tree,
        )
    }

    #[test]
    fn persist_hold_and_unique_finalize() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
                 VALUES ('pair','p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'old','idle',0,0)",
                [],
            )
            .unwrap();
        db.set_state("last_git_hash", "stale-global").unwrap();
        let paths = vec![IntendedPath {
            action: "A".into(),
            path: "feature.txt".into(),
            content_sha256: Some("d".repeat(64)),
        }];
        let (intent, _, _) = sample_intent(paths, "post-tree");
        let op = db.begin_git_to_svn_commit(intent).unwrap();
        assert_eq!(op.state, SvnCommitOperationState::Running);
        let held = db
            .hold_git_to_svn_reconciliation("pair", &op.id, "lost reply")
            .unwrap();
        assert_eq!(held.state, SvnCommitOperationState::ReconciliationRequired);
        let done = db
            .finalize_verified_svn_commit("pair", &op.id, 3, "post-tree", "fp")
            .unwrap();
        assert!(done.finalized);
        assert_eq!(done.operation.state, SvnCommitOperationState::Completed);
        assert!(db.active_svn_commit_operation("pair").unwrap().is_none());
        let mapped: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id='pair' AND svn_rev=3 AND git_sha=?1",
                ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mapped, 1);
        assert_eq!(
            db.get_repo_watermark("pair").unwrap().1,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            db.get_state("last_git_hash").unwrap().as_deref(),
            Some("stale-global"),
            "team-mode git-to-svn finalize must not advance global last_git_hash"
        );
        let conn = db.conn();
        let scope = crate::db::repo_scope_identity::repository_scope_uuid(&conn, "pair").unwrap();
        let generation = crate::echo_receipt_scope::repo_echo_generation_tx(&conn, "pair").unwrap();
        let checkpoint = crate::db::lineage_checkpoints::load_directional_checkpoint(
            &conn,
            &scope,
            generation,
            crate::db::lineage_checkpoints::CheckpointDirection::GitToSvn,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            checkpoint.outcome,
            crate::db::lineage_checkpoints::MappingOutcome::AppliedAndVerified
        );
        assert_eq!(checkpoint.target_svn_rev, Some(3));
    }

    #[test]
    fn generation_read_failure_refuses_finalize_without_generation_one_record() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
                 VALUES ('pair','p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'old','idle',0,0)",
                [],
            )
            .unwrap();
        crate::db::repo_scope_identity::migrate_v13_scope_uuid(&db.conn()).unwrap();
        let scope =
            crate::db::repo_scope_identity::repository_scope_uuid(&db.conn(), "pair").unwrap();
        let generation_key = crate::db::repo_scope_identity::repo_echo_generation_kv_key(&scope);
        db.set_state(&generation_key, "placeholder").unwrap();
        db.conn()
            .execute(
                "UPDATE kv_state SET value = x'80' WHERE key = ?1",
                [generation_key],
            )
            .unwrap();
        let paths = vec![IntendedPath {
            action: "A".into(),
            path: "feature.txt".into(),
            content_sha256: Some("d".repeat(64)),
        }];
        let (intent, _, _) = sample_intent(paths, "post-tree");
        let op = db.begin_git_to_svn_commit(intent).unwrap();
        let err = db.confirm_git_to_svn_commit("pair", &op.id, 3, "post-tree");
        assert!(err.is_err());
        let still = db
            .get_svn_commit_operation("pair", &op.id)
            .unwrap()
            .unwrap();
        assert_eq!(still.state, SvnCommitOperationState::Running);
        let conn = db.conn();
        assert!(crate::db::lineage_checkpoints::load_directional_checkpoint(
            &conn,
            &scope,
            1,
            crate::db::lineage_checkpoints::CheckpointDirection::GitToSvn,
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn direct_mode_finalize_advances_global_last_git_hash() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
                 VALUES ('pair','p','file:///svn','','','local','','repo','main','direct',5,0,0,1,'t','t',2,'old','idle',0,0)",
                [],
            )
            .unwrap();
        db.set_state("last_git_hash", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        let paths = vec![IntendedPath {
            action: "A".into(),
            path: "feature.txt".into(),
            content_sha256: Some("d".repeat(64)),
        }];
        let (intent, _, _) = sample_intent(paths, "post-tree");
        let op = db.begin_git_to_svn_commit(intent).unwrap();
        let done = db
            .confirm_git_to_svn_commit("pair", &op.id, 3, "post-tree")
            .unwrap();
        assert_eq!(done.state, SvnCommitOperationState::Completed);
        assert_eq!(
            db.get_state("last_git_hash").unwrap().as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "direct-mode managed git-to-svn finalize must advance global last_git_hash"
        );
    }

    #[test]
    fn absent_effect_may_resume_same_intent_only() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let paths = vec![IntendedPath {
            action: "A".into(),
            path: "feature.txt".into(),
            content_sha256: Some("e".repeat(64)),
        }];
        let (intent, paths, tree) = sample_intent(paths, "post-tree");
        let op = db.begin_git_to_svn_commit(intent).unwrap();
        db.hold_git_to_svn_reconciliation("pair", &op.id, "before write")
            .unwrap();
        let authorized = db
            .authorize_git_to_svn_resume("pair", &op.id, "absent and unchanged")
            .unwrap();
        assert!(authorized.resume_authorized);
        let (resume, _, _) = sample_intent(paths, tree);
        let resumed = db.begin_git_to_svn_commit(resume).unwrap();
        assert_eq!(resumed.id, op.id);
        assert_eq!(resumed.state, SvnCommitOperationState::Running);
        let mut other = sample_intent(
            vec![IntendedPath {
                action: "A".into(),
                path: "other.txt".into(),
                content_sha256: Some("f".repeat(64)),
            }],
            "other-tree",
        );
        other.0.source_git_sha = "dddddddddddddddddddddddddddddddddddddddd";
        db.hold_git_to_svn_reconciliation("pair", &op.id, "held again")
            .unwrap();
        db.authorize_git_to_svn_resume("pair", &op.id, "absent")
            .unwrap();
        assert!(db.begin_git_to_svn_commit(other.0).is_err());
    }

    #[test]
    fn interrupted_running_write_is_held() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let paths = vec![];
        let (intent, _, _) = sample_intent(paths, "tree");
        let op = db.begin_git_to_svn_commit(intent).unwrap();
        db.hold_interrupted_svn_commits().unwrap();
        let held = db
            .get_svn_commit_operation("pair", &op.id)
            .unwrap()
            .unwrap();
        assert_eq!(held.state, SvnCommitOperationState::ReconciliationRequired);
        assert!(!held.resume_authorized);
    }

    #[test]
    fn personal_confirm_inserts_scoped_commit_map_when_managed_row_exists() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
                 VALUES (3, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'git_to_svn', 't', 'team', 'Team', 'managed-pair')",
                [],
            )
            .unwrap();
        let paths = vec![IntendedPath {
            action: "A".into(),
            path: "feature.txt".into(),
            content_sha256: Some("d".repeat(64)),
        }];
        let (intent, _, _) = sample_intent(paths, "post-tree");
        let op = db.begin_git_to_svn_commit(intent).unwrap();
        let done = db
            .confirm_personal_git_to_svn_commit("pair", &op.id, 3, "post-tree", "Dev User")
            .unwrap();
        assert_eq!(done.state, SvnCommitOperationState::Completed);
        let personal_rows: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM commit_map WHERE svn_rev=3 AND git_sha=?1 AND repo_id='pair' AND direction='git_to_svn'",
                ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(personal_rows, 1);
    }

    #[test]
    fn personal_confirm_records_commit_map_without_repo_row() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let paths = vec![IntendedPath {
            action: "A".into(),
            path: "feature.txt".into(),
            content_sha256: Some("d".repeat(64)),
        }];
        let (intent, _, _) = sample_intent(paths, "post-tree");
        let op = db.begin_git_to_svn_commit(intent).unwrap();
        let done = db
            .confirm_personal_git_to_svn_commit("pair", &op.id, 3, "post-tree", "Dev User")
            .unwrap();
        assert_eq!(done.state, SvnCommitOperationState::Completed);
        assert!(db.active_svn_commit_operation("pair").unwrap().is_none());
        let mapped: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM commit_map WHERE svn_rev=3 AND git_sha=?1 AND direction='git_to_svn'",
                ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mapped, 1);
        let receipt: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id='pair' AND svn_rev=3 AND git_sha=?1 AND direction='git_to_svn'",
                ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipt, 1);
        let git_author: String = db
            .conn()
            .query_row(
                "SELECT git_author FROM commit_map WHERE git_sha=?1",
                ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(git_author, "Dev User");
    }

    #[test]
    fn pr245_3_canonical_write_failure_is_atomic_after_reopen() {
        let fx = pr245_3_prepare();
        let (db, owner) =
            Database::open_with_exclusive_owner(&fx.data_dir).expect("owned file-backed database");
        pr245_3_assert_owned_file(&db, &fx.data_dir);
        pr245_3_register(&db, &fx.repo_id, &fx.svn_root);
        crate::db::lineage_checkpoints::record_independently_established_svn_uuid(
            &db,
            &fx.repo_id,
            &fx.svn_uuid,
        )
        .expect("svn uuid recorded before any revision");
        let (scope, generation) = pr245_3_scope(&db, &fx.repo_id);
        assert_eq!(generation, crate::pair_refresh::PAIR_GENERATION);
        assert_eq!(pr245_3_canonical_tables(&db), 3);
        assert_eq!(pr245_3_repo_row(&db, &fx.repo_id), (0, String::new(), 0));
        assert!(pr245_3_scoped_git(&db, &fx.repo_id).is_none());
        let native = pr245_3_native_effect(&fx);
        assert_eq!(native.svn_rev, 1);
        assert_eq!(native.bare_count, 1);
        assert_ne!(native.observed_tree, fx.pre_tree);
        assert_eq!(native.observed_tree.len(), 64);
        let paths = vec![IntendedPath {
            action: "A".into(),
            path: "feature.txt".into(),
            content_sha256: Some(native.content_sha.clone()),
        }];
        let op = db
            .begin_git_to_svn_commit(SvnCommitIntent {
                repo_id: &fx.repo_id,
                initiator_id: "pr245-3",
                request_id: &native.request_id,
                target_fingerprint: &native.fingerprint,
                source_git_sha: &native.source_sha,
                source_git_parent: None,
                source_git_tree: &native.source_tree,
                target_svn_uuid: &fx.svn_uuid,
                target_svn_path: &native.trunk_url,
                target_svn_root_url: &fx.svn_root,
                target_svn_branch_path: "trunk",
                pre_write_svn_rev: 0,
                pre_write_svn_tree: &fx.pre_tree,
                projection: &native.projection,
                intended_changed_paths: paths,
                intended_svn_tree: &native.observed_tree,
                author: &native.git_author,
                source_message: &native.git_message,
            })
            .expect("journal the observed commit");
        Uuid::parse_str(&op.id).expect("operation id is a real uuid");
        assert_ne!(op.id, fx.repo_id);
        assert_eq!(op.source_git_sha, native.source_sha);
        assert_eq!(op.intended_svn_tree, native.observed_tree);
        let held = db
            .hold_git_to_svn_reconciliation(
                &fx.repo_id,
                &op.id,
                "native svn result retained; canonical write not recorded",
            )
            .expect("hold for reconciliation");
        assert_eq!(held.state, SvnCommitOperationState::ReconciliationRequired);
        let source_key = crate::db::lineage_checkpoints::git_source_key(&native.source_sha);
        pr245_3_install_abort(
            &db,
            "pr245_3_git_to_svn_canonical_write",
            &scope,
            generation,
            "git_to_svn",
            &source_key,
            "pr245-3-git-to-svn-canonical-write-abort",
        );
        let failed = db.finalize_verified_svn_commit(
            &fx.repo_id,
            &op.id,
            native.svn_rev,
            &native.observed_tree,
            &native.fingerprint,
        );
        let failed = match failed {
            Ok(done) => panic!(
                "canonical write was not aborted: {:?}",
                done.operation.state
            ),
            Err(error) => error,
        };
        assert!(
            failed
                .to_string()
                .contains("pr245-3-git-to-svn-canonical-write-abort"),
            "{failed}"
        );
        pr245_3_assert_native(&fx, &native);
        pr245_3_assert_unresolved(&db, &fx.repo_id, &op.id, &native, &scope, generation);
        let (db, owner) = pr245_3_reopen(&fx.data_dir, db, owner);
        pr245_3_assert_owned_file(&db, &fx.data_dir);
        pr245_3_assert_native(&fx, &native);
        pr245_3_assert_unresolved(&db, &fx.repo_id, &op.id, &native, &scope, generation);
        assert_eq!(
            pr245_3_trigger_count(&db, "pr245_3_git_to_svn_canonical_write"),
            2,
            "named fault must still be the persisted trigger after reopen"
        );
        pr245_3_drop_abort(&db, "pr245_3_git_to_svn_canonical_write");
        assert_eq!(
            pr245_3_trigger_count(&db, "pr245_3_git_to_svn_canonical_write"),
            0
        );
        let done = db
            .finalize_verified_svn_commit(
                &fx.repo_id,
                &op.id,
                native.svn_rev,
                &native.observed_tree,
                &native.fingerprint,
            )
            .expect("same observed commit finalizes after the named fault is gone");
        assert!(done.finalized);
        assert!(!done.resume_authorized);
        pr245_3_assert_native(&fx, &native);
        pr245_3_assert_resolved(&db, &fx.repo_id, &op.id, &native, &scope, generation);
        let (db, owner) = pr245_3_reopen(&fx.data_dir, db, owner);
        pr245_3_assert_native(&fx, &native);
        pr245_3_assert_resolved(&db, &fx.repo_id, &op.id, &native, &scope, generation);
        let repeated = db.finalize_verified_svn_commit(
            &fx.repo_id,
            &op.id,
            native.svn_rev,
            &native.observed_tree,
            &native.fingerprint,
        );
        let repeated = match repeated {
            Ok(done) => panic!("repeat finalized a second time: {:?}", done.operation.id),
            Err(error) => error,
        };
        let repeated = repeated.to_string();
        assert!(
            repeated.contains("inactive"),
            "repeat must be the inactive-operation refusal: {repeated}"
        );
        assert!(!repeated.contains("pr245-3-git-to-svn-canonical-write-abort"));
        pr245_3_assert_native(&fx, &native);
        pr245_3_assert_resolved(&db, &fx.repo_id, &op.id, &native, &scope, generation);
        drop(db);
        drop(owner);
    }

    struct Pr2453Fixture {
        _root: tempfile::TempDir,
        data_dir: std::path::PathBuf,
        home: std::path::PathBuf,
        svn_config: std::path::PathBuf,
        svn_repo: std::path::PathBuf,
        svn_root: String,
        svn_uuid: String,
        pre_tree: String,
        bare: std::path::PathBuf,
        work: std::path::PathBuf,
        repo_id: String,
    }

    struct Pr2453Native {
        request_id: String,
        trunk_url: String,
        svn_rev: i64,
        svn_uuid: String,
        source_sha: String,
        source_tree: String,
        git_author: String,
        git_message: String,
        observed_tree: String,
        content_sha: String,
        projection: String,
        fingerprint: String,
        bare_count: i64,
        feature: Vec<u8>,
    }

    fn pr245_3_prepare() -> Pr2453Fixture {
        let root = tempfile::tempdir().expect("private fixture root");
        let repo_id = format!("pr245-{}", Uuid::new_v4());
        let data_dir = root.path().join("data");
        let home = root.path().join("home");
        let svn_config = root.path().join("svn-config");
        let svn_repo = root.path().join("svn");
        let bare = root.path().join("bare.git");
        let work = root.path().join("work");
        let empty = root.path().join("empty-tree");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&svn_config).unwrap();
        std::fs::write(
            svn_config.join("config"),
            "[auth]\npassword-stores =\nstore-passwords = no\nstore-auth-creds = no\n",
        )
        .unwrap();
        std::fs::write(
            svn_config.join("servers"),
            "[global]\nstore-passwords = no\nstore-plaintext-passwords = no\n",
        )
        .unwrap();
        let pre_tree = crate::svn_commit::hash_regular_file_tree(&empty).unwrap();
        pr245_3_run(
            "svnadmin",
            &["create", svn_repo.to_str().unwrap()],
            None,
            &home,
        );
        let svn_uuid = pr245_3_run(
            "svnlook",
            &["uuid", svn_repo.to_str().unwrap()],
            None,
            &home,
        );
        assert!(Uuid::parse_str(&svn_uuid).is_ok());
        assert!(Uuid::parse_str(&repo_id).is_err());
        assert_eq!(
            pr245_3_run(
                "svnlook",
                &["youngest", svn_repo.to_str().unwrap()],
                None,
                &home
            ),
            "0",
            "svn identity is recorded before the source revision"
        );
        let svn_root = format!("file://{}", svn_repo.display());
        Pr2453Fixture {
            _root: root,
            data_dir,
            home,
            svn_config,
            svn_repo,
            svn_root,
            svn_uuid,
            pre_tree,
            bare,
            work,
            repo_id,
        }
    }

    fn pr245_3_native_effect(fx: &Pr2453Fixture) -> Pr2453Native {
        let feature = b"feature\n".to_vec();
        pr245_3_run(
            "git",
            &[
                "init",
                "--bare",
                "--initial-branch",
                "main",
                fx.bare.to_str().unwrap(),
            ],
            None,
            &fx.home,
        );
        pr245_3_run(
            "git",
            &[
                "init",
                "--initial-branch",
                "main",
                fx.work.to_str().unwrap(),
            ],
            None,
            &fx.home,
        );
        pr245_3_run(
            "git",
            &["config", "user.name", "Fixture"],
            Some(&fx.work),
            &fx.home,
        );
        pr245_3_run(
            "git",
            &["config", "user.email", "fixture@example.invalid"],
            Some(&fx.work),
            &fx.home,
        );
        std::fs::write(fx.work.join("feature.txt"), &feature).unwrap();
        pr245_3_run("git", &["add", "feature.txt"], Some(&fx.work), &fx.home);
        pr245_3_run(
            "git",
            &["commit", "-m", "feature"],
            Some(&fx.work),
            &fx.home,
        );
        let source_sha = pr245_3_run("git", &["rev-parse", "HEAD"], Some(&fx.work), &fx.home);
        let source_tree = pr245_3_run(
            "git",
            &["rev-parse", "HEAD^{tree}"],
            Some(&fx.work),
            &fx.home,
        );
        let git_author = pr245_3_run(
            "git",
            &["log", "-1", "--format=%an"],
            Some(&fx.work),
            &fx.home,
        );
        let git_message = pr245_3_run(
            "git",
            &["log", "-1", "--format=%s"],
            Some(&fx.work),
            &fx.home,
        );
        assert_eq!(git_author, "Fixture");
        assert_eq!(git_message, "feature");
        assert!(
            !pr245_3_ok(
                "git",
                &["rev-parse", "--verify", "HEAD^"],
                Some(&fx.work),
                &fx.home
            ),
            "source commit must be the one new commit"
        );
        pr245_3_run(
            "git",
            &["remote", "add", "origin", fx.bare.to_str().unwrap()],
            Some(&fx.work),
            &fx.home,
        );
        pr245_3_run(
            "git",
            &["push", "-u", "origin", "main"],
            Some(&fx.work),
            &fx.home,
        );
        let seed = fx._root.path().join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::write(seed.join("feature.txt"), &feature).unwrap();
        let trunk_url = format!("{}/trunk", fx.svn_root);
        pr245_3_svn(
            fx,
            &[
                "import",
                seed.to_str().unwrap(),
                &trunk_url,
                "-m",
                "feature",
            ],
        );
        let svn_rev: i64 = pr245_3_run(
            "svnlook",
            &["youngest", fx.svn_repo.to_str().unwrap()],
            None,
            &fx.home,
        )
        .parse()
        .unwrap();
        assert_eq!(svn_rev, 1);
        let svn_author = pr245_3_run(
            "svnlook",
            &["author", "-r", "1", fx.svn_repo.to_str().unwrap()],
            None,
            &fx.home,
        );
        assert!(!svn_author.is_empty());
        let observed_tree = pr245_3_observe_tree(&trunk_url, svn_rev);
        let bare_count: i64 = pr245_3_run(
            "git",
            &[
                "--git-dir",
                fx.bare.to_str().unwrap(),
                "rev-list",
                "--count",
                "refs/heads/main",
            ],
            None,
            &fx.home,
        )
        .parse()
        .unwrap();
        assert_eq!(bare_count, 1);
        let fingerprint = hex::encode(Sha256::digest(
            format!("{}\n{}", fx.svn_uuid, fx.bare.display()).as_bytes(),
        ));
        let projection = format!(
            "{{\"svn_uuid\":\"{}\",\"root\":\"{}\"}}",
            fx.svn_uuid, fx.svn_root
        );
        Pr2453Native {
            request_id: Uuid::new_v4().to_string(),
            trunk_url,
            svn_rev,
            svn_uuid: fx.svn_uuid.clone(),
            source_sha,
            source_tree,
            git_author,
            git_message,
            observed_tree,
            content_sha: hex::encode(Sha256::digest(&feature)),
            projection,
            fingerprint,
            bare_count,
            feature,
        }
    }

    fn pr245_3_observe_tree(url: &str, rev: i64) -> String {
        let svn = crate::svn::SvnClient::new_isolated_refresh(url, "fixture", Some("fixture"))
            .expect("isolated svn observation client");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(crate::svn_commit::observed_svn_tree_at_revision(&svn, rev))
            .expect("observed svn tree")
    }

    fn pr245_3_assert_owned_file(db: &Database, data_dir: &std::path::Path) {
        let opened = db.opened_file().expect("file-backed owner");
        let data_dir = data_dir.canonicalize().expect("data dir");
        assert!(opened.path.ends_with("reposync.db"));
        assert!(opened.path.starts_with(&data_dir));
        assert!(!opened.aliased);
    }

    fn pr245_3_register(db: &Database, repo_id: &str, svn_url: &str) {
        let now = Utc::now().to_rfc3339();
        db.insert_repository(&crate::models::Repository {
            id: repo_id.into(),
            name: repo_id.into(),
            svn_url: svn_url.into(),
            svn_branch: "trunk".into(),
            svn_username: "fixture".into(),
            git_provider: "local".into(),
            git_api_url: String::new(),
            git_repo: "bare.git".into(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 30,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now,
            last_svn_rev: 0,
            last_git_sha: String::new(),
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        })
        .expect("managed repository");
    }

    fn pr245_3_scope(db: &Database, repo_id: &str) -> (String, i64) {
        let conn = db.conn();
        let scope = crate::db::repo_scope_identity::repository_scope_uuid(&conn, repo_id)
            .expect("managed scope");
        let generation = crate::echo_receipt_scope::repo_echo_generation_tx(&conn, repo_id)
            .expect("managed generation");
        assert!(!scope.is_empty());
        (scope, generation)
    }

    fn pr245_3_repo_row(db: &Database, repo_id: &str) -> (i64, String, i64) {
        db.conn()
            .query_row(
                "SELECT last_svn_rev, last_git_sha, total_syncs FROM repositories WHERE id=?1",
                [repo_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    fn pr245_3_scoped_git(db: &Database, repo_id: &str) -> Option<String> {
        crate::echo_receipt_scope::read_scoped_last_git_sha_kv(db, repo_id).unwrap()
    }

    fn pr245_3_canonical_tables(db: &Database) -> i64 {
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN \
                 ('canonical_lineage','canonical_checkpoints','canonical_mapping_outcomes')",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn pr245_3_mapping_count(db: &Database, repo_id: &str, sha: &str, rev: i64) -> i64 {
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records \
                 WHERE repo_id=?1 AND git_sha=?2 AND svn_rev=?3 AND direction='git_to_svn'",
                params![repo_id, sha, rev],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn pr245_3_install_abort(
        db: &Database,
        name: &str,
        scope: &str,
        generation: i64,
        direction: &str,
        source_key: &str,
        marker: &str,
    ) {
        for value in [name, scope, direction, source_key, marker] {
            assert!(
                value
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':')),
                "{value}"
            );
        }
        let sql = format!(
            r#"CREATE TRIGGER {name}_insert
BEFORE INSERT ON canonical_checkpoints
WHEN NEW.scope_uuid = '{scope}'
 AND NEW.generation = {generation}
 AND NEW.direction = '{direction}'
 AND NEW.handled_source_key = '{source_key}'
 AND NEW.outcome = 'applied_and_verified'
BEGIN
 SELECT RAISE(ABORT, '{marker}');
END;
CREATE TRIGGER {name}_update
BEFORE UPDATE ON canonical_checkpoints
WHEN NEW.scope_uuid = '{scope}'
 AND NEW.generation = {generation}
 AND NEW.direction = '{direction}'
 AND NEW.handled_source_key = '{source_key}'
 AND NEW.outcome = 'applied_and_verified'
BEGIN
 SELECT RAISE(ABORT, '{marker}');
END;"#
        );
        db.conn()
            .execute_batch(&sql)
            .expect("named canonical-write trigger");
    }

    fn pr245_3_drop_abort(db: &Database, name: &str) {
        assert!(name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'));
        db.conn()
            .execute_batch(&format!(
                "DROP TRIGGER IF EXISTS {name}_insert; DROP TRIGGER IF EXISTS {name}_update;"
            ))
            .unwrap();
    }

    fn pr245_3_trigger_count(db: &Database, name: &str) -> i64 {
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND (name=?1 OR name=?2)",
                params![format!("{name}_insert"), format!("{name}_update")],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn pr245_3_reopen(
        data_dir: &std::path::Path,
        db: Database,
        owner: crate::data_dir::DataDirOwner,
    ) -> (Database, crate::data_dir::DataDirOwner) {
        drop(db);
        drop(owner);
        Database::open_with_exclusive_owner(data_dir).expect("reopen owned file")
    }

    fn pr245_3_assert_native(fx: &Pr2453Fixture, native: &Pr2453Native) {
        assert_eq!(
            pr245_3_run(
                "svnlook",
                &["youngest", fx.svn_repo.to_str().unwrap()],
                None,
                &fx.home
            ),
            native.svn_rev.to_string()
        );
        assert_eq!(
            pr245_3_stdout(
                "svnlook",
                &[
                    "cat",
                    "-r",
                    "1",
                    fx.svn_repo.to_str().unwrap(),
                    "trunk/feature.txt"
                ],
                None,
                &fx.home
            ),
            native.feature
        );
        assert_eq!(
            pr245_3_stdout(
                "git",
                &[
                    "--git-dir",
                    fx.bare.to_str().unwrap(),
                    "show",
                    "refs/heads/main:feature.txt"
                ],
                None,
                &fx.home
            ),
            native.feature
        );
        assert_eq!(
            pr245_3_run(
                "git",
                &[
                    "--git-dir",
                    fx.bare.to_str().unwrap(),
                    "rev-parse",
                    "refs/heads/main"
                ],
                None,
                &fx.home
            ),
            native.source_sha
        );
        assert_eq!(
            pr245_3_run(
                "git",
                &[
                    "--git-dir",
                    fx.bare.to_str().unwrap(),
                    "rev-parse",
                    "refs/heads/main^{tree}"
                ],
                None,
                &fx.home
            ),
            native.source_tree
        );
        assert_eq!(
            pr245_3_run(
                "git",
                &[
                    "--git-dir",
                    fx.bare.to_str().unwrap(),
                    "rev-list",
                    "--count",
                    "refs/heads/main"
                ],
                None,
                &fx.home
            ),
            native.bare_count.to_string()
        );
        assert!(
            !pr245_3_ok(
                "git",
                &[
                    "--git-dir",
                    fx.bare.to_str().unwrap(),
                    "rev-parse",
                    "--verify",
                    "refs/heads/main^"
                ],
                None,
                &fx.home
            ),
            "recovery must not create a second git commit"
        );
    }

    fn pr245_3_assert_unresolved(
        db: &Database,
        repo_id: &str,
        op_id: &str,
        native: &Pr2453Native,
        scope: &str,
        generation: i64,
    ) {
        let op = db
            .get_svn_commit_operation(repo_id, op_id)
            .unwrap()
            .expect("held operation");
        assert_eq!(op.state, SvnCommitOperationState::ReconciliationRequired);
        assert!(!op.resume_authorized);
        assert_eq!(op.source_git_sha, native.source_sha);
        assert_eq!(op.intended_svn_tree, native.observed_tree);
        assert_eq!(op.target_svn_uuid, native.svn_uuid);
        assert!(op.last_confirmed_svn_rev.is_none());
        assert_eq!(
            db.active_svn_commit_operation(repo_id).unwrap().unwrap().id,
            op_id
        );
        assert_eq!(pr245_3_repo_row(db, repo_id), (0, String::new(), 0));
        assert_eq!(
            pr245_3_mapping_count(db, repo_id, &native.source_sha, native.svn_rev),
            0
        );
        assert!(pr245_3_scoped_git(db, repo_id).is_none());
        assert!(db.get_state("last_git_hash").unwrap().is_none());
        let source_key = crate::db::lineage_checkpoints::git_source_key(&native.source_sha);
        let conn = db.conn();
        assert!(crate::db::lineage_checkpoints::load_directional_checkpoint(
            &conn,
            scope,
            generation,
            crate::db::lineage_checkpoints::CheckpointDirection::GitToSvn,
        )
        .unwrap()
        .is_none());
        assert!(crate::db::lineage_checkpoints::load_mapping_outcome(
            &conn,
            scope,
            generation,
            crate::db::lineage_checkpoints::CheckpointDirection::GitToSvn,
            &source_key,
        )
        .unwrap()
        .is_none());
    }

    fn pr245_3_assert_resolved(
        db: &Database,
        repo_id: &str,
        op_id: &str,
        native: &Pr2453Native,
        scope: &str,
        generation: i64,
    ) {
        let op = db
            .get_svn_commit_operation(repo_id, op_id)
            .unwrap()
            .expect("completed operation");
        assert_eq!(op.state, SvnCommitOperationState::Completed);
        assert!(!op.resume_authorized);
        assert_eq!(op.last_confirmed_svn_rev, Some(native.svn_rev));
        assert_eq!(
            op.last_confirmed_svn_tree.as_deref(),
            Some(native.observed_tree.as_str())
        );
        assert_eq!(op.source_git_sha, native.source_sha);
        assert_eq!(op.source_git_tree, native.source_tree);
        assert_eq!(op.author, native.git_author);
        assert_eq!(op.source_message, native.git_message);
        assert!(db.active_svn_commit_operation(repo_id).unwrap().is_none());
        assert_eq!(
            pr245_3_repo_row(db, repo_id),
            (0, native.source_sha.clone(), 1)
        );
        assert_eq!(
            pr245_3_mapping_count(db, repo_id, &native.source_sha, native.svn_rev),
            1
        );
        assert_eq!(
            pr245_3_scoped_git(db, repo_id).as_deref(),
            Some(native.source_sha.as_str())
        );
        assert!(db.get_state("last_git_hash").unwrap().is_none());
        let source_key = crate::db::lineage_checkpoints::git_source_key(&native.source_sha);
        let conn = db.conn();
        let checkpoint = crate::db::lineage_checkpoints::load_directional_checkpoint(
            &conn,
            scope,
            generation,
            crate::db::lineage_checkpoints::CheckpointDirection::GitToSvn,
        )
        .unwrap()
        .expect("directional checkpoint");
        assert_eq!(checkpoint.scope_uuid, scope);
        assert_eq!(checkpoint.generation, generation);
        assert_eq!(
            checkpoint.direction,
            crate::db::lineage_checkpoints::CheckpointDirection::GitToSvn
        );
        assert_eq!(checkpoint.handled_source_key, source_key);
        assert_eq!(
            checkpoint.outcome,
            crate::db::lineage_checkpoints::MappingOutcome::AppliedAndVerified
        );
        assert_eq!(checkpoint.target_svn_rev, Some(native.svn_rev));
        assert!(checkpoint.target_git_sha.is_none());
        let mapping = crate::db::lineage_checkpoints::load_mapping_outcome(
            &conn,
            scope,
            generation,
            crate::db::lineage_checkpoints::CheckpointDirection::GitToSvn,
            &source_key,
        )
        .unwrap()
        .expect("mapping outcome");
        assert_eq!(
            mapping.outcome,
            crate::db::lineage_checkpoints::MappingOutcome::AppliedAndVerified
        );
        assert_eq!(mapping.target_svn_rev, Some(native.svn_rev));
        assert!(mapping.target_git_sha.is_none());
    }

    fn pr245_3_svn(fx: &Pr2453Fixture, args: &[&str]) {
        let config = fx.svn_config.to_str().unwrap();
        let mut full = vec![
            "--config-dir",
            config,
            "--non-interactive",
            "--no-auth-cache",
            "--username",
            "fixture",
            "--password",
            "fixture",
        ];
        full.extend_from_slice(args);
        pr245_3_run("svn", &full, None, &fx.home);
    }

    fn pr245_3_run(
        program: &str,
        args: &[&str],
        dir: Option<&std::path::Path>,
        home: &std::path::Path,
    ) -> String {
        String::from_utf8(pr245_3_stdout(program, args, dir, home))
            .unwrap()
            .trim()
            .to_string()
    }

    fn pr245_3_ok(
        program: &str,
        args: &[&str],
        dir: Option<&std::path::Path>,
        home: &std::path::Path,
    ) -> bool {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args)
            .env("HOME", home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        if let Some(dir) = dir {
            cmd.current_dir(dir);
        }
        cmd.status().unwrap().success()
    }

    fn pr245_3_stdout(
        program: &str,
        args: &[&str],
        dir: Option<&std::path::Path>,
        home: &std::path::Path,
    ) -> Vec<u8> {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args)
            .env("HOME", home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        if let Some(dir) = dir {
            cmd.current_dir(dir);
        }
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
}
