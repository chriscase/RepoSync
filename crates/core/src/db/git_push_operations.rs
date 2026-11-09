//! Bounded v12 operation journal for one team-engine SVN→Git push.
//! Documents live in `kv_state`; ordinary schema stays v12.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Database;
use crate::errors::DatabaseError;
use crate::models::{SyncDirection, SyncRecord, SyncRecordStatus};

const PREFIX: &str = "svn_to_git_push_v1:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPushOperationState {
    Queued,
    Running,
    Completed,
    Failed,
    ReconciliationRequired,
}

impl GitPushOperationState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::ReconciliationRequired
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPushOperation {
    pub version: u8,
    pub id: String,
    pub repo_id: String,
    pub operation_type: String,
    pub initiator_id: String,
    pub request_id: String,
    pub target_fingerprint: String,
    pub created_at: String,
    pub updated_at: String,
    pub state: GitPushOperationState,
    pub source_svn_rev: i64,
    pub source_svn_author: String,
    pub source_svn_message: String,
    pub pre_push_git_remote: String,
    pub pre_push_git_branch: String,
    pub pre_push_git_sha: String,
    pub pre_push_git_tree: Option<String>,
    pub intended_local_git_sha: String,
    pub intended_local_git_parent: Option<String>,
    pub intended_local_git_tree: String,
    pub last_confirmed_git_sha: Option<String>,
    pub last_confirmed_git_tree: Option<String>,
    #[serde(default)]
    pub resume_authorized: bool,
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

fn parse(raw: &str) -> Result<GitPushOperation, DatabaseError> {
    let op: GitPushOperation = serde_json::from_str(raw)
        .map_err(|e| DatabaseError::Other(format!("invalid svn-to-git push operation: {e}")))?;
    if op.version != 1 {
        return Err(DatabaseError::Other(
            "unsupported svn-to-git push operation version".into(),
        ));
    }
    if op.operation_type != "svn_to_git_push" {
        return Err(DatabaseError::Other(
            "unsupported svn-to-git push operation type".into(),
        ));
    }
    Ok(op)
}

pub fn git_push_target_fingerprint(repo_id: &str, git_remote: &str, git_branch: &str) -> String {
    let source = serde_json::json!({
        "repo_id": repo_id,
        "git_remote": git_remote,
        "git_branch": git_branch,
    })
    .to_string();
    hex::encode(Sha256::digest(source.as_bytes()))
}

fn write_op(tx: &Connection, op: &GitPushOperation) -> Result<(), DatabaseError> {
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
         WHERE repo_id=?1 AND git_sha=?2 AND svn_rev=?3 AND direction='svn_to_git'",
        params![repo_id, git_sha, svn_rev],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn finalize_tx(
    tx: &Connection,
    mut op: GitPushOperation,
    git_sha: &str,
    git_tree: &str,
    svn_rev: i64,
) -> Result<GitPushOperation, DatabaseError> {
    if git_sha.len() != 40 || !git_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DatabaseError::Other(
            "svn-to-git push lacks a full observed Git SHA".into(),
        ));
    }
    if git_tree.len() != 40 || !git_tree.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DatabaseError::Other(
            "svn-to-git push lacks a full observed Git tree".into(),
        ));
    }
    if git_sha != op.intended_local_git_sha {
        return Err(DatabaseError::Other(
            "observed Git SHA differs from the recorded local intent".into(),
        ));
    }
    if !mapping_exists(tx, &op.repo_id, git_sha, svn_rev)? {
        let now = Utc::now();
        let record = SyncRecord {
            id: Uuid::new_v4().to_string(),
            repo_id: Some(op.repo_id.clone()),
            svn_revision: Some(svn_rev),
            git_hash: Some(git_sha.into()),
            direction: SyncDirection::SvnToGit,
            author: op.source_svn_author.clone(),
            message: op.source_svn_message.clone(),
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
        let updated_svn = tx.execute(
            "UPDATE repositories SET last_svn_rev=?1 WHERE id=?2",
            params![svn_rev, op.repo_id],
        )?;
        let updated_git = tx.execute(
            "UPDATE repositories SET last_git_sha=?1 WHERE id=?2",
            params![git_sha, op.repo_id],
        )?;
        let updated_syncs = tx.execute(
            "UPDATE repositories SET total_syncs = total_syncs + 1 WHERE id=?1",
            params![op.repo_id],
        )?;
        if updated_svn != 1 || updated_git != 1 || updated_syncs != 1 {
            return Err(DatabaseError::Other(
                "svn-to-git push lost its repository registration".into(),
            ));
        }
        write_value(
            tx,
            &format!("last_svn_rev_{}", op.repo_id),
            &svn_rev.to_string(),
        )?;
    }
    if !removed {
        crate::db::lineage_checkpoints::try_persist_directional_checkpoint_tx(
            tx,
            &op.repo_id,
            crate::db::lineage_checkpoints::MappingWrite {
                direction: crate::db::lineage_checkpoints::CheckpointDirection::SvnToGit,
                source_key: &crate::db::lineage_checkpoints::svn_source_key(svn_rev),
                outcome: crate::db::lineage_checkpoints::MappingOutcome::AppliedAndVerified,
                target_git_sha: Some(git_sha),
                target_svn_rev: None,
                reason: None,
                policy_snapshot: None,
            },
        )?;
    }
    if removed {
        op.outcome_detail =
            Some("recorded after repository removal; registration was not restored".into());
    }
    op.state = GitPushOperationState::Completed;
    op.last_confirmed_git_sha = Some(git_sha.into());
    op.last_confirmed_git_tree = Some(git_tree.into());
    op.resume_authorized = false;
    op.updated_at = now;
    if !removed {
        op.outcome_detail = Some("SVN-to-Git push verified and recorded".into());
    }
    write_op(tx, &op)?;
    clear_active(tx, &op.repo_id, &op.id)?;
    Ok(op)
}

