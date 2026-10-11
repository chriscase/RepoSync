use reposync_core::db::{personal_scope::PERSONAL_SCOPE_KEY, Database};
use reposync_core::errors::GitError;
use reposync_core::git::{resolve_git_http_auth_token_for_clone, GitClient};

#[test]
fn missing_clone_credentials_still_allow_anonymous_local_clone() {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let origin = tmp.path().join("origin.git");
    git2::Repository::init_bare(&origin).unwrap();
    for (case, config_token) in [("missing", None), ("empty", Some(""))] {
        let token =
            resolve_git_http_auth_token_for_clone(&db, PERSONAL_SCOPE_KEY, config_token).unwrap();
        assert!(token.is_none());
        GitClient::clone_repo(
            origin.to_str().unwrap(),
            &tmp.path().join(case),
            token.as_deref(),
        )
        .unwrap();
    }
}

#[test]
fn revoked_clone_credentials_refuse_global_and_config_fallback() {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    db.set_state("secret_git_token", "global-fixture-token")
        .unwrap();
    db.set_state(&format!("secret_git_token_{PERSONAL_SCOPE_KEY}"), "")
        .unwrap();
    for config_token in [None, Some(""), Some("config-fixture-token")] {
        let error = resolve_git_http_auth_token_for_clone(&db, PERSONAL_SCOPE_KEY, config_token)
            .unwrap_err();
        assert!(matches!(error, GitError::CredentialsRevoked));
        assert!(error.is_permanent());
    }
}
