//! Initial-import exclusion must precede LFS, API and worktree work.
use std::sync::{Arc, Mutex};

use reposync_core::config::GitProvider;
use reposync_core::db::Database;
use reposync_core::git::{github::GitHubClient, GitClient};
use reposync_core::personal_config::PersonalConfig;
use reposync_core::svn::SvnClient;
use reposync_personal::commit_format::CommitFormatter;
use reposync_personal::initial_import::{ImportMode, InitialImport};

fn config(path: &std::path::Path) -> PersonalConfig {
    serde_json::from_value(serde_json::json!({
        "personal": {"data_dir":path},
        "svn": {"url":"file:///nonexistent", "username":"fixture", "password_env":"UNUSED"},
        "github": {"api_url":"http://127.0.0.1:1", "repo":"fixture/repo", "token_env":"UNUSED", "default_branch":"main", "auto_create":true, "private":true},
        "developer": {"name":"Fixture", "email":"fixture@example.com", "svn_username":"fixture"},
        "options": {"lfs_threshold":1}
    })).unwrap()
}

async fn attempt(db: &Database, path: &std::path::Path, mode: ImportMode) -> String {
    let workdir = path.join("git");
    let git = Arc::new(Mutex::new(GitClient::init(&workdir).unwrap()));
    std::fs::write(workdir.join("keep.txt"), b"unchanged").unwrap();
    let before = std::fs::read(workdir.join(".git/config")).unwrap();
    let config = config(path);
    let formatter = CommitFormatter::new(&config.commit_format);
    let svn = SvnClient::new(&config.svn.url, "fixture", "");
    let github = GitHubClient::new(&config.github.api_url, "", GitProvider::GitHub);
    let importer = InitialImport {
        svn_client: &svn,
        git_client: &git,
        github_client: &github,
        db,
        config: &config,
        formatter: &formatter,
    };
    let error = importer.import(mode).await.unwrap_err();
    assert_eq!(
        std::fs::read(workdir.join("keep.txt")).unwrap(),
        b"unchanged"
    );
    assert_eq!(std::fs::read(workdir.join(".git/config")).unwrap(), before);
    assert!(!workdir.join(".gitattributes").exists());
    assert!(!workdir.join(".git/index").exists());
    assert!(db.list_commit_map(10).unwrap().is_empty());
    assert!(db.get_watermark("svn_rev").unwrap().is_none());
    assert!(db.get_watermark("git_sha").unwrap().is_none());
    format!("{error:#}")
}

#[tokio::test]
async fn both_modes_refuse_existing_global_preparation_and_uncertain_publication() {
    for uncertain in [false, true] {
        for mode in [ImportMode::Snapshot, ImportMode::Full] {
            let dir = tempfile::tempdir().unwrap();
            let db = Database::in_memory().unwrap();
            db.initialize().unwrap();
            let window = db.begin_ordinary_writer_window(None).unwrap();
            if uncertain {
                window.mark_unjournalled_publication_possible().unwrap();
            }
            let error = attempt(&db, dir.path(), mode).await;
            assert!(
                error.contains("another run owns this writer scope"),
                "{error}"
            );
            drop(window);
            assert_eq!(db.begin_ordinary_writer_window(None).is_err(), uncertain);
        }
    }
}

#[tokio::test]
async fn both_modes_refuse_managed_global_checkpoints_before_external_or_local_work() {
    for mode in [ImportMode::Snapshot, ImportMode::Full] {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let repo = serde_json::from_value(serde_json::json!({
            "id":"managed", "name":"managed", "svn_url":"file:///nonexistent", "svn_branch":"trunk", "svn_username":"fixture", "git_provider":"gitea", "git_api_url":"https://example.com", "git_repo":"fixture/repo", "git_branch":"main", "sync_mode":"team", "poll_interval_secs":5, "lfs_threshold_mb":0, "auto_merge":false, "enabled":true, "created_by":null, "created_at":"t", "updated_at":"t"
        })).unwrap();
        db.insert_repository(&repo).unwrap();
        let error = attempt(&db, dir.path(), mode).await;
        assert!(
            error.contains("refusing global git_sha watermark"),
            "{error}"
        );
        drop(db.begin_ordinary_writer_window(None).unwrap());
    }
}
