//! Bounded v12 operation journal for per-repository full imports.
//! The active pointer and versioned document always change in one SQLite transaction.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;

use super::Database;
use crate::errors::DatabaseError;
use crate::models::Repository;

const PREFIX: &str = "import_operation_v1:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportOperationState {
    Queued,
    Running,
    CancelRequested,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
    ReconciliationRequired,
}

impl ImportOperationState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Cancelled | Self::Failed | Self::ReconciliationRequired
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportOperation {
    pub version: u8,
    pub id: String,
    pub repo_id: String,
    pub operation_type: String,
    pub initiator_id: String,
    pub request_id: String,
    pub target_fingerprint: String,
    pub created_at: String,
    pub updated_at: String,
    pub state: ImportOperationState,
    pub cancel_requested: bool,
    #[serde(default)]
    pub processed_revisions: u64,
    #[serde(default)]
    pub total_revisions: Option<u64>,
    #[serde(default)]
    pub local_commits: u64,
    #[serde(default)]
    pub confirmed_batches: u64,
    pub last_local_svn_rev: Option<i64>,
    pub last_local_git_sha: Option<String>,
    pub last_confirmed_svn_rev: Option<i64>,
    pub last_confirmed_git_sha: Option<String>,
    pub intended_ref: Option<String>,
    pub intended_git_sha: Option<String>,
    pub outcome_detail: Option<String>,
}

// This is the original v1 fingerprint vocabulary. Keep its JSON representation
// stable so operations written before #64-B remain verifiable after restart.
struct ImportTargetSettings {
    svn_url: String,
    svn_branch: String,
    git_api_url: String,
    git_repo: String,
    git_branch: String,
    allowed_paths: Option<String>,
    blocked_patterns: Option<String>,
    lfs_threshold_mb: i64,
    sync_mode: String,
    auto_merge: bool,
}

impl ImportTargetSettings {
    fn from_repo(repo: &Repository) -> Self {
        Self {
            svn_url: repo.svn_url.clone(),
            svn_branch: repo.svn_branch.clone(),
            git_api_url: repo.git_api_url.clone(),
            git_repo: repo.git_repo.clone(),
            git_branch: repo.git_branch.clone(),
            allowed_paths: repo.allowed_paths.clone(),
            blocked_patterns: repo.blocked_patterns.clone(),
            lfs_threshold_mb: repo.lfs_threshold_mb,
            sync_mode: repo.sync_mode.clone(),
            auto_merge: repo.auto_merge,
        }
    }

    fn fingerprint(&self, workdir: &Path) -> String {
        let source = serde_json::json!({
            "svn_url":self.svn_url, "svn_branch":self.svn_branch,
            "git_api_url":self.git_api_url, "git_repo":self.git_repo,
            "git_branch":self.git_branch,
            "workdir":workdir.display().to_string(),
            "allowed_paths":self.allowed_paths,
            "blocked_patterns":self.blocked_patterns,
            "lfs_threshold_mb":self.lfs_threshold_mb,
            "sync_mode":self.sync_mode, "auto_merge":self.auto_merge,
        })
        .to_string();
        hex::encode(Sha256::digest(source.as_bytes()))
    }
}

pub fn import_target_fingerprint(repo: &Repository, workdir: &Path) -> String {
    ImportTargetSettings::from_repo(repo).fingerprint(workdir)
}

