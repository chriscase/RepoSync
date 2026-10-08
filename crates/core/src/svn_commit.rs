//! Read-then-maybe-finalize inspection for one team-engine Git→SVN commit.

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::db::svn_commit_operations::{
    svn_commit_target_fingerprint, IntendedPath, ReconciledSvnCommit, SvnCommitOperation,
    SvnCommitOperationState,
};
use crate::db::Database;
use crate::errors::{DatabaseError, SvnError};
use crate::path_projection::{
    git_intent_path, svn_log_path_to_branch_relative, svn_path_identity, SvnPathIdentity,
};
use crate::svn::SvnClient;

const OPERATION_TRAILER: &str = "RepoSync-Operation:";
const GIT_SHA_TRAILER: &str = "RepoSync-Git-SHA:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvnCommitInspect {
    UniqueMatch {
        svn_rev: i64,
        svn_tree: String,
        changed_paths: Vec<IntendedPath>,
    },
    AbsentUnchanged,
    Conflict {
        reason: String,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone)]
pub struct SvnCommitReconcileResult {
    pub operation: SvnCommitOperation,
    pub inspect: SvnCommitInspect,
    pub finalized: bool,
    pub resume_authorized: bool,
}

pub fn operation_commit_message(original: &str, git_sha: &str, operation_id: &str) -> String {
    let short = &git_sha[..8.min(git_sha.len())];
    format!(
        "{original}\n\n[reposync] synced from Git {short}\n{OPERATION_TRAILER} {operation_id}\n{GIT_SHA_TRAILER} {git_sha}"
    )
}

/// Append durable operation identity trailers to a formatted personal Git→SVN message.
pub fn append_durable_git_to_svn_identity(
    message: &str,
    git_sha: &str,
    operation_id: &str,
) -> String {
    format!("{message}\n{OPERATION_TRAILER} {operation_id}\n{GIT_SHA_TRAILER} {git_sha}")
}

pub fn hash_regular_file_tree(root: &Path) -> Result<String, std::io::Error> {
    let mut files = BTreeMap::new();
    collect_regular_files(root, root, &mut files)?;
    let mut hasher = Sha256::new();
    for (path, digest) in files {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(digest.as_bytes());
        hasher.update(*b"\n");
    }
    Ok(hex::encode(hasher.finalize()))
}

fn collect_regular_files(
    root: &Path,
    current: &Path,
    files: &mut BTreeMap<String, String>,
) -> Result<(), std::io::Error> {
    if !current.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if name == ".svn" {
            continue;
        }
        if path.is_dir() {
            collect_regular_files(root, &path, files)?;
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path)?;
        files.insert(relative, hex::encode(Sha256::digest(&bytes)));
    }
    Ok(())
}

pub fn intended_paths_from_contents(
    files: &[(String, String, Option<Vec<u8>>)],
) -> Vec<IntendedPath> {
    let mut paths: Vec<IntendedPath> = files
        .iter()
        .map(|(action, path, content)| IntendedPath {
            action: action.clone(),
            path: git_intent_path(path),
            content_sha256: content
                .as_ref()
                .map(|bytes| hex::encode(Sha256::digest(bytes))),
        })
        .collect();
    paths.sort_by(|a, b| a.path.cmp(&b.path));
    paths
}

pub fn normalize_svn_path(path: &str) -> String {
    git_intent_path(path)
}

/// Re-read the regular-file tree hash for an exact SVN revision export.
pub async fn observed_svn_tree_at_revision(
    svn: &SvnClient,
    revision: i64,
) -> Result<String, SvnError> {
    let snapshot = tempfile::tempdir().map_err(SvnError::IoError)?;
    let dest = snapshot.path().join("export");
    svn.export("", revision, &dest).await?;
    hash_regular_file_tree(&dest).map_err(SvnError::IoError)
}

fn operation_path_identity(op: &SvnCommitOperation, live_root_url: &str) -> SvnPathIdentity {
    if !op.target_svn_root_url.is_empty() {
        return SvnPathIdentity {
            root_url: op.target_svn_root_url.clone(),
            branch_path: op.target_svn_branch_path.clone(),
        };
    }
    svn_path_identity(live_root_url, &op.target_svn_path)
}