fn personal_finalize_tx(
    tx: &Connection,
    mut op: GitPushOperation,
    git_sha: &str,
    git_tree: &str,
    watermark_source: &str,
    git_author: &str,
) -> Result<GitPushOperation, DatabaseError> {
    if git_sha.len() != 40 || !git_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DatabaseError::Other(
            "svn-to-git push lacks a full observed Git SHA".into(),
        ));
    }
    if git_tree.len() != 40 || !git_tree.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DatabaseError::Other(
            "svn-to-git push lacks a full observed Git tree".into(),
        ));
    }
    if git_sha != op.intended_local_git_sha {
        return Err(DatabaseError::Other(
            "observed Git SHA differs from the recorded local intent".into(),
        ));
    }
    let svn_rev = op.source_svn_rev;
    if !mapping_exists(tx, &op.repo_id, git_sha, svn_rev)? {
        let now = Utc::now();
        let record = SyncRecord {
            id: Uuid::new_v4().to_string(),
            repo_id: Some(op.repo_id.clone()),
            svn_revision: Some(svn_rev),
            git_hash: Some(git_sha.into()),
            direction: SyncDirection::SvnToGit,
            author: op.source_svn_author.clone(),
            message: op.source_svn_message.clone(),
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
            WHERE svn_rev = ?1 AND direction = 'svn_to_git' AND repo_id = ?2
        )",
        params![svn_rev, op.repo_id],
        |row| row.get(0),
    )?;
    if !exists {
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
             VALUES (?1, ?2, 'svn_to_git', ?3, ?4, ?5, ?6)",
            params![
                svn_rev,
                git_sha,
                now,
                op.source_svn_author,
                git_author,
                op.repo_id
            ],
        )?;
    }
    let watermark_now = Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO watermarks (source, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(source) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![watermark_source, svn_rev.to_string(), watermark_now],
    )?;
    let now = Utc::now().to_rfc3339();
    op.state = GitPushOperationState::Completed;
    op.last_confirmed_git_sha = Some(git_sha.into());
    op.last_confirmed_git_tree = Some(git_tree.into());
    op.resume_authorized = false;
    op.updated_at = now;
    op.outcome_detail = Some("personal SVN-to-Git push verified and recorded".into());
    write_op(tx, &op)?;
    clear_active(tx, &op.repo_id, &op.id)?;
    Ok(op)
}

