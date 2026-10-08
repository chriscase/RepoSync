//! #67 late-pair publish: baseline SVN copy + pending Git replay via SyncEngine.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use chrono::Utc;
use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::Database;
use reposync_core::git::client::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::late_pair::probe_svn_target;
use reposync_core::late_pair::{
    collect_verified_mappings, evaluate_admission, LatePairPlan, LatePairRequest,
};
use reposync_core::late_pair_publish::{
    publish_admitted_late_pair, validate_git_publish_preflight, PublishCredentials,
};
use reposync_core::models::{Repository, SyncDirection, SyncRecord, SyncRecordStatus};
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use tempfile::TempDir;

fn git_cmd(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "f@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "f@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn svn_available() -> bool {
    Command::new("svn").arg("--version").output().is_ok()
        && Command::new("svnadmin").arg("--version").output().is_ok()
}

struct PublishFixture {
    tmp: TempDir,
    db: Database,
    parent_id: String,
    parent: Repository,
    svn_url: String,
    target_url: String,
    _bare: std::path::PathBuf,
    request: LatePairRequest,
    plan: LatePairPlan,
    probe: reposync_core::late_pair::SvnTargetProbe,
    tip: String,
    _base_sha: String,
    _copy_rev: i64,
    config: AppConfig,
    identity: Arc<IdentityMapper>,
}

async fn build_publish_fixture() -> PublishFixture {
    let tmp = TempDir::new().unwrap();
    let svn_repo = tmp.path().join("svn");
    assert!(Command::new("svnadmin")
        .args(["create", svn_repo.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let svn_url = format!("file://{}", svn_repo.display());
    let wc = tmp.path().join("wc");
    assert!(Command::new("svn")
        .args(["mkdir", &format!("{svn_url}/trunk"), "-m", "trunk"])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("svn")
        .args([
            "checkout",
            &format!("{svn_url}/trunk"),
            wc.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    std::fs::write(wc.join("origin.txt"), "SVN origin\n").unwrap();
    assert!(Command::new("svn")
        .args(["add", "origin.txt"])
        .current_dir(&wc)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("svn")
        .args(["commit", "-m", "origin"])
        .current_dir(&wc)
        .status()
        .unwrap()
        .success());
    std::fs::write(wc.join("origin.txt"), "SVN origin v2\n").unwrap();
    assert!(Command::new("svn")
        .args(["commit", "-m", "second"])
        .current_dir(&wc)
        .status()
        .unwrap()
        .success());

    let local = tmp.path().join("local");
    std::fs::create_dir_all(&local).unwrap();
    let bare = local.join("history.git");
    let git_work = tmp.path().join("git-work");
    std::fs::create_dir_all(&git_work).unwrap();
    git_cmd(tmp.path(), &["init", "--bare", bare.to_str().unwrap()]);
    git_cmd(&git_work, &["init", "-b", "main"]);
    git_cmd(
        &git_work,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    std::fs::write(git_work.join("origin.txt"), "SVN origin\n").unwrap();
    git_cmd(&git_work, &["add", "origin.txt"]);
    git_cmd(&git_work, &["commit", "-m", "Verified SVN origin"]);
    git_cmd(&git_work, &["push", "-u", "origin", "main"]);
    let base_sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&git_work)
        .output()
        .unwrap();
    let base_sha = String::from_utf8_lossy(&base_sha.stdout).trim().to_string();

    git_cmd(&git_work, &["checkout", "-b", "feature"]);
    std::fs::write(git_work.join("feature.txt"), "step one\n").unwrap();
    git_cmd(&git_work, &["add", "feature.txt"]);
    git_cmd(&git_work, &["commit", "-m", "Feature step one"]);
    std::fs::write(git_work.join("feature.txt"), "step two\n").unwrap();
    git_cmd(&git_work, &["commit", "-am", "Feature step two"]);
    git_cmd(&git_work, &["push", "-u", "origin", "feature"]);
    let tip = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&git_work)
        .output()
        .unwrap();
    let tip = String::from_utf8_lossy(&tip.stdout).trim().to_string();

    let db_path = tmp.path().join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let now = Utc::now();
    let parent_id = "parent".to_string();
    db.insert_repository(&Repository {
        id: parent_id.clone(),
        name: "Parent".into(),
        svn_url: svn_url.clone(),
        svn_branch: "trunk".into(),
        svn_username: String::new(),
        git_provider: "local".into(),
        git_api_url: format!("file://{}", tmp.path().display()),
        git_repo: "local/history".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 5,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: now.to_rfc3339(),
        updated_at: now.to_rfc3339(),
        last_svn_rev: 2,
        last_git_sha: base_sha.clone(),
        last_sync_at: None,
        sync_status: "idle".into(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    })
    .unwrap();
    db.insert_sync_record(&SyncRecord {
        id: "m1".into(),
        repo_id: Some(parent_id.clone()),
        svn_revision: Some(2),
        git_hash: Some(base_sha.clone()),
        direction: SyncDirection::SvnToGit,
        author: "svn".into(),
        message: "origin".into(),
        timestamp: now,
        synced_at: now,
        status: SyncRecordStatus::Applied,
    })
    .unwrap();

    let parent_git = tmp.path().join("repos").join(&parent_id).join("git-repo");
    std::fs::create_dir_all(parent_git.parent().unwrap()).unwrap();
    assert!(Command::new("git")
        .args([
            "clone",
            bare.to_str().unwrap(),
            parent_git.to_str().unwrap(),
        ])
        .status()
        .unwrap()
        .success());

    let request = LatePairRequest {
        parent_id: parent_id.clone(),
        git_branch: "feature".into(),
        svn_branch: "branches/feature".into(),
        skip_import: false,
        compatibility_skip_import: false,
        dry_run: false,
    };
    let mappings = collect_verified_mappings(&db, &parent_id).unwrap();
    let plan = evaluate_admission(&mappings, Some(&parent_git), None, &request).unwrap();
    assert_eq!(plan.pending_git.count, 2);

    let parent = db.get_repository(&parent_id).unwrap().unwrap();
    let parent_svn_url = format!("{svn_url}/trunk");
    let target_url = format!("{svn_url}/branches/feature");
    let parent_client = SvnClient::new(&parent_svn_url, "", "");
    let target_client = SvnClient::new(&target_url, "", "");
    let (probe, _) = probe_svn_target(
        &parent_client,
        &target_client,
        &parent_svn_url,
        &target_url,
        plan.svn_source_revision,
    )
    .await;

    let copy_rev = plan.proposed_svn_copy_source_revision.unwrap();
    let config = app_config(tmp.path(), &svn_url);
    let identity = Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap());
    PublishFixture {
        tmp,
        db,
        parent_id,
        parent,
        svn_url,
        target_url,
        _bare: bare,
        request,
        plan,
        probe,
        tip,
        _base_sha: base_sha,
        _copy_rev: copy_rev,
        config,
        identity,
    }
}

fn app_config(tmp: &Path, svn_url: &str) -> AppConfig {
    let toml = format!(
        r#"
[daemon]
data_dir = "{data}"

[svn]
url = "{svn}"
username = ""
password_env = ""
layout = "custom"

[github]
repo = "test/repo"
default_branch = "main"
token_env = ""
"#,
        data = tmp.display().to_string().replace('\\', "/"),
        svn = svn_url
    );
    toml::from_str(&toml).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_late_pair_publish_replays_pending_git() {
    assert!(
        svn_available(),
        "svn and svnadmin are required; do not count a skipped test as evidence"
    );
    let fx = build_publish_fixture().await;
    let child_id = "child-feature".to_string();
    let published = publish_admitted_late_pair(
        &fx.db,
        &fx.config,
        &PublishCredentials {
            svn_password: String::new(),
        },
        &fx.parent,
        &fx.request,
        &fx.plan,
        &fx.probe,
        &child_id,
        "fixture",
        "req-1",
        &fx.identity,
    )
    .await
    .expect("publish should succeed");

    assert!(published.published);
    assert!(published.scheduler_active);
    let child = fx.db.get_repository(&child_id).unwrap().unwrap();
    assert!(child.enabled);
    assert_eq!(child.last_git_sha, fx.tip);

    let target = SvnClient::new(&fx.target_url, "", "");
    let head = target.info().await.unwrap().latest_rev;
    let export = fx.tmp.path().join("export");
    target.export("", head, &export).await.unwrap();
    let content = std::fs::read_to_string(export.join("feature.txt")).unwrap();
    assert_eq!(content, "step two\n");

    let child_git = fx.tmp.path().join("repos").join(&child_id).join("git-repo");
    let db_path = fx.tmp.path().join("reposync.db");
    let mut cfg = app_config(fx.tmp.path(), &fx.svn_url);
    cfg.github.default_branch = "feature".into();
    let mut child_engine = SyncEngine::new(
        cfg,
        Database::new(&db_path).unwrap(),
        target,
        GitClient::new(&child_git).unwrap(),
        fx.identity.clone(),
    );
    child_engine.set_repo_id(child_id.clone());
    let stats = child_engine.run_sync_cycle().await.unwrap();
    assert_eq!(stats.git_to_svn_count, 0);

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R06_LATE_PAIR_PUBLISH_REPLAY",
            "pending_git":2,
            "svn_feature_file":"step two\\n",
            "repeat_noop":true
        })
    );
}

#[test]
fn https_validate_preflight_refuses_missing_and_revoked_tokens_quickly() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let parent = Repository {
        id: "https-parent".into(),
        name: "Https".into(),
        svn_url: "file:///tmp/unused".into(),
        svn_branch: "trunk".into(),
        svn_username: String::new(),
        git_provider: "github".into(),
        git_api_url: "https://api.github.com".into(),
        git_repo: "acme/widget".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 5,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
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
    };
    db.insert_repository(&parent).unwrap();
    let err = validate_git_publish_preflight(&parent, "feature", &db).unwrap_err();
    assert_eq!(err.reason, "git_credentials_missing");
    db.set_state("secret_git_token_https-parent", "").unwrap();
    let err_revoked = validate_git_publish_preflight(&parent, "feature", &db).unwrap_err();
    assert_eq!(err_revoked.reason, "git_credentials_revoked");
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R06_LATE_PAIR_PUBLISH_HTTPS_CREDENTIALS_REFUSE",
            "preflight_before_svn":true
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreign_preexisting_svn_target_still_blocks_publish() {
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    assert!(Command::new("svn")
        .args([
            "mkdir",
            &format!("{}/branches", fx.svn_url),
            "-m",
            "branches",
        ])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("svn")
        .args([
            "copy",
            &format!("{}/trunk@1", fx.svn_url),
            &fx.target_url,
            "-m",
            "foreign branch",
        ])
        .status()
        .unwrap()
        .success());

    let parent_svn_url = format!("{}/trunk", fx.svn_url);
    let target_client = SvnClient::new(&fx.target_url, "", "");
    let parent_client = SvnClient::new(&parent_svn_url, "", "");
    let (probe, _) = probe_svn_target(
        &parent_client,
        &target_client,
        &parent_svn_url,
        &fx.target_url,
        fx.plan.svn_source_revision,
    )
    .await;
    assert!(probe.exists);

    let err = publish_admitted_late_pair(
        &fx.db,
        &fx.config,
        &PublishCredentials {
            svn_password: String::new(),
        },
        &fx.parent,
        &fx.request,
        &fx.plan,
        &probe,
        "child-foreign",
        "fixture",
        "req-foreign",
        &fx.identity,
    )
    .await
    .unwrap_err();
    assert_eq!(err.reason, "existing_svn_target_blocks_publish");
    assert!(fx
        .db
        .list_child_repositories(&fx.parent_id)
        .unwrap()
        .is_empty());
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R06_NO_ACTIVE_ON_PARTIAL",
            "foreign_copyfrom_rev":1,
            "child_rows":0
        })
    );
}
