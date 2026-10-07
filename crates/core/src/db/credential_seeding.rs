//! Startup credential seeding for managed repositories.
//!
//! Seeds missing per-repo keys from a parent or (for a sole root repo) the
//! global key. Explicit revocations — a stored empty string — are never
//! overwritten.

use tracing::info;

use crate::errors::DatabaseError;
use crate::models::Repository;

use super::Database;

/// Ensure managed repositories have per-repo credential keys where appropriate.
///
/// - Child repos without a key inherit a non-empty parent value.
/// - A sole root repo without a key may inherit a non-empty global value.
/// - Any existing per-repo key, including an explicit empty revocation, is left
///   unchanged.
pub fn seed_per_repo_credentials(db: &Database) -> Result<(), DatabaseError> {
    let repos = db.list_repositories()?;
    let parent_count = repos.iter().filter(|r| r.parent_id.is_none()).count();

    for repo in &repos {
        if seed_repo_credential(db, repo, parent_count, "secret_svn_password")? {
            info!(repo_name = %repo.name, "migrated SVN password to per-repo key");
        }
        if seed_repo_credential(db, repo, parent_count, "secret_git_token")? {
            info!(repo_name = %repo.name, "migrated Git token to per-repo key");
        }
    }
    Ok(())
}

/// Seed one credential when the repo has no per-repo key yet.
///
/// Returns `true` when a value was written.
fn seed_repo_credential(
    db: &Database,
    repo: &Repository,
    parent_count: usize,
    key_prefix: &str,
) -> Result<bool, DatabaseError> {
    let repo_key = format!("{}_{}", key_prefix, repo.id);
    if db.get_state(&repo_key)?.is_some() {
        return Ok(false);
    }

    let source = repo
        .parent_id
        .as_ref()
        .and_then(|pid| {
            db.get_state(&format!("{}_{}", key_prefix, pid))
                .ok()
                .flatten()
                .filter(|v| !v.is_empty())
        })
        .or_else(|| {
            if repo.parent_id.is_none() && parent_count == 1 {
                db.get_state(key_prefix)
                    .ok()
                    .flatten()
                    .filter(|v| !v.is_empty())
            } else {
                None
            }
        });

    if let Some(val) = source {
        db.set_state(&repo_key, &val)?;
        Ok(true)
    } else {
        Ok(false)
    }
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
            db.get_state("secret_svn_password_child")
                .unwrap()
                .as_deref(),
            Some("parent-svn")
        );
    }
}
