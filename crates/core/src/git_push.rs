//! Read helpers for one team-engine SVN→Git push verification.

use crate::errors::GitError;
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
