//! Git remote credential application from managed credential-chain state.

use tracing::{debug, warn};

use crate::db::queries::CredentialChainState;
use crate::db::Database;
use crate::errors::GitError;

use super::GitClient;

/// Apply a resolved credential-chain state to an HTTP(S) remote.
///
/// - **Resolved** — embed the token in the remote URL.
/// - **Explicitly revoked** — strip userinfo from the remote URL.
/// - **Not found / error** — leave the remote unchanged (fail-safe).
pub fn apply_git_credential_chain_state(
    git: &GitClient,
    remote_name: &str,
    state: &CredentialChainState,
) -> Result<(), GitError> {
    if state.explicitly_revoked {
        git.clear_remote_credentials(remote_name)
    } else if let Some(tok) = state.value.as_deref() {
        git.ensure_remote_credentials(remote_name, Some(tok))
    } else {
        Ok(())
    }
}

/// Resolve the managed git-token chain for `repo_id` and apply it to `remote_name`.
///
/// This is the shared entry point used by the scheduler, daemon startup, and
/// import paths so a chain miss or DB error never strips URL-embedded tokens.
pub fn apply_managed_git_credentials(
    git: &GitClient,
    db: &Database,
    repo_id: &str,
    remote_name: &str,
) -> Result<(), GitError> {
    let state = db.resolve_credential_chain_state(repo_id, "secret_git_token");
    if state.explicitly_revoked {
        debug!(
            repo_id,
            "explicit git token revocation via credential chain"
        );
    } else if state.value.is_some() {
        debug!(repo_id, "resolved git token via credential chain");
    } else {
        warn!(
            repo_id,
            "git token not found via credential chain; preserving remote URL"
        );
    }
    apply_git_credential_chain_state(git, remote_name, &state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn git_origin_url(repo_path: &Path) -> String {
        std::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(repo_path)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    #[test]
    fn apply_chain_miss_leaves_embedded_token() {
        let tmp = TempDir::new().unwrap();
        let bare = tmp.path().join("origin.git");
        std::process::Command::new("git")
            .args(["init", "--bare", bare.to_str().unwrap()])
            .status()
            .unwrap();
        let bridge = tmp.path().join("bridge");
        std::process::Command::new("git")
            .args(["clone", bare.to_str().unwrap(), bridge.to_str().unwrap()])
            .status()
            .unwrap();
        let embedded = "embedded-unit-token";
        let url = format!("https://x-access-token:{embedded}@git.invalid/repo.git");
        std::process::Command::new("git")
            .args(["remote", "set-url", "origin", &url])
            .current_dir(&bridge)
            .status()
            .unwrap();

        let git = GitClient::new(&bridge).unwrap();
        apply_git_credential_chain_state(&git, "origin", &CredentialChainState::not_found())
            .unwrap();

        assert_eq!(git_origin_url(&bridge), url);
    }

    #[test]
    fn apply_explicit_revocation_strips_embedded_token() {
        let tmp = TempDir::new().unwrap();
        let bare = tmp.path().join("origin.git");
        std::process::Command::new("git")
            .args(["init", "--bare", bare.to_str().unwrap()])
            .status()
            .unwrap();
        let bridge = tmp.path().join("bridge");
        std::process::Command::new("git")
            .args(["clone", bare.to_str().unwrap(), bridge.to_str().unwrap()])
            .status()
            .unwrap();
        let embedded = "embedded-unit-token";
        let url = format!("https://x-access-token:{embedded}@git.invalid/repo.git");
        std::process::Command::new("git")
            .args(["remote", "set-url", "origin", &url])
            .current_dir(&bridge)
            .status()
            .unwrap();

        let git = GitClient::new(&bridge).unwrap();
        apply_git_credential_chain_state(&git, "origin", &CredentialChainState::revoked()).unwrap();

        let after = git_origin_url(&bridge);
        assert!(!after.contains("x-access-token:"));
        assert_eq!(after, "https://git.invalid/repo.git");
    }
}
