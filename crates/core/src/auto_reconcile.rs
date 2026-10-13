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
    let writer_window = db.begin_ordinary_writer_window(Some(&repo.id))?;
    reconcile_with_window(db, repo, svn, git_workdir, writer_window).await
}

/// Scheduler entry: credential resolution and client construction occur only
/// after exclusion. The client-taking variant remains for explicit callers.
pub async fn reconcile_held_external_writes_with_resolved_credentials(
    db: &Database,
    repo: &Repository,
    git_workdir: Option<&Path>,
) -> Result<AutoReconcileResult, DatabaseError> {
    let writer_window = db.begin_ordinary_writer_window(Some(&repo.id))?;
    let credential = db.resolve_credential_chain_state(&repo.id, "secret_svn_password");
    if credential.explicitly_revoked {
        return Err(DatabaseError::Other(
            "automatic reconciliation SVN credential explicitly revoked".into(),
        ));
    }
    let url = if repo.svn_branch.is_empty() {
        repo.svn_url.clone()
    } else {
        format!(
            "{}/{}",
            repo.svn_url.trim_end_matches('/'),
            repo.svn_branch.trim_start_matches('/')
        )
    };
    let svn = SvnClient::new(
        &url,
        &repo.svn_username,
        credential.value.as_deref().unwrap_or(""),
    );
    reconcile_with_window(db, repo, &svn, git_workdir, writer_window).await
}

async fn reconcile_with_window(
    db: &Database,
    repo: &Repository,
    svn: &SvnClient,
    git_workdir: Option<&Path>,
    writer_window: crate::db::ordinary_writer_windows::OrdinaryWriterWindow<'_>,
) -> Result<AutoReconcileResult, DatabaseError> {
    writer_window.require_engine_scope(db, Some(&repo.id))?;
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

    writer_window.finish()?;
    Ok(result)
}
