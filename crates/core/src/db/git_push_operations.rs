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
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM commit_map WHERE svn_rev = ?1)",
        params![svn_rev],
        |row| row.get(0),
    )?;
    if !exists {
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author)
             VALUES (?1, ?2, 'svn_to_git', ?3, ?4, ?5)",
            params![svn_rev, git_sha, now, op.source_svn_author, git_author],
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
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
                 VALUES ('pair','p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'old','idle',0,0)",
                [],
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
            (3, "dddddddddddddddddddddddddddddddddddddddd".into())
        );
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
        assert_eq!(db.get_watermark("svn_rev").unwrap().as_deref(), Some("3"));
    }
}