impl Database {
    pub fn get_git_push_operation(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> Result<Option<GitPushOperation>, DatabaseError> {
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

    pub fn latest_git_push_operation(
        &self,
        repo_id: &str,
    ) -> Result<Option<GitPushOperation>, DatabaseError> {
        let id = self.get_state(&key("latest", repo_id))?;
        id.map(|id| self.get_git_push_operation(repo_id, &id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn active_git_push_operation(
        &self,
        repo_id: &str,
    ) -> Result<Option<GitPushOperation>, DatabaseError> {
        let id = self.get_state(&key("active", repo_id))?;
        id.map(|id| self.get_git_push_operation(repo_id, &id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn has_any_blocking_git_push_hold(&self) -> Result<bool, DatabaseError> {
        let conn = self.conn();
        let mut query = conn.prepare(
            "SELECT key,value FROM kv_state WHERE key LIKE 'svn_to_git_push_v1:active:%'",
        )?;
        let rows = query
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(query);
        drop(conn);
        for (active_key, id) in rows {
            let repo_id = active_key
                .strip_prefix(&key("active", ""))
                .ok_or_else(|| DatabaseError::Other("invalid active svn-to-git push key".into()))?;
            if let Some(op) = self.get_git_push_operation(repo_id, &id)? {
                if op.state == GitPushOperationState::ReconciliationRequired
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

    pub fn begin_svn_to_git_push(
        &self,
        intent: GitPushIntent<'_>,
    ) -> Result<GitPushOperation, DatabaseError> {
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
                    "repository removal blocks a new SVN-to-Git push".into(),
                ));
            }
            if let Some(existing_id) = active_existing {
                let mut existing = parse(
                    &read_value(tx, &key("document", &existing_id))?.ok_or_else(|| {
                        DatabaseError::Other("missing svn-to-git push document".into())
                    })?,
                )?;
                if existing.repo_id != intent.repo_id {
                    return Err(DatabaseError::Other(
                        "svn-to-git push repository mismatch".into(),
                    ));
                }
                let same_intent = existing.source_svn_rev == intent.source_svn_rev
                    && existing.pre_push_git_remote == intent.pre_push_git_remote
                    && existing.pre_push_git_branch == intent.pre_push_git_branch
                    && existing.pre_push_git_sha == intent.pre_push_git_sha
                    && existing.pre_push_git_tree == intent.pre_push_git_tree.map(str::to_owned)
                    && existing.intended_local_git_sha == intent.intended_local_git_sha
                    && existing.intended_local_git_parent
                        == intent.intended_local_git_parent.map(str::to_owned)
                    && existing.intended_local_git_tree == intent.intended_local_git_tree
                    && existing.target_fingerprint == intent.target_fingerprint;
                if existing.resume_authorized
                    && existing.state == GitPushOperationState::ReconciliationRequired
                    && same_intent
                {
                    existing.state = GitPushOperationState::Running;
                    existing.resume_authorized = false;
                    existing.updated_at = Utc::now().to_rfc3339();
                    existing.outcome_detail = Some("resuming the one planned push".into());
                    write_op(tx, &existing)?;
                    return Ok(existing);
                }
                return Err(DatabaseError::Other(
                    "repository has an active or held svn-to-git push".into(),
                ));
            }
            let now = Utc::now().to_rfc3339();
            let op = GitPushOperation {
                version: 1,
                id: Uuid::new_v4().to_string(),
                repo_id: intent.repo_id.into(),
                operation_type: "svn_to_git_push".into(),
                initiator_id: intent.initiator_id.into(),
                request_id: intent.request_id.into(),
                target_fingerprint: intent.target_fingerprint.into(),
                created_at: now.clone(),
                updated_at: now,
                state: GitPushOperationState::Running,
                source_svn_rev: intent.source_svn_rev,
                source_svn_author: intent.source_svn_author.into(),
                source_svn_message: intent.source_svn_message.into(),
                pre_push_git_remote: intent.pre_push_git_remote.into(),
                pre_push_git_branch: intent.pre_push_git_branch.into(),
                pre_push_git_sha: intent.pre_push_git_sha.into(),
                pre_push_git_tree: intent.pre_push_git_tree.map(str::to_owned),
                intended_local_git_sha: intent.intended_local_git_sha.into(),
                intended_local_git_parent: intent.intended_local_git_parent.map(str::to_owned),
                intended_local_git_tree: intent.intended_local_git_tree.into(),
                last_confirmed_git_sha: None,
                last_confirmed_git_tree: None,
                resume_authorized: false,
                outcome_detail: None,
            };
            write_op(tx, &op)?;
            write_value(tx, &active, &op.id)?;
            write_value(tx, &key("latest", intent.repo_id), &op.id)?;
            Ok(op)
        })
    }

    pub fn hold_svn_to_git_reconciliation(
        &self,
        repo_id: &str,
        op_id: &str,
        detail: &str,
    ) -> Result<GitPushOperation, DatabaseError> {
        self.update_git_push_operation(repo_id, op_id, |op| {
            if op.state == GitPushOperationState::Completed {
                return Err(DatabaseError::Other(
                    "terminal svn-to-git outcome cannot be rewritten".into(),
                ));
            }
            if op.state == GitPushOperationState::ReconciliationRequired {
                op.outcome_detail = Some(detail.into());
                return Ok(());
            }
            op.state = GitPushOperationState::ReconciliationRequired;
            op.resume_authorized = false;
            op.outcome_detail = Some(detail.into());
            Ok(())
        })
    }

    pub fn note_git_push_reconciliation_reason(
        &self,
        repo_id: &str,
        op_id: &str,
        reason: &str,
    ) -> Result<GitPushOperation, DatabaseError> {
        self.update_git_push_operation(repo_id, op_id, |op| {
            if op.state != GitPushOperationState::ReconciliationRequired {
                return Err(DatabaseError::Other(
                    "operation is not an active svn-to-git reconciliation hold".into(),
                ));
            }
            op.outcome_detail = Some(reason.into());
            op.resume_authorized = false;
            Ok(())
        })
    }

    pub fn authorize_svn_to_git_resume(
        &self,
        repo_id: &str,
        op_id: &str,
        reason: &str,
    ) -> Result<GitPushOperation, DatabaseError> {
        self.update_git_push_operation(repo_id, op_id, |op| {
            if op.state != GitPushOperationState::ReconciliationRequired {
                return Err(DatabaseError::Other(
                    "operation is not an active svn-to-git reconciliation hold".into(),
                ));
            }
            if op.last_confirmed_git_sha.is_some() {
                return Err(DatabaseError::Other(
                    "confirmed svn-to-git push cannot be resumed".into(),
                ));
            }
            op.resume_authorized = true;
            op.outcome_detail = Some(reason.into());
            Ok(())
        })
    }

    pub fn finalize_verified_svn_to_git_push(
        &self,
        repo_id: &str,
        op_id: &str,
        observed_git_sha: &str,
        observed_git_tree: &str,
        fingerprint: &str,
    ) -> Result<ReconciledGitPush, DatabaseError> {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, repo_id)?;
            crate::writer_fence::require_current(tx)?;
            if read_value(tx, &key("active", repo_id))?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive svn-to-git push operation".into(),
                ));
            }
            let op =
                parse(&read_value(tx, &key("document", op_id))?.ok_or_else(|| {
                    DatabaseError::Other("missing svn-to-git push document".into())
                })?)?;
            if op.repo_id != repo_id {
                return Err(DatabaseError::Other(
                    "svn-to-git push repository mismatch".into(),
                ));
            }
            if op.state != GitPushOperationState::ReconciliationRequired {
                return Err(DatabaseError::Other(
                    "operation is not an active svn-to-git reconciliation hold".into(),
                ));
            }
            if op.target_fingerprint != fingerprint {
                return Err(DatabaseError::Other(
                    "svn-to-git push target fingerprint changed".into(),
                ));
            }
            if observed_git_sha != op.intended_local_git_sha {
                return Err(DatabaseError::Other(
                    "observed Git SHA differs from the recorded local intent".into(),
                ));
            }
            if observed_git_tree != op.intended_local_git_tree {
                return Err(DatabaseError::Other(
                    "observed Git tree differs from the recorded local intent".into(),
                ));
            }
            let svn_rev = op.source_svn_rev;
            let operation = finalize_tx(tx, op, observed_git_sha, observed_git_tree, svn_rev)?;
            Ok(ReconciledGitPush {
                operation,
                finalized: true,
                resume_authorized: false,
            })
        })
    }

