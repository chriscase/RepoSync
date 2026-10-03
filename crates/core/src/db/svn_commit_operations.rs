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
        write_value(
            tx,
            &format!("last_git_sha_{}", op.repo_id),
            &op.source_git_sha,
        )?;
        write_value(tx, "last_git_hash", &op.source_git_sha)?;
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
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM commit_map WHERE git_sha = ?1 AND direction = 'git_to_svn')",
        params![op.source_git_sha],
        |row| row.get(0),
    )?;
    if !exists {
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author)
             VALUES (?1, ?2, 'git_to_svn', ?3, ?4, ?5)",
            params![svn_rev, op.source_git_sha, now, op.author, git_author],
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
}