fn transaction_target_fingerprint(
    tx: &Connection,
    repo_id: &str,
    workdir: &Path,
) -> Result<Option<(String, String)>, DatabaseError> {
    let settings = tx.query_row(
        "SELECT svn_url,svn_branch,git_api_url,git_repo,git_branch,allowed_paths,blocked_patterns,lfs_threshold_mb,sync_mode,auto_merge FROM repositories WHERE id=?1",
        [repo_id],
        |row| Ok(ImportTargetSettings {
            svn_url: row.get(0)?, svn_branch: row.get(1)?,
            git_api_url: row.get(2)?, git_repo: row.get(3)?, git_branch: row.get(4)?,
            allowed_paths: row.get(5)?, blocked_patterns: row.get(6)?,
            lfs_threshold_mb: row.get(7)?, sync_mode: row.get(8)?,
            auto_merge: row.get::<_, i64>(9)? != 0,
        }),
    ).optional()?;
    Ok(settings.map(|settings| {
        let reference = format!("refs/heads/{}", settings.git_branch);
        (settings.fingerprint(workdir), reference)
    }))
}

pub struct ReconciledImport {
    pub operation: ImportOperation,
    pub publication_recorded: bool,
    pub completed: bool,
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

fn parse(raw: &str) -> Result<ImportOperation, DatabaseError> {
    let op: ImportOperation = serde_json::from_str(raw)
        .map_err(|e| DatabaseError::Other(format!("invalid import operation: {e}")))?;
    if op.version != 1 {
        return Err(DatabaseError::Other(
            "unsupported import operation version".into(),
        ));
    }
    Ok(op)
}

fn complete_import_tx(
    tx: &Connection,
    repo_id: &str,
    op_id: &str,
    mut op: ImportOperation,
    svn_rev: i64,
    sha: &str,
) -> Result<ImportOperation, DatabaseError> {
    if op.intended_git_sha.is_some()
        || op.intended_ref.is_some()
        || op.last_local_svn_rev != Some(svn_rev)
        || op.last_confirmed_svn_rev != Some(svn_rev)
        || op.last_local_git_sha.as_deref() != Some(sha)
        || op.last_confirmed_git_sha.as_deref() != Some(sha)
    {
        return Err(DatabaseError::Other(
            "import lacks a fully confirmed final tip".into(),
        ));
    }
    if tx.execute(
        "UPDATE repositories SET last_svn_rev=?1,last_git_sha=?2,last_sync_at=datetime('now') WHERE id=?3",
        params![svn_rev, sha, repo_id],
    )? != 1 {
        return Err(DatabaseError::Other(
            "repository disappeared during finalization".into(),
        ));
    }
    write_value(tx, &format!("last_svn_rev_{repo_id}"), &svn_rev.to_string())?;
    write_value(tx, &format!("last_git_sha_{repo_id}"), sha)?;
    op.state = ImportOperationState::Completed;
    op.updated_at = Utc::now().to_rfc3339();
    write_value(
        tx,
        &key("document", op_id),
        &serde_json::to_string(&op).unwrap(),
    )?;
    tx.execute(
        "DELETE FROM kv_state WHERE key=?1 AND value=?2",
        params![key("active", repo_id), op_id],
    )?;
    Ok(op)
}

impl Database {
    pub fn has_any_active_import_operation(&self) -> Result<bool, DatabaseError> {
        let conn = self.conn();
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM kv_state WHERE key LIKE 'import_operation_v1:active:%')",
            [],
            |r| r.get::<_, i64>(0),
        )? != 0)
    }
    pub fn create_import_operation(
        &self,
        repo_id: &str,
        initiator_id: &str,
        request_id: &str,
        fingerprint: &str,
    ) -> Result<ImportOperation, DatabaseError> {
        self.transaction(|tx| {
            let active = key("active", repo_id);
            if read_value(tx, &active)?.is_some() {
                return Err(DatabaseError::Other(
                    "repository has an active or held import operation".into(),
                ));
            }
            let now = Utc::now().to_rfc3339();
            let op = ImportOperation {
                version: 1,
                id: Uuid::new_v4().to_string(),
                repo_id: repo_id.into(),
                operation_type: "full_import".into(),
                initiator_id: initiator_id.into(),
                request_id: request_id.into(),
                target_fingerprint: fingerprint.into(),
                created_at: now.clone(),
                updated_at: now,
                state: ImportOperationState::Queued,
                cancel_requested: false,
                processed_revisions: 0,
                total_revisions: None,
                local_commits: 0,
                confirmed_batches: 0,
                last_local_svn_rev: None,
                last_local_git_sha: None,
                last_confirmed_svn_rev: None,
                last_confirmed_git_sha: None,
                intended_ref: None,
                intended_git_sha: None,
                outcome_detail: None,
            };
            write_value(
                tx,
                &key("document", &op.id),
                &serde_json::to_string(&op).unwrap(),
            )?;
            write_value(tx, &active, &op.id)?;
            write_value(tx, &key("latest", repo_id), &op.id)?;
            Ok(op)
        })
    }

    pub fn get_import_operation(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> Result<Option<ImportOperation>, DatabaseError> {
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

    pub fn latest_import_operation(
        &self,
        repo_id: &str,
    ) -> Result<Option<ImportOperation>, DatabaseError> {
        let id = self.get_state(&key("latest", repo_id))?;
        id.map(|id| self.get_import_operation(repo_id, &id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn active_import_operation(
        &self,
        repo_id: &str,
    ) -> Result<Option<ImportOperation>, DatabaseError> {
        let id = self.get_state(&key("active", repo_id))?;
        id.map(|id| self.get_import_operation(repo_id, &id))
            .transpose()
            .map(Option::flatten)
    }

    /// Compare-and-update exact active identity; closure runs under the transaction.
    fn update_import_operation<F>(
        &self,
        repo_id: &str,
        op_id: &str,
        edit: F,
    ) -> Result<ImportOperation, DatabaseError>
    where
        F: FnOnce(&mut ImportOperation) -> Result<(), DatabaseError>,
    {
        self.transaction(|tx| {
            let active = key("active", repo_id);
            if read_value(tx, &active)?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive import operation".into(),
                ));
            }
            let document = key("document", op_id);
            let mut op = parse(&read_value(tx, &document)?.ok_or_else(|| {
                DatabaseError::Other("missing import operation document".into())
            })?)?;
            if op.repo_id != repo_id {
                return Err(DatabaseError::Other(
                    "import operation repository mismatch".into(),
                ));
            }
            let original = op.clone();
            edit(&mut op)?;
            if op == original {
                return Ok(op);
            }
            op.updated_at = Utc::now().to_rfc3339();
            write_value(tx, &document, &serde_json::to_string(&op).unwrap())?;
            if op.state == ImportOperationState::Completed {
                tx.execute(
                    "DELETE FROM kv_state WHERE key=?1 AND value=?2",
                    params![active, op_id],
                )?;
            }
            Ok(op)
        })
    }

    pub fn request_import_cancel(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> Result<ImportOperation, DatabaseError> {
        if let Some(op) = self.get_import_operation(repo_id, op_id)? {
            if op.state.is_terminal() {
                return Ok(op);
            }
        }
        self.update_import_operation(repo_id, op_id, |op| {
            if op.state.is_terminal() {
                return Ok(());
            }
            op.cancel_requested = true;
            op.state = ImportOperationState::CancelRequested;
            Ok(())
        })
    }

    pub fn start_import_operation(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> Result<ImportOperation, DatabaseError> {
        self.update_import_operation(repo_id, op_id, |op| {
            if op.state != ImportOperationState::Queued {
                return Err(DatabaseError::Other("import no longer queued".into()));
            }
            op.state = ImportOperationState::Running;
            Ok(())
        })
    }

    pub fn note_import_local(
        &self,
        repo_id: &str,
        op_id: &str,
        svn_rev: i64,
        sha: &str,
        processed_revisions: u64,
        local_commits: u64,
    ) -> Result<ImportOperation, DatabaseError> {
        self.update_import_operation(repo_id, op_id, |op| {
            if !matches!(
                op.state,
                ImportOperationState::Running | ImportOperationState::CancelRequested
            ) {
                return Err(DatabaseError::Other("import is not running".into()));
            }
            if op.last_local_svn_rev.is_some_and(|old| svn_rev <= old) {
                return Err(DatabaseError::Other(
                    "non-forward local import revision".into(),
                ));
            }
            op.last_local_svn_rev = Some(svn_rev);
            op.last_local_git_sha = Some(sha.into());
            op.processed_revisions = processed_revisions;
            op.local_commits = local_commits;
            Ok(())
        })
    }

    pub fn note_import_total(
        &self,
        repo_id: &str,
        op_id: &str,
        total: u64,
    ) -> Result<(), DatabaseError> {
        self.update_import_operation(repo_id, op_id, |op| {
            if !matches!(
                op.state,
                ImportOperationState::Running | ImportOperationState::CancelRequested
            ) {
                return Err(DatabaseError::Other("import is not running".into()));
            }
            op.total_revisions = Some(total);
            Ok(())
        })?;
        Ok(())
    }

    pub fn begin_import_publication(
        &self,
        repo_id: &str,
        op_id: &str,
        git_ref: &str,
        sha: &str,
    ) -> Result<(), DatabaseError> {
        self.update_import_operation(repo_id, op_id, |op| {
            if op.state != ImportOperationState::Running
                || op.cancel_requested
                || op.intended_git_sha.is_some()
            {
                return Err(DatabaseError::Other(
                    "publication cannot start in this state".into(),
                ));
            }
            if op.last_local_git_sha.as_deref() != Some(sha) {
                return Err(DatabaseError::Other(
                    "publication SHA is not the recorded local tip".into(),
                ));
            }
            op.intended_ref = Some(git_ref.into());
            op.intended_git_sha = Some(sha.into());
            Ok(())
        })?;
        Ok(())
    }

    pub fn confirm_import_publication(
        &self,
        repo_id: &str,
        op_id: &str,
        observed_sha: &str,
    ) -> Result<(), DatabaseError> {
        self.update_import_operation(repo_id, op_id, |op| {
            if op.intended_git_sha.as_deref() != Some(observed_sha) {
                return Err(DatabaseError::Other(
                    "published ref differs from recorded intent".into(),
                ));
            }
            op.last_confirmed_svn_rev = op.last_local_svn_rev;
            op.last_confirmed_git_sha = Some(observed_sha.into());
            op.confirmed_batches += 1;
            op.intended_ref = None;
            op.intended_git_sha = None;
            Ok(())
        })?;
        Ok(())
    }

    pub fn finish_import_operation(
        &self,
        repo_id: &str,
        op_id: &str,
        state: ImportOperationState,
        detail: &str,
    ) -> Result<ImportOperation, DatabaseError> {
        if !matches!(
            state,
            ImportOperationState::Cancelled
                | ImportOperationState::Failed
                | ImportOperationState::ReconciliationRequired
        ) {
            return Err(DatabaseError::Other("invalid held terminal state".into()));
        }
        self.update_import_operation(repo_id, op_id, |op| {
            if op.state.is_terminal() {
                if op.state == state {
                    return Ok(());
                }
                return Err(DatabaseError::Other(
                    "terminal import outcome cannot be rewritten".into(),
                ));
            }
            op.state = state;
            op.outcome_detail = Some(detail.into());
            Ok(())
        })
    }

    /// Complete the operation and both per-repository legacy cursor copies atomically.
    pub fn complete_import_operation(
        &self,
        repo_id: &str,
        op_id: &str,
        svn_rev: i64,
        sha: &str,
    ) -> Result<ImportOperation, DatabaseError> {
        self.transaction(|tx| {
            let active = key("active", repo_id);
            if read_value(tx, &active)?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive import operation".into(),
                ));
            }
            let document = key("document", op_id);
            let op = parse(
                &read_value(tx, &document)?
                    .ok_or_else(|| DatabaseError::Other("missing operation document".into()))?,
            )?;
            if op.repo_id != repo_id
                || !matches!(
                    op.state,
                    ImportOperationState::Running | ImportOperationState::CancelRequested
                )
            {
                return Err(DatabaseError::Other(
                    "import lacks a fully confirmed final tip".into(),
                ));
            }
            complete_import_tx(tx, repo_id, op_id, op, svn_rev, sha)
        })
    }

    /// Record only evidence from a fresh exact-ref read. The caller must hold
    /// the process-wide repo busy slot throughout local/remote inspection and
    /// this transaction. No external command is issued here.
    pub fn reconcile_verified_import(
        &self,
        repo_id: &str,
        op_id: &str,
        workdir: &Path,
        reference: &str,
        observed_sha: &str,
    ) -> Result<ReconciledImport, DatabaseError> {
        self.transaction(|tx| {
            if read_value(tx, &key("active", repo_id))?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other("stale or inactive import operation".into()));
            }
            let document = key("document", op_id);
            let mut op = parse(&read_value(tx, &document)?.ok_or_else(|| {
                DatabaseError::Other("missing import operation document".into())
            })?)?;
            if op.repo_id != repo_id
                || op.operation_type != "full_import"
                || op.state != ImportOperationState::ReconciliationRequired
            {
                return Err(DatabaseError::Other("operation is not an active reconciliation hold".into()));
            }
            let (fingerprint, configured_ref) = transaction_target_fingerprint(tx, repo_id, workdir)?
                .ok_or_else(|| DatabaseError::Other("repository disappeared".into()))?;
            if op.target_fingerprint.is_empty()
                || op.target_fingerprint != fingerprint
                || configured_ref != reference
            {
                return Err(DatabaseError::Other("import target fingerprint changed".into()));
            }
            if observed_sha.len() != 40 || !observed_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(DatabaseError::Other("remote did not return a full Git SHA".into()));
            }
            let (checkpoint, checkpoint_sha, last_sync_at): (i64, String, Option<String>) = tx.query_row(
                "SELECT last_svn_rev,last_git_sha,last_sync_at FROM repositories WHERE id=?1",
                [repo_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if checkpoint != 0 || !checkpoint_sha.is_empty() || last_sync_at.is_some()
                || read_value(tx, &format!("last_svn_rev_{repo_id}"))?.is_some_and(|v| v != "0")
                || read_value(tx, &format!("last_git_sha_{repo_id}"))?.is_some_and(|v| !v.is_empty()) {
                return Err(DatabaseError::Other("repository checkpoint changed during held import".into()));
            }
            let svn_rev = op.last_local_svn_rev.ok_or_else(|| DatabaseError::Other("missing local import revision".into()))?;
            let total = op.total_revisions.ok_or_else(|| DatabaseError::Other("missing import revision total".into()))?;
            if svn_rev <= 0 || total == 0 || op.processed_revisions == 0
                || op.processed_revisions > total || op.local_commits != op.processed_revisions
                || op.last_local_git_sha.as_deref() != Some(observed_sha) {
                return Err(DatabaseError::Other("remote SHA is not the recorded local import tip".into()));
            }
            if op.last_confirmed_svn_rev.is_some() != op.last_confirmed_git_sha.is_some() {
                return Err(DatabaseError::Other("incomplete prior publication receipt".into()));
            }
            let publication_recorded = match (&op.intended_ref, &op.intended_git_sha) {
                (Some(intent_ref), Some(intent_sha)) if intent_ref == reference && intent_sha == observed_sha
                    && op.last_confirmed_svn_rev.is_none_or(|rev| rev < svn_rev)
                    && op.last_confirmed_git_sha.as_deref() != Some(observed_sha) => {
                    op.last_confirmed_svn_rev = Some(svn_rev);
                    op.last_confirmed_git_sha = Some(observed_sha.into());
                    op.confirmed_batches = op.confirmed_batches.checked_add(1)
                        .ok_or_else(|| DatabaseError::Other("invalid publication counter".into()))?;
                    op.intended_ref = None;
                    op.intended_git_sha = None;
                    true
                }
                (None, None) if op.confirmed_batches > 0 && op.last_confirmed_svn_rev == Some(svn_rev)
                    && op.last_confirmed_git_sha.as_deref() == Some(observed_sha) => false,
                _ => return Err(DatabaseError::Other("remote SHA differs from publication evidence".into())),
            };
            let complete = op.processed_revisions == total;
            if complete {
                op.outcome_detail = Some("Import completed after remote verification".into());
                let operation = complete_import_tx(tx, repo_id, op_id, op, svn_rev, observed_sha)?;
                return Ok(ReconciledImport { operation, publication_recorded, completed: true });
            }
            op.outcome_detail = Some("Publication verified. Import is still partial and remains held; safe resume is not yet implemented.".into());
            op.updated_at = Utc::now().to_rfc3339();
            write_value(tx, &document, &serde_json::to_string(&op).unwrap())?;
            Ok(ReconciledImport { operation: op, publication_recorded, completed: false })
        })
    }

    pub fn note_import_reconciliation_reason(
        &self,
        repo_id: &str,
        op_id: &str,
        reason: &str,
    ) -> Result<ImportOperation, DatabaseError> {
        self.update_import_operation(repo_id, op_id, |op| {
            if op.state != ImportOperationState::ReconciliationRequired {
                return Err(DatabaseError::Other(
                    "operation is not an active reconciliation hold".into(),
                ));
            }
            op.outcome_detail = Some(reason.into());
            Ok(())
        })
    }

    /// Unfinished jobs cannot be mistaken for completed jobs after process restart.
    pub fn hold_interrupted_imports(&self) -> Result<(), DatabaseError> {
        let active = {
            let conn = self.conn();
            let mut query = conn.prepare(
                "SELECT key,value FROM kv_state WHERE key LIKE 'import_operation_v1:active:%'",
            )?;
            let rows = query
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for (active_key, id) in active {
            let repo_id = active_key
                .strip_prefix(&key("active", ""))
                .ok_or_else(|| DatabaseError::Other("invalid active import key".into()))?;
            let op = self.get_import_operation(repo_id, &id)?.ok_or_else(|| {
                DatabaseError::Other("active import document missing or misowned".into())
            })?;
            if !op.state.is_terminal() {
                self.update_import_operation(repo_id, &op.id, |o| {
                    o.state = ImportOperationState::ReconciliationRequired;
                    o.outcome_detail = Some("worker stopped before a verified terminal result; inspect local and remote refs".into());
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_identity_cancel_and_reopen_hold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let db = Database::new(&path).unwrap();
        db.initialize().unwrap();
        let first = db
            .create_import_operation("repo-a", "legacy", "request-a", "fingerprint")
            .unwrap();
        assert!(db
            .create_import_operation("repo-a", "legacy", "request-b", "fingerprint")
            .is_err());
        assert!(db.request_import_cancel("repo-b", &first.id).is_err());
        assert!(db.request_import_cancel("repo-a", "stale").is_err());
        assert_eq!(
            db.request_import_cancel("repo-a", &first.id).unwrap().state,
            ImportOperationState::CancelRequested
        );
        assert_eq!(
            db.request_import_cancel("repo-a", &first.id).unwrap().state,
            ImportOperationState::CancelRequested
        );
        drop(db);
        let reopened = Database::new(&path).unwrap();
        reopened.initialize().unwrap();
        reopened.hold_interrupted_imports().unwrap();
        let op = reopened.latest_import_operation("repo-a").unwrap().unwrap();
        assert_eq!(op.state, ImportOperationState::ReconciliationRequired);
        assert!(reopened
            .active_import_operation("repo-a")
            .unwrap()
            .is_some());
    }
}
