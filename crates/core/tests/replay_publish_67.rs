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
    // Constructor-created on-disk UUID (`svnadmin create` above), recorded before
    // seeded last_svn_rev / applied mapping. Not a later live URL observation.
    let constructor_uuid = {
        let output = Command::new("svnlook")
            .args(["uuid", svn_repo.to_str().unwrap()])
            .output()
            .expect("svnlook uuid of constructor-created repository");
        assert!(
            output.status.success(),
            "svnlook uuid failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let uuid = String::from_utf8(output.stdout)
            .expect("constructor-created SVN UUID is utf-8")
            .trim()
            .to_string();
        assert!(
            !uuid.is_empty(),
            "constructor-created SVN UUID is empty for {}",
            svn_repo.display()
        );
        uuid
    };
    reposync_core::db::lineage_checkpoints::record_independently_established_svn_uuid(
        &db,
        &parent_id,
        &constructor_uuid,
    )
    .unwrap_or_else(|error| panic!("record constructor-created SVN UUID for {parent_id}: {error}"));
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

/// `replay_pending_git` must authenticate HTTP inspect via the credential chain and
/// replay pending commits (not merely avoid `remote_auth_failed`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_pending_git_inspect_team_history_http_auth() {
    use reposync_core::late_pair_publish::replay_pending_git_for_integration_test;
    use reposync_core::models::{SyncDirection, SyncRecord, SyncRecordStatus};
    use std::io::{BufRead, BufReader};

    assert!(
        svn_available(),
        "svn and svnadmin are required; do not count a skipped test as evidence"
    );

    const TOKEN: &str = "reposync-replay-pending-git-http-token";
    const CHILD_ID: &str = "child-http-replay";

    let tmp = TempDir::new().unwrap();
    let bare = tmp.path().join("repo.git");
    git_cmd(tmp.path(), &["init", "--bare", bare.to_str().unwrap()]);
    let seed = tmp.path().join("seed");
    std::fs::create_dir(&seed).unwrap();
    git_cmd(&seed, &["init", "-b", "main"]);
    std::fs::write(seed.join("seed.txt"), "baseline\n").unwrap();
    git_cmd(&seed, &["add", "seed.txt"]);
    git_cmd(&seed, &["commit", "-m", "baseline"]);
    git_cmd(&seed, &["push", bare.to_str().unwrap(), "main"]);
    git_cmd(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let checkpoint = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&seed)
        .output()
        .unwrap();
    let checkpoint = String::from_utf8_lossy(&checkpoint.stdout)
        .trim()
        .to_string();

    std::fs::write(seed.join("delta.txt"), "tip\n").unwrap();
    git_cmd(&seed, &["add", "delta.txt"]);
    git_cmd(&seed, &["commit", "-m", "tip"]);
    git_cmd(&seed, &["push", bare.to_str().unwrap(), "main"]);
    let tip = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&seed)
        .output()
        .unwrap();
    let tip = String::from_utf8_lossy(&tip.stdout).trim().to_string();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/git_http_auth_server.py");
    let mut server = Command::new("python3");
    server
        .arg(script)
        .env("GIT_PROJECT_ROOT", tmp.path())
        .env("GIT_HTTP_PORT", port.to_string())
        .env("GIT_TEST_TOKEN", TOKEN)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());
    let mut child = server.spawn().expect("spawn git http-auth server");
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    assert!(rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .contains("listening"));
    struct ServerGuard(std::process::Child);
    impl Drop for ServerGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _guard = ServerGuard(child);

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
    git_cmd(&bridge, &["config", "credential.helper", ""]);
    git_cmd(&bridge, &["reset", "--hard", &checkpoint]);

    let svn_repo = tmp.path().join("svn");
    assert!(Command::new("svnadmin")
        .args(["create", svn_repo.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let svn_url = format!("file://{}", svn_repo.display());
    assert!(Command::new("svn")
        .args(["mkdir", &format!("{svn_url}/trunk"), "-m", "trunk"])
        .status()
        .unwrap()
        .success());
    let wc = tmp.path().join("svn-wc");
    assert!(Command::new("svn")
        .args([
            "checkout",
            &format!("{svn_url}/trunk"),
            wc.to_str().unwrap(),
        ])
        .status()
        .unwrap()
        .success());
    std::fs::write(wc.join("seed.txt"), "baseline\n").unwrap();
    assert!(Command::new("svn")
        .args(["add", "seed.txt"])
        .current_dir(&wc)
        .status()
        .unwrap()
        .success());
    let identity_file = tmp.path().join("identity.toml");
    std::fs::write(
        &identity_file,
        "[authors.dev]\nname = \"Fixture\"\nemail = \"f@example.invalid\"\n",
    )
    .unwrap();
    assert!(Command::new("svn")
        .args(["commit", "-m", "baseline svn", "--username", "dev"])
        .current_dir(&wc)
        .status()
        .unwrap()
        .success());

    let db_path = tmp.path().join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let now = Utc::now();
    // Constructor-created on-disk UUID (`svnadmin create` above), recorded before
    // seeded last_svn_rev / applied mapping. Not a later live URL observation.
    let constructor_uuid = {
        let output = Command::new("svnlook")
            .args(["uuid", svn_repo.to_str().unwrap()])
            .output()
            .expect("svnlook uuid of constructor-created repository");
        assert!(
            output.status.success(),
            "svnlook uuid failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let uuid = String::from_utf8(output.stdout)
            .expect("constructor-created SVN UUID is utf-8")
            .trim()
            .to_string();
        assert!(
            !uuid.is_empty(),
            "constructor-created SVN UUID is empty for {}",
            svn_repo.display()
        );
        uuid
    };
    reposync_core::db::lineage_checkpoints::record_independently_established_svn_uuid(
        &db,
        CHILD_ID,
        &constructor_uuid,
    )
    .unwrap_or_else(|error| panic!("record constructor-created SVN UUID for {CHILD_ID}: {error}"));
    db.insert_repository(&Repository {
        id: CHILD_ID.into(),
        name: "Child".into(),
        svn_url: svn_url.clone(),
        svn_branch: "trunk".into(),
        svn_username: String::new(),
        git_provider: "local".into(),
        git_api_url: format!("http://127.0.0.1:{port}"),
        git_repo: "local/repo".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 5,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: Some("parent".into()),
        created_at: now.to_rfc3339(),
        updated_at: now.to_rfc3339(),
        last_svn_rev: 2,
        last_git_sha: checkpoint.clone(),
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
        id: "import-map".into(),
        repo_id: Some(CHILD_ID.into()),
        svn_revision: Some(2),
        git_hash: Some(checkpoint.clone()),
        direction: SyncDirection::SvnToGit,
        author: "dev".into(),
        message: "import".into(),
        timestamp: now,
        synced_at: now,
        status: SyncRecordStatus::Applied,
    })
    .unwrap();
    db.set_state(&format!("last_git_sha_{CHILD_ID}"), &checkpoint)
        .unwrap();
    let baseline = serde_json::json!({
        "version": 1,
        "repo_id": CHILD_ID,
        "git_sha": checkpoint,
        "svn_rev": 2,
        "projection": "{}",
    });
    db.set_state(
        &format!("handled_git_baseline_{CHILD_ID}"),
        &baseline.to_string(),
    )
    .unwrap();

    let config = app_config(tmp.path(), &svn_url);
    let identity_cfg = IdentityConfig {
        mapping_file: Some(identity_file),
        email_domain: None,
        ldap_url: None,
        ldap_base_dn: None,
        ldap_bind_dn: None,
        ldap_bind_password_env: None,
        ldap_bind_password: None,
    };
    let identity = Arc::new(IdentityMapper::new(&identity_cfg).unwrap());
    let child = db.get_repository(CHILD_ID).unwrap().unwrap();

    let denied = replay_pending_git_for_integration_test(
        &config,
        &db,
        &child,
        &bridge,
        "",
        &identity,
        &tip,
        &checkpoint,
    )
    .await;
    assert!(
        denied.is_err(),
        "replay without git token must fail: {denied:?}"
    );
    let denied = denied.unwrap_err();
    assert!(
        denied.detail.contains("remote_auth_failed"),
        "expected remote_auth_failed, got: {denied:?}"
    );

    db.set_state(&format!("secret_git_token_{CHILD_ID}"), TOKEN)
        .unwrap();
    let replayed = replay_pending_git_for_integration_test(
        &config,
        &db,
        &child,
        &bridge,
        "",
        &identity,
        &tip,
        &checkpoint,
    )
    .await
    .expect("authenticated replay_pending_git must succeed");
    assert!(
        !replayed.is_empty(),
        "replay must apply at least one pending Git commit"
    );
    let after = db.get_repository(CHILD_ID).unwrap().unwrap();
    assert_eq!(after.last_git_sha, tip);
}

fn svnlook_uuid(repo_dir: &Path) -> String {
    let output = Command::new("svnlook")
        .args(["uuid", repo_dir.to_str().unwrap()])
        .output()
        .expect("svnlook uuid");
    assert!(
        output.status.success(),
        "svnlook uuid failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let uuid = String::from_utf8(output.stdout)
        .expect("SVN UUID is utf-8")
        .trim()
        .to_string();
    assert!(!uuid.is_empty());
    uuid
}

fn overwrite_established_uuid(db: &Database, repo_id: &str, uuid: Option<&str>) {
    let key =
        reposync_core::db::lineage_checkpoints::independently_established_svn_uuid_kv_key(repo_id);
    db.conn()
        .execute(
            "DELETE FROM kv_state WHERE key = ?1",
            rusqlite::params![key],
        )
        .unwrap();
    if let Some(uuid) = uuid {
        db.conn()
            .execute(
                "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)",
                rusqlite::params![key, uuid, Utc::now().to_rfc3339()],
            )
            .unwrap();
    }
}

fn rs67_create_independent_target(fx: &PublishFixture, file: &str, bytes: &[u8]) {
    let command = |args: &[&str], cwd: &Path| {
        let result = Command::new("svn")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    command(
        &[
            "mkdir",
            &format!("{}/branches", fx.svn_url),
            "-m",
            "branches",
            "--non-interactive",
        ],
        fx.tmp.path(),
    );
    command(
        &[
            "copy",
            "-r",
            "2",
            &format!("{}/trunk@2", fx.svn_url),
            &fx.target_url,
            "-m",
            "copy proven B",
            "--non-interactive",
        ],
        fx.tmp.path(),
    );
    let wc = fx.tmp.path().join("existing-target");
    command(
        &[
            "checkout",
            &fx.target_url,
            wc.to_str().unwrap(),
            "--non-interactive",
        ],
        fx.tmp.path(),
    );
    std::fs::write(wc.join(file), bytes).unwrap();
    command(&["add", file], &wc);
    command(
        &[
            "commit",
            "-m",
            "independent SVN target work",
            "--non-interactive",
        ],
        &wc,
    );
}

async fn rs67_prepare(
    fx: &PublishFixture,
    include_parent: bool,
) -> anyhow::Result<reposync_core::late_pair::reconciliation::PreparedPlan> {
    reposync_core::late_pair::reconciliation::prepare(
        reposync_core::late_pair::reconciliation::PrepareContext {
            db: &fx.db,
            parent: &fx.parent,
            git_workdir: &fx.tmp.path().join("repos/parent/git-repo"),
            request: &fx.request,
            admission: &fx.plan,
            svn_password: "",
            actor: "fixture-admin",
            include_parent,
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_existing_target_plan_pins_both_sides_without_publishing() {
    let _guard = replay_publish_67_guard().await;
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    rs67_create_independent_target(&fx, "svn-only.txt", b"independent SVN\n");
    let refs = Command::new("git")
        .args(["--git-dir", fx._bare.to_str().unwrap(), "show-ref"])
        .output()
        .unwrap()
        .stdout;
    let before = SvnClient::new(&fx.target_url, "", "")
        .info()
        .await
        .unwrap()
        .latest_rev;
    let plan = rs67_prepare(&fx, false).await.unwrap();
    assert!(plan.conflicts.is_empty());
    assert_eq!(plan.pending_git_shas.len(), 2);
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(
        plan.steps[0].intended_tree["feature.txt"].bytes,
        b"step one\n"
    );
    assert_eq!(
        plan.steps[1].intended_tree["feature.txt"].bytes,
        b"step two\n"
    );
    assert_eq!(
        plan.intended_tree["svn-only.txt"].bytes,
        b"independent SVN\n"
    );
    assert_eq!(
        plan.intended_tree["origin.txt"].bytes, b"SVN origin\n",
        "unselected parent advancement must stay outside the intended pair"
    );
    assert_eq!(
        rs67_prepare(&fx, false).await.unwrap().review_pin,
        plan.review_pin,
        "fresh repeated capture is deterministic"
    );
    let with_parent = rs67_prepare(&fx, true).await.unwrap();
    assert!(with_parent.conflicts.is_empty());
    assert_eq!(with_parent.steps.len(), 3);
    assert_eq!(
        with_parent.intended_tree["origin.txt"].bytes,
        b"SVN origin v2\n"
    );
    assert_eq!(
        with_parent.intended_tree["svn-only.txt"].bytes,
        b"independent SVN\n"
    );
    assert_ne!(with_parent.review_pin, plan.review_pin);
    let bridge = fx.tmp.path().join("repos/parent/git-repo");
    let repository = git2::Repository::open(&bridge).unwrap();
    let intended = git2::Oid::from_str(plan.intended_git_sha.as_ref().unwrap()).unwrap();
    assert!(repository
        .graph_descendant_of(intended, git2::Oid::from_str(&fx.tip).unwrap())
        .unwrap());
    assert_eq!(plan.intended_git_parents[0], fx.tip);
    assert_eq!(
        plan.intended_git_parents[1],
        plan.steps.last().unwrap().git_result.sha
    );
    for step in &plan.steps {
        assert_eq!(
            reposync_core::late_pair::reconciliation::capture_git_tree(
                &bridge,
                &step.git_result.sha
            )
            .unwrap(),
            step.intended_tree
        );
    }
    assert_eq!(
        Command::new("git")
            .args(["--git-dir", fx._bare.to_str().unwrap(), "show-ref"])
            .output()
            .unwrap()
            .stdout,
        refs
    );
    assert_eq!(
        SvnClient::new(&fx.target_url, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        before
    );
    assert!(fx
        .db
        .list_child_repositories(&fx.parent_id)
        .unwrap()
        .is_empty());
    assert!(fx
        .db
        .latest_late_pair_publish_operation(&fx.parent_id)
        .unwrap()
        .is_none());
    let mut tampered = plan.clone();
    tampered
        .intended_tree
        .get_mut("svn-only.txt")
        .unwrap()
        .bytes
        .push(b'!');
    assert!(tampered.validate().is_err());
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"RS67_PINNED_EXISTING_TARGET_PLAN","git_pending":2,"svn_independent_work_retained":true,"parent_selection_explicit":true,"remote_refs_and_svn_unchanged":true,"review_pin":plan.review_pin})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_existing_target_overlap_plan_is_blocked_without_publishable_result() {
    let _guard = replay_publish_67_guard().await;
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    rs67_create_independent_target(&fx, "feature.txt", b"independent conflicting SVN\n");
    let plan = rs67_prepare(&fx, false).await.unwrap();
    assert_eq!(plan.conflicts.len(), 1);
    assert_eq!(plan.conflicts[0].path, "feature.txt");
    assert!(plan.intended_git_sha.is_none());
    assert!(plan.steps.is_empty());
    assert!(fx
        .db
        .list_child_repositories(&fx.parent_id)
        .unwrap()
        .is_empty());
    assert!(fx
        .db
        .latest_late_pair_publish_operation(&fx.parent_id)
        .unwrap()
        .is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_new_target_plan_reuses_original_git_objects_without_duplicate_history() {
    let _guard = replay_publish_67_guard().await;
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    let plan = rs67_prepare(&fx, false).await.unwrap();
    assert!(plan.target_revision.is_none());
    assert_eq!(plan.initial_target_git.sha, fx._base_sha);
    assert_eq!(plan.intended_git_sha.as_deref(), Some(fx.tip.as_str()));
    assert!(plan.conflicts.is_empty());
    assert_eq!(plan.steps.len(), 2);
    for step in &plan.steps {
        let reposync_core::late_pair::reconciliation::StepSource::Git { sha, .. } = &step.source
        else {
            panic!("unexpected parent replay")
        };
        assert_eq!(
            sha, &step.git_result.sha,
            "new target must reuse original A1/A2 objects"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_reviewed_claim_is_exclusive_actor_bound_and_cancellable_before_effects() {
    let _guard = replay_publish_67_guard().await;
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    let plan = rs67_prepare(&fx, false).await.unwrap();
    let bridge = fx.tmp.path().join("repos/parent/git-repo");
    let fixture = &fx;
    let bridge_path = bridge.as_path();
    let begin = |pin: String| async move {
        reposync_core::late_pair_publish::begin_reviewed_reconciliation(
            reposync_core::late_pair::reconciliation::PrepareContext {
                db: &fixture.db,
                parent: &fixture.parent,
                git_workdir: bridge_path,
                request: &fixture.request,
                admission: &fixture.plan,
                svn_password: "",
                actor: "fixture-admin",
                include_parent: false,
            },
            &pin,
            "reviewed-request",
        )
        .await
    };
    assert!(begin("unreviewed".into()).await.is_err());
    assert!(fx
        .db
        .latest_late_pair_publish_operation(&fx.parent_id)
        .unwrap()
        .is_none());
    let op = begin(plan.review_pin.clone()).await.unwrap();
    assert_eq!(op.version, 2);
    assert_eq!(op.state, LatePairPublishState::Queued);
    assert!(!op.state.is_terminal());
    assert!(
        begin(plan.review_pin.clone()).await.is_err(),
        "exclusive parent claim"
    );
    assert!(
        reposync_core::late_pair_publish::cancel_reviewed_reconciliation(
            &fx.db,
            &fx.parent_id,
            &op.id,
            "foreign-actor"
        )
        .is_err()
    );
    assert_eq!(
        fx.db
            .get_late_pair_publish_operation(&fx.parent_id, &op.id)
            .unwrap()
            .unwrap(),
        op
    );
    let cancelled = reposync_core::late_pair_publish::cancel_reviewed_reconciliation(
        &fx.db,
        &fx.parent_id,
        &op.id,
        "fixture-admin",
    )
    .unwrap();
    assert_eq!(cancelled.state, LatePairPublishState::Cancelled);
    assert_eq!(
        reposync_core::late_pair_publish::cancel_reviewed_reconciliation(
            &fx.db,
            &fx.parent_id,
            &op.id,
            "fixture-admin"
        )
        .unwrap(),
        cancelled
    );
    assert!(fx
        .db
        .list_child_repositories(&fx.parent_id)
        .unwrap()
        .is_empty());
    assert!(SvnClient::new(&fx.target_url, "", "").info().await.is_err());
    let resumed = begin(plan.review_pin).await.unwrap();
    assert_ne!(
        resumed.id, op.id,
        "cancelled pre-effect claim releases its reservation without deleting history"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_reviewed_existing_target_publishes_both_histories_and_repeats_quietly() {
    rs67_native_continuation_case(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_reviewed_filtered_pair_continues_with_later_git_and_svn_work() {
    rs67_native_continuation_case(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_reviewed_explicit_parent_advancement_is_published_with_both_sides() {
    rs67_native_continuation_case(false, true).await;
}

async fn rs67_native_continuation_case(filtered: bool, include_parent: bool) {
    let _guard = replay_publish_67_guard().await;
    assert!(svn_available());
    let mut fx = build_publish_fixture().await;
    fx.identity = Arc::new(
        IdentityMapper::new(&IdentityConfig {
            email_domain: Some("fixture.invalid".into()),
            ..IdentityConfig::default()
        })
        .unwrap(),
    );
    let allowed = if filtered {
        vec![
            "feature.txt".to_owned(),
            "svn-only.txt".into(),
            "first-svn.txt".into(),
            "later-git.txt".into(),
            "later-svn.txt".into(),
        ]
    } else {
        Vec::new()
    };
    if filtered {
        let encoded = serde_json::to_string(&allowed).unwrap();
        fx.db
            .conn()
            .execute(
                "UPDATE repositories SET allowed_paths=?1 WHERE id=?2",
                rusqlite::params![encoded, fx.parent_id],
            )
            .unwrap();
        fx.parent = fx.db.get_repository(&fx.parent_id).unwrap().unwrap();
    }
    rs67_create_independent_target(&fx, "svn-only.txt", b"independent SVN\n");
    let plan = rs67_prepare(&fx, include_parent).await.unwrap();
    let bridge = fx.tmp.path().join("repos/parent/git-repo");
    let operation = reposync_core::late_pair_publish::begin_reviewed_reconciliation(
        reposync_core::late_pair::reconciliation::PrepareContext {
            db: &fx.db,
            parent: &fx.parent,
            git_workdir: &bridge,
            request: &fx.request,
            admission: &fx.plan,
            svn_password: "",
            actor: "fixture-admin",
            include_parent,
        },
        &plan.review_pin,
        "existing-target-native",
    )
    .await
    .unwrap();
    let completed = reposync_core::late_pair_publish::reviewed::run(
        &fx.db,
        &fx.config,
        &fx.parent,
        "",
        &bridge,
        &operation.id,
        "fixture-admin",
    )
    .await
    .unwrap();
    assert_eq!(completed.state, LatePairPublishState::Completed);
    let progress = completed.reconciliation.as_ref().unwrap();
    assert_eq!(progress.receipts.len(), if include_parent { 3 } else { 2 });
    assert!(progress.receipts.iter().all(|r| r.svn_operation.is_some()));
    assert!(progress.publication.is_some());
    let child = fx
        .db
        .get_repository(completed.child_repo_id.as_deref().unwrap())
        .unwrap()
        .unwrap();
    let observed_uuid = svnlook_uuid(&fx.tmp.path().join("svn"));
    let child_uuid = reposync_core::db::lineage_checkpoints::independently_established_svn_uuid(
        &fx.db.conn(),
        &child.id,
    )
    .unwrap()
    .expect("reviewed producer recorded child UUID before SyncEngine");
    assert_eq!(child_uuid, observed_uuid);
    assert_eq!(child_uuid, progress.plan.svn_uuid);
    if let Some(copy) = progress.copy_evidence.as_ref() {
        assert_eq!(copy.uuid, child_uuid);
        assert_eq!(copy.target_path, progress.plan.target_path);
    } else {
        assert!(progress.plan.target_revision.is_some());
    }
    assert!(child.enabled);
    assert_eq!(Some(child.last_svn_rev), progress.svn_revision);
    assert_eq!(
        Some(child.last_git_sha.as_str()),
        plan.intended_git_sha.as_deref()
    );
    let svn = SvnClient::new(&fx.target_url, "", "");
    assert_eq!(
        reposync_core::late_pair::reconciliation::capture_svn_tree(&svn, child.last_svn_rev)
            .await
            .unwrap(),
        plan.intended_tree
    );
    for (index, receipt) in progress.receipts.iter().enumerate() {
        assert_eq!(
            reposync_core::late_pair::reconciliation::capture_svn_tree(&svn, receipt.svn_revision)
                .await
                .unwrap(),
            plan.steps[index].intended_tree
        );
    }
    let result = Command::new("git")
        .args([
            "--git-dir",
            fx._bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/feature",
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap().trim(),
        child.last_git_sha
    );
    let repository = git2::Repository::open_bare(&fx._bare).unwrap();
    assert!(repository
        .graph_descendant_of(
            git2::Oid::from_str(&child.last_git_sha).unwrap(),
            git2::Oid::from_str(&fx.tip).unwrap()
        )
        .unwrap());
    let revision = svn.info().await.unwrap().latest_rev;
    let count: i64 = fx
        .db
        .conn()
        .query_row(
            "SELECT count(*) FROM sync_records WHERE repo_id=?1",
            [&child.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        reposync_core::late_pair_publish::reviewed::run(
            &fx.db,
            &fx.config,
            &fx.parent,
            "",
            &bridge,
            &operation.id,
            "fixture-admin"
        )
        .await
        .unwrap(),
        completed
    );
    assert_eq!(svn.info().await.unwrap().latest_rev, revision);
    assert_eq!(
        fx.db
            .conn()
            .query_row(
                "SELECT count(*) FROM sync_records WHERE repo_id=?1",
                [&child.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        count
    );
    let child_git = fx.tmp.path().join("repos").join(&child.id).join("git-repo");
    drop(fx.db);
    let reopened = Database::new(fx.tmp.path().join("reposync.db")).unwrap();
    let mut configuration = fx.config.clone();
    configuration.github.default_branch = "feature".into();
    let mut engine = SyncEngine::new(
        configuration,
        reopened,
        svn,
        GitClient::new(&child_git).unwrap(),
        fx.identity.clone(),
    );
    engine.set_repo_id(child.id.clone());
    engine.set_path_rules(allowed.clone(), Vec::new());
    let quiet = engine.run_sync_cycle().await.unwrap();
    assert_eq!(quiet.git_to_svn_count, 0);
    assert_eq!(quiet.svn_to_git_count, 0);
    assert_eq!(quiet.conflicts_detected, 0);
    let target = SvnClient::new(&fx.target_url, "", "");
    assert_eq!(target.info().await.unwrap().latest_rev, revision);
    assert_eq!(
        engine
            .db()
            .conn()
            .query_row(
                "SELECT count(*) FROM sync_records WHERE repo_id=?1",
                [&child.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        count
    );
    let first_wc = fx.tmp.path().join("svn-first-after-pair");
    let writer = SvnClient::new(&fx.target_url, "fixture-svn", "");
    writer.checkout_head(&first_wc).await.unwrap();
    std::fs::write(
        first_wc.join("first-svn.txt"),
        b"SVN advances before any later Git work\n",
    )
    .unwrap();
    writer.add(&first_wc, &["first-svn.txt"]).await.unwrap();
    let first_revision = writer
        .commit(&first_wc, "SVN-first continuation", "fixture-svn")
        .await
        .unwrap();
    assert_eq!(first_revision, revision + 1);
    let first_cycle = engine.run_sync_cycle().await.unwrap();
    assert_eq!(first_cycle.svn_to_git_count, 1);
    assert_eq!(first_cycle.git_to_svn_count, 0);
    let first_quiet = engine.run_sync_cycle().await.unwrap();
    assert_eq!(first_quiet.svn_to_git_count, 0);
    assert_eq!(first_quiet.git_to_svn_count, 0);
    let work = fx.tmp.path().join("git-work");
    git_cmd(&work, &["pull", "--ff-only", "origin", "feature"]);
    std::fs::write(
        work.join("later-git.txt"),
        b"ordinary Git after paired completion\n",
    )
    .unwrap();
    git_cmd(&work, &["add", "later-git.txt"]);
    git_cmd(&work, &["commit", "-m", "later ordinary Git"]);
    git_cmd(&work, &["push", "origin", "feature"]);
    let git_cycle = engine.run_sync_cycle().await.unwrap();
    assert_eq!(git_cycle.git_to_svn_count, 1);
    assert_eq!(git_cycle.svn_to_git_count, 0);
    let after_git = target.info().await.unwrap().latest_rev;
    assert_eq!(after_git, first_revision + 1);
    assert_eq!(
        reposync_core::late_pair::reconciliation::capture_svn_tree(&target, after_git)
            .await
            .unwrap()["later-git.txt"]
            .bytes,
        b"ordinary Git after paired completion\n"
    );
    let wc = fx.tmp.path().join("later-svn-work");
    target.checkout_head(&wc).await.unwrap();
    std::fs::write(
        wc.join("later-svn.txt"),
        b"ordinary SVN after paired completion\n",
    )
    .unwrap();
    target.add(&wc, &["later-svn.txt"]).await.unwrap();
    let writer = SvnClient::new(&fx.target_url, "fixture-svn", "");
    let after_svn = writer
        .commit(&wc, "later ordinary SVN", "fixture-svn")
        .await
        .unwrap();
    assert_eq!(after_svn, after_git + 1);
    let svn_cycle = engine.run_sync_cycle().await.unwrap();
    assert_eq!(svn_cycle.svn_to_git_count, 1);
    assert_eq!(svn_cycle.git_to_svn_count, 0);
    assert_eq!(
        Command::new("git")
            .args([
                "--git-dir",
                fx._bare.to_str().unwrap(),
                "show",
                "feature:later-svn.txt"
            ])
            .output()
            .unwrap()
            .stdout,
        b"ordinary SVN after paired completion\n"
    );
    let final_count: i64 = engine
        .db()
        .conn()
        .query_row(
            "SELECT count(*) FROM sync_records WHERE repo_id=?1",
            [&child.id],
            |r| r.get(0),
        )
        .unwrap();
    drop(engine);
    let reopened = Database::new(fx.tmp.path().join("reposync.db")).unwrap();
    let mut configuration = fx.config.clone();
    configuration.github.default_branch = "feature".into();
    let mut engine = SyncEngine::new(
        configuration,
        reopened,
        SvnClient::new(&fx.target_url, "", ""),
        GitClient::new(&child_git).unwrap(),
        fx.identity.clone(),
    );
    engine.set_repo_id(child.id.clone());
    engine.set_path_rules(allowed, Vec::new());
    let quiet = engine.run_sync_cycle().await.unwrap();
    assert_eq!(quiet.git_to_svn_count, 0);
    assert_eq!(quiet.svn_to_git_count, 0);
    assert_eq!(quiet.conflicts_detected, 0);
    assert_eq!(target.info().await.unwrap().latest_rev, after_svn);
    assert_eq!(
        engine
            .db()
            .conn()
            .query_row(
                "SELECT count(*) FROM sync_records WHERE repo_id=?1",
                [&child.id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        final_count
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_reviewed_restart_finishes_consumed_copy_replay_and_publication_once() {
    use reposync_core::late_pair_publish::reviewed::{self, Fault};
    let _guard = replay_publish_67_guard().await;
    assert!(svn_available());
    for point in [
        Fault::AfterCopy,
        Fault::AfterSvnCommit(1),
        Fault::AfterGitPublication,
    ] {
        let fx = build_publish_fixture().await;
        let plan = rs67_prepare(&fx, false).await.unwrap();
        let bridge = fx.tmp.path().join("repos/parent/git-repo");
        let op = reposync_core::late_pair_publish::begin_reviewed_reconciliation(
            reposync_core::late_pair::reconciliation::PrepareContext {
                db: &fx.db,
                parent: &fx.parent,
                git_workdir: &bridge,
                request: &fx.request,
                admission: &fx.plan,
                svn_password: "",
                actor: "fixture-admin",
                include_parent: false,
            },
            &plan.review_pin,
            "controlled-restart",
        )
        .await
        .unwrap();
        reviewed::set_fault(&fx.parent_id, point.clone());
        let error = reviewed::run(
            &fx.db,
            &fx.config,
            &fx.parent,
            "",
            &bridge,
            &op.id,
            "fixture-admin",
        )
        .await
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("controlled late-pair interruption"),
            "{point:?}: {error:#}"
        );
        let held = fx
            .db
            .get_late_pair_publish_operation(&fx.parent_id, &op.id)
            .unwrap()
            .unwrap();
        assert_eq!(held.state, LatePairPublishState::ReconciliationHeld);
        assert!(
            !held.state.is_terminal(),
            "uncertain work must retain scheduler isolation"
        );
        if let Some(child) = held.child_repo_id.as_deref() {
            assert!(!fx.db.get_repository(child).unwrap().unwrap().enabled);
        }
        let cancelled = reposync_core::late_pair_publish::cancel_reviewed_reconciliation(
            &fx.db,
            &fx.parent_id,
            &op.id,
            "fixture-admin",
        )
        .unwrap();
        assert_eq!(cancelled.state, LatePairPublishState::ReconciliationHeld);
        assert!(cancelled.reconciliation.as_ref().unwrap().cancel_requested);
        assert!(reviewed::run(
            &fx.db,
            &fx.config,
            &fx.parent,
            "",
            &bridge,
            &op.id,
            "fixture-admin"
        )
        .await
        .is_err());
        assert!(
            reposync_core::late_pair_publish::continue_reviewed_reconciliation(
                &fx.db,
                &fx.parent_id,
                &op.id,
                "foreign",
                &plan.review_pin
            )
            .is_err()
        );
        reposync_core::late_pair_publish::continue_reviewed_reconciliation(
            &fx.db,
            &fx.parent_id,
            &op.id,
            "fixture-admin",
            &plan.review_pin,
        )
        .unwrap();
        // A separate DB handle consumes only durable producer evidence.
        drop(fx.db);
        let reopened = Database::new(&fx.tmp.path().join("reposync.db")).unwrap();
        let done = reviewed::run(
            &reopened,
            &fx.config,
            &fx.parent,
            "",
            &bridge,
            &op.id,
            "fixture-admin",
        )
        .await
        .unwrap();
        assert_eq!(done.state, LatePairPublishState::Completed, "{point:?}");
        let progress = done.reconciliation.as_ref().unwrap();
        assert_eq!(progress.receipts.len(), 2);
        assert_eq!(
            progress.controls.len(),
            2,
            "cancel and explicit continuation are retained"
        );
        assert!(progress.receipts[0].svn_revision < progress.receipts[1].svn_revision);
        let svn = SvnClient::new(&fx.target_url, "", "");
        let log = svn
            .copy_history_at_rev(progress.svn_revision.unwrap())
            .await
            .unwrap();
        assert_eq!(
            log.len(),
            3,
            "one copy and exactly two replay commits: {point:?}"
        );
        assert_eq!(
            reposync_core::late_pair::reconciliation::capture_svn_tree(
                &svn,
                progress.svn_revision.unwrap()
            )
            .await
            .unwrap(),
            plan.intended_tree
        );
        assert_eq!(
            reviewed::run(
                &reopened,
                &fx.config,
                &fx.parent,
                "",
                &bridge,
                &op.id,
                "fixture-admin"
            )
            .await
            .unwrap(),
            done
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_contract_completed_v2_requires_exact_retained_producers_and_unique_mappings() {
    use reposync_core::db::late_pair_publish_operations::{
        completed_for_foreign_journal, verified_reconciliation_frontier,
    };
    let _guard = replay_publish_67_guard().await;
    let fx = build_publish_fixture().await;
    let plan = rs67_prepare(&fx, false).await.unwrap();
    let bridge = fx.tmp.path().join("repos/parent/git-repo");
    let op = reposync_core::late_pair_publish::begin_reviewed_reconciliation(
        reposync_core::late_pair::reconciliation::PrepareContext {
            db: &fx.db,
            parent: &fx.parent,
            git_workdir: &bridge,
            request: &fx.request,
            admission: &fx.plan,
            svn_password: "",
            actor: "fixture-admin",
            include_parent: false,
        },
        &plan.review_pin,
        "native-frontier-contract",
    )
    .await
    .unwrap();
    let completed = reposync_core::late_pair_publish::reviewed::run(
        &fx.db,
        &fx.config,
        &fx.parent,
        "",
        &bridge,
        &op.id,
        "fixture-admin",
    )
    .await
    .unwrap();
    let raw = serde_json::to_string(&completed).unwrap();
    assert!(completed_for_foreign_journal(&raw, &op.id, &fx.parent_id).unwrap());
    assert!(!completed_for_foreign_journal(&raw, "wrong-id", &fx.parent_id).unwrap());
    assert!(!completed_for_foreign_journal(&raw, &op.id, "foreign-parent").unwrap());
    for field in [
        "version",
        "policy_version",
        "child_repo_id",
        "reconciliation",
    ] {
        let mut malformed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        malformed[field] = serde_json::Value::Null;
        assert!(
            completed_for_foreign_journal(&malformed.to_string(), &op.id, &fx.parent_id).is_err(),
            "{field}"
        );
    }
    let mut partial = completed.clone();
    partial
        .reconciliation
        .as_deref_mut()
        .unwrap()
        .receipts
        .pop();
    assert!(completed_for_foreign_journal(
        &serde_json::to_string(&partial).unwrap(),
        &op.id,
        &fx.parent_id
    )
    .is_err());
    let mut blended = completed.clone();
    blended.version = 1;
    assert!(completed_for_foreign_journal(
        &serde_json::to_string(&blended).unwrap(),
        &op.id,
        &fx.parent_id
    )
    .is_err());
    let child = completed.child_repo_id.as_deref().unwrap();
    let sha = plan.intended_git_sha.as_deref().unwrap();
    assert!(verified_reconciliation_frontier(&fx.db.conn(), child, sha, &plan.projection).unwrap());
    assert!(
        verified_reconciliation_frontier(&fx.db.conn(), child, sha, "changed projection").is_err()
    );
    assert!(!verified_reconciliation_frontier(
        &fx.db.conn(),
        child,
        &"e".repeat(40),
        &plan.projection
    )
    .unwrap());
    let journal_key = format!("late_pair_publish_v1:document:{}", completed.id);
    let original_journal = fx.db.get_state(&journal_key).unwrap().unwrap();
    let mut altered = completed.clone();
    altered.outcome_detail = Some("changed immutable completion".into());
    fx.db
        .set_state(&journal_key, &serde_json::to_string(&altered).unwrap())
        .unwrap();
    assert!(verified_reconciliation_frontier(&fx.db.conn(), child, sha, &plan.projection).is_err());
    fx.db.set_state(&journal_key, &original_journal).unwrap();
    let scope = reposync_core::db::repo_scope_identity::repository_scope_uuid(&fx.db.conn(), child)
        .unwrap();
    let generation_key = format!("repo_echo_generation_{scope}");
    let previous_generation = fx.db.get_state(&generation_key).unwrap();
    fx.db.set_state(&generation_key, "99").unwrap();
    assert!(verified_reconciliation_frontier(&fx.db.conn(), child, sha, &plan.projection).is_err());
    if let Some(value) = previous_generation {
        fx.db.set_state(&generation_key, &value).unwrap();
    } else {
        fx.db.delete_kv_state(&generation_key).unwrap();
    }
    fx.db
        .conn()
        .execute(
            "UPDATE repositories SET scope_uuid=?1 WHERE id=?2",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), child],
        )
        .unwrap();
    assert!(verified_reconciliation_frontier(&fx.db.conn(), child, sha, &plan.projection).is_err());
    fx.db
        .conn()
        .execute(
            "UPDATE repositories SET scope_uuid=?1 WHERE id=?2",
            rusqlite::params![scope, child],
        )
        .unwrap();
    let producer = completed.reconciliation.as_ref().unwrap().receipts[0]
        .svn_operation
        .as_ref()
        .unwrap();
    let key = format!("git_to_svn_commit_v1:document:{}", producer.id);
    let original = fx.db.get_state(&key).unwrap().unwrap();
    fx.db.set_state(&key, "{}").unwrap();
    assert!(verified_reconciliation_frontier(&fx.db.conn(), child, sha, &plan.projection).is_err());
    fx.db.set_state(&key, &original).unwrap();
    let mapping_id:String=fx.db.conn().query_row("SELECT id FROM sync_records WHERE repo_id=?1 AND git_sha=?2 AND direction='git_to_svn'",rusqlite::params![child,producer.source_git_sha],|r|r.get(0)).unwrap();
    fx.db.conn().execute("INSERT INTO sync_records (id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) SELECT 'duplicate-proof',repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status FROM sync_records WHERE id=?1",[&mapping_id]).unwrap();
    assert!(
        verified_reconciliation_frontier(&fx.db.conn(), child, sha, &plan.projection).is_err(),
        "duplicates must never be silently selected"
    );
    fx.db
        .conn()
        .execute("DELETE FROM sync_records WHERE id='duplicate-proof'", [])
        .unwrap();
    assert!(verified_reconciliation_frontier(&fx.db.conn(), child, sha, &plan.projection).unwrap());
    fx.db
        .conn()
        .execute("DELETE FROM sync_records WHERE id=?1", [mapping_id])
        .unwrap();
    assert!(
        verified_reconciliation_frontier(&fx.db.conn(), child, sha, &plan.projection).is_err(),
        "loss of retained proof is not old-import authority"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_contract_foreign_endpoint_and_stale_registered_source_refuse_before_journal() {
    let _guard = replay_publish_67_guard().await;
    let fx = build_publish_fixture().await;
    let bridge = fx.tmp.path().join("repos/parent/git-repo");
    let original = git2::Repository::open(&bridge)
        .unwrap()
        .find_remote("origin")
        .unwrap()
        .url()
        .unwrap()
        .to_owned();
    let other = fx.tmp.path().join("foreign.git");
    git2::Repository::init_bare(&other).unwrap();
    git_cmd(
        &bridge,
        &["remote", "set-url", "origin", other.to_str().unwrap()],
    );
    assert!(format!("{:#}", rs67_prepare(&fx, false).await.unwrap_err()).contains("origin differs"));
    assert!(fx
        .db
        .latest_late_pair_publish_operation(&fx.parent_id)
        .unwrap()
        .is_none());
    git_cmd(&bridge, &["remote", "set-url", "origin", &original]);
    let plan = rs67_prepare(&fx, false).await.unwrap();
    fx.db
        .conn()
        .execute(
            "UPDATE repositories SET updated_at='changed-after-review' WHERE id=?1",
            [&fx.parent_id],
        )
        .unwrap();
    let error = reposync_core::late_pair_publish::begin_reviewed_reconciliation(
        reposync_core::late_pair::reconciliation::PrepareContext {
            db: &fx.db,
            parent: &fx.parent,
            git_workdir: &bridge,
            request: &fx.request,
            admission: &fx.plan,
            svn_password: "",
            actor: "fixture-admin",
            include_parent: false,
        },
        &plan.review_pin,
        "stale-registration",
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("current registered source"));
    assert!(fx
        .db
        .latest_late_pair_publish_operation(&fx.parent_id)
        .unwrap()
        .is_none());
    assert!(fx
        .db
        .list_child_repositories(&fx.parent_id)
        .unwrap()
        .is_empty());
    assert!(SvnClient::new(&fx.target_url, "", "").info().await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_contract_valid_covered_frontier_cannot_authorize_unproved_unrelated_or_missing_column(
) {
    let _guard = replay_publish_67_guard().await;
    for case in [
        "unproved_descendant",
        "unrelated_emitted_column",
        "missing_object",
    ] {
        let mut fx = build_publish_fixture().await;
        let allowed = vec!["feature.txt".to_string(), "svn-only.txt".into()];
        fx.db
            .conn()
            .execute(
                "UPDATE repositories SET allowed_paths=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&allowed).unwrap(), fx.parent_id],
            )
            .unwrap();
        fx.parent = fx.db.get_repository(&fx.parent_id).unwrap().unwrap();
        rs67_create_independent_target(&fx, "svn-only.txt", b"retained SVN\n");
        let plan = rs67_prepare(&fx, false).await.unwrap();
        let bridge = fx.tmp.path().join("repos/parent/git-repo");
        let op = reposync_core::late_pair_publish::begin_reviewed_reconciliation(
            reposync_core::late_pair::reconciliation::PrepareContext {
                db: &fx.db,
                parent: &fx.parent,
                git_workdir: &bridge,
                request: &fx.request,
                admission: &fx.plan,
                svn_password: "",
                actor: "fixture-admin",
                include_parent: false,
            },
            &plan.review_pin,
            "invalid-column-contract",
        )
        .await
        .unwrap();
        let done = reposync_core::late_pair_publish::reviewed::run(
            &fx.db,
            &fx.config,
            &fx.parent,
            "",
            &bridge,
            &op.id,
            "fixture-admin",
        )
        .await
        .unwrap();
        let child = done.child_repo_id.as_deref().unwrap();
        let child_git = fx.tmp.path().join("repos").join(child).join("git-repo");
        assert!(
            reposync_core::db::late_pair_publish_operations::verified_reconciliation_frontier(
                &fx.db.conn(),
                child,
                plan.intended_git_sha.as_deref().unwrap(),
                &plan.projection
            )
            .unwrap()
        );
        let original = plan.intended_git_sha.as_deref().unwrap();
        if case == "missing_object" {
            std::fs::rename(
                &child_git,
                child_git.with_file_name("preserved-real-git-repo"),
            )
            .unwrap();
            let repo = git2::Repository::init(&child_git).unwrap();
            repo.remote("origin", &plan.git_remote_url).unwrap();
            let tree = repo
                .find_tree(repo.treebuilder(None).unwrap().write().unwrap())
                .unwrap();
            let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
            repo.commit(
                Some("refs/heads/feature"),
                &signature,
                &signature,
                "unrelated local fixture without handled object",
                &tree,
                &[],
            )
            .unwrap();
            repo.set_head("refs/heads/feature").unwrap();
        } else {
            let repo = git2::Repository::open(&child_git).unwrap();
            let parent = repo
                .find_commit(git2::Oid::from_str(original).unwrap())
                .unwrap();
            let tree = parent.tree().unwrap();
            let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
            let parents = if case == "unproved_descendant" {
                vec![&parent]
            } else {
                Vec::new()
            };
            let column = repo
                .commit(None, &signature, &signature, case, &tree, &parents)
                .unwrap()
                .to_string();
            fx.db
                .conn()
                .execute(
                    "UPDATE repositories SET last_git_sha=?1 WHERE id=?2",
                    rusqlite::params![column, child],
                )
                .unwrap();
            if case == "unrelated_emitted_column" {
                // Deliberately invalid durable fixture evidence must not turn
                // the valid old M proof into authority for unrelated U.
                let now = Utc::now();
                fx.db
                    .insert_sync_record(&SyncRecord {
                        id: "unrelated-column-fixture".into(),
                        repo_id: Some(child.into()),
                        svn_revision: done.reconciliation.as_ref().unwrap().svn_revision,
                        git_hash: Some(column),
                        direction: SyncDirection::SvnToGit,
                        author: "fixture".into(),
                        message: "unrelated invalid column fixture".into(),
                        timestamp: now,
                        synced_at: now,
                        status: SyncRecordStatus::Applied,
                    })
                    .unwrap();
            }
        }
        let prior = fx.db.get_repository(child).unwrap().unwrap();
        let records: i64 = fx
            .db
            .conn()
            .query_row(
                "SELECT count(*) FROM sync_records WHERE repo_id=?1",
                [child],
                |r| r.get(0),
            )
            .unwrap();
        let refs = Command::new("git")
            .args(["--git-dir", fx._bare.to_str().unwrap(), "show-ref"])
            .output()
            .unwrap()
            .stdout;
        let svn = SvnClient::new(&fx.target_url, "", "");
        let revision = svn.info().await.unwrap().latest_rev;
        drop(fx.db);
        let reopened = Database::new(fx.tmp.path().join("reposync.db")).unwrap();
        let mut config = fx.config.clone();
        config.github.default_branch = "feature".into();
        let mut engine = SyncEngine::new(
            config,
            reopened,
            svn,
            GitClient::new(&child_git).unwrap(),
            fx.identity.clone(),
        );
        engine.set_repo_id(child.into());
        engine.set_path_rules(allowed, Vec::new());
        let error = engine.run_sync_cycle().await.unwrap_err();
        let detail = error.to_string();
        assert!(
            detail.contains(if case == "missing_object" {
                "missing_checkpoint_object"
            } else {
                "ambiguous_checkpoint"
            }),
            "{case}: {detail}"
        );
        let after = engine.db().get_repository(child).unwrap().unwrap();
        assert_eq!(after.last_git_sha, prior.last_git_sha);
        assert_eq!(after.last_svn_rev, prior.last_svn_rev);
        assert_eq!(
            engine
                .db()
                .conn()
                .query_row(
                    "SELECT count(*) FROM sync_records WHERE repo_id=?1",
                    [child],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            records
        );
        assert_eq!(
            SvnClient::new(&fx.target_url, "", "")
                .info()
                .await
                .unwrap()
                .latest_rev,
            revision
        );
        assert_eq!(
            Command::new("git")
                .args(["--git-dir", fx._bare.to_str().unwrap(), "show-ref"])
                .output()
                .unwrap()
                .stdout,
            refs
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_contract_binary_overlap_is_blocked_and_missing_copy_is_never_reissued() {
    use reposync_core::late_pair_publish::reviewed::{self, Fault};
    let _guard = replay_publish_67_guard().await;
    let fx = build_publish_fixture().await;
    rs67_create_independent_target(&fx, "feature.txt", b"\0independent binary SVN\n");
    let plan = rs67_prepare(&fx, false).await.unwrap();
    assert!(!plan.conflicts.is_empty());
    assert!(plan.intended_git_sha.is_none());
    let revision = SvnClient::new(&fx.target_url, "", "")
        .info()
        .await
        .unwrap()
        .latest_rev;
    let bridge = fx.tmp.path().join("repos/parent/git-repo");
    assert!(
        reposync_core::late_pair_publish::begin_reviewed_reconciliation(
            reposync_core::late_pair::reconciliation::PrepareContext {
                db: &fx.db,
                parent: &fx.parent,
                git_workdir: &bridge,
                request: &fx.request,
                admission: &fx.plan,
                svn_password: "",
                actor: "fixture-admin",
                include_parent: false
            },
            &plan.review_pin,
            "binary-blocked"
        )
        .await
        .is_err()
    );
    assert!(fx
        .db
        .latest_late_pair_publish_operation(&fx.parent_id)
        .unwrap()
        .is_none());
    assert_eq!(
        SvnClient::new(&fx.target_url, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        revision
    );
    drop(fx);
    let fx = build_publish_fixture().await;
    let plan = rs67_prepare(&fx, false).await.unwrap();
    let bridge = fx.tmp.path().join("repos/parent/git-repo");
    let op = reposync_core::late_pair_publish::begin_reviewed_reconciliation(
        reposync_core::late_pair::reconciliation::PrepareContext {
            db: &fx.db,
            parent: &fx.parent,
            git_workdir: &bridge,
            request: &fx.request,
            admission: &fx.plan,
            svn_password: "",
            actor: "fixture-admin",
            include_parent: false,
        },
        &plan.review_pin,
        "missing-copy-held",
    )
    .await
    .unwrap();
    reviewed::set_fault(&fx.parent_id, Fault::AfterCopy);
    assert!(reviewed::run(
        &fx.db,
        &fx.config,
        &fx.parent,
        "",
        &bridge,
        &op.id,
        "fixture-admin"
    )
    .await
    .is_err());
    let result = Command::new("svn")
        .args([
            "delete",
            &fx.target_url,
            "-m",
            "external fixture removes uncertain target",
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let root = SvnClient::new(&fx.svn_url, "", "");
    let revision = root.info().await.unwrap().latest_rev;
    assert!(reviewed::run(
        &fx.db,
        &fx.config,
        &fx.parent,
        "",
        &bridge,
        &op.id,
        "fixture-admin"
    )
    .await
    .is_err());
    assert_eq!(
        root.info().await.unwrap().latest_rev,
        revision,
        "unknown/absent outcome is not permission to copy again"
    );
    assert!(SvnClient::new(&fx.target_url, "", "").info().await.is_err());
    let held = fx
        .db
        .get_late_pair_publish_operation(&fx.parent_id, &op.id)
        .unwrap()
        .unwrap();
    assert_eq!(held.state, LatePairPublishState::ReconciliationHeld);
    assert!(!held.state.is_terminal());
    assert!(fx
        .db
        .list_child_repositories(&fx.parent_id)
        .unwrap()
        .is_empty());
}

#[path = "fixtures/pair_refresh_job.rs"]
mod rs67_refresh_storage;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_contract_foreign_refresh_reader_keeps_active_refusal_and_admits_real_completed_versions(
) {
    let _guard = replay_publish_67_guard().await;
    let mut documents = Vec::new();
    {
        let fx = build_publish_fixture().await;
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
            "foreign-journal-legacy-child",
            "fixture-admin",
            "legacy-native-completion",
            &fx.identity,
        )
        .await
        .unwrap();
        let completed = fx
            .db
            .latest_late_pair_publish_operation(&fx.parent_id)
            .unwrap()
            .unwrap();
        assert_eq!(completed.version, 1);
        assert_eq!(completed.state, LatePairPublishState::Completed);
        documents.push(completed);
    }
    {
        let fx = build_publish_fixture().await;
        let plan = rs67_prepare(&fx, false).await.unwrap();
        let bridge = fx.tmp.path().join("repos/parent/git-repo");
        let op = reposync_core::late_pair_publish::begin_reviewed_reconciliation(
            reposync_core::late_pair::reconciliation::PrepareContext {
                db: &fx.db,
                parent: &fx.parent,
                git_workdir: &bridge,
                request: &fx.request,
                admission: &fx.plan,
                svn_password: "",
                actor: "fixture-admin",
                include_parent: false,
            },
            &plan.review_pin,
            "v2-native-completion",
        )
        .await
        .unwrap();
        documents.push(
            reposync_core::late_pair_publish::reviewed::run(
                &fx.db,
                &fx.config,
                &fx.parent,
                "",
                &bridge,
                &op.id,
                "fixture-admin",
            )
            .await
            .unwrap(),
        );
    }
    // Only storage admission is tested here. The retained fixture's synthetic
    // refresh pins do not start a worker, authorize effects or prove remotes.
    for completed in documents {
        let f = rs67_refresh_storage::Fixture::new();
        assert!(f.dir.path().exists());
        let raw = serde_json::to_string(&completed).unwrap();
        let document = format!("late_pair_publish_v1:document:{}", completed.id);
        f.db.set_state(&document, &raw).unwrap();
        f.db.set_state("late_pair_publish_v1:latest:parent", &completed.id)
            .unwrap();
        f.db.set_state("late_pair_publish_v1:active:parent", &completed.id)
            .unwrap();
        let pins = f.pins();
        let digest = pins.approval_digest().unwrap();
        let busy = f.busy();
        let refusal =
            f.db.admit_pair_refresh(
                "admin",
                "fixture-storage-request",
                &digest,
                pins.clone(),
                &busy,
            )
            .unwrap_err();
        assert!(
            refusal
                .to_string()
                .contains("another operation owns the repository"),
            "{refusal}"
        );
        assert!(f.no_hold(None));
        f.db.delete_kv_state("late_pair_publish_v1:active:parent")
            .unwrap();
        f.db.set_state(&document, "{}").unwrap();
        assert!(f
            .db
            .admit_pair_refresh(
                "admin",
                "fixture-storage-request",
                &digest,
                pins.clone(),
                &busy
            )
            .is_err());
        assert!(f.no_hold(None));
        f.db.set_state(&document, &raw).unwrap();
        let id = f.admit(pins, &busy);
        assert!(!f.no_hold(Some("parent")));
        assert!(!f.no_hold(Some("child")));
        assert!(
            f.db.get_state(&format!("pair_refresh_execution_v3:document:{id}"))
                .unwrap()
                .is_none(),
            "storage-only test does not create authenticated execution authority"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_missing_ambiguous_rewritten_and_out_of_scope_sources_refuse_without_effects() {
    let _guard = replay_publish_67_guard().await;
    for case in [
        "missing_mapping",
        "ambiguous_mapping",
        "rewritten_history",
        "out_of_scope",
    ] {
        let mut fx = build_publish_fixture().await;
        let bridge = fx.tmp.path().join("repos/parent/git-repo");
        match case {
            "missing_mapping" => {
                fx.db
                    .conn()
                    .execute("DELETE FROM sync_records WHERE repo_id=?1", [&fx.parent_id])
                    .unwrap();
            }
            "ambiguous_mapping" => {
                fx.db.conn().execute("INSERT INTO sync_records (id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) SELECT 'ambiguous-baseline',repo_id,svn_rev+1,git_sha,direction,author,message,timestamp,synced_at,status FROM sync_records WHERE repo_id=?1 AND git_sha=?2", rusqlite::params![fx.parent_id,fx._base_sha]).unwrap();
            }
            "rewritten_history" => {
                let remote = git2::Repository::open_bare(&fx._bare).unwrap();
                let tree = remote
                    .find_commit(git2::Oid::from_str(&fx.tip).unwrap())
                    .unwrap()
                    .tree()
                    .unwrap();
                let signature =
                    git2::Signature::now("synthetic-rewrite", "fixture@invalid").unwrap();
                let orphan = remote
                    .commit(
                        None,
                        &signature,
                        &signature,
                        "Unrelated replacement root",
                        &tree,
                        &[],
                    )
                    .unwrap();
                remote
                    .find_reference("refs/heads/feature")
                    .unwrap()
                    .set_target(orphan, "synthetic test rewrite")
                    .unwrap();
            }
            "out_of_scope" => {
                fx.db
                    .conn()
                    .execute(
                        "UPDATE repositories SET allowed_paths='[\"origin.txt\"]' WHERE id=?1",
                        [&fx.parent_id],
                    )
                    .unwrap();
                fx.parent = fx.db.get_repository(&fx.parent_id).unwrap().unwrap();
            }
            _ => unreachable!(),
        }
        let mappings = collect_verified_mappings(&fx.db, &fx.parent_id).unwrap();
        let parent_before = serde_json::to_value(&fx.parent).unwrap();
        let counts_before: (i64, i64) = fx
            .db
            .conn()
            .query_row(
                "SELECT (SELECT count(*) FROM sync_records),(SELECT count(*) FROM kv_state)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let refs_before = Command::new("git")
            .args(["--git-dir", fx._bare.to_str().unwrap(), "show-ref"])
            .output()
            .unwrap();
        assert!(refs_before.status.success());
        let svn_before = SvnClient::new(&fx.svn_url, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev;
        if case == "missing_mapping" {
            let refusal =
                evaluate_admission(&mappings, Some(&bridge), None, &fx.request, None).unwrap_err();
            assert_eq!(refusal.reason, "missing_baseline");
        } else {
            let error = format!("{:#}", rs67_prepare(&fx, false).await.unwrap_err());
            assert!(
                error.contains(match case {
                    "ambiguous_mapping" => "ambiguous SVN revision",
                    "rewritten_history" => "Git tip differs from fresh remote observation",
                    _ => "out_of_scope_path",
                }),
                "{case}: {error}"
            );
        }
        assert_eq!(
            serde_json::to_value(fx.db.get_repository(&fx.parent_id).unwrap().unwrap()).unwrap(),
            parent_before,
            "{case}"
        );
        let counts_after: (i64, i64) = fx
            .db
            .conn()
            .query_row(
                "SELECT (SELECT count(*) FROM sync_records),(SELECT count(*) FROM kv_state)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts_after, counts_before, "{case}");
        assert!(
            fx.db
                .list_child_repositories(&fx.parent_id)
                .unwrap()
                .is_empty(),
            "{case}"
        );
        assert!(
            fx.db
                .latest_late_pair_publish_operation(&fx.parent_id)
                .unwrap()
                .is_none(),
            "{case}"
        );
        let refs_after = Command::new("git")
            .args(["--git-dir", fx._bare.to_str().unwrap(), "show-ref"])
            .output()
            .unwrap();
        assert!(refs_after.status.success());
        assert_eq!(refs_after.stdout, refs_before.stdout, "{case}");
        assert_eq!(
            SvnClient::new(&fx.svn_url, "", "")
                .info()
                .await
                .unwrap()
                .latest_rev,
            svn_before,
            "{case}"
        );
        assert!(
            SvnClient::new(&fx.target_url, "", "").info().await.is_err(),
            "{case}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_held_resume_revalidates_git_before_new_svn_effects() {
    use reposync_core::late_pair_publish::{self as publisher, reviewed};
    let _guard = replay_publish_67_guard().await;
    fn history(url: &str) -> Vec<String> {
        let output = Command::new("svn")
            .args(["log", "--xml", "--verbose", "--", url])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // XML attribute order varies between native invocations. Preserve every
        // element, attribute value and text node while comparing that order
        // canonically; dates, messages, copy paths and revisions remain exact.
        let xml = String::from_utf8(output.stdout).unwrap();
        let document = roxmltree::Document::parse(&xml).unwrap();
        document
            .descendants()
            .filter(|node| node.is_element() || node.is_text())
            .map(|node| {
                if node.is_element() {
                    let mut attributes = node
                        .attributes()
                        .map(|a| (a.namespace(), a.name(), a.value()))
                        .collect::<Vec<_>>();
                    attributes.sort();
                    format!("element {:?} {attributes:?}", node.tag_name())
                } else {
                    format!("text {:?}", node.text())
                }
            })
            .collect()
    }
    fn producers(db: &Database) -> Vec<(String, String)> {
        db.conn().prepare("SELECT key,value FROM kv_state WHERE key LIKE 'git_to_svn_commit_v1:%' OR key LIKE 'svn_to_git_push_v1:%' ORDER BY key").unwrap()
            .query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap()
    }
    fn mappings(db: &Database) -> i64 {
        db.conn()
            .query_row("SELECT count(*) FROM sync_records", [], |r| r.get(0))
            .unwrap()
    }
    for rewritten in [false, true] {
        let fx = build_publish_fixture().await;
        rs67_create_independent_target(&fx, "svn-only.txt", b"independent SVN\n");
        let plan = rs67_prepare(&fx, false).await.unwrap();
        let bridge = fx.tmp.path().join("repos/parent/git-repo");
        let op = publisher::begin_reviewed_reconciliation(
            reposync_core::late_pair::reconciliation::PrepareContext {
                db: &fx.db,
                parent: &fx.parent,
                git_workdir: &bridge,
                request: &fx.request,
                admission: &fx.plan,
                svn_password: "",
                actor: "fixture-admin",
                include_parent: false,
            },
            &plan.review_pin,
            "held-stale-remote",
        )
        .await
        .unwrap();
        let bare = git2::Repository::open_bare(&fx._bare).unwrap();
        let parent = bare
            .find_commit(git2::Oid::from_str(&fx.tip).unwrap())
            .unwrap();
        let tree = parent.tree().unwrap();
        let signature = git2::Signature::now("synthetic-remote-writer", "fixture@invalid").unwrap();
        let parents = if rewritten { Vec::new() } else { vec![&parent] };
        let g2 = bare
            .commit(
                None,
                &signature,
                &signature,
                "Changed after reviewed claim",
                &tree,
                &parents,
            )
            .unwrap();
        bare.find_reference("refs/heads/feature")
            .unwrap()
            .set_target(g2, "synthetic post-claim change")
            .unwrap();
        let before_history = history(&fx.svn_url);
        let target = SvnClient::new(&fx.target_url, "", "");
        let before_revision = target.info().await.unwrap().latest_rev;
        let before_tree =
            reposync_core::late_pair::reconciliation::capture_svn_tree(&target, before_revision)
                .await
                .unwrap();
        let before_producers = producers(&fx.db);
        let before_mappings = mappings(&fx.db);
        let parent_before =
            serde_json::to_value(fx.db.get_repository(&fx.parent_id).unwrap()).unwrap();
        for resumed in [false, true] {
            if resumed {
                // This is the same actual continuation/run pair used by the
                // authenticated branch action="resume" product handler.
                publisher::continue_reviewed_reconciliation(
                    &fx.db,
                    &fx.parent_id,
                    &op.id,
                    "fixture-admin",
                    &plan.review_pin,
                )
                .unwrap();
            }
            let error = reviewed::run(
                &fx.db,
                &fx.config,
                &fx.parent,
                "",
                &bridge,
                &op.id,
                "fixture-admin",
            )
            .await
            .unwrap_err();
            assert_eq!(target.info().await.unwrap().latest_rev, before_revision);
            assert_eq!(
                history(&fx.svn_url),
                before_history,
                "resume must not write SVN history"
            );
            assert_eq!(
                reposync_core::late_pair::reconciliation::capture_svn_tree(
                    &target,
                    before_revision
                )
                .await
                .unwrap(),
                before_tree
            );
            assert_eq!(
                producers(&fx.db),
                before_producers,
                "resume must not create native intents"
            );
            assert_eq!(mappings(&fx.db), before_mappings);
            assert!(fx
                .db
                .list_child_repositories(&fx.parent_id)
                .unwrap()
                .is_empty());
            assert_eq!(
                serde_json::to_value(fx.db.get_repository(&fx.parent_id).unwrap()).unwrap(),
                parent_before
            );
            assert_eq!(
                bare.find_reference("refs/heads/feature").unwrap().target(),
                Some(g2)
            );
            let held = fx
                .db
                .get_late_pair_publish_operation(&fx.parent_id, &op.id)
                .unwrap()
                .unwrap();
            assert_eq!(held.state, LatePairPublishState::ReconciliationHeld);
            assert!(!held.state.is_terminal());
            assert!(held.child_repo_id.is_none());
            assert!(
                format!("{error:#}").contains("Git tip changed before effects"),
                "rewrite={rewritten} resume={resumed}: {error:#}"
            );
        }
        // Restoring this disposable ref restores the originally reviewed source;
        // it is not new approval or authority to repeat a consumed native intent.
        bare.find_reference("refs/heads/feature")
            .unwrap()
            .set_target(parent.id(), "restore synthetic reviewed source")
            .unwrap();
        publisher::continue_reviewed_reconciliation(
            &fx.db,
            &fx.parent_id,
            &op.id,
            "fixture-admin",
            &plan.review_pin,
        )
        .unwrap();
        let done = reviewed::run(
            &fx.db,
            &fx.config,
            &fx.parent,
            "",
            &bridge,
            &op.id,
            "fixture-admin",
        )
        .await
        .unwrap();
        assert_eq!(done.state, LatePairPublishState::Completed);
        let progress = done.reconciliation.as_ref().unwrap();
        assert_eq!(progress.receipts.len(), 2);
        assert_eq!(progress.receipts[0].svn_revision, before_revision + 1);
        assert_eq!(progress.receipts[1].svn_revision, before_revision + 2);
        let completed_history = history(&fx.svn_url);
        let completed_producers = producers(&fx.db);
        let completed_mappings = mappings(&fx.db);
        assert_eq!(
            reviewed::run(
                &fx.db,
                &fx.config,
                &fx.parent,
                "",
                &bridge,
                &op.id,
                "fixture-admin"
            )
            .await
            .unwrap(),
            done
        );
        assert_eq!(history(&fx.svn_url), completed_history);
        assert_eq!(producers(&fx.db), completed_producers);
        assert_eq!(mappings(&fx.db), completed_mappings);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_recovery_inspects_consumed_svn_but_holds_next_step_for_stale_git() {
    use reposync_core::late_pair_publish::{
        self as publisher,
        reviewed::{self, Fault},
    };
    let _guard = replay_publish_67_guard().await;
    let fx = build_publish_fixture().await;
    rs67_create_independent_target(&fx, "svn-only.txt", b"independent SVN\n");
    let plan = rs67_prepare(&fx, false).await.unwrap();
    let bridge = fx.tmp.path().join("repos/parent/git-repo");
    let op = publisher::begin_reviewed_reconciliation(
        reposync_core::late_pair::reconciliation::PrepareContext {
            db: &fx.db,
            parent: &fx.parent,
            git_workdir: &bridge,
            request: &fx.request,
            admission: &fx.plan,
            svn_password: "",
            actor: "fixture-admin",
            include_parent: false,
        },
        &plan.review_pin,
        "consumed-step-stale-next",
    )
    .await
    .unwrap();
    reviewed::set_fault(&fx.parent_id, Fault::AfterSvnCommit(1));
    assert!(format!(
        "{:#}",
        reviewed::run(
            &fx.db,
            &fx.config,
            &fx.parent,
            "",
            &bridge,
            &op.id,
            "fixture-admin"
        )
        .await
        .unwrap_err()
    )
    .contains("controlled late-pair interruption"));
    let held = fx
        .db
        .get_late_pair_publish_operation(&fx.parent_id, &op.id)
        .unwrap()
        .unwrap();
    let child = held.child_repo_id.as_deref().unwrap();
    let pending = held
        .reconciliation
        .as_ref()
        .unwrap()
        .pending_svn_operation
        .clone()
        .unwrap();
    let target = SvnClient::new(&fx.target_url, "", "");
    let consumed_revision = target.info().await.unwrap().latest_rev;
    assert_eq!(consumed_revision, plan.target_revision.unwrap() + 1);
    let consumed_tree =
        reposync_core::late_pair::reconciliation::capture_svn_tree(&target, consumed_revision)
            .await
            .unwrap();
    assert_eq!(consumed_tree, plan.steps[0].intended_tree);
    let bare = git2::Repository::open_bare(&fx._bare).unwrap();
    let original = bare
        .find_commit(git2::Oid::from_str(&fx.tip).unwrap())
        .unwrap();
    let signature = git2::Signature::now("synthetic-remote-writer", "fixture@invalid").unwrap();
    let g2 = bare
        .commit(
            None,
            &signature,
            &signature,
            "Advance while first SVN outcome is pending",
            &original.tree().unwrap(),
            &[&original],
        )
        .unwrap();
    bare.find_reference("refs/heads/feature")
        .unwrap()
        .set_target(g2, "synthetic drift after consumed SVN")
        .unwrap();
    let producer_count = |db: &Database| {
        db.conn()
            .query_row(
                "SELECT count(*) FROM kv_state WHERE key LIKE 'git_to_svn_commit_v1:document:%'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
    };
    let count = producer_count(&fx.db);
    publisher::continue_reviewed_reconciliation(
        &fx.db,
        &fx.parent_id,
        &op.id,
        "fixture-admin",
        &plan.review_pin,
    )
    .unwrap();
    let error = reviewed::run(
        &fx.db,
        &fx.config,
        &fx.parent,
        "",
        &bridge,
        &op.id,
        "fixture-admin",
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("Git tip changed before effects"));
    assert_eq!(target.info().await.unwrap().latest_rev, consumed_revision);
    assert_eq!(
        reposync_core::late_pair::reconciliation::capture_svn_tree(&target, consumed_revision)
            .await
            .unwrap(),
        consumed_tree
    );
    assert_eq!(
        producer_count(&fx.db),
        count,
        "no second intent from stale source"
    );
    let inspected = fx
        .db
        .get_late_pair_publish_operation(&fx.parent_id, &op.id)
        .unwrap()
        .unwrap();
    assert_eq!(inspected.state, LatePairPublishState::ReconciliationHeld);
    let p = inspected.reconciliation.as_ref().unwrap();
    assert_eq!(
        p.receipts.len(),
        1,
        "consumed outcome inspection is allowed"
    );
    assert_eq!(p.receipts[0].svn_operation.as_ref().unwrap().id, pending);
    assert!(p.pending_svn_operation.is_none());
    assert!(!fx.db.get_repository(child).unwrap().unwrap().enabled);
    assert_eq!(
        bare.find_reference("refs/heads/feature").unwrap().target(),
        Some(g2)
    );
    bare.find_reference("refs/heads/feature")
        .unwrap()
        .set_target(original.id(), "restore synthetic reviewed source")
        .unwrap();
    publisher::continue_reviewed_reconciliation(
        &fx.db,
        &fx.parent_id,
        &op.id,
        "fixture-admin",
        &plan.review_pin,
    )
    .unwrap();
    let done = reviewed::run(
        &fx.db,
        &fx.config,
        &fx.parent,
        "",
        &bridge,
        &op.id,
        "fixture-admin",
    )
    .await
    .unwrap();
    assert_eq!(done.state, LatePairPublishState::Completed);
    let completed = done.reconciliation.as_ref().unwrap();
    assert_eq!(completed.receipts.len(), 2);
    assert_eq!(
        completed.receipts[0], p.receipts[0],
        "never repeat the consumed step"
    );
    assert_eq!(completed.svn_revision, Some(consumed_revision + 1));
    assert_eq!(producer_count(&fx.db), count + 1);
    let revision = target.info().await.unwrap().latest_rev;
    assert_eq!(
        reviewed::run(
            &fx.db,
            &fx.config,
            &fx.parent,
            "",
            &bridge,
            &op.id,
            "fixture-admin"
        )
        .await
        .unwrap(),
        done
    );
    assert_eq!(target.info().await.unwrap().latest_rev, revision);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_reviewed_child_identity_refuses_foreign_parent_uuid_without_child_or_recopy() {
    use reposync_core::late_pair_publish::reviewed::{self, Fault};
    let _guard = replay_publish_67_guard().await;
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    let plan = rs67_prepare(&fx, false).await.unwrap();
    let bridge = fx.tmp.path().join("repos/parent/git-repo");
    let op = reposync_core::late_pair_publish::begin_reviewed_reconciliation(
        reposync_core::late_pair::reconciliation::PrepareContext {
            db: &fx.db,
            parent: &fx.parent,
            git_workdir: &bridge,
            request: &fx.request,
            admission: &fx.plan,
            svn_password: "",
            actor: "fixture-admin",
            include_parent: false,
        },
        &plan.review_pin,
        "foreign-parent-uuid",
    )
    .await
    .unwrap();
    reviewed::set_fault(&fx.parent_id, Fault::AfterCopy);
    assert!(reviewed::run(
        &fx.db,
        &fx.config,
        &fx.parent,
        "",
        &bridge,
        &op.id,
        "fixture-admin"
    )
    .await
    .is_err());
    let genuine = reposync_core::db::lineage_checkpoints::independently_established_svn_uuid(
        &fx.db.conn(),
        &fx.parent_id,
    )
    .unwrap()
    .unwrap();
    overwrite_established_uuid(
        &fx.db,
        &fx.parent_id,
        Some("ffffffff-ffff-ffff-ffff-ffffffffffff"),
    );
    let before = {
        let output = Command::new("svn")
            .args(["log", "-q", "--", &fx.target_url])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    let mappings: i64 = fx
        .db
        .conn()
        .query_row("SELECT count(*) FROM sync_records", [], |r| r.get(0))
        .unwrap();
    let error = reviewed::run(
        &fx.db,
        &fx.config,
        &fx.parent,
        "",
        &bridge,
        &op.id,
        "fixture-admin",
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("parent established UUID contradicts reviewed child identity"),
        "{error:#}"
    );
    let held = fx
        .db
        .get_late_pair_publish_operation(&fx.parent_id, &op.id)
        .unwrap()
        .unwrap();
    assert_eq!(held.state, LatePairPublishState::ReconciliationHeld);
    assert!(
        held.child_repo_id.is_none() || {
            fx.db
                .get_repository(held.child_repo_id.as_deref().unwrap())
                .unwrap()
                .map(|child| !child.enabled)
                .unwrap_or(true)
        }
    );
    let after = {
        let output = Command::new("svn")
            .args(["log", "-q", "--", &fx.target_url])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    };
    assert_eq!(after, before, "native copy must not be reissued");
    let mappings_after: i64 = fx
        .db
        .conn()
        .query_row("SELECT count(*) FROM sync_records", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mappings_after, mappings);
    overwrite_established_uuid(&fx.db, &fx.parent_id, Some(&genuine));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_reviewed_child_restart_refuses_missing_changed_foreign_uuid_enabled_and_reused_scope()
{
    use reposync_core::late_pair_publish::reviewed::{self, Fault};
    let _guard = replay_publish_67_guard().await;
    assert!(svn_available());
    async fn interrupt_after_first_svn(
        fx: &PublishFixture,
        request: &str,
    ) -> reposync_core::db::late_pair_publish_operations::LatePairPublishOperation {
        let plan = rs67_prepare(fx, false).await.unwrap();
        let bridge = fx.tmp.path().join("repos/parent/git-repo");
        let op = reposync_core::late_pair_publish::begin_reviewed_reconciliation(
            reposync_core::late_pair::reconciliation::PrepareContext {
                db: &fx.db,
                parent: &fx.parent,
                git_workdir: &bridge,
                request: &fx.request,
                admission: &fx.plan,
                svn_password: "",
                actor: "fixture-admin",
                include_parent: false,
            },
            &plan.review_pin,
            request,
        )
        .await
        .unwrap();
        reviewed::set_fault(&fx.parent_id, Fault::AfterSvnCommit(1));
        assert!(reviewed::run(
            &fx.db,
            &fx.config,
            &fx.parent,
            "",
            &bridge,
            &op.id,
            "fixture-admin"
        )
        .await
        .is_err());
        fx.db
            .get_late_pair_publish_operation(&fx.parent_id, &op.id)
            .unwrap()
            .unwrap()
    }
    async fn refuse(
        fx: &PublishFixture,
        op_id: &str,
        needle: &str,
    ) -> reposync_core::db::late_pair_publish_operations::LatePairPublishOperation {
        let bridge = fx.tmp.path().join("repos/parent/git-repo");
        let mappings: i64 = fx
            .db
            .conn()
            .query_row("SELECT count(*) FROM sync_records", [], |r| r.get(0))
            .unwrap();
        let log_before = String::from_utf8(
            Command::new("svn")
                .args(["log", "-q", "--", &fx.target_url])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let error = reviewed::run(
            &fx.db,
            &fx.config,
            &fx.parent,
            "",
            &bridge,
            op_id,
            "fixture-admin",
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains(needle), "{error:#}");
        let held = fx
            .db
            .get_late_pair_publish_operation(&fx.parent_id, op_id)
            .unwrap()
            .unwrap();
        assert_eq!(held.state, LatePairPublishState::ReconciliationHeld);
        if let Some(child_id) = held.child_repo_id.as_deref() {
            let _child = fx.db.get_repository(child_id).unwrap().unwrap();
            assert_ne!(held.state, LatePairPublishState::Completed);
        }
        let mappings_after: i64 = fx
            .db
            .conn()
            .query_row("SELECT count(*) FROM sync_records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mappings_after, mappings);
        let log_after = String::from_utf8(
            Command::new("svn")
                .args(["log", "-q", "--", &fx.target_url])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert_eq!(log_after, log_before);
        held
    }

    {
        let fx = build_publish_fixture().await;
        let held = interrupt_after_first_svn(&fx, "missing-uuid").await;
        let child = held.child_repo_id.clone().unwrap();
        let original = reposync_core::db::lineage_checkpoints::independently_established_svn_uuid(
            &fx.db.conn(),
            &child,
        )
        .unwrap()
        .unwrap();
        overwrite_established_uuid(&fx.db, &child, None);
        refuse(
            &fx,
            &held.id,
            "disabled child restart has no independently established UUID",
        )
        .await;
        assert!(!fx.db.get_repository(&child).unwrap().unwrap().enabled);
        assert!(
            reposync_core::db::lineage_checkpoints::independently_established_svn_uuid(
                &fx.db.conn(),
                &child
            )
            .unwrap()
            .is_none()
        );
        overwrite_established_uuid(&fx.db, &child, Some(&original));
    }
    {
        let fx = build_publish_fixture().await;
        let held = interrupt_after_first_svn(&fx, "foreign-uuid").await;
        let child = held.child_repo_id.clone().unwrap();
        let original = reposync_core::db::lineage_checkpoints::independently_established_svn_uuid(
            &fx.db.conn(),
            &child,
        )
        .unwrap()
        .unwrap();
        overwrite_established_uuid(&fx.db, &child, Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"));
        refuse(
            &fx,
            &held.id,
            "disabled child restart UUID does not match this operation's reviewed identity",
        )
        .await;
        assert!(!fx.db.get_repository(&child).unwrap().unwrap().enabled);
        assert_eq!(
            reposync_core::db::lineage_checkpoints::independently_established_svn_uuid(
                &fx.db.conn(),
                &child
            )
            .unwrap()
            .as_deref(),
            Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        );
        overwrite_established_uuid(&fx.db, &child, Some(&original));
    }
    {
        let fx = build_publish_fixture().await;
        let held = interrupt_after_first_svn(&fx, "enabled-child").await;
        let child_id = held.child_repo_id.clone().unwrap();
        fx.db
            .conn()
            .execute(
                "UPDATE repositories SET enabled=1 WHERE id=?1",
                rusqlite::params![child_id],
            )
            .unwrap();
        refuse(
            &fx,
            &held.id,
            "child registration was replaced or activated before completion",
        )
        .await;
        assert!(fx.db.get_repository(&child_id).unwrap().unwrap().enabled);
        assert_ne!(
            fx.db
                .get_late_pair_publish_operation(&fx.parent_id, &held.id)
                .unwrap()
                .unwrap()
                .state,
            LatePairPublishState::Completed
        );
    }
    {
        let fx = build_publish_fixture().await;
        let held = interrupt_after_first_svn(&fx, "reused-scope").await;
        let child_id = held.child_repo_id.clone().unwrap();
        let original_scope = held
            .reconciliation
            .as_ref()
            .unwrap()
            .child_scope_uuid
            .clone()
            .unwrap();
        fx.db
            .conn()
            .execute(
                "DELETE FROM repositories WHERE id=?1",
                rusqlite::params![child_id],
            )
            .unwrap();
        let mut replacement = fx.parent.clone();
        replacement.id = child_id.clone();
        replacement.name = format!("reused-child-{child_id}");
        replacement.parent_id = Some(fx.parent_id.clone());
        replacement.enabled = false;
        replacement.sync_status = "reconciling".into();
        replacement.git_branch = held.git_branch.clone();
        replacement.svn_branch = held.svn_branch.clone();
        fx.db.insert_repository(&replacement).unwrap();
        let reused =
            reposync_core::db::repo_scope_identity::repository_scope_uuid(&fx.db.conn(), &child_id)
                .unwrap();
        assert_ne!(reused, original_scope);
        refuse(&fx, &held.id, "child registration incarnation changed").await;
        assert!(!fx.db.get_repository(&child_id).unwrap().unwrap().enabled);
        assert_eq!(
            reposync_core::db::repo_scope_identity::repository_scope_uuid(&fx.db.conn(), &child_id)
                .unwrap(),
            reused
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs67_reviewed_child_identity_incomplete_handoff_stays_disabled_held_on_reopen() {
    use reposync_core::late_pair_publish::reviewed::{self, Fault};
    let _guard = replay_publish_67_guard().await;
    assert!(svn_available());
    let fx = build_publish_fixture().await;
    let plan = rs67_prepare(&fx, false).await.unwrap();
    let bridge = fx.tmp.path().join("repos/parent/git-repo");
    let op = reposync_core::late_pair_publish::begin_reviewed_reconciliation(
        reposync_core::late_pair::reconciliation::PrepareContext {
            db: &fx.db,
            parent: &fx.parent,
            git_workdir: &bridge,
            request: &fx.request,
            admission: &fx.plan,
            svn_password: "",
            actor: "fixture-admin",
            include_parent: false,
        },
        &plan.review_pin,
        "incomplete-handoff",
    )
    .await
    .unwrap();
    reviewed::set_fault(&fx.parent_id, Fault::AfterSvnCommit(1));
    assert!(reviewed::run(
        &fx.db,
        &fx.config,
        &fx.parent,
        "",
        &bridge,
        &op.id,
        "fixture-admin"
    )
    .await
    .is_err());
    let held = fx
        .db
        .get_late_pair_publish_operation(&fx.parent_id, &op.id)
        .unwrap()
        .unwrap();
    let child_id = held.child_repo_id.clone().unwrap();
    let copy = held.reconciliation.as_ref().unwrap().copy_evidence.clone();
    let receipts = held.reconciliation.as_ref().unwrap().receipts.clone();
    overwrite_established_uuid(&fx.db, &child_id, None);
    drop(fx.db);
    let reopened = Database::new(fx.tmp.path().join("reposync.db")).unwrap();
    let error = reviewed::run(
        &reopened,
        &fx.config,
        &fx.parent,
        "",
        &bridge,
        &op.id,
        "fixture-admin",
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("disabled child restart has no independently established UUID"),
        "{error:#}"
    );
    let again = reopened
        .get_late_pair_publish_operation(&fx.parent_id, &op.id)
        .unwrap()
        .unwrap();
    assert_eq!(again.state, LatePairPublishState::ReconciliationHeld);
    assert_ne!(again.state, LatePairPublishState::Completed);
    let child = reopened.get_repository(&child_id).unwrap().unwrap();
    assert!(!child.enabled);
    assert_eq!(again.reconciliation.as_ref().unwrap().copy_evidence, copy);
    assert_eq!(again.reconciliation.as_ref().unwrap().receipts, receipts);
    assert!(
        reposync_core::db::lineage_checkpoints::independently_established_svn_uuid(
            &reopened.conn(),
            &child_id
        )
        .unwrap()
        .is_none()
    );
}
