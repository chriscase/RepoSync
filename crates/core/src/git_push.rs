//! Read-then-maybe-finalize inspection for one team-engine SVN→Git push.

use crate::db::git_push_operations::{
    git_push_target_fingerprint, GitPushOperation, GitPushOperationState,
};
use crate::db::Database;
use crate::errors::{DatabaseError, GitError};
use crate::git::client::GitClient;

/// Re-read the exact remote branch tip after a push attempt.
pub fn observed_git_ref(git: &GitClient, remote: &str, branch: &str) -> Result<String, GitError> {
    git.ls_remote_ref(remote, branch)?
        .ok_or_else(|| GitError::RefNotFound(format!("refs/heads/{branch}")))
}

/// Tree object id for an exact commit SHA already present locally.
pub fn observed_git_tree(git: &GitClient, sha: &str) -> Result<String, GitError> {
    let (_, tree) = git.commit_parent_and_tree(sha)?;
    Ok(tree)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitPushInspect {
    UniqueMatch { git_sha: String, git_tree: String },
    AbsentUnchanged,
    Conflict { reason: String },
    Unavailable { reason: String },
}

#[derive(Debug, Clone)]
pub struct GitPushReconcileResult {
    pub operation: GitPushOperation,
    pub inspect: GitPushInspect,
    pub finalized: bool,
    pub resume_authorized: bool,
}

pub fn inspect_svn_to_git_push(git: &GitClient, op: &GitPushOperation) -> GitPushInspect {
    let remote = &op.pre_push_git_remote;
    let branch = &op.pre_push_git_branch;
    let observed_sha = match observed_git_ref(git, remote, branch) {
        Ok(sha) => sha,
        Err(error) => {
            return GitPushInspect::Unavailable {
                reason: format!("Git inspection unavailable: {error}"),
            };
        }
    };
    let observed_tree = match observed_git_tree(git, &observed_sha) {
        Ok(tree) => tree,
        Err(error) => {
            return GitPushInspect::Unavailable {
                reason: format!("could not read the observed Git tree: {error}"),
            };
        }
    };
    if observed_sha == op.pre_push_git_sha {
        let pre_tree = op.pre_push_git_tree.as_deref();
        if pre_tree.is_some_and(|tree| tree != observed_tree) {
            return GitPushInspect::Conflict {
                reason: "pre-push revision is unchanged but its tree is not".into(),
            };
        }
        return GitPushInspect::AbsentUnchanged;
    }
    if observed_sha == op.intended_local_git_sha && observed_tree == op.intended_local_git_tree {
        return GitPushInspect::UniqueMatch {
            git_sha: observed_sha,
            git_tree: observed_tree,
        };
    }
    if observed_sha != op.intended_local_git_sha {
        return GitPushInspect::Conflict {
            reason: format!(
                "remote ref {branch} is {observed_sha} but intended local commit was {}",
                op.intended_local_git_sha
            ),
        };
    }
    GitPushInspect::Conflict {
        reason: format!(
            "remote commit tree {observed_tree} does not match intended local tree {}",
            op.intended_local_git_tree
        ),
    }
}

pub fn apply_git_push_reconciliation(
    db: &Database,
    repo_id: &str,
    op_id: &str,
    git: &GitClient,
) -> Result<GitPushReconcileResult, DatabaseError> {
    let requested = db
        .get_git_push_operation(repo_id, op_id)?
        .ok_or_else(|| DatabaseError::Other("svn-to-git push operation not found".into()))?;
    let active = db.active_git_push_operation(repo_id)?;
    if active.is_none() && requested.state == GitPushOperationState::Completed {
        return Ok(GitPushReconcileResult {
            inspect: GitPushInspect::UniqueMatch {
                git_sha: requested.last_confirmed_git_sha.clone().unwrap_or_default(),
                git_tree: requested
                    .last_confirmed_git_tree
                    .clone()
                    .unwrap_or_default(),
            },
            operation: requested,
            finalized: true,
            resume_authorized: false,
        });
    }
    if active.as_ref().is_none_or(|op| op.id != op_id)
        || requested.state != GitPushOperationState::ReconciliationRequired
    {
        return Err(DatabaseError::Other(
            "operation is not this repository's active svn-to-git reconciliation hold".into(),
        ));
    }
    let fingerprint = git_push_target_fingerprint(
        repo_id,
        &requested.pre_push_git_remote,
        &requested.pre_push_git_branch,
    );
    if requested.target_fingerprint != fingerprint {
        let operation = db.note_git_push_reconciliation_reason(
            repo_id,
            op_id,
            "SVN-to-Git target fingerprint changed; review required",
        )?;
        return Ok(GitPushReconcileResult {
            operation,
            inspect: GitPushInspect::Conflict {
                reason: "SVN-to-Git target fingerprint changed; review required".into(),
            },
            finalized: false,
            resume_authorized: false,
        });
    }
    let inspect = inspect_svn_to_git_push(git, &requested);
    match &inspect {
        GitPushInspect::UniqueMatch {
            git_sha, git_tree, ..
        } => match db.finalize_verified_svn_to_git_push(
            repo_id,
            op_id,
            git_sha,
            git_tree,
            &fingerprint,
        ) {
            Ok(reconciled) => Ok(GitPushReconcileResult {
                operation: reconciled.operation,
                inspect,
                finalized: reconciled.finalized,
                resume_authorized: reconciled.resume_authorized,
            }),
            Err(DatabaseError::Other(reason))
                if reason.contains("fingerprint")
                    || reason.contains("differs")
                    || reason.contains("mismatch") =>
            {
                let operation = db.note_git_push_reconciliation_reason(repo_id, op_id, &reason)?;
                Ok(GitPushReconcileResult {
                    operation,
                    inspect: GitPushInspect::Conflict { reason },
                    finalized: false,
                    resume_authorized: false,
                })
            }
            Err(error) => Err(error),
        },
        GitPushInspect::AbsentUnchanged => {
            let operation = db.authorize_svn_to_git_resume(
                repo_id,
                op_id,
                "Effect is absent and the pre-push Git target is unchanged; the worker may resume that one planned push",
            )?;
            Ok(GitPushReconcileResult {
                operation,
                inspect,
                finalized: false,
                resume_authorized: true,
            })
        }
        GitPushInspect::Conflict { reason } | GitPushInspect::Unavailable { reason } => {
            let operation = db.note_git_push_reconciliation_reason(repo_id, op_id, reason)?;
            Ok(GitPushReconcileResult {
                operation,
                inspect,
                finalized: false,
                resume_authorized: false,
            })
        }
    }
}