fn paths_match(
    intended: &[IntendedPath],
    observed: &[IntendedPath],
    identity: &SvnPathIdentity,
) -> bool {
    let intended_keys: Vec<(String, String)> = intended
        .iter()
        .map(|p| (p.action.clone(), git_intent_path(&p.path)))
        .collect();
    for (action, path) in &intended_keys {
        if !observed.iter().any(|item| {
            let Some(obs_path) = svn_log_path_to_branch_relative(&item.path, identity) else {
                return false;
            };
            item.action == *action && obs_path == *path
        }) {
            return false;
        }
    }
    observed.iter().all(|item| {
        let Some(path) = svn_log_path_to_branch_relative(&item.path, identity) else {
            return false;
        };
        if intended_keys
            .iter()
            .any(|(action, intended)| action == &item.action && intended == &path)
        {
            return true;
        }
        item.action == "A"
            && intended_keys.iter().any(|(action, intended)| {
                action == "A" && (intended.starts_with(&format!("{path}/")) || intended == &path)
            })
    })
}

/// RepoSync appends durable trailers as the final `RepoSync-Operation` /
/// `RepoSync-Git-SHA` pair in the SVN log message. Earlier quoted copies in
/// `{original_message}` must not override that block.
fn reposync_appended_trailer_pair(message: &str) -> Option<(&str, &str)> {
    let lines: Vec<&str> = message.lines().map(str::trim).collect();
    if lines.len() < 2 {
        return None;
    }
    let sha_line = lines[lines.len() - 1];
    let op_line = lines[lines.len() - 2];
    let sha = sha_line.strip_prefix(GIT_SHA_TRAILER)?.trim();
    let op_id = op_line.strip_prefix(OPERATION_TRAILER)?.trim();
    Some((op_id, sha))
}

fn message_carries_identity(message: &str, op: &SvnCommitOperation) -> bool {
    if let Some((op_id, sha)) = reposync_appended_trailer_pair(message) {
        return op_id == op.id && sha == op.source_git_sha;
    }

    let mut saw_personal_git_sha = false;
    for line in message.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("Git-SHA:") {
            saw_personal_git_sha |= value.trim() == op.source_git_sha;
        }
    }
    // Personal-format commits may omit RepoSync-Operation; the Git-SHA trailer
    // still binds the durable intent when tree/path proof succeeds below.
    saw_personal_git_sha
}

