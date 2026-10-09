//! Git remote credential application from managed credential-chain state.

use std::path::Path;

use tracing::{debug, warn};

use crate::db::queries::CredentialChainState;
use crate::db::Database;
use crate::errors::GitError;

use super::startup_handoff;
use super::GitClient;

/// Resolve HTTP(S) git token: scoped chain first, then optional config token.
pub fn resolve_git_http_auth_token(
    db: &Database,
    scope_id: &str,
    config_token: Option<&str>,
) -> Option<String> {
    let state = db.resolve_credential_chain_state(scope_id, "secret_git_token");
    if state.explicitly_revoked {
        return None;
    }
    if let Some(tok) = state.value.filter(|value| !value.is_empty()) {
        return Some(tok);
    }
    config_token
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Like [`resolve_git_http_auth_token`] plus same-process startup `clone_repo` handoff.
pub fn resolve_git_http_auth_token_for_workdir(
    db: &Database,
    scope_id: &str,
    config_token: Option<&str>,
    workdir: &Path,
) -> Option<String> {
    let chain_state = db.resolve_credential_chain_state(scope_id, "secret_git_token");
    if chain_state.explicitly_revoked {
        startup_handoff::forget_startup_http_auth(workdir);
        return None;
    }
    resolve_git_http_auth_token(db, scope_id, config_token)
        .or_else(|| startup_handoff::peek_startup_http_auth(workdir))
}

/// Apply the current chain/config/handoff resolution to a clean-path [`GitClient`].
pub fn sync_git_http_auth_from_resolution(
    git: &GitClient,
    workdir: &Path,
    db: &Database,
    scope_id: &str,
    config_token: Option<&str>,
) -> Result<(), GitError> {
    let has_origin = git.repo().find_remote("origin").is_ok();
    let chain_state = db.resolve_credential_chain_state(scope_id, "secret_git_token");
    if chain_state.explicitly_revoked {
        startup_handoff::forget_startup_http_auth(workdir);
        if has_origin {
            git.clear_http_auth_memory("origin")?;
        } else {
            git.clear_in_memory_http_auth("origin")?;
        }
        return Ok(());
    }
    let token = resolve_git_http_auth_token_for_workdir(db, scope_id, config_token, workdir);
    if let Some(tok) = token.as_deref() {
        if has_origin {
            git.ensure_remote_credentials("origin", Some(tok))?;
        } else {
            git.set_in_memory_http_auth(tok);
            startup_handoff::remember_startup_http_auth(workdir, tok);
        }
    } else if has_origin {
        git.clear_http_auth_memory("origin")?;
    } else {
        git.clear_in_memory_http_auth("origin")?;
    }
    Ok(())
}

/// Apply a resolved credential-chain state to an HTTP(S) remote.
///
/// - **Resolved** — store the token for CLI/callback auth; remote URL stays clean.
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
        git.clear_in_memory_http_auth(remote_name)
    }
}

/// Scheduler/daemon sync reload only: embed scoped tokens in `remote.origin.url`.
///
/// RS-11 credential-isolation tests depend on URL-embedded canaries on this path.
/// Import, setup, and late-pair publish/replay must use
/// [`apply_git_credential_chain_state`] (clean URL + subprocess env auth) instead.
pub fn apply_git_credential_chain_state_for_sync(
    git: &GitClient,
    remote_name: &str,
    state: &CredentialChainState,
) -> Result<(), GitError> {
    if state.explicitly_revoked {
        git.clear_remote_credentials(remote_name)
    } else if let Some(tok) = state.value.as_deref() {
        git.ensure_remote_credentials_embedded(remote_name, Some(tok))
    } else {
        Ok(())
    }
}

