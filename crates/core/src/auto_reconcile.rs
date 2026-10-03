//! Background observe-first reconciliation for held external-write journals.
//!
//! Reuses the same inspect+finalize paths as the admin reconcile endpoints
//! (`apply_svn_commit_reconciliation`, `apply_git_push_reconciliation`,
//! `apply_import_reconciliation`).
//! Callers must hold the per-repository busy slot before invoking.
//!
//! Absent/unchanged Git→SVN and SVN→Git journals set `resume_authorized`.
//! The team worker then issues that one recorded write. For SVN→Git this
//! must happen before history inspect so the unpushed intended commit is
//! not classified as `unpublished_local_history`.

use std::path::Path;

use crate::db::git_push_operations::GitPushOperationState;
use crate::db::import_operations::ImportOperationState;
use crate::db::svn_commit_operations::SvnCommitOperationState;
use crate::db::Database;
use crate::errors::DatabaseError;
use crate::git::GitClient;
use crate::git_push;
use crate::import;
use crate::models::Repository;
use crate::svn::SvnClient;
use crate::svn_commit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldExternalWriteKind {
    GitToSvnCommit,
    SvnToGitPush,
    ImportOperation,
}

#[derive(Debug, Clone)]
pub struct AutoReconcileAttempt {
    pub kind: HeldExternalWriteKind,
    pub operation_id: String,
    pub finalized: bool,
    pub resume_authorized: bool,
    pub skipped: bool,
    pub skip_reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AutoReconcileResult {
    pub attempts: Vec<AutoReconcileAttempt>,
}

/// Returns true when the repository has an active held external-write journal
/// that still requires observe-first reconciliation.
pub fn repo_has_reconciliation_hold(db: &Database, repo_id: &str) -> Result<bool, DatabaseError> {
    if let Some(op) = db.active_import_operation(repo_id)? {
        if op.state == ImportOperationState::ReconciliationRequired {
            return Ok(true);
        }
    }
    if let Some(op) = db.active_svn_commit_operation(repo_id)? {
        if op.state == SvnCommitOperationState::ReconciliationRequired && !op.resume_authorized {
            return Ok(true);
        }
    }
    if let Some(op) = db.active_git_push_operation(repo_id)? {
        if op.state == GitPushOperationState::ReconciliationRequired && !op.resume_authorized {
            return Ok(true);
        }
    }
    Ok(false)
}

fn projection_json(repo: &Repository) -> String {
    let allowed_paths: Vec<String> = repo
        .allowed_paths
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let blocked_patterns: Vec<String> = repo
        .blocked_patterns
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    serde_json::json!({
        "allowed_paths": allowed_paths,
        "blocked_patterns": blocked_patterns,
    })
    .to_string()
}

/// Observe-first reconciliation for held `import_operation_v1`, `svn_commit`, and
/// `svn_to_git_push` journals.
///
/// Finalizes only on a unique remote match, authorizes at most one resume when the
/// effect is absent/unchanged, and never mutates watermarks or mappings on refuse.
pub async fn reconcile_held_external_writes(
    db: &Database,
    repo: &Repository,
    svn: &SvnClient,
    git_workdir: Option<&Path>,
) -> Result<AutoReconcileResult, DatabaseError> {
    let mut result = AutoReconcileResult::default();

    if let Some(op) = db.active_import_operation(&repo.id)? {
        if op.state == ImportOperationState::ReconciliationRequired {
            let workdir = git_workdir.filter(|path| path.join(".git").exists());
            match workdir {
                Some(workdir) => {
                    match import::apply_import_reconciliation(db, repo, &op.id, workdir).await {
                        Ok(reconciled) => {
                            result.attempts.push(AutoReconcileAttempt {
                                kind: HeldExternalWriteKind::ImportOperation,
                                operation_id: op.id,
                                finalized: reconciled.finalized,
                                resume_authorized: reconciled.resume_authorized,
                                skipped: false,
                                skip_reason: None,
                            });
                        }
                        Err(error) => {
                            result.attempts.push(AutoReconcileAttempt {
                                kind: HeldExternalWriteKind::ImportOperation,
                                operation_id: op.id,
                                finalized: false,
                                resume_authorized: false,
                                skipped: true,
                                skip_reason: Some(error.to_string()),
                            });
                        }
                    }
                }
                None => {
                    result.attempts.push(AutoReconcileAttempt {
                        kind: HeldExternalWriteKind::ImportOperation,
                        operation_id: op.id,
                        finalized: false,
                        resume_authorized: false,
                        skipped: true,
                        skip_reason: Some("git workdir unavailable".into()),
                    });
                }
            }
        }
    }

    if let Some(op) = db.active_svn_commit_operation(&repo.id)? {
        if op.state == SvnCommitOperationState::ReconciliationRequired && !op.resume_authorized {
            let projection = projection_json(repo);
            match svn_commit::apply_svn_commit_reconciliation(
                db,
                &repo.id,
                &op.id,
                svn,
                &projection,
            )
            .await
            {
                Ok(reconciled) => {
                    result.attempts.push(AutoReconcileAttempt {
                        kind: HeldExternalWriteKind::GitToSvnCommit,
                        operation_id: op.id,
                        finalized: reconciled.finalized,
                        resume_authorized: reconciled.resume_authorized,
                        skipped: false,
                        skip_reason: None,
                    });
                }
                Err(error) => {
                    result.attempts.push(AutoReconcileAttempt {
                        kind: HeldExternalWriteKind::GitToSvnCommit,
                        operation_id: op.id,
                        finalized: false,
                        resume_authorized: false,
                        skipped: true,
                        skip_reason: Some(error.to_string()),
                    });
                }
            }
        }
    }

    if let Some(op) = db.active_git_push_operation(&repo.id)? {
        if op.state == GitPushOperationState::ReconciliationRequired && !op.resume_authorized {
            let git_client = git_workdir
                .filter(|path| path.join(".git").exists())
                .and_then(|path| GitClient::new(path).ok());
            match git_client.as_ref() {
                Some(git) => {
                    match git_push::apply_git_push_reconciliation(db, &repo.id, &op.id, git) {
                        Ok(reconciled) => {
                            result.attempts.push(AutoReconcileAttempt {
                                kind: HeldExternalWriteKind::SvnToGitPush,
                                operation_id: op.id,
                                finalized: reconciled.finalized,
                                resume_authorized: reconciled.resume_authorized,
                                skipped: false,
                                skip_reason: None,
                            });
                        }
                        Err(error) => {
                            result.attempts.push(AutoReconcileAttempt {
                                kind: HeldExternalWriteKind::SvnToGitPush,
                                operation_id: op.id,
                                finalized: false,
                                resume_authorized: false,
                                skipped: true,
                                skip_reason: Some(error.to_string()),
                            });
                        }
                    }
                }
                None => {
                    result.attempts.push(AutoReconcileAttempt {
                        kind: HeldExternalWriteKind::SvnToGitPush,
                        operation_id: op.id,
                        finalized: false,
                        resume_authorized: false,
                        skipped: true,
                        skip_reason: Some("git workdir unavailable".into()),
                    });
                }
            }
        }
    }

    Ok(result)
}