pub async fn inspect_git_to_svn_commit(
    svn: &SvnClient,
    op: &SvnCommitOperation,
) -> SvnCommitInspect {
    let info = match svn.info().await {
        Ok(info) => info,
        Err(error) => {
            return SvnCommitInspect::Unavailable {
                reason: format!("SVN inspection unavailable: {error}"),
            }
        }
    };
    if info.uuid != op.target_svn_uuid || info.url != op.target_svn_path {
        return SvnCommitInspect::Conflict {
            reason: "SVN UUID or path differs from the recorded target".into(),
        };
    }
    if info.latest_rev < op.pre_write_svn_rev {
        return SvnCommitInspect::Conflict {
            reason: "SVN revision is behind the recorded pre-write revision".into(),
        };
    }
    if info.latest_rev == op.pre_write_svn_rev {
        return match observed_svn_tree_at_revision(svn, info.latest_rev).await {
            Ok(tree) if tree == op.pre_write_svn_tree => SvnCommitInspect::AbsentUnchanged,
            Ok(_) => SvnCommitInspect::Conflict {
                reason: "pre-write revision is unchanged but its tree is not".into(),
            },
            Err(error) => SvnCommitInspect::Unavailable {
                reason: format!("could not hash the pre-write SVN tree: {error}"),
            },
        };
    }
    if info.latest_rev != op.pre_write_svn_rev + 1 {
        return SvnCommitInspect::Conflict {
            reason: "SVN advanced by more than the one planned revision".into(),
        };
    }
    let log = match svn.log(info.latest_rev, info.latest_rev).await {
        Ok(entries) => entries,
        Err(error) => {
            return SvnCommitInspect::Unavailable {
                reason: format!("SVN log inspection failed: {error}"),
            }
        }
    };
    if log.len() != 1 {
        return SvnCommitInspect::Unavailable {
            reason: "SVN log did not return exactly one candidate revision".into(),
        };
    }
    let entry = &log[0];
    if entry.revision != info.latest_rev {
        return SvnCommitInspect::Conflict {
            reason: "SVN log revision is not the immediate successor".into(),
        };
    }
    if !message_carries_identity(&entry.message, op) {
        return SvnCommitInspect::Conflict {
            reason: "successor revision does not carry the durable operation identity".into(),
        };
    }
    let identity = operation_path_identity(op, &info.root_url);
    let raw_observed: Vec<IntendedPath> = entry
        .changed_paths
        .iter()
        .map(|path| IntendedPath {
            action: path.action.clone(),
            path: path.path.clone(),
            content_sha256: None,
        })
        .collect();
    if raw_observed
        .iter()
        .any(|item| svn_log_path_to_branch_relative(&item.path, &identity).is_none())
    {
        return SvnCommitInspect::Conflict {
            reason: "successor changed paths include entries outside the pinned branch namespace"
                .into(),
        };
    }
    if !paths_match(&op.intended_changed_paths, &raw_observed, &identity) {
        return SvnCommitInspect::Conflict {
            reason: "successor changed paths are not a unique match for the intended write".into(),
        };
    }
    let mut observed: Vec<IntendedPath> = raw_observed
        .into_iter()
        .filter_map(|item| {
            let branch_relative = svn_log_path_to_branch_relative(&item.path, &identity)?;
            Some(IntendedPath {
                action: item.action,
                path: branch_relative,
                content_sha256: None,
            })
        })
        .collect();
    observed.sort_by(|a, b| a.path.cmp(&b.path));
    let tree = match observed_svn_tree_at_revision(svn, info.latest_rev).await {
        Ok(tree) => tree,
        Err(error) => {
            return SvnCommitInspect::Unavailable {
                reason: format!("could not hash the successor SVN tree: {error}"),
            }
        }
    };
    if tree != op.intended_svn_tree {
        return SvnCommitInspect::Conflict {
            reason: "successor tree is not the intended Git-to-SVN tree".into(),
        };
    }
    SvnCommitInspect::UniqueMatch {
        svn_rev: info.latest_rev,
        svn_tree: tree,
        changed_paths: observed,
    }
}

pub async fn apply_svn_commit_reconciliation(
    db: &Database,
    repo_id: &str,
    op_id: &str,
    svn: &SvnClient,
    projection: &str,
) -> Result<SvnCommitReconcileResult, DatabaseError> {
    let requested = db
        .get_svn_commit_operation(repo_id, op_id)?
        .ok_or_else(|| DatabaseError::Other("git-to-svn commit operation not found".into()))?;
    let active = db.active_svn_commit_operation(repo_id)?;
    if active.is_none() && requested.state == SvnCommitOperationState::Completed {
        return Ok(SvnCommitReconcileResult {
            inspect: SvnCommitInspect::UniqueMatch {
                svn_rev: requested.last_confirmed_svn_rev.unwrap_or(0),
                svn_tree: requested
                    .last_confirmed_svn_tree
                    .clone()
                    .unwrap_or_default(),
                changed_paths: requested.intended_changed_paths.clone(),
            },
            operation: requested,
            finalized: true,
            resume_authorized: false,
        });
    }
    if active.as_ref().is_none_or(|op| op.id != op_id)
        || requested.state != SvnCommitOperationState::ReconciliationRequired
    {
        return Err(DatabaseError::Other(
            "operation is not this repository's active git-to-svn reconciliation hold".into(),
        ));
    }
    let info = svn
        .info()
        .await
        .map_err(|e| DatabaseError::Other(format!("SVN inspection unavailable: {e}")))?;
    let fingerprint =
        svn_commit_target_fingerprint(repo_id, &info.uuid, svn.url(), &info.url, projection);
    if requested.target_fingerprint != fingerprint {
        let operation = db.note_svn_commit_reconciliation_reason(
            repo_id,
            op_id,
            "Git-to-SVN target fingerprint changed; review required",
        )?;
        return Ok(SvnCommitReconcileResult {
            operation,
            inspect: SvnCommitInspect::Conflict {
                reason: "Git-to-SVN target fingerprint changed; review required".into(),
            },
            finalized: false,
            resume_authorized: false,
        });
    }
    let inspect = inspect_git_to_svn_commit(svn, &requested).await;
    match &inspect {
        SvnCommitInspect::UniqueMatch {
            svn_rev, svn_tree, ..
        } => {
            match db.finalize_verified_svn_commit(repo_id, op_id, *svn_rev, svn_tree, &fingerprint)
            {
                Ok(ReconciledSvnCommit {
                    operation,
                    finalized,
                    resume_authorized,
                }) => Ok(SvnCommitReconcileResult {
                    operation,
                    inspect,
                    finalized,
                    resume_authorized,
                }),
                Err(DatabaseError::Other(reason))
                    if reason.contains("fingerprint")
                        || reason.contains("differs")
                        || reason.contains("not after") =>
                {
                    let operation =
                        db.note_svn_commit_reconciliation_reason(repo_id, op_id, &reason)?;
                    Ok(SvnCommitReconcileResult {
                        operation,
                        inspect: SvnCommitInspect::Conflict { reason },
                        finalized: false,
                        resume_authorized: false,
                    })
                }
                Err(error) => Err(error),
            }
        }
        SvnCommitInspect::AbsentUnchanged => {
            let operation = db.authorize_git_to_svn_resume(
                repo_id,
                op_id,
                "Effect is absent and the pre-write SVN target is unchanged; the worker may resume that one planned write",
            )?;
            Ok(SvnCommitReconcileResult {
                operation,
                inspect,
                finalized: false,
                resume_authorized: true,
            })
        }
        SvnCommitInspect::Conflict { reason } | SvnCommitInspect::Unavailable { reason } => {
            let operation = db.note_svn_commit_reconciliation_reason(repo_id, op_id, reason)?;
            Ok(SvnCommitReconcileResult {
                operation,
                inspect,
                finalized: false,
                resume_authorized: false,
            })
        }
    }
}