/// Apply only config/env git credentials to the legacy config git-repo remote.
///
/// Managed-repo credential chains must never be applied to `data_dir/git-repo`.
/// Tokens are kept out of `.git/config` (see [`GitClient::ensure_remote_credentials`]).
pub fn apply_config_remote_git_credentials(
    git: &GitClient,
    config_token: Option<&str>,
) -> Result<(), GitError> {
    if let Some(tok) = config_token.filter(|value| !value.is_empty()) {
        git.ensure_remote_credentials("origin", Some(tok))
    } else {
        git.clear_in_memory_http_auth("origin")
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
    apply_git_credential_chain_state_for_sync(git, remote_name, &state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::path::Path;
    use tempfile::TempDir;

    fn git_origin_url(repo_path: &Path) -> String {
        std::process::Command::new("git")
            .args(["config", "--get", "remote.origin.url"])
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

    #[test]
    fn apply_chain_resolved_keeps_clean_remote_url() {
        let tmp = TempDir::new().unwrap();
        let bare = tmp.path().join("origin.git");
        std::process::Command::new("git")
            .args(["init", "--bare", bare.to_str().unwrap()])
            .status()
            .unwrap();
        let work = tmp.path().join("work");
        std::process::Command::new("git")
            .args(["clone", bare.to_str().unwrap(), work.to_str().unwrap()])
            .status()
            .unwrap();
        let clean = "https://github.com/example/widget.git";
        let secret = "ghp_chain_resolved_secret";
        let legacy = "https://x-access-token:legacy@github.com/example/widget.git";
        std::process::Command::new("git")
            .args(["remote", "set-url", "origin", legacy])
            .current_dir(&work)
            .status()
            .unwrap();

        let git = GitClient::new(&work).unwrap();
        apply_git_credential_chain_state(
            &git,
            "origin",
            &CredentialChainState::resolved(secret.to_string()),
        )
        .unwrap();
        let after = git_origin_url(&work);
        let config = std::fs::read_to_string(work.join(".git/config")).unwrap();
        assert!(!after.contains(secret));
        assert!(!after.contains("legacy"));
        assert!(!after.contains("x-access-token:"));
        assert!(!config.contains(secret));
        assert_eq!(after, clean);
    }

    #[test]
    fn resolve_prefers_chain_over_config_token() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("secret_git_token_scope", "chain-token")
            .unwrap();
        let resolved = resolve_git_http_auth_token(&db, "scope", Some("config-token"));
        assert_eq!(resolved.as_deref(), Some("chain-token"));
    }

    #[test]
    fn resolve_chain_rotation_changes_token() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("secret_git_token_scope", "token-a").unwrap();
        assert_eq!(
            resolve_git_http_auth_token(&db, "scope", None).as_deref(),
            Some("token-a")
        );
        db.set_state("secret_git_token_scope", "token-b").unwrap();
        assert_eq!(
            resolve_git_http_auth_token(&db, "scope", None).as_deref(),
            Some("token-b")
        );
    }

    #[test]
    fn resolve_revoked_chain_ignores_config_fallback() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("secret_git_token_scope", "").unwrap();
        assert!(resolve_git_http_auth_token(&db, "scope", Some("config-token")).is_none());
    }

    #[test]
    fn resolve_revoked_chain_clears_startup_handoff() {
        let tmp = TempDir::new().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        startup_handoff::remember_startup_http_auth(&work, "handoff-token");
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("secret_git_token_scope", "").unwrap();
        assert!(
            resolve_git_http_auth_token_for_workdir(&db, "scope", Some("config-token"), &work)
                .is_none()
        );
        assert!(startup_handoff::peek_startup_http_auth(&work).is_none());
    }

    #[test]
    fn resolve_revoked_chain_beats_clone_handoff_and_config_token() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        // Same-process handoff recorded by HTTPS `clone_repo` with a config token.
        startup_handoff::remember_startup_http_auth(&dest, "clone-token");
        assert_eq!(
            startup_handoff::peek_startup_http_auth(&dest).as_deref(),
            Some("clone-token")
        );
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("secret_git_token_scope", "").unwrap();
        assert!(
            resolve_git_http_auth_token_for_workdir(&db, "scope", Some("clone-token"), &dest)
                .is_none()
        );
        assert!(startup_handoff::peek_startup_http_auth(&dest).is_none());
    }

    #[test]
    fn sync_from_resolution_clears_stale_memory_when_chain_revoked() {
        let tmp = TempDir::new().unwrap();
        let bare = tmp.path().join("bare.git");
        std::process::Command::new("git")
            .args(["init", "--bare", bare.to_str().unwrap()])
            .status()
            .unwrap();
        let work = tmp.path().join("work");
        std::process::Command::new("git")
            .args(["clone", bare.to_str().unwrap(), work.to_str().unwrap()])
            .status()
            .unwrap();
        let git = GitClient::new(&work).unwrap();
        git.ensure_remote_credentials("origin", Some("stale-token"))
            .unwrap();
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("secret_git_token_scope", "").unwrap();
        sync_git_http_auth_from_resolution(&git, &work, &db, "scope", Some("config-token"))
            .unwrap();
        assert!(git.stored_http_auth_token().is_none());
    }

    #[test]
    fn clone_repo_leaves_clean_config_for_file_remote() {
        let tmp = TempDir::new().unwrap();
        let bare = tmp.path().join("origin.git");
        std::process::Command::new("git")
            .args(["init", "--bare", bare.to_str().unwrap()])
            .status()
            .unwrap();
        let dest = tmp.path().join("dest");
        let url = format!("file://{}", bare.display());
        GitClient::clone_repo(&url, &dest, Some("unused-token")).unwrap();
        let config = std::fs::read_to_string(dest.join(".git/config")).unwrap();
        assert!(!config.contains("unused-token"));
        assert!(!config.contains("x-access-token:"));
    }
}
