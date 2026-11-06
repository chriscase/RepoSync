//! Startup credential seeding for managed repositories.
//!
//! Seeding never copies a parent or global secret into a missing child key.
//! Keyless descendants inherit at resolve time so parent rotation stays
//! truthful. Explicit revocations — a stored empty string — are never
//! overwritten. A sole root without a key continues to resolve the global
//! value without materializing an explicit override.

use crate::errors::DatabaseError;

use super::Database;

/// Ensure managed repositories keep truthful credential provenance.
///
/// Existing per-repo keys, including explicit empty revocations, are left
/// unchanged. Missing child keys are not materialized from a parent or global
/// value; [`Database::resolve_credential_chain_state`] inherits at read time.
pub fn seed_per_repo_credentials(db: &Database) -> Result<(), DatabaseError> {
    // Touch each key so a later writer cannot treat absence as a license to
    // copy inherited secrets into an explicit child override.
    for repo in db.list_repositories()? {
        for prefix in ["secret_svn_password", "secret_git_token"] {
            let _ = db.get_state(&format!("{}_{}", prefix, repo.id))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::models::Repository;

    fn insert_repo(db: &Database, repo: Repository) {
        db.insert_repository(&repo).unwrap();
    }

    fn sample_repo(id: &str, parent_id: Option<&str>) -> Repository {
        let now = Utc::now().to_rfc3339();
        Repository {
            id: id.into(),
            name: id.into(),
            svn_url: "file:///dev/null".into(),
            svn_branch: String::new(),
            svn_username: "fixture".into(),
            git_provider: "local".into(),
            git_api_url: String::new(),
            git_repo: "origin.git".into(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 5,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: parent_id.map(str::to_string),
            created_at: now.clone(),
            updated_at: now,
            last_svn_rev: 0,
            last_git_sha: String::new(),
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        }
    }

    fn setup_db() -> Database {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db
    }

    #[test]
    fn child_revocation_is_not_overwritten_by_parent() {
        let db = setup_db();
        insert_repo(&db, sample_repo("parent", None));
        insert_repo(&db, sample_repo("child", Some("parent")));
        db.set_state("secret_git_token_parent", "parent-token")
            .unwrap();
        db.set_state("secret_git_token_child", "").unwrap();

        seed_per_repo_credentials(&db).unwrap();

        assert_eq!(
            db.get_state("secret_git_token_child").unwrap().as_deref(),
            Some("")
        );
    }

    #[test]
    fn keyless_child_inherits_parent_secret() {
        let db = setup_db();
        insert_repo(&db, sample_repo("parent", None));
        insert_repo(&db, sample_repo("child", Some("parent")));
        db.set_state("secret_svn_password_parent", "parent-svn")
            .unwrap();

        seed_per_repo_credentials(&db).unwrap();

        assert_eq!(
            db.get_state("secret_svn_password_child").unwrap(),
            None,
            "startup seeding must not materialize an inherited child override"
        );
        assert_eq!(
            db.resolve_credential_chain_state("child", "secret_svn_password"),
            crate::db::queries::CredentialChainState::inherited("parent-svn".into())
        );
    }

    #[test]
    fn startup_rotation_reload_preserves_inherited_explicit_and_revoked() {
        let db = setup_db();
        insert_repo(&db, sample_repo("parent", None));
        insert_repo(&db, sample_repo("inherited", Some("parent")));
        insert_repo(&db, sample_repo("explicit", Some("parent")));
        insert_repo(&db, sample_repo("revoked", Some("parent")));
        insert_repo(&db, sample_repo("other-root", None));
        db.set_state("secret_git_token_parent", "token-a").unwrap();
        db.set_state("secret_git_token_explicit", "child-override")
            .unwrap();
        db.set_state("secret_git_token_revoked", "").unwrap();
        db.set_state("secret_git_token_other-root", "other-root-token")
            .unwrap();

        seed_per_repo_credentials(&db).unwrap();
        assert_eq!(db.get_state("secret_git_token_inherited").unwrap(), None);
        assert_eq!(
            db.resolve_credential_chain_state("inherited", "secret_git_token"),
            crate::db::queries::CredentialChainState::inherited("token-a".into())
        );

        db.save_repository_credentials("parent", None, Some("token-b"), "admin", None, None)
            .unwrap();
        seed_per_repo_credentials(&db).unwrap();

        assert_eq!(
            db.resolve_credential_chain_state("inherited", "secret_git_token"),
            crate::db::queries::CredentialChainState::inherited("token-b".into())
        );
        assert_eq!(
            db.resolve_credential_chain_state("explicit", "secret_git_token"),
            crate::db::queries::CredentialChainState::resolved("child-override".into())
        );
        assert_eq!(
            db.resolve_credential_chain_state("revoked", "secret_git_token"),
            crate::db::queries::CredentialChainState::revoked()
        );
        assert_eq!(
            db.resolve_credential_chain_state("other-root", "secret_git_token"),
            crate::db::queries::CredentialChainState::resolved("other-root-token".into())
        );
        assert_eq!(db.get_state("secret_git_token_inherited").unwrap(), None);
    }
}