#[cfg(test)]
mod identity_tests {
    use super::message_carries_identity;
    use crate::db::svn_commit_operations::{SvnCommitOperation, SvnCommitOperationState};

    fn sample_op(operation_id: &str, git_sha: &str) -> SvnCommitOperation {
        SvnCommitOperation {
            version: 1,
            id: operation_id.into(),
            repo_id: "pair".into(),
            operation_type: "git_to_svn".into(),
            initiator_id: "test".into(),
            request_id: "req".into(),
            target_fingerprint: "fp".into(),
            created_at: "2020-01-01T00:00:00Z".into(),
            updated_at: "2020-01-01T00:00:00Z".into(),
            state: SvnCommitOperationState::ReconciliationRequired,
            source_git_sha: git_sha.into(),
            source_git_parent: None,
            source_git_tree: "tree".into(),
            target_svn_uuid: "uuid".into(),
            target_svn_path: "/svn".into(),
            target_svn_root_url: String::new(),
            target_svn_branch_path: String::new(),
            pre_write_svn_rev: 1,
            pre_write_svn_tree: "pre".into(),
            projection: "{}".into(),
            intended_changed_paths: Vec::new(),
            intended_svn_tree: "intended".into(),
            author: "svn".into(),
            source_message: "msg".into(),
            last_confirmed_svn_rev: None,
            last_confirmed_svn_tree: None,
            resume_authorized: false,
            outcome_detail: None,
        }
    }

    #[test]
    fn repo_sync_operation_trailer_must_match_operation_id() {
        let sha = "c".repeat(40);
        let op = sample_op("op-expected", &sha);
        let message = format!("sync\n\nRepoSync-Operation: op-other\nRepoSync-Git-SHA: {sha}");
        assert!(!message_carries_identity(&message, &op));
    }

    #[test]
    fn legacy_git_sha_without_operation_trailer_still_matches() {
        let sha = "d".repeat(40);
        let op = sample_op("op-expected", &sha);
        let message = format!("sync\n\nGit-SHA: {sha}");
        assert!(message_carries_identity(&message, &op));
    }

    #[test]
    fn quoted_operation_trailer_in_body_does_not_override_appended_block() {
        let sha = "e".repeat(40);
        let op = sample_op("op-expected", &sha);
        let message = format!(
            "release notes\n\nRepoSync-Operation: op-decoy\nRepoSync-Git-SHA: decoysha\n\nRepoSync-Operation: op-expected\nRepoSync-Git-SHA: {sha}"
        );
        assert!(message_carries_identity(&message, &op));
    }
}
