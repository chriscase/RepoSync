//! Git object preparation for the bounded clean-child refresh case. The caller
//! must separately prove mappings, SVN state, credentials, identities and holds.

use git2::{ObjectType, Oid, Repository, Signature, Time, Tree, TreeWalkMode, TreeWalkResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::publication::require_complete_git_graph;
use crate::errors::GitError;
use crate::history_inspect::is_full_git_oid;

const MAX_PARENT_ADVANCE: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanChildGitPins {
    pub child_tip: String,
    pub parent_tip: String,
    pub parent_baseline_tip: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedCleanChildGit {
    pub pins: CleanChildGitPins,
    pub operation_id: Uuid,
    pub prepared_at: i64,
    pub prepared_sha: String,
    pub tree_sha: String,
    pub parent_advance_commits: usize,
}

fn refused(detail: &str) -> GitError {
    git2::Error::from_str(detail).into()
}

fn pin(raw: &str) -> Result<Oid, GitError> {
    if !is_full_git_oid(raw) {
        return Err(refused("pair refresh requires complete Git object IDs"));
    }
    Ok(Oid::from_str(raw)?)
}

/// Check pinned objects, not a mutable working copy or inherited attribute
/// configuration. This first case supports regular files and executable bits.
/// Attribute rules, LFS pointers, symlinks and submodules need a later policy.
pub fn require_supported_refresh_tree(repo: &Repository, tree: &Tree<'_>) -> Result<(), GitError> {
    let mut failure = None;
    tree.walk(TreeWalkMode::PreOrder, |_, entry| {
        let result = (|| {
            match (entry.kind(), entry.filemode()) {
                (Some(ObjectType::Tree), 0o040000) => return Ok(()),
                (Some(ObjectType::Blob), 0o100644 | 0o100755) => {}
                _ => return Err(refused("unsupported pair refresh file semantics")),
            }
            let blob = repo.find_blob(entry.id())?;
            let body = blob.content();
            if crate::lfs::is_lfs_pointer(body)
                || body.starts_with(b"version https://git-lfs.github.com/spec/v1")
            {
                return Err(refused("pair refresh cannot publish Git LFS pointers"));
            }
            if entry.name_bytes() == b".gitattributes" {
                let rules = std::str::from_utf8(body)
                    .map_err(|_| refused("unsupported pair refresh attribute rules"))?;
                if rules
                    .lines()
                    .map(str::trim)
                    .any(|line| !line.is_empty() && !line.starts_with('#'))
                {
                    return Err(refused("unsupported pair refresh attribute rules"));
                }
            }
            Ok::<(), GitError>(())
        })();
        match result {
            Ok(()) => TreeWalkResult::Ok,
            Err(error) => {
                failure = Some(error);
                TreeWalkResult::Abort
            }
        }
    })
    .or_else(|error| {
        if failure.is_some() {
            Ok(())
        } else {
            Err(error)
        }
    })?;
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Create an unreferenced, reproducible two-parent commit whose tree is exactly
/// the pinned parent tree. Never update HEAD or any branch here. Job recovery
/// pins the returned SHA before publishing either external side.
pub fn prepare_clean_child_git(
    repo: &Repository,
    pins: &CleanChildGitPins,
    operation_id: Uuid,
    prepared_at: i64,
) -> Result<PreparedCleanChildGit, GitError> {
    require_complete_git_graph(repo)?;
    if operation_id.is_nil() || prepared_at < 0 {
        return Err(refused("invalid pair refresh preparation identity"));
    }
    let child_id = pin(&pins.child_tip)?;
    let parent_id = pin(&pins.parent_tip)?;
    let baseline_id = pin(&pins.parent_baseline_tip)?;
    let child = repo.find_commit(child_id)?;
    let parent = repo.find_commit(parent_id)?;
    let baseline = repo.find_commit(baseline_id)?;
    if parent_id == baseline_id {
        return Err(refused("pair refresh has no parent advance"));
    }
    if !repo.graph_descendant_of(parent_id, baseline_id)?
        || (child_id != baseline_id && !repo.graph_descendant_of(child_id, baseline_id)?)
        || child.tree_id() != baseline.tree_id()
    {
        return Err(refused(
            "pair refresh requires unchanged child and shared baseline",
        ));
    }
    let mut current = parent_id;
    let mut parent_advance_commits = 0;
    while current != baseline_id {
        let commit = repo.find_commit(current)?;
        if commit.parent_count() != 1 || parent_advance_commits == MAX_PARENT_ADVANCE {
            return Err(refused("unsupported nonlinear or oversized parent advance"));
        }
        require_supported_refresh_tree(repo, &commit.tree()?)?;
        current = commit.parent_id(0)?;
        parent_advance_commits += 1;
    }
    // Also scan the unchanged input trees. A deleted LFS pointer or attribute
    // rule in the baseline must not bypass the admitted content policy.
    for tree in [child.tree()?, baseline.tree()?, parent.tree()?] {
        require_supported_refresh_tree(repo, &tree)?;
    }
    let tree = parent.tree()?;
    let signature = Signature::new("RepoSync", "reposync@localhost", &Time::new(prepared_at, 0))?;
    let message =
        format!("[reposync] Update pair from parent\n\nRepoSync-Pair-Refresh: {operation_id}\n");
    let prepared = repo.commit(
        None,
        &signature,
        &signature,
        &message,
        &tree,
        &[&child, &parent],
    )?;
    Ok(PreparedCleanChildGit {
        pins: pins.clone(),
        operation_id,
        prepared_at,
        prepared_sha: prepared.to_string(),
        tree_sha: tree.id().to_string(),
        parent_advance_commits,
    })
}
