//! #67 late-pair publish: baseline SVN copy + pending Git replay via SyncEngine.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, LazyLock};

/// `LatePairPublishTestHook` is keyed by parent repo id (all fixtures use `parent`).
static REPLAY_PUBLISH_67_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

async fn replay_publish_67_guard() -> tokio::sync::MutexGuard<'static, ()> {
    REPLAY_PUBLISH_67_LOCK.lock().await
}

use chrono::Utc;
use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::late_pair_publish_operations::LatePairPublishState;
use reposync_core::db::Database;
use reposync_core::git::client::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::late_pair::probe_svn_target;
use reposync_core::late_pair::{
    collect_verified_mappings, evaluate_admission, LatePairPlan, LatePairRequest,
};
use reposync_core::late_pair_publish::{
    clear_late_pair_publish_test_hook, publish_admitted_late_pair, set_late_pair_publish_test_hook,
    validate_git_publish_preflight, LatePairPublishTestHook, PublishCredentials,
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
    let plan = evaluate_admission(&mappings, Some(&parent_git), None, &request, None).unwrap();
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
    let _guard = replay_publish_67_guard().await;
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
    let _guard = replay_publish_67_guard().await;
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

async fn publish_with_timeout(
    fx: &PublishFixture,
    child_id: &str,
    request_id: &str,
) -> Result<LatePairPlan, reposync_core::late_pair_publish::LatePairPublishRefusal> {
    tokio::time::timeout(
        std::time::Duration::from_secs(90),
        publish_admitted_late_pair(
            &fx.db,
            &fx.config,
            &PublishCredentials {
                svn_password: String::new(),
            },
            &fx.parent,
            &fx.request,
            &fx.plan,
            &fx.probe,
            child_id,
            "fixture",
            request_id,
            &fx.identity,
        ),
    )
    .await
    .expect("publish must finish within 90s (deadlock guard)")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_late_pair_publish_resumes_after_replay_hook() {
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    let _guard = replay_publish_67_guard().await;
    let child_id = "child-replay-hook".to_string();
    set_late_pair_publish_test_hook(
        &fx.parent_id,
        LatePairPublishTestHook {
            fail_replay_once: true,
            abort_after_svn_copy_before_journal: false,
            ..Default::default()
        },
    );
    let first = publish_with_timeout(&fx, &child_id, "req-replay-1")
        .await
        .expect("hook path returns Ok stub");
    clear_late_pair_publish_test_hook(&fx.parent_id);
    assert_eq!(first.mode, "publish_refused");
    assert!(!first.published);
    let op = fx
        .db
        .latest_late_pair_publish_operation(&fx.parent_id)
        .unwrap()
        .expect("journal row");
    assert_eq!(op.state, LatePairPublishState::ReplayInProgress);
    assert_eq!(op.child_repo_id.as_deref(), Some(child_id.as_str()));

    let second = publish_with_timeout(&fx, &child_id, "req-replay-2")
        .await
        .expect("resume publish");
    assert!(second.published);
    assert_eq!(second.mode, "published");
    let child = fx.db.get_repository(&child_id).unwrap().unwrap();
    assert!(child.enabled);
    assert_eq!(child.last_git_sha, fx.tip);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_late_pair_publish_resumes_after_svn_copy_hook() {
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    let _guard = replay_publish_67_guard().await;
    let child_id = "child-svn-hook".to_string();
    set_late_pair_publish_test_hook(
        &fx.parent_id,
        LatePairPublishTestHook {
            fail_replay_once: false,
            abort_after_svn_copy_before_journal: true,
            ..Default::default()
        },
    );
    let first = publish_with_timeout(&fx, &child_id, "req-svn-1")
        .await
        .expect("hook path returns Ok stub");
    clear_late_pair_publish_test_hook(&fx.parent_id);
    assert_eq!(first.mode, "publish_refused");
    let op = fx
        .db
        .latest_late_pair_publish_operation(&fx.parent_id)
        .unwrap()
        .expect("journal");
    assert_eq!(op.state, LatePairPublishState::SvnCopyPending);
    assert!(fx.db.get_repository(&child_id).unwrap().is_none());

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

    let mut plan_resume = fx.plan.clone();
    plan_resume.proposed_svn_copy_source_revision = None;

    let second = publish_admitted_late_pair(
        &fx.db,
        &fx.config,
        &PublishCredentials {
            svn_password: String::new(),
        },
        &fx.parent,
        &fx.request,
        &plan_resume,
        &probe,
        &child_id,
        "fixture",
        "req-svn-2",
        &fx.identity,
    )
    .await
    .expect("resume after svn copy hook");
    assert!(second.published);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_late_pair_publish_refuses_fingerprint_mismatch() {
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    let _guard = replay_publish_67_guard().await;
    let child_id = "child-fp".to_string();
    set_late_pair_publish_test_hook(
        &fx.parent_id,
        LatePairPublishTestHook {
            fail_replay_once: false,
            abort_after_svn_copy_before_journal: true,
            ..Default::default()
        },
    );
    publish_with_timeout(&fx, &child_id, "req-fp-1")
        .await
        .expect("partial publish");
    clear_late_pair_publish_test_hook(&fx.parent_id);

    let mut bumped = fx.request.clone();
    bumped.svn_branch = "branches/other-feature".into();
    let err = publish_admitted_late_pair(
        &fx.db,
        &fx.config,
        &PublishCredentials {
            svn_password: String::new(),
        },
        &fx.parent,
        &bumped,
        &fx.plan,
        &fx.probe,
        &child_id,
        "fixture",
        "req-fp-2",
        &fx.identity,
    )
    .await
    .unwrap_err();
    assert_eq!(err.reason, "publish_fingerprint_mismatch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_late_pair_publish_resumes_after_mid_replay_failure() {
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    let _guard = replay_publish_67_guard().await;
    let child_id = "child-mid-replay".to_string();
    set_late_pair_publish_test_hook(
        &fx.parent_id,
        LatePairPublishTestHook {
            fail_replay_after_commits: 1,
            ..Default::default()
        },
    );
    let first = publish_with_timeout(&fx, &child_id, "req-mid-1")
        .await
        .unwrap_err();
    clear_late_pair_publish_test_hook(&fx.parent_id);
    assert_eq!(first.reason, "replay_failed");
    let op = fx
        .db
        .latest_late_pair_publish_operation(&fx.parent_id)
        .unwrap()
        .expect("journal");
    assert_eq!(op.state, LatePairPublishState::ReplayInProgress);

    let second = publish_with_timeout(&fx, &child_id, "req-mid-2")
        .await
        .expect("resume after mid-replay failure");
    assert!(second.published);
    let child = fx
        .db
        .list_child_repositories(&fx.parent_id)
        .unwrap()
        .into_iter()
        .find(|c| c.id == child_id)
        .expect("child row");
    let replayed_git_to_svn: i64 = fx
        .db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1 AND direction='git_to_svn'",
            [&child.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        replayed_git_to_svn, 2,
        "mid-replay resume must not double-apply Git commits"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_late_pair_publish_journaled_svn_copy_rev_wins() {
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    let _guard = replay_publish_67_guard().await;
    let child_id = "child-journal-copy-rev".to_string();
    set_late_pair_publish_test_hook(
        &fx.parent_id,
        LatePairPublishTestHook {
            fail_svn_copy_once: true,
            ..Default::default()
        },
    );
    let first = publish_with_timeout(&fx, &child_id, "req-copy-rev-1")
        .await
        .unwrap_err();
    clear_late_pair_publish_test_hook(&fx.parent_id);
    assert_eq!(first.reason, "svn_copy_failed");

    let mut op = fx
        .db
        .latest_late_pair_publish_operation(&fx.parent_id)
        .unwrap()
        .expect("journal");
    op.svn_copy_source_rev = 2;
    fx.db.update_late_pair_publish_operation(op).unwrap();

    let mut plan_alt = fx.plan.clone();
    plan_alt.proposed_svn_copy_source_revision = Some(99);

    let published = publish_admitted_late_pair(
        &fx.db,
        &fx.config,
        &PublishCredentials {
            svn_password: String::new(),
        },
        &fx.parent,
        &fx.request,
        &plan_alt,
        &fx.probe,
        &child_id,
        "fixture",
        "req-copy-rev-2",
        &fx.identity,
    )
    .await
    .expect("resume uses journaled svn copy source revision");
    assert!(published.published);

    let trunk_url = format!("{}/trunk", fx.svn_url);
    let trunk_r2 = Command::new("svn")
        .args(["cat", "-r", "2", &format!("{}/origin.txt", trunk_url)])
        .output()
        .unwrap();
    assert!(trunk_r2.status.success());
    let branch_origin = Command::new("svn")
        .args(["cat", &format!("{}/origin.txt", fx.target_url)])
        .output()
        .unwrap();
    assert!(
        branch_origin.status.success(),
        "{}",
        String::from_utf8_lossy(&branch_origin.stderr)
    );
    assert_eq!(trunk_r2.stdout, branch_origin.stdout);
    assert!(
        !String::from_utf8_lossy(&branch_origin.stdout).contains("v2"),
        "journaled copy rev 2 must not pick up trunk revision 3 content"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_late_pair_publish_resumes_after_svn_copy_error() {
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    let _guard = replay_publish_67_guard().await;
    let child_id = "child-svn-err".to_string();
    set_late_pair_publish_test_hook(
        &fx.parent_id,
        LatePairPublishTestHook {
            fail_svn_copy_once: true,
            ..Default::default()
        },
    );
    let first = publish_with_timeout(&fx, &child_id, "req-svn-err-1")
        .await
        .unwrap_err();
    clear_late_pair_publish_test_hook(&fx.parent_id);
    assert_eq!(first.reason, "svn_copy_failed");
    let op = fx
        .db
        .latest_late_pair_publish_operation(&fx.parent_id)
        .unwrap()
        .expect("journal");
    assert_eq!(op.state, LatePairPublishState::SvnCopyPending);

    assert!(
        !Command::new("svn")
            .args(["info", &fx.target_url])
            .output()
            .unwrap()
            .status
            .success(),
        "failed copy must not create the branch"
    );

    let second = publish_with_timeout(&fx, &child_id, "req-svn-err-2")
        .await
        .expect("retry after svn copy error");
    assert!(second.published);
    assert!(
        Command::new("svn")
            .args(["info", &fx.target_url])
            .output()
            .unwrap()
            .status
            .success(),
        "retry must create the branch exactly once"
    );
}

/// Team replay with `set_git_credential_apply_clean(true)` runs `inspect_fetched_history`
/// before push; this exercises the same authenticated `git ls-remote` / `git fetch` path
/// against a real HTTP smart server (not `file://`).
#[test]
fn clean_path_inspect_fetched_history_authenticates_http_git() {
    if !cfg!(target_os = "linux") {
        eprintln!(
            "skip clean_path_inspect_fetched_history_authenticates_http_git: \
             git http-backend smart HTTP fixture runs on Linux CI only"
        );
        return;
    }

    const TOKEN: &str = "reposync-http-git-integration-token";

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("port").port();
    drop(listener);

    let tmp = TempDir::new().unwrap();
    let http_root = tmp.path().join("http-root");
    std::fs::create_dir_all(&http_root).unwrap();
    let bare = http_root.join("repo.git");
    git_cmd(tmp.path(), &["init", "--bare", bare.to_str().unwrap()]);

    let seed_work = tmp.path().join("seed-work");
    std::fs::create_dir_all(&seed_work).unwrap();
    git_cmd(&seed_work, &["init", "-b", "main"]);
    git_cmd(
        &seed_work,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    std::fs::write(seed_work.join("README.md"), "seed\n").unwrap();
    git_cmd(&seed_work, &["add", "README.md"]);
    git_cmd(&seed_work, &["commit", "-m", "seed"]);
    git_cmd(&seed_work, &["push", "-u", "origin", "main"]);
    git_cmd(
        tmp.path(),
        &[
            "-C",
            bare.to_str().unwrap(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
    );
    let checkpoint = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&seed_work)
        .output()
        .unwrap();
    let checkpoint = String::from_utf8_lossy(&checkpoint.stdout)
        .trim()
        .to_string();

    let bridge = tmp.path().join("bridge");
    assert!(Command::new("git")
        .args([
            "clone",
            "-b",
            "main",
            bare.to_str().unwrap(),
            bridge.to_str().unwrap(),
        ])
        .status()
        .unwrap()
        .success());

    let remote_url = format!("http://127.0.0.1:{port}/repo.git");
    git_cmd(&bridge, &["remote", "set-url", "origin", &remote_url]);
    git_cmd(&bridge, &["checkout", "main"]);

    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/git_http_auth_server.py");
    assert!(
        script.exists(),
        "missing fixture script {}",
        script.display()
    );

    let mut server = Command::new("python3");
    server
        .arg(script)
        .env("GIT_PROJECT_ROOT", &http_root)
        .env("GIT_HTTP_PORT", port.to_string())
        .env("GIT_TEST_TOKEN", TOKEN)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = server.spawn().expect("spawn git http-auth server");
    let mut saw_listen = false;
    if let Some(mut out) = child.stdout.take() {
        use std::io::Read;
        let mut buf = [0u8; 256];
        for _ in 0..50 {
            if out.read(&mut buf).unwrap_or(0) > 0 {
                let text = String::from_utf8_lossy(&buf);
                if text.contains("listening") {
                    saw_listen = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    assert!(saw_listen, "git http-auth server did not start");

    struct ServerGuard(std::process::Child);
    impl Drop for ServerGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _guard = ServerGuard(child);

    let git_client = GitClient::new(&bridge).unwrap();
    let state = reposync_core::db::queries::CredentialChainState::resolved(TOKEN.to_string());
    reposync_core::git::apply_git_credential_chain_state(&git_client, "origin", &state).unwrap();

    let origin_url = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(&bridge)
        .output()
        .unwrap();
    let origin_url = String::from_utf8_lossy(&origin_url.stdout);
    assert!(
        !origin_url.contains(TOKEN),
        "remote URL must stay token-free: {origin_url}"
    );
    assert!(
        !origin_url.contains("x-access-token"),
        "remote URL must stay token-free: {origin_url}"
    );

    let ls_cmd = reposync_core::git::build_git_cli_command(
        &bridge,
        &["ls-remote", "--exit-code", "origin", "refs/heads/main"],
        Some(TOKEN),
    );
    assert!(
        !reposync_core::git::command_args_contain_secret(&ls_cmd, TOKEN),
        "ls-remote argv must not embed token"
    );

    let denied = reposync_core::history_inspect::inspect_fetched_history(
        &bridge,
        "main",
        Some(checkpoint.clone()),
        None,
    )
    .unwrap_err();
    assert_eq!(denied.reason, "remote_auth_failed");

    let admission = reposync_core::history_inspect::inspect_fetched_history(
        &bridge,
        "main",
        Some(checkpoint),
        Some(TOKEN),
    )
    .expect("authenticated inspect must succeed against HTTP origin");
    assert!(reposync_core::history_inspect::is_full_git_oid(
        &admission.remote_tip
    ));
}
