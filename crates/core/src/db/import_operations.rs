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

/// Pinned SVN snapshot identity recorded once on a snapshot import.
/// Old v1 import documents omit this field and deserialize as `None`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotPin {
    pub svn_uuid: String,
    pub canonical_url: String,
    pub operative_rev: i64,
    pub peg_rev: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_from_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_from_rev: Option<i64>,
    /// Original request: `HEAD` or an explicit numeric revision.
    pub requested: String,
}

impl SnapshotPin {
    pub fn history_boundary(&self) -> String {
        format!(
            "SVN history before r{} was not imported; later revisions remain pending",
            self.operative_rev
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
    /// When true, an admin may resume a held partial import from the confirmed checkpoint.
    #[serde(default)]
    pub resume_authorized: bool,
    /// Present for snapshot imports. Absent on pre-#68 full-import documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_pin: Option<SnapshotPin>,
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

#[derive(Debug)]
pub struct ReconciledImport {
    pub operation: ImportOperation,
    pub publication_recorded: bool,
    pub completed: bool,
    pub resume_authorized: bool,
}

/// Repository-owned import baseline authority for team mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoImportBaseline {
    /// No verified baseline; the repository may start a new import.
    Pending,
    /// Verified import baseline from repository columns and scoped kv_state only.
    Verified { svn_rev: i64, git_sha: String },
    /// Missing or conflicting scoped provenance; do not adopt global state.
    ReconciliationRequired { reason: String, detail: String },
}

impl RepoImportBaseline {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// Decide whether a repository has a verified import baseline.
///
/// Global `last_svn_rev`, `watermarks.svn_rev`, and commit-map maxima are never
/// adopted as completion proof. Repository columns plus a matching scoped
/// `last_svn_rev_<repo>` copy authorize import completion. A scoped
/// `last_git_sha_<repo>` copy is validated when present but is not required
/// because sync cycles persist the SVN cursor without always mirroring Git.
pub fn resolve_repo_import_baseline(
    db: &Database,
    repo_id: &str,
) -> Result<RepoImportBaseline, DatabaseError> {
    let repo = db
        .get_repository(repo_id)?
        .ok_or_else(|| DatabaseError::Other("repository not found".into()))?;

    if let Some(op) = db.active_import_operation(repo_id)? {
        return Ok(RepoImportBaseline::ReconciliationRequired {
            reason: "import_operation_held".into(),
            detail: format!(
                "import operation {} is active in {:?} state",
                op.id, op.state
            ),
        });
    }

    if let Some(op) = db.latest_import_operation(repo_id)? {
        match op.state {
            ImportOperationState::ReconciliationRequired => {
                return Ok(RepoImportBaseline::ReconciliationRequired {
                    reason: "import_reconciliation_required".into(),
                    detail: op
                        .outcome_detail
                        .unwrap_or_else(|| "import held for reconciliation".into()),
                });
            }
            ImportOperationState::Completed => {
                if repo.last_svn_rev <= 0
                    || repo.last_sync_at.is_none()
                    || repo.last_git_sha.is_empty()
                {
                    return Ok(RepoImportBaseline::ReconciliationRequired {
                        reason: "completed_import_missing_checkpoint".into(),
                        detail: "completed import operation lacks a matching repository checkpoint"
                            .into(),
                    });
                }
            }
            ImportOperationState::Queued
            | ImportOperationState::Running
            | ImportOperationState::CancelRequested
            | ImportOperationState::Cancelling => {
                return Ok(RepoImportBaseline::ReconciliationRequired {
                    reason: "import_operation_in_progress".into(),
                    detail: format!(
                        "import operation {} is unfinished in {:?} state",
                        op.id, op.state
                    ),
                });
            }
            ImportOperationState::Cancelled | ImportOperationState::Failed => {}
        }
    }

    let scoped_svn = db
        .get_state(&format!("last_svn_rev_{repo_id}"))?
        .and_then(|value| value.parse::<i64>().ok());
    let scoped_git = db
        .get_state(&format!("last_git_sha_{repo_id}"))?
        .filter(|value| !value.is_empty());
    let has_scoped_svn = scoped_svn.is_some_and(|rev| rev > 0);
    let has_scoped_git = scoped_git.is_some();

    // Team sync checkpoints columns plus scoped `last_svn_rev_<repo>` without always
    // setting `last_sync_at` or `last_git_sha_<repo>`.
    let column_imported = repo.last_svn_rev > 0 && !repo.last_git_sha.is_empty();

    if column_imported {
        if !has_scoped_svn {
            return Ok(RepoImportBaseline::ReconciliationRequired {
                reason: "missing_scoped_import_checkpoint".into(),
                detail: "repository checkpoint lacks matching per-repo svn_rev kv_state copy"
                    .into(),
            });
        }
        if scoped_svn != Some(repo.last_svn_rev) {
            return Ok(RepoImportBaseline::ReconciliationRequired {
                reason: "conflicting_import_checkpoint".into(),
                detail: "repository column and scoped svn_rev cursor disagree".into(),
            });
        }
        if has_scoped_git && scoped_git.as_deref() != Some(&repo.last_git_sha) {
            return Ok(RepoImportBaseline::ReconciliationRequired {
                reason: "conflicting_import_checkpoint".into(),
                detail: "repository column and scoped git_sha cursor disagree".into(),
            });
        }
        return Ok(RepoImportBaseline::Verified {
            svn_rev: repo.last_svn_rev,
            git_sha: repo.last_git_sha.clone(),
        });
    }

    if has_scoped_svn || has_scoped_git {
        return Ok(RepoImportBaseline::ReconciliationRequired {
            reason: "orphan_scoped_import_checkpoint".into(),
            detail: "per-repo import cursors exist without a finalized repository checkpoint"
                .into(),
        });
    }

    // Global watermarks and singleton import progress are display references only.
    Ok(RepoImportBaseline::Pending)
}

/// A partial import may resume only when the confirmed SVN/Git prefix matches
/// the durable local tip and no publication intent remains outstanding.
pub fn import_resume_checkpoint(op: &ImportOperation) -> Option<(i64, u64, u64)> {
    if op.operation_type != "full_import"
        || op.intended_git_sha.is_some()
        || op.intended_ref.is_some()
    {
        return None;
    }
    let total = op.total_revisions.unwrap_or(0);
    if op.processed_revisions == 0 || op.processed_revisions >= total {
        return None;
    }
    let confirmed = op.last_confirmed_svn_rev?;
    let local = op.last_local_svn_rev?;
    if confirmed != local || confirmed <= 0 {
        return None;
    }
    Some((confirmed, op.local_commits, op.confirmed_batches))
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

/// Snapshot holds may finish only as one pinned baseline. Anything else stays
/// an error so reconcile cannot treat a partial or unpinned snapshot as done.
fn snapshot_hold_is_honest(op: &ImportOperation) -> Result<(), String> {
    match op.operation_type.as_str() {
        "full_import" => Ok(()),
        "snapshot_import" => {
            let Some(pin) = &op.snapshot_pin else {
                return Err("snapshot import is missing its pin".into());
            };
            if op.total_revisions != Some(1) || op.processed_revisions != 1 || op.local_commits != 1
            {
                return Err("snapshot import is not a single verified baseline".into());
            }
            if pin.operative_rev != pin.peg_rev || op.last_local_svn_rev != Some(pin.operative_rev)
            {
                return Err("snapshot pin does not match the recorded local revision".into());
            }
            Ok(())
        }
        _ => Err("operation is not an active reconciliation hold".into()),
    }
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
            crate::writer_fence::require_current(tx)?;
            if super::managed_remove::new_work_blocked(tx, repo_id)? {
                return Err(DatabaseError::Other(
                    "repository removal blocks a new import".into(),
                ));
            }
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
                resume_authorized: false,
                snapshot_pin: None,
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
            crate::writer_fence::require_current(tx)?;
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

    pub fn mark_snapshot_import_request(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> Result<ImportOperation, DatabaseError> {
        self.update_import_operation(repo_id, op_id, |op| {
            if !matches!(
                op.state,
                ImportOperationState::Queued | ImportOperationState::Running
            ) {
                return Err(DatabaseError::Other(
                    "cannot mark snapshot mode on a terminal import".into(),
                ));
            }
            op.operation_type = "snapshot_import".into();
            op.total_revisions = Some(1);
            Ok(())
        })
    }

    /// Record the once-resolved snapshot pin on a queued or running import.
    /// The pin is immutable after the first successful write.
    pub fn pin_snapshot_import(
        &self,
        repo_id: &str,
        op_id: &str,
        pin: SnapshotPin,
    ) -> Result<ImportOperation, DatabaseError> {
        self.update_import_operation(repo_id, op_id, |op| {
            if !matches!(
                op.state,
                ImportOperationState::Queued | ImportOperationState::Running
            ) {
                return Err(DatabaseError::Other(
                    "cannot pin a snapshot on a terminal import".into(),
                ));
            }
            if let Some(existing) = &op.snapshot_pin {
                if existing != &pin {
                    return Err(DatabaseError::Other(
                        "snapshot pin already recorded and must not change".into(),
                    ));
                }
                return Ok(());
            }
            op.operation_type = "snapshot_import".into();
            op.total_revisions = Some(1);
            op.snapshot_pin = Some(pin);
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
            crate::writer_fence::require_current(tx)?;
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
            crate::writer_fence::require_current(tx)?;
            if read_value(tx, &key("active", repo_id))?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other("stale or inactive import operation".into()));
            }
            let document = key("document", op_id);
            let mut op = parse(&read_value(tx, &document)?.ok_or_else(|| {
                DatabaseError::Other("missing import operation document".into())
            })?)?;
            if op.repo_id != repo_id || op.state != ImportOperationState::ReconciliationRequired {
                return Err(DatabaseError::Other("operation is not an active reconciliation hold".into()));
            }
            if let Err(reason) = snapshot_hold_is_honest(&op) {
                return Err(DatabaseError::Other(reason));
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
                return Ok(ReconciledImport {
                    operation,
                    publication_recorded,
                    completed: true,
                    resume_authorized: false,
                });
            }
            op.resume_authorized = true;
            op.outcome_detail = Some(
                "Publication verified. Partial import remains held; resume from the confirmed checkpoint when ready.".into(),
            );
            op.updated_at = Utc::now().to_rfc3339();
            write_value(tx, &document, &serde_json::to_string(&op).unwrap())?;
            Ok(ReconciledImport {
                operation: op,
                publication_recorded,
                completed: false,
                resume_authorized: true,
            })
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

    /// Transition a held partial import back to `running` from the confirmed checkpoint.
    pub fn resume_import_operation(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> Result<ImportOperation, DatabaseError> {
        self.update_import_operation(repo_id, op_id, |op| {
            let authorized = matches!(
                op.state,
                ImportOperationState::ReconciliationRequired | ImportOperationState::Cancelled
            ) && op.resume_authorized;
            if !authorized {
                return Err(DatabaseError::Other(
                    "import resume is not authorized for this operation".into(),
                ));
            }
            if import_resume_checkpoint(op).is_none() {
                return Err(DatabaseError::Other(
                    "import lacks a confirmed partial checkpoint".into(),
                ));
            }
            op.state = ImportOperationState::Running;
            op.resume_authorized = false;
            op.cancel_requested = false;
            op.outcome_detail = Some("Resuming import from durable checkpoint".into());
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
    use rusqlite::params;

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

    #[test]
    fn snapshot_pin_is_recorded_once_and_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("state.db")).unwrap();
        db.initialize().unwrap();
        let op = db
            .create_import_operation("repo-s", "admin", "snap-1", "fingerprint")
            .unwrap();
        assert_eq!(op.operation_type, "full_import");
        assert!(op.snapshot_pin.is_none());
        let pin = SnapshotPin {
            svn_uuid: "uuid-1".into(),
            canonical_url: "file:///tmp/svn/trunk".into(),
            operative_rev: 4,
            peg_rev: 4,
            copy_from_path: Some("/tags/cut".into()),
            copy_from_rev: Some(3),
            requested: "HEAD".into(),
        };
        let pinned = db
            .pin_snapshot_import("repo-s", &op.id, pin.clone())
            .unwrap();
        assert_eq!(pinned.operation_type, "snapshot_import");
        assert_eq!(pinned.snapshot_pin.as_ref(), Some(&pin));
        assert_eq!(pinned.total_revisions, Some(1));
        let changed = SnapshotPin {
            operative_rev: 5,
            ..pin.clone()
        };
        assert!(db.pin_snapshot_import("repo-s", &op.id, changed).is_err());
        drop(db);
        let reopened = Database::new(dir.path().join("state.db")).unwrap();
        reopened.initialize().unwrap();
        let loaded = reopened.latest_import_operation("repo-s").unwrap().unwrap();
        assert_eq!(loaded.snapshot_pin.as_ref(), Some(&pin));
        assert_eq!(
            loaded.snapshot_pin.unwrap().history_boundary(),
            pin.history_boundary()
        );
    }

    const BASELINE_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn open_repo(id: &str) -> (tempfile::TempDir, Database, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(dir.path().join("state.db")).unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
                 VALUES (?1,'p','file:///svn','trunk','','local','file:///git','repo','main','team',5,0,0,1,'t','t',0,'','idle',0,0)",
                [id],
            )
            .unwrap();
        let workdir = dir.path().join("git-repo");
        (dir, db, workdir)
    }

    fn fingerprint(db: &Database, id: &str, workdir: &Path) -> String {
        import_target_fingerprint(&db.get_repository(id).unwrap().unwrap(), workdir)
    }

    fn pin_at(rev: i64) -> SnapshotPin {
        SnapshotPin {
            svn_uuid: "uuid-1".into(),
            canonical_url: "file:///svn/trunk".into(),
            operative_rev: rev,
            peg_rev: rev,
            copy_from_path: None,
            copy_from_rev: None,
            requested: rev.to_string(),
        }
    }

    #[test]
    fn snapshot_reconcile_completes_exact_baseline_and_refuses_dishonest_evidence() {
        let (_dir, db, workdir) = open_repo("snap");
        let fp = fingerprint(&db, "snap", &workdir);
        let op = db
            .create_import_operation("snap", "admin", "req-snap", &fp)
            .unwrap();
        db.pin_snapshot_import("snap", &op.id, pin_at(4)).unwrap();
        db.start_import_operation("snap", &op.id).unwrap();
        db.note_import_local("snap", &op.id, 4, BASELINE_SHA, 1, 1)
            .unwrap();
        db.begin_import_publication("snap", &op.id, "refs/heads/main", BASELINE_SHA)
            .unwrap();
        db.finish_import_operation(
            "snap",
            &op.id,
            ImportOperationState::ReconciliationRequired,
            "lost reply",
        )
        .unwrap();
        let wrong = db
            .reconcile_verified_import(
                "snap",
                &op.id,
                &workdir,
                "refs/heads/main",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap_err()
            .to_string();
        assert!(
            wrong.contains("remote SHA is not the recorded local import tip"),
            "{wrong}"
        );
        assert_eq!(db.get_repo_watermark("snap").unwrap().0, 0);
        assert_eq!(
            db.get_import_operation("snap", &op.id)
                .unwrap()
                .unwrap()
                .state,
            ImportOperationState::ReconciliationRequired
        );

        let done = db
            .reconcile_verified_import("snap", &op.id, &workdir, "refs/heads/main", BASELINE_SHA)
            .unwrap();
        assert!(done.completed);
        assert!(done.publication_recorded);
        assert_eq!(done.operation.state, ImportOperationState::Completed);
        assert_eq!(
            db.get_repo_watermark("snap").unwrap(),
            (4, BASELINE_SHA.to_string())
        );
        assert!(db.active_import_operation("snap").unwrap().is_none());
        assert!(db
            .reconcile_verified_import("snap", &op.id, &workdir, "refs/heads/main", BASELINE_SHA)
            .is_err());

        let (_dir, confirmed, workdir) = open_repo("confirmed");
        let fp = fingerprint(&confirmed, "confirmed", &workdir);
        let op = confirmed
            .create_import_operation("confirmed", "admin", "req-c", &fp)
            .unwrap();
        confirmed
            .pin_snapshot_import("confirmed", &op.id, pin_at(2))
            .unwrap();
        confirmed
            .start_import_operation("confirmed", &op.id)
            .unwrap();
        confirmed
            .note_import_local("confirmed", &op.id, 2, BASELINE_SHA, 1, 1)
            .unwrap();
        confirmed
            .begin_import_publication("confirmed", &op.id, "refs/heads/main", BASELINE_SHA)
            .unwrap();
        confirmed
            .confirm_import_publication("confirmed", &op.id, BASELINE_SHA)
            .unwrap();
        confirmed
            .finish_import_operation(
                "confirmed",
                &op.id,
                ImportOperationState::ReconciliationRequired,
                "finalizer",
            )
            .unwrap();
        let done = confirmed
            .reconcile_verified_import(
                "confirmed",
                &op.id,
                &workdir,
                "refs/heads/main",
                BASELINE_SHA,
            )
            .unwrap();
        assert!(done.completed);
        assert!(!done.publication_recorded);
        assert_eq!(confirmed.get_repo_watermark("confirmed").unwrap().0, 2);

        let (_dir, missing, workdir) = open_repo("missing-pin");
        let fp = fingerprint(&missing, "missing-pin", &workdir);
        let op = missing
            .create_import_operation("missing-pin", "admin", "req-m", &fp)
            .unwrap();
        missing
            .mark_snapshot_import_request("missing-pin", &op.id)
            .unwrap();
        missing
            .start_import_operation("missing-pin", &op.id)
            .unwrap();
        missing
            .note_import_local("missing-pin", &op.id, 4, BASELINE_SHA, 1, 1)
            .unwrap();
        missing
            .begin_import_publication("missing-pin", &op.id, "refs/heads/main", BASELINE_SHA)
            .unwrap();
        missing
            .finish_import_operation(
                "missing-pin",
                &op.id,
                ImportOperationState::ReconciliationRequired,
                "held",
            )
            .unwrap();
        let err = missing
            .reconcile_verified_import(
                "missing-pin",
                &op.id,
                &workdir,
                "refs/heads/main",
                BASELINE_SHA,
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("snapshot import is missing its pin"), "{err}");
        assert_eq!(missing.get_repo_watermark("missing-pin").unwrap().0, 0);

        let (_dir, drifted, workdir) = open_repo("drift");
        let fp = fingerprint(&drifted, "drift", &workdir);
        let op = drifted
            .create_import_operation("drift", "admin", "req-d", &fp)
            .unwrap();
        drifted
            .pin_snapshot_import("drift", &op.id, pin_at(4))
            .unwrap();
        drifted.start_import_operation("drift", &op.id).unwrap();
        drifted
            .note_import_local("drift", &op.id, 5, BASELINE_SHA, 1, 1)
            .unwrap();
        drifted.note_import_total("drift", &op.id, 1).unwrap();
        drifted
            .begin_import_publication("drift", &op.id, "refs/heads/main", BASELINE_SHA)
            .unwrap();
        drifted
            .finish_import_operation(
                "drift",
                &op.id,
                ImportOperationState::ReconciliationRequired,
                "held",
            )
            .unwrap();
        let err = drifted
            .reconcile_verified_import("drift", &op.id, &workdir, "refs/heads/main", BASELINE_SHA)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("snapshot pin does not match the recorded local revision"),
            "{err}"
        );
        assert_eq!(drifted.get_repo_watermark("drift").unwrap().0, 0);

        let (_dir, wide, workdir) = open_repo("wide");
        let fp = fingerprint(&wide, "wide", &workdir);
        let op = wide
            .create_import_operation("wide", "admin", "req-w", &fp)
            .unwrap();
        wide.pin_snapshot_import("wide", &op.id, pin_at(4)).unwrap();
        wide.start_import_operation("wide", &op.id).unwrap();
        wide.note_import_local("wide", &op.id, 4, BASELINE_SHA, 1, 1)
            .unwrap();
        wide.note_import_total("wide", &op.id, 3).unwrap();
        wide.begin_import_publication("wide", &op.id, "refs/heads/main", BASELINE_SHA)
            .unwrap();
        wide.finish_import_operation(
            "wide",
            &op.id,
            ImportOperationState::ReconciliationRequired,
            "held",
        )
        .unwrap();
        let err = wide
            .reconcile_verified_import("wide", &op.id, &workdir, "refs/heads/main", BASELINE_SHA)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("snapshot import is not a single verified baseline"),
            "{err}"
        );

        let (_dir, full, workdir) = open_repo("full");
        let fp = fingerprint(&full, "full", &workdir);
        let op = full
            .create_import_operation("full", "admin", "req-f", &fp)
            .unwrap();
        full.start_import_operation("full", &op.id).unwrap();
        full.note_import_total("full", &op.id, 3).unwrap();
        full.note_import_local("full", &op.id, 3, BASELINE_SHA, 3, 3)
            .unwrap();
        full.begin_import_publication("full", &op.id, "refs/heads/main", BASELINE_SHA)
            .unwrap();
        full.finish_import_operation(
            "full",
            &op.id,
            ImportOperationState::ReconciliationRequired,
            "lost reply",
        )
        .unwrap();
        let done = full
            .reconcile_verified_import("full", &op.id, &workdir, "refs/heads/main", BASELINE_SHA)
            .unwrap();
        assert!(done.completed);
        assert_eq!(full.get_repo_watermark("full").unwrap().0, 3);

        let (_dir, other, workdir) = open_repo("other");
        let fp = fingerprint(&other, "other", &workdir);
        let op = other
            .create_import_operation("other", "admin", "req-o", &fp)
            .unwrap();
        other.start_import_operation("other", &op.id).unwrap();
        other.note_import_total("other", &op.id, 1).unwrap();
        other
            .note_import_local("other", &op.id, 1, BASELINE_SHA, 1, 1)
            .unwrap();
        other
            .begin_import_publication("other", &op.id, "refs/heads/main", BASELINE_SHA)
            .unwrap();
        other
            .finish_import_operation(
                "other",
                &op.id,
                ImportOperationState::ReconciliationRequired,
                "held",
            )
            .unwrap();
        let mut stored = other
            .get_import_operation("other", &op.id)
            .unwrap()
            .unwrap();
        stored.operation_type = "side_import".into();
        other
            .conn()
            .execute(
                "UPDATE kv_state SET value=?1 WHERE key=?2",
                params![
                    serde_json::to_string(&stored).unwrap(),
                    format!("import_operation_v1:document:{}", op.id)
                ],
            )
            .unwrap();
        let err = other
            .reconcile_verified_import("other", &op.id, &workdir, "refs/heads/main", BASELINE_SHA)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("operation is not an active reconciliation hold"),
            "{err}"
        );
        assert_eq!(other.get_repo_watermark("other").unwrap().0, 0);
    }

    #[test]
    fn partial_reconcile_authorizes_resume_without_completing_checkpoint() {
        let (_dir, db, workdir) = open_repo("partial-resume");
        let fp = fingerprint(&db, "partial-resume", &workdir);
        let op = db
            .create_import_operation("partial-resume", "admin", "req-partial", &fp)
            .unwrap();
        db.start_import_operation("partial-resume", &op.id).unwrap();
        db.note_import_total("partial-resume", &op.id, 52).unwrap();
        let tip = "3333333333333333333333333333333333333333";
        for rev in 1..50 {
            let sha = format!("{:040x}", rev);
            db.note_import_local("partial-resume", &op.id, rev, &sha, rev as u64, rev as u64)
                .unwrap();
        }
        db.note_import_local("partial-resume", &op.id, 50, tip, 50, 50)
            .unwrap();
        db.begin_import_publication("partial-resume", &op.id, "refs/heads/main", tip)
            .unwrap();
        db.finish_import_operation(
            "partial-resume",
            &op.id,
            ImportOperationState::ReconciliationRequired,
            "lost reply",
        )
        .unwrap();
        let reconciled = db
            .reconcile_verified_import("partial-resume", &op.id, &workdir, "refs/heads/main", tip)
            .unwrap();
        assert!(!reconciled.completed);
        assert!(reconciled.resume_authorized);
        assert!(reconciled.operation.resume_authorized);
        assert_eq!(db.get_repo_watermark("partial-resume").unwrap().0, 0);
        assert_eq!(
            import_resume_checkpoint(&reconciled.operation),
            Some((50, 50, 1))
        );
    }

    #[test]
    fn resume_transitions_authorized_hold_back_to_running() {
        let (_dir, db, workdir) = open_repo("resume-run");
        let fp = fingerprint(&db, "resume-run", &workdir);
        let op = db
            .create_import_operation("resume-run", "admin", "req-resume", &fp)
            .unwrap();
        db.start_import_operation("resume-run", &op.id).unwrap();
        db.note_import_total("resume-run", &op.id, 3).unwrap();
        db.note_import_local("resume-run", &op.id, 1, BASELINE_SHA, 1, 1)
            .unwrap();
        db.begin_import_publication("resume-run", &op.id, "refs/heads/main", BASELINE_SHA)
            .unwrap();
        db.confirm_import_publication("resume-run", &op.id, BASELINE_SHA)
            .unwrap();
        db.finish_import_operation(
            "resume-run",
            &op.id,
            ImportOperationState::ReconciliationRequired,
            "worker stopped",
        )
        .unwrap();
        let mut held = db
            .get_import_operation("resume-run", &op.id)
            .unwrap()
            .unwrap();
        held.resume_authorized = true;
        held.last_confirmed_svn_rev = Some(1);
        held.last_confirmed_git_sha = Some(BASELINE_SHA.into());
        db.conn()
            .execute(
                "UPDATE kv_state SET value=?1 WHERE key=?2",
                params![
                    serde_json::to_string(&held).unwrap(),
                    format!("import_operation_v1:document:{}", op.id)
                ],
            )
            .unwrap();
        let resumed = db.resume_import_operation("resume-run", &op.id).unwrap();
        assert_eq!(resumed.state, ImportOperationState::Running);
        assert!(!resumed.resume_authorized);
        assert_eq!(import_resume_checkpoint(&resumed), Some((1, 1, 1)));
    }

    #[test]
    fn resume_refuses_without_authorization() {
        let (_dir, db, workdir) = open_repo("resume-deny");
        let fp = fingerprint(&db, "resume-deny", &workdir);
        let op = db
            .create_import_operation("resume-deny", "admin", "req-deny", &fp)
            .unwrap();
        db.start_import_operation("resume-deny", &op.id).unwrap();
        db.note_import_total("resume-deny", &op.id, 3).unwrap();
        db.note_import_local("resume-deny", &op.id, 1, BASELINE_SHA, 1, 1)
            .unwrap();
        db.finish_import_operation(
            "resume-deny",
            &op.id,
            ImportOperationState::ReconciliationRequired,
            "held",
        )
        .unwrap();
        let err = db
            .resume_import_operation("resume-deny", &op.id)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not authorized"), "{err}");
    }

    #[test]
    fn cancelled_without_resume_authorized_cannot_resume() {
        let (_dir, db, workdir) = open_repo("cancel-no-auth");
        let fp = fingerprint(&db, "cancel-no-auth", &workdir);
        let op = db
            .create_import_operation("cancel-no-auth", "admin", "req-cancel", &fp)
            .unwrap();
        db.start_import_operation("cancel-no-auth", &op.id).unwrap();
        db.note_import_total("cancel-no-auth", &op.id, 3).unwrap();
        db.note_import_local("cancel-no-auth", &op.id, 1, BASELINE_SHA, 1, 1)
            .unwrap();
        db.begin_import_publication("cancel-no-auth", &op.id, "refs/heads/main", BASELINE_SHA)
            .unwrap();
        db.confirm_import_publication("cancel-no-auth", &op.id, BASELINE_SHA)
            .unwrap();
        db.finish_import_operation(
            "cancel-no-auth",
            &op.id,
            ImportOperationState::Cancelled,
            "stopped after confirmed prefix",
        )
        .unwrap();
        assert!(import_resume_checkpoint(
            &db.get_import_operation("cancel-no-auth", &op.id)
                .unwrap()
                .unwrap()
        )
        .is_some());
        let err = db
            .resume_import_operation("cancel-no-auth", &op.id)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not authorized"), "{err}");
        assert_eq!(
            db.get_import_operation("cancel-no-auth", &op.id)
                .unwrap()
                .unwrap()
                .state,
            ImportOperationState::Cancelled
        );
    }

    #[test]
    fn reconcile_rejects_processed_beyond_frozen_total() {
        let (_dir, db, workdir) = open_repo("stale-total");
        let fp = fingerprint(&db, "stale-total", &workdir);
        let op = db
            .create_import_operation("stale-total", "admin", "req-stale", &fp)
            .unwrap();
        db.start_import_operation("stale-total", &op.id).unwrap();
        db.note_import_total("stale-total", &op.id, 52).unwrap();
        let tip = "6666666666666666666666666666666666666666";
        for rev in 1..=52 {
            let sha = format!("{:040x}", rev);
            db.note_import_local("stale-total", &op.id, rev, &sha, rev as u64, rev as u64)
                .unwrap();
        }
        db.note_import_local("stale-total", &op.id, 53, tip, 53, 53)
            .unwrap();
        db.begin_import_publication("stale-total", &op.id, "refs/heads/main", tip)
            .unwrap();
        db.finish_import_operation(
            "stale-total",
            &op.id,
            ImportOperationState::ReconciliationRequired,
            "lost reply",
        )
        .unwrap();
        let err = db
            .reconcile_verified_import("stale-total", &op.id, &workdir, "refs/heads/main", tip)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("remote SHA is not the recorded local import tip"),
            "{err}"
        );
    }

    #[test]
    fn resume_refreshes_total_when_svn_log_grows_and_allows_re_reconcile() {
        let (_dir, db, workdir) = open_repo("grow-log");
        let fp = fingerprint(&db, "grow-log", &workdir);
        let op = db
            .create_import_operation("grow-log", "admin", "req-grow", &fp)
            .unwrap();
        db.start_import_operation("grow-log", &op.id).unwrap();
        db.note_import_total("grow-log", &op.id, 52).unwrap();
        let batch_tip = "4444444444444444444444444444444444444444";
        for rev in 1..50 {
            let sha = format!("{:040x}", rev);
            db.note_import_local("grow-log", &op.id, rev, &sha, rev as u64, rev as u64)
                .unwrap();
        }
        db.note_import_local("grow-log", &op.id, 50, batch_tip, 50, 50)
            .unwrap();
        db.begin_import_publication("grow-log", &op.id, "refs/heads/main", batch_tip)
            .unwrap();
        db.finish_import_operation(
            "grow-log",
            &op.id,
            ImportOperationState::ReconciliationRequired,
            "lost reply",
        )
        .unwrap();
        let first = db
            .reconcile_verified_import("grow-log", &op.id, &workdir, "refs/heads/main", batch_tip)
            .unwrap();
        assert!(first.resume_authorized);
        assert_eq!(first.operation.total_revisions, Some(52));

        db.resume_import_operation("grow-log", &op.id).unwrap();
        db.note_import_total("grow-log", &op.id, 54).unwrap();
        let mid_tip = "5555555555555555555555555555555555555555";
        db.note_import_local("grow-log", &op.id, 51, mid_tip, 51, 51)
            .unwrap();
        db.note_import_local("grow-log", &op.id, 52, mid_tip, 52, 52)
            .unwrap();
        db.begin_import_publication("grow-log", &op.id, "refs/heads/main", mid_tip)
            .unwrap();
        db.finish_import_operation(
            "grow-log",
            &op.id,
            ImportOperationState::ReconciliationRequired,
            "lost reply after resume",
        )
        .unwrap();

        let held = db
            .get_import_operation("grow-log", &op.id)
            .unwrap()
            .unwrap();
        assert_eq!(held.processed_revisions, 52);
        assert_eq!(held.total_revisions, Some(54));
        assert!(
            import_resume_checkpoint(&held).is_none(),
            "checkpoint requires confirmed prefix before another 64B reconcile"
        );

        let second = db
            .reconcile_verified_import("grow-log", &op.id, &workdir, "refs/heads/main", mid_tip)
            .unwrap();
        assert!(!second.completed);
        assert!(second.resume_authorized);
        assert_eq!(second.operation.total_revisions, Some(54));
        assert_eq!(
            import_resume_checkpoint(&second.operation),
            Some((52, 52, 2))
        );
    }

    fn finalize_import_checkpoint(db: &Database, repo_id: &str, svn_rev: i64, sha: &str) {
        db.conn()
            .execute(
                "UPDATE repositories SET last_svn_rev=?1,last_git_sha=?2,last_sync_at=datetime('now') WHERE id=?3",
                params![svn_rev, sha, repo_id],
            )
            .unwrap();
        db.set_state(&format!("last_svn_rev_{repo_id}"), &svn_rev.to_string())
            .unwrap();
        db.set_state(&format!("last_git_sha_{repo_id}"), sha)
            .unwrap();
    }

    #[test]
    fn resolve_repo_import_baseline_verified_from_scoped_state_only() {
        let (_dir, db, _workdir) = open_repo("verified");
        finalize_import_checkpoint(&db, "verified", 2, BASELINE_SHA);
        let baseline = resolve_repo_import_baseline(&db, "verified").unwrap();
        assert_eq!(
            baseline,
            RepoImportBaseline::Verified {
                svn_rev: 2,
                git_sha: BASELINE_SHA.to_string(),
            }
        );
    }

    #[test]
    fn resolve_repo_import_baseline_ignores_global_svn_watermark() {
        let (_dir, db, _workdir) = open_repo("pending");
        db.set_state("last_svn_rev", "2").unwrap();
        db.set_watermark("svn_rev", "2").unwrap();
        assert_eq!(
            resolve_repo_import_baseline(&db, "pending").unwrap(),
            RepoImportBaseline::Pending
        );
    }

    #[test]
    fn resolve_repo_import_baseline_rejects_orphan_scoped_cursor() {
        let (_dir, db, _workdir) = open_repo("orphan");
        db.set_state("last_svn_rev_orphan", "2").unwrap();
        let baseline = resolve_repo_import_baseline(&db, "orphan").unwrap();
        assert!(matches!(
            baseline,
            RepoImportBaseline::ReconciliationRequired {
                reason,
                ..
            } if reason == "orphan_scoped_import_checkpoint"
        ));
    }

    #[test]
    fn resolve_repo_import_baseline_accepts_sync_watermark_without_scoped_git_kv() {
        let (_dir, db, _workdir) = open_repo("sync-only");
        db.conn()
            .execute(
                "UPDATE repositories SET last_svn_rev=2,last_git_sha=?1,last_sync_at=datetime('now') WHERE id='sync-only'",
                [BASELINE_SHA],
            )
            .unwrap();
        db.set_state("last_svn_rev_sync-only", "2").unwrap();
        let baseline = resolve_repo_import_baseline(&db, "sync-only").unwrap();
        assert_eq!(
            baseline,
            RepoImportBaseline::Verified {
                svn_rev: 2,
                git_sha: BASELINE_SHA.to_string(),
            }
        );
    }

    #[test]
    fn resolve_repo_import_baseline_rejects_split_checkpoint() {
        let (_dir, db, _workdir) = open_repo("split");
        db.conn()
            .execute(
                "UPDATE repositories SET last_svn_rev=2,last_git_sha=?1,last_sync_at=datetime('now') WHERE id='split'",
                [BASELINE_SHA],
            )
            .unwrap();
        db.set_state("last_svn_rev_split", "3").unwrap();
        db.set_state("last_git_sha_split", BASELINE_SHA).unwrap();
        let baseline = resolve_repo_import_baseline(&db, "split").unwrap();
        assert!(matches!(
            baseline,
            RepoImportBaseline::ReconciliationRequired {
                reason,
                ..
            } if reason == "conflicting_import_checkpoint"
        ));
    }
}