    pub fn confirm_svn_to_git_push(
        &self,
        repo_id: &str,
        op_id: &str,
        observed_git_sha: &str,
        observed_git_tree: &str,
    ) -> Result<GitPushOperation, DatabaseError> {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, repo_id)?;
            crate::writer_fence::require_current(tx)?;
            if read_value(tx, &key("active", repo_id))?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive svn-to-git push operation".into(),
                ));
            }
            let op =
                parse(&read_value(tx, &key("document", op_id))?.ok_or_else(|| {
                    DatabaseError::Other("missing svn-to-git push document".into())
                })?)?;
            if op.repo_id != repo_id || op.state != GitPushOperationState::Running {
                return Err(DatabaseError::Other(
                    "svn-to-git push is not running".into(),
                ));
            }
            let svn_rev = op.source_svn_rev;
            finalize_tx(tx, op, observed_git_sha, observed_git_tree, svn_rev)
        })
    }

    /// True when personal mode already has an applied SVN→Git receipt for this revision.
    pub fn has_personal_svn_to_git_receipt(
        &self,
        repo_id: &str,
        svn_rev: i64,
    ) -> Result<bool, DatabaseError> {
        let conn = self.conn();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sync_records
                WHERE repo_id = ?1 AND svn_rev = ?2 AND direction = 'svn_to_git' AND status = 'applied'
            )",
            params![repo_id, svn_rev],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    /// Personal-mode checkpoint: journal completion plus `commit_map` and watermark.
    pub fn confirm_personal_svn_to_git_push(
        &self,
        repo_id: &str,
        op_id: &str,
        observed_git_sha: &str,
        observed_git_tree: &str,
        watermark_source: &str,
        git_author: &str,
    ) -> Result<GitPushOperation, DatabaseError> {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, repo_id)?;
            crate::writer_fence::require_current(tx)?;
            if read_value(tx, &key("active", repo_id))?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive svn-to-git push operation".into(),
                ));
            }
            let op =
                parse(&read_value(tx, &key("document", op_id))?.ok_or_else(|| {
                    DatabaseError::Other("missing svn-to-git push document".into())
                })?)?;
            if op.repo_id != repo_id || op.state != GitPushOperationState::Running {
                return Err(DatabaseError::Other(
                    "svn-to-git push is not running".into(),
                ));
            }
            personal_finalize_tx(
                tx,
                op,
                observed_git_sha,
                observed_git_tree,
                watermark_source,
                git_author,
            )
        })
    }

    fn update_git_push_operation<F>(
        &self,
        repo_id: &str,
        op_id: &str,
        edit: F,
    ) -> Result<GitPushOperation, DatabaseError>
    where
        F: FnOnce(&mut GitPushOperation) -> Result<(), DatabaseError>,
    {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, repo_id)?;
            crate::writer_fence::require_current(tx)?;
            let active = key("active", repo_id);
            if read_value(tx, &active)?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive svn-to-git push operation".into(),
                ));
            }
            let document = key("document", op_id);
            let mut op =
                parse(&read_value(tx, &document)?.ok_or_else(|| {
                    DatabaseError::Other("missing svn-to-git push document".into())
                })?)?;
            if op.repo_id != repo_id {
                return Err(DatabaseError::Other(
                    "svn-to-git push repository mismatch".into(),
                ));
            }
            let original = op.clone();
            edit(&mut op)?;
            if op == original {
                return Ok(op);
            }
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            if op.state == GitPushOperationState::Completed {
                clear_active(tx, repo_id, op_id)?;
            }
            Ok(op)
        })
    }

    /// Unfinished SVN→Git pushes cannot be mistaken for completed work after restart.
    pub fn hold_interrupted_git_pushes(&self) -> Result<(), DatabaseError> {
        let active = {
            let conn = self.conn();
            let mut query = conn.prepare(
                "SELECT key,value FROM kv_state WHERE key LIKE 'svn_to_git_push_v1:active:%'",
            )?;
            let rows = query
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for (active_key, id) in active {
            let repo_id = active_key
                .strip_prefix(&key("active", ""))
                .ok_or_else(|| DatabaseError::Other("invalid active svn-to-git push key".into()))?;
            let op = self.get_git_push_operation(repo_id, &id)?.ok_or_else(|| {
                DatabaseError::Other("active svn-to-git push document missing or misowned".into())
            })?;
            if !op.state.is_terminal() {
                self.update_git_push_operation(repo_id, &op.id, |o| {
                    o.state = GitPushOperationState::ReconciliationRequired;
                    o.resume_authorized = false;
                    o.outcome_detail = Some(
                        "worker stopped before a verified SVN-to-Git result; inspect the exact Git ref"
                            .into(),
                    );
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
}

pub struct ReconciledGitPush {
    pub operation: GitPushOperation,
    pub finalized: bool,
    pub resume_authorized: bool,
}

pub struct GitPushIntent<'a> {
    pub repo_id: &'a str,
    pub initiator_id: &'a str,
    pub request_id: &'a str,
    pub target_fingerprint: &'a str,
    pub source_svn_rev: i64,
    pub source_svn_author: &'a str,
    pub source_svn_message: &'a str,
    pub pre_push_git_remote: &'a str,
    pub pre_push_git_branch: &'a str,
    pub pre_push_git_sha: &'a str,
    pub pre_push_git_tree: Option<&'a str>,
    pub intended_local_git_sha: &'a str,
    pub intended_local_git_parent: Option<&'a str>,
    pub intended_local_git_tree: &'a str,
}

impl GitPushOperation {
    /// Reconstruct the recorded intent so a resume re-uses the same operation.
    pub fn intent(&self) -> GitPushIntent<'_> {
        GitPushIntent {
            repo_id: &self.repo_id,
            initiator_id: &self.initiator_id,
            request_id: &self.request_id,
            target_fingerprint: &self.target_fingerprint,
            source_svn_rev: self.source_svn_rev,
            source_svn_author: &self.source_svn_author,
            source_svn_message: &self.source_svn_message,
            pre_push_git_remote: &self.pre_push_git_remote,
            pre_push_git_branch: &self.pre_push_git_branch,
            pre_push_git_sha: &self.pre_push_git_sha,
            pre_push_git_tree: self.pre_push_git_tree.as_deref(),
            intended_local_git_sha: &self.intended_local_git_sha,
            intended_local_git_parent: self.intended_local_git_parent.as_deref(),
            intended_local_git_tree: &self.intended_local_git_tree,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_intent<'a>() -> GitPushIntent<'a> {
        GitPushIntent {
            repo_id: "pair",
            initiator_id: "worker",
            request_id: "req-1",
            target_fingerprint: "fp",
            source_svn_rev: 3,
            source_svn_author: "dev",
            source_svn_message: "add feature",
            pre_push_git_remote: "origin",
            pre_push_git_branch: "main",
            pre_push_git_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            pre_push_git_tree: Some("cccccccccccccccccccccccccccccccccccccccc"),
            intended_local_git_sha: "dddddddddddddddddddddddddddddddddddddddd",
            intended_local_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            intended_local_git_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        }
    }

    #[test]
    fn persist_hold_and_confirm_with_observed_evidence() {
        let inbound_cursor = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let emitted_tip = "dddddddddddddddddddddddddddddddddddddddd";
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
                 VALUES ('pair','p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'old','idle',0,0)",
                [],
            )
            .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
            &db.conn(),
            "pair",
            inbound_cursor,
            &now,
        )
        .unwrap();
        let op = db.begin_svn_to_git_push(sample_intent()).unwrap();
        assert_eq!(op.state, GitPushOperationState::Running);
        let held = db
            .hold_svn_to_git_reconciliation("pair", &op.id, "lost reply")
            .unwrap();
        assert_eq!(held.state, GitPushOperationState::ReconciliationRequired);
        let resumed = db
            .authorize_svn_to_git_resume("pair", &op.id, "absent and unchanged")
            .unwrap();
        assert!(resumed.resume_authorized);
        let running = db.begin_svn_to_git_push(sample_intent()).unwrap();
        assert_eq!(running.id, op.id);
        assert_eq!(running.state, GitPushOperationState::Running);
        let done = db
            .confirm_svn_to_git_push(
                "pair",
                &op.id,
                "dddddddddddddddddddddddddddddddddddddddd",
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            )
            .unwrap();
        assert_eq!(done.state, GitPushOperationState::Completed);
        assert_eq!(
            done.last_confirmed_git_sha.as_deref(),
            Some("dddddddddddddddddddddddddddddddddddddddd")
        );
        assert_eq!(
            done.last_confirmed_git_tree.as_deref(),
            Some("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")
        );
        assert!(db.active_git_push_operation("pair").unwrap().is_none());
        let mapped: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id='pair' AND svn_rev=3 AND git_sha=?1",
                ["dddddddddddddddddddddddddddddddddddddddd"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mapped, 1);
        assert_eq!(
            db.get_repo_watermark("pair").unwrap(),
            (3, emitted_tip.into())
        );
        assert_eq!(
            crate::echo_receipt_scope::read_scoped_last_git_sha_kv(&db, "pair")
                .unwrap()
                .as_deref(),
            Some(inbound_cursor),
            "svn-to-git finalize must preserve scoped inbound git cursor kv"
        );
        assert_eq!(
            db.get_state("last_git_hash").unwrap(),
            None,
            "team-mode svn-to-git finalize must not write global last_git_hash"
        );
        let conn = db.conn();
        let scope = crate::db::repo_scope_identity::repository_scope_uuid(&conn, "pair").unwrap();
        let generation = crate::echo_receipt_scope::repo_echo_generation_tx(&conn, "pair").unwrap();
        let checkpoint = crate::db::lineage_checkpoints::load_directional_checkpoint(
            &conn,
            &scope,
            generation,
            crate::db::lineage_checkpoints::CheckpointDirection::SvnToGit,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            checkpoint.outcome,
            crate::db::lineage_checkpoints::MappingOutcome::AppliedAndVerified
        );
        assert_eq!(
            checkpoint.handled_source_key,
            crate::db::lineage_checkpoints::svn_source_key(3)
        );
    }

    #[test]
    fn malformed_generation_refuses_finalize_without_legacy_advancement() {
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
        db.set_state(
            &crate::db::repo_scope_identity::repo_echo_generation_kv_key(&scope),
            "not-a-number",
        )
        .unwrap();
        let op = db.begin_svn_to_git_push(sample_intent()).unwrap();
        let err = db.confirm_svn_to_git_push(
            "pair",
            &op.id,
            "dddddddddddddddddddddddddddddddddddddddd",
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        );
        assert!(err.is_err());
        let still = db.get_git_push_operation("pair", &op.id).unwrap().unwrap();
        assert_eq!(still.state, GitPushOperationState::Running);
        assert_eq!(db.get_repo_watermark("pair").unwrap().0, 2);
        let conn = db.conn();
        assert!(crate::db::lineage_checkpoints::load_directional_checkpoint(
            &conn,
            &scope,
            1,
            crate::db::lineage_checkpoints::CheckpointDirection::SvnToGit,
        )
        .unwrap()
        .is_none());
        let maps: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id='pair' AND svn_rev=3",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(maps, 0);
    }

    #[test]
    fn interrupted_running_push_is_held() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let op = db.begin_svn_to_git_push(sample_intent()).unwrap();
        db.hold_interrupted_git_pushes().unwrap();
        let held = db.get_git_push_operation("pair", &op.id).unwrap().unwrap();
        assert_eq!(held.state, GitPushOperationState::ReconciliationRequired);
        assert!(!held.resume_authorized);
    }

    #[test]
    fn personal_confirm_inserts_scoped_commit_map_when_managed_row_exists() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
                 VALUES (3, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'svn_to_git', 't', 'team', 'Team', 'managed-pair')",
                [],
            )
            .unwrap();
        let op = db.begin_svn_to_git_push(sample_intent()).unwrap();
        let done = db
            .confirm_personal_svn_to_git_push(
                "pair",
                &op.id,
                "dddddddddddddddddddddddddddddddddddddddd",
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                "svn_rev",
                "Dev <dev@example.com>",
            )
            .unwrap();
        assert_eq!(done.state, GitPushOperationState::Completed);
        let personal_rows: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM commit_map WHERE svn_rev=3 AND git_sha=?1 AND repo_id='pair'",
                ["dddddddddddddddddddddddddddddddddddddddd"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(personal_rows, 1);
    }

    #[test]
    fn personal_confirm_records_commit_map_and_watermark() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let op = db.begin_svn_to_git_push(sample_intent()).unwrap();
        let done = db
            .confirm_personal_svn_to_git_push(
                "pair",
                &op.id,
                "dddddddddddddddddddddddddddddddddddddddd",
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                "svn_rev",
                "Dev <dev@example.com>",
            )
            .unwrap();
        assert_eq!(done.state, GitPushOperationState::Completed);
        assert!(db.active_git_push_operation("pair").unwrap().is_none());
        let mapped: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM commit_map WHERE svn_rev=3 AND git_sha=?1",
                ["dddddddddddddddddddddddddddddddddddddddd"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mapped, 1);
        let receipt: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id='pair' AND svn_rev=3 AND git_sha=?1 AND direction='svn_to_git'",
                ["dddddddddddddddddddddddddddddddddddddddd"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipt, 1);
        assert_eq!(db.get_watermark("svn_rev").unwrap().as_deref(), Some("3"));
    }

    #[test]
    fn pr245_3_canonical_write_failure_is_atomic_after_reopen() {
        let _writer_fence_test_lock = crate::writer_fence::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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
        assert_eq!(native.bare_count, 2);
        assert_eq!(native.effect_parent, native.baseline_sha);
        crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
            &db.conn(),
            &fx.repo_id,
            &native.baseline_sha,
            &Utc::now().to_rfc3339(),
        )
        .expect("pre-push inbound cursor");
        let op = db
            .begin_svn_to_git_push(GitPushIntent {
                repo_id: &fx.repo_id,
                initiator_id: "pr245-3",
                request_id: &native.request_id,
                target_fingerprint: &native.fingerprint,
                source_svn_rev: native.svn_rev,
                source_svn_author: &native.svn_author,
                source_svn_message: &native.svn_message,
                pre_push_git_remote: "origin",
                pre_push_git_branch: "main",
                pre_push_git_sha: &native.baseline_sha,
                pre_push_git_tree: Some(&native.baseline_tree),
                intended_local_git_sha: &native.effect_sha,
                intended_local_git_parent: Some(&native.effect_parent),
                intended_local_git_tree: &native.effect_tree,
            })
            .expect("journal the observed push");
        Uuid::parse_str(&op.id).expect("operation id is a real uuid");
        assert_ne!(op.id, fx.repo_id);
        let held = db
            .hold_svn_to_git_reconciliation(
                &fx.repo_id,
                &op.id,
                "native git result retained; canonical write not recorded",
            )
            .expect("hold for reconciliation");
        assert_eq!(held.state, GitPushOperationState::ReconciliationRequired);
        let source_key = crate::db::lineage_checkpoints::svn_source_key(native.svn_rev);
        pr245_3_install_abort(
            &db,
            "pr245_3_svn_to_git_canonical_write",
            &scope,
            generation,
            "svn_to_git",
            &source_key,
            "pr245-3-svn-to-git-canonical-write-abort",
        );
        let failed = db.finalize_verified_svn_to_git_push(
            &fx.repo_id,
            &op.id,
            &native.effect_sha,
            &native.effect_tree,
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
                .contains("pr245-3-svn-to-git-canonical-write-abort"),
            "{failed}"
        );
        pr245_3_assert_native(&fx, &native);
        pr245_3_assert_unresolved(&db, &fx.repo_id, &op.id, &native, &scope, generation);
        let (db, owner) = pr245_3_reopen(&fx.data_dir, db, owner);
        pr245_3_assert_owned_file(&db, &fx.data_dir);
        pr245_3_assert_native(&fx, &native);
        pr245_3_assert_unresolved(&db, &fx.repo_id, &op.id, &native, &scope, generation);
        assert_eq!(
            pr245_3_trigger_count(&db, "pr245_3_svn_to_git_canonical_write"),
            2,
            "named fault must still be the persisted trigger after reopen"
        );
        pr245_3_drop_abort(&db, "pr245_3_svn_to_git_canonical_write");
        assert_eq!(
            pr245_3_trigger_count(&db, "pr245_3_svn_to_git_canonical_write"),
            0
        );
        let done = db
            .finalize_verified_svn_to_git_push(
                &fx.repo_id,
                &op.id,
                &native.effect_sha,
                &native.effect_tree,
                &native.fingerprint,
            )
            .expect("same observed push finalizes after the named fault is gone");
        assert!(done.finalized);
        assert!(!done.resume_authorized);
        pr245_3_assert_native(&fx, &native);
        pr245_3_assert_resolved(&db, &fx.repo_id, &op.id, &native, &scope, generation);
        let (db, owner) = pr245_3_reopen(&fx.data_dir, db, owner);
        pr245_3_assert_native(&fx, &native);
        pr245_3_assert_resolved(&db, &fx.repo_id, &op.id, &native, &scope, generation);
        let repeated = db.finalize_verified_svn_to_git_push(
            &fx.repo_id,
            &op.id,
            &native.effect_sha,
            &native.effect_tree,
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
        assert!(!repeated.contains("pr245-3-svn-to-git-canonical-write-abort"));
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
        bare: std::path::PathBuf,
        work: std::path::PathBuf,
        repo_id: String,
    }

    struct Pr2453Native {
        request_id: String,
        svn_rev: i64,
        svn_author: String,
        svn_message: String,
        baseline_sha: String,
        baseline_tree: String,
        effect_sha: String,
        effect_tree: String,
        effect_parent: String,
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
        std::fs::create_dir_all(&home).unwrap();
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
            bare,
            work,
            repo_id,
        }
    }

    fn pr245_3_native_effect(fx: &Pr2453Fixture) -> Pr2453Native {
        let feature = b"feature\n".to_vec();
        let seed = fx._root.path().join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::write(seed.join("feature.txt"), &feature).unwrap();
        let trunk = format!("{}/trunk", fx.svn_root);
        pr245_3_svn(
            fx,
            &["import", seed.to_str().unwrap(), &trunk, "-m", "feature"],
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
        let svn_message = pr245_3_run(
            "svnlook",
            &["log", "-r", "1", fx.svn_repo.to_str().unwrap()],
            None,
            &fx.home,
        );
        let cat = pr245_3_stdout(
            "svnlook",
            &[
                "cat",
                "-r",
                "1",
                fx.svn_repo.to_str().unwrap(),
                "trunk/feature.txt",
            ],
            None,
            &fx.home,
        );
        assert_eq!(cat, feature);
        assert!(!svn_author.is_empty());
        assert_eq!(svn_message, "feature");
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
        std::fs::write(fx.work.join("base.txt"), b"base\n").unwrap();
        pr245_3_run("git", &["add", "base.txt"], Some(&fx.work), &fx.home);
        pr245_3_run(
            "git",
            &["commit", "-m", "baseline"],
            Some(&fx.work),
            &fx.home,
        );
        let baseline_sha = pr245_3_run("git", &["rev-parse", "HEAD"], Some(&fx.work), &fx.home);
        let baseline_tree = pr245_3_run(
            "git",
            &["rev-parse", "HEAD^{tree}"],
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
        let effect_sha = pr245_3_run("git", &["rev-parse", "HEAD"], Some(&fx.work), &fx.home);
        let effect_tree = pr245_3_run(
            "git",
            &["rev-parse", "HEAD^{tree}"],
            Some(&fx.work),
            &fx.home,
        );
        let effect_parent = pr245_3_run("git", &["rev-parse", "HEAD^"], Some(&fx.work), &fx.home);
        assert_eq!(effect_parent, baseline_sha);
        assert_ne!(effect_sha, baseline_sha);
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
        let bare_tip = pr245_3_run(
            "git",
            &[
                "--git-dir",
                fx.bare.to_str().unwrap(),
                "rev-parse",
                "refs/heads/main",
            ],
            None,
            &fx.home,
        );
        assert_eq!(bare_tip, effect_sha);
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
        let fingerprint = hex::encode(Sha256::digest(
            format!("{}\n{}", fx.svn_uuid, fx.bare.display()).as_bytes(),
        ));
        Pr2453Native {
            request_id: Uuid::new_v4().to_string(),
            svn_rev,
            svn_author,
            svn_message,
            baseline_sha,
            baseline_tree,
            effect_sha,
            effect_tree,
            effect_parent,
            fingerprint,
            bare_count,
            feature,
        }
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
                 WHERE repo_id=?1 AND git_sha=?2 AND svn_rev=?3 AND direction='svn_to_git'",
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
            native.effect_sha
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
        assert_eq!(
            pr245_3_run(
                "git",
                &[
                    "--git-dir",
                    fx.bare.to_str().unwrap(),
                    "rev-parse",
                    "refs/heads/main^"
                ],
                None,
                &fx.home
            ),
            native.baseline_sha
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
            native.effect_tree
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
            .get_git_push_operation(repo_id, op_id)
            .unwrap()
            .expect("held operation");
        assert_eq!(op.state, GitPushOperationState::ReconciliationRequired);
        assert!(!op.resume_authorized);
        assert_eq!(op.intended_local_git_sha, native.effect_sha);
        assert_eq!(op.source_svn_rev, native.svn_rev);
        assert!(op.last_confirmed_git_sha.is_none());
        assert_eq!(
            db.active_git_push_operation(repo_id).unwrap().unwrap().id,
            op_id
        );
        assert_eq!(pr245_3_repo_row(db, repo_id), (0, String::new(), 0));
        assert_eq!(
            pr245_3_mapping_count(db, repo_id, &native.effect_sha, native.svn_rev),
            0
        );
        assert_eq!(
            pr245_3_scoped_git(db, repo_id).as_deref(),
            Some(native.baseline_sha.as_str())
        );
        assert!(db.get_state("last_git_hash").unwrap().is_none());
        let source_key = crate::db::lineage_checkpoints::svn_source_key(native.svn_rev);
        let conn = db.conn();
        assert!(crate::db::lineage_checkpoints::load_directional_checkpoint(
            &conn,
            scope,
            generation,
            crate::db::lineage_checkpoints::CheckpointDirection::SvnToGit,
        )
        .unwrap()
        .is_none());
        assert!(crate::db::lineage_checkpoints::load_mapping_outcome(
            &conn,
            scope,
            generation,
            crate::db::lineage_checkpoints::CheckpointDirection::SvnToGit,
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
            .get_git_push_operation(repo_id, op_id)
            .unwrap()
            .expect("completed operation");
        assert_eq!(op.state, GitPushOperationState::Completed);
        assert!(!op.resume_authorized);
        assert_eq!(
            op.last_confirmed_git_sha.as_deref(),
            Some(native.effect_sha.as_str())
        );
        assert_eq!(
            op.last_confirmed_git_tree.as_deref(),
            Some(native.effect_tree.as_str())
        );
        assert_eq!(op.source_svn_rev, native.svn_rev);
        assert!(db.active_git_push_operation(repo_id).unwrap().is_none());
        assert_eq!(
            pr245_3_repo_row(db, repo_id),
            (native.svn_rev, native.effect_sha.clone(), 1)
        );
        assert_eq!(
            pr245_3_mapping_count(db, repo_id, &native.effect_sha, native.svn_rev),
            1
        );
        assert_eq!(
            db.get_state(&format!("last_svn_rev_{repo_id}"))
                .unwrap()
                .as_deref(),
            Some(native.svn_rev.to_string().as_str())
        );
        assert_eq!(
            pr245_3_scoped_git(db, repo_id).as_deref(),
            Some(native.baseline_sha.as_str())
        );
        assert!(db.get_state("last_git_hash").unwrap().is_none());
        let source_key = crate::db::lineage_checkpoints::svn_source_key(native.svn_rev);
        let conn = db.conn();
        let checkpoint = crate::db::lineage_checkpoints::load_directional_checkpoint(
            &conn,
            scope,
            generation,
            crate::db::lineage_checkpoints::CheckpointDirection::SvnToGit,
        )
        .unwrap()
        .expect("directional checkpoint");
        assert_eq!(checkpoint.scope_uuid, scope);
        assert_eq!(checkpoint.generation, generation);
        assert_eq!(
            checkpoint.direction,
            crate::db::lineage_checkpoints::CheckpointDirection::SvnToGit
        );
        assert_eq!(checkpoint.handled_source_key, source_key);
        assert_eq!(
            checkpoint.outcome,
            crate::db::lineage_checkpoints::MappingOutcome::AppliedAndVerified
        );
        assert_eq!(
            checkpoint.target_git_sha.as_deref(),
            Some(native.effect_sha.as_str())
        );
        let mapping = crate::db::lineage_checkpoints::load_mapping_outcome(
            &conn,
            scope,
            generation,
            crate::db::lineage_checkpoints::CheckpointDirection::SvnToGit,
            &source_key,
        )
        .unwrap()
        .expect("mapping outcome");
        assert_eq!(
            mapping.outcome,
            crate::db::lineage_checkpoints::MappingOutcome::AppliedAndVerified
        );
        assert_eq!(
            mapping.target_git_sha.as_deref(),
            Some(native.effect_sha.as_str())
        );
        drop(conn);
        // The fixture is re-read by the caller through pr245_3_assert_native; this
        // assertion is the repository-side half of the same native identity.
        assert_eq!(op.source_svn_author, native.svn_author);
        assert_eq!(op.source_svn_message, native.svn_message);
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
        String::from_utf8(output.stdout).unwrap().trim().to_string()
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
