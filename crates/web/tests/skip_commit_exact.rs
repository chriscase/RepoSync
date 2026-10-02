//! Exact skip-commit disposition (#66 / RS-C04 / RSUI-03).
//!
//! Validates pinned cursor + selected commits + observed tip tokens, exclusion
//! receipts, refuse paths with zero mutation, and concurrent tick safety.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::ImportProgress;
use reposync_core::models::Repository;
use reposync_core::skip_commit::{
    exclusion_receipt_key, is_commit_excluded, reason as skip_reason, SkipCommitRequest,
};
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_web::api;
use reposync_web::AppState;
use tempfile::TempDir;

const TEST_TOKEN: &str = "test-session-token-skip-commit-exact";

fn git_cli(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture Developer")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture Developer")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_sha(repo: &Path, rev: &str) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", rev])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn pending_after(repo: &Path, cursor: &str) -> Vec<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-list", "--reverse", &format!("{cursor}..HEAD")])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn authed_client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {TEST_TOKEN}")).unwrap(),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap()
}

fn fixture_repo(id: &str, last_git_sha: &str) -> Repository {
    let now = chrono::Utc::now().to_rfc3339();
    Repository {
        id: id.into(),
        name: "skip-commit-fixture".into(),
        svn_url: "file:///nonexistent/svn".into(),
        svn_branch: "trunk".into(),
        svn_username: "fixture".into(),
        git_provider: "github".into(),
        git_api_url: "http://127.0.0.1:1".into(),
        git_repo: "test/repo".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 60,
        lfs_threshold_mb: 1,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 4,
        last_git_sha: last_git_sha.into(),
        last_sync_at: Some("2026-10-01T00:00:00+00:00".into()),
        sync_status: "error_paused".into(),
        total_syncs: 3,
        total_errors: 5,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 3,
        teams_webhook_url: None,
    }
}

struct WatermarkSnapshot {
    last_svn_rev: i64,
    last_git_sha: String,
    consecutive_errors: i64,
    sync_status: String,
    kv_last_git_sha: Option<String>,
    kv_last_svn_rev: Option<String>,
    kv_last_git_hash: Option<String>,
}

fn snapshot_watermarks(db: &Database, repo_id: &str) -> WatermarkSnapshot {
    let repo = db.get_repository(repo_id).unwrap().unwrap();
    WatermarkSnapshot {
        last_svn_rev: repo.last_svn_rev,
        last_git_sha: repo.last_git_sha,
        consecutive_errors: repo.consecutive_errors,
        sync_status: repo.sync_status,
        kv_last_git_sha: db.get_state(&format!("last_git_sha_{repo_id}")).unwrap(),
        kv_last_svn_rev: db.get_state(&format!("last_svn_rev_{repo_id}")).unwrap(),
        kv_last_git_hash: db.get_state("last_git_hash").unwrap(),
    }
}

fn assert_watermarks_unchanged(before: &WatermarkSnapshot, after: &WatermarkSnapshot) {
    assert_eq!(after.last_svn_rev, before.last_svn_rev);
    assert_eq!(after.last_git_sha, before.last_git_sha);
    assert_eq!(after.consecutive_errors, before.consecutive_errors);
    assert_eq!(after.sync_status, before.sync_status);
    assert_eq!(after.kv_last_git_sha, before.kv_last_git_sha);
    assert_eq!(after.kv_last_svn_rev, before.kv_last_svn_rev);
    assert_eq!(after.kv_last_git_hash, before.kv_last_git_hash);
}

fn seed_pinned_watermarks(db: &Database, repo_id: &str, git_sha: &str) {
    db.set_state(&format!("last_git_sha_{repo_id}"), git_sha)
        .unwrap();
    db.set_state(&format!("last_svn_rev_{repo_id}"), "4")
        .unwrap();
    db.set_state("last_git_hash", git_sha).unwrap();
}

fn linear_pending_history(tmp: &Path) -> (PathBuf, String, String, String, Vec<String>) {
    let work = tmp.join("pending-work");
    std::fs::create_dir_all(&work).unwrap();
    git_cli(&work, &["init", "-b", "main"]);
    git_cli(&work, &["config", "user.email", "fixture@example.invalid"]);
    git_cli(&work, &["config", "user.name", "Fixture Developer"]);
    std::fs::write(work.join("file.txt"), "A\n").unwrap();
    git_cli(&work, &["add", "file.txt"]);
    git_cli(&work, &["commit", "-m", "commit A"]);
    let sha_a = git_sha(&work, "HEAD");
    std::fs::write(work.join("file.txt"), "B\n").unwrap();
    git_cli(&work, &["commit", "-am", "commit B"]);
    let sha_b = git_sha(&work, "HEAD");
    std::fs::write(work.join("file.txt"), "C\n").unwrap();
    git_cli(&work, &["commit", "-am", "commit C"]);
    let sha_c = git_sha(&work, "HEAD");
    let pending = pending_after(&work, &sha_a);
    assert_eq!(pending, vec![sha_b.clone(), sha_c.clone()]);
    (work, sha_a, sha_b, sha_c, pending)
}

struct ExactSkipFixture {
    addr: std::net::SocketAddr,
    state: Arc<AppState>,
    server: tokio::task::JoinHandle<()>,
    repo_id: String,
    workdir: PathBuf,
    sha_a: String,
    sha_b: String,
    sha_c: String,
}

async fn exact_skip_fixture(tmp: &TempDir) -> ExactSkipFixture {
    let (work, sha_a, sha_b, sha_c, _pending) = linear_pending_history(tmp.path());

    let toml = format!(
        r#"
[daemon]
data_dir = "{}"

[svn]
url = "file:///nonexistent/svn"
username = "fixture"
password_env = ""

[github]
repo = "test/repo"
token_env = ""
"#,
        tmp.path().display().to_string().replace('\\', "/")
    );
    let mut config: AppConfig = toml::from_str(&toml).unwrap();
    config.web.admin_password = Some("test-admin-pass".into());
    config.github.default_branch = "main".into();

    let db_path = tmp.path().join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let engine_db = Database::new(&db_path).unwrap();
    engine_db.initialize().unwrap();

    let engine = Arc::new(SyncEngine::new(
        config.clone(),
        engine_db,
        SvnClient::new("file:///nonexistent/svn", "fixture", ""),
        GitClient::new(&work).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    ));
    let (sync_tx, _) = tokio::sync::mpsc::channel(1);
    let (ws_tx, _) = tokio::sync::broadcast::channel(8);
    let state = Arc::new(AppState {
        db,
        sync_engine: engine,
        config,
        sync_trigger: sync_tx,
        ws_broadcast: ws_tx,
        sessions: tokio::sync::RwLock::new(HashMap::new()),
        import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        config_path: tmp.path().join("config.toml"),
        prev_net_snapshot: std::sync::Mutex::new(None),
        repo_import_progress: tokio::sync::RwLock::new(HashMap::new()),
        login_attempts: std::sync::Mutex::new(HashMap::new()),
        import_handles: tokio::sync::Mutex::new(Vec::new()),
    });
    state.sessions.write().await.insert(
        TEST_TOKEN.into(),
        chrono::Utc::now() + chrono::Duration::hours(1),
    );

    let repo_id = "skip-pair".to_string();
    state
        .db
        .insert_repository(&fixture_repo(&repo_id, &sha_a))
        .unwrap();
    seed_pinned_watermarks(&state.db, &repo_id, &sha_a);

    let repo_git = tmp.path().join("repos").join(&repo_id).join("git-repo");
    std::fs::create_dir_all(repo_git.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&repo_git).unwrap();
    git_cli(&work, &["clone", ".", &repo_git.to_string_lossy()]);

    let app = Router::new()
        .merge(api::repos::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    ExactSkipFixture {
        addr,
        state,
        server,
        repo_id,
        workdir: work,
        sha_a,
        sha_b,
        sha_c,
    }
}

async fn post_skip(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    repo_id: &str,
    body: &SkipCommitRequest,
) -> (reqwest::StatusCode, serde_json::Value) {
    let response = client
        .post(format!("http://{addr}/api/repos/{repo_id}/skip-commit"))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::json!({}));
    (status, body)
}

fn skip_body(
    pinned: &str,
    selected: &[&str],
    remote_tip: &str,
    bridge_tip: Option<&str>,
) -> SkipCommitRequest {
    SkipCommitRequest {
        pinned_cursor: pinned.into(),
        selected_commits: selected.iter().map(|s| (*s).into()).collect(),
        expected_remote_tip: remote_tip.into(),
        expected_bridge_tip: bridge_tip.map(str::to_string),
        reason: "test_exact_skip".into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skip_commit_refuses_tip_token_mismatch_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let fixture = exact_skip_fixture(&tmp).await;
    let before = snapshot_watermarks(&fixture.state.db, &fixture.repo_id);
    let client = authed_client();

    let (status, body) = post_skip(
        &client,
        fixture.addr,
        &fixture.repo_id,
        &skip_body(&fixture.sha_a, &[&fixture.sha_b], "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef", Some(&fixture.sha_c)),
    )
    .await;

    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains(skip_reason::TIP_MISMATCH),
        "expected tip mismatch refuse: {body}"
    );
    let after = snapshot_watermarks(&fixture.state.db, &fixture.repo_id);
    assert_watermarks_unchanged(&before, &after);
    assert!(!is_commit_excluded(&fixture.state.db, &fixture.repo_id, &fixture.sha_b).unwrap());
    fixture.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skip_commit_accepts_selected_commit_with_exclusion_receipt() {
    let tmp = TempDir::new().unwrap();
    let fixture = exact_skip_fixture(&tmp).await;
    let client = authed_client();

    let (status, body) = post_skip(
        &client,
        fixture.addr,
        &fixture.repo_id,
        &skip_body(
            &fixture.sha_a,
            &[&fixture.sha_b],
            &fixture.sha_c,
            Some(&fixture.sha_c),
        ),
    )
    .await;

    assert_eq!(status, reqwest::StatusCode::OK, "skip failed: {body}");
    assert_eq!(body["new_sha"].as_str(), Some(fixture.sha_b.as_str()));
    assert_eq!(
        body["remaining_pending"].as_array().map(|a| a.len()),
        Some(1)
    );
    assert_eq!(
        body["remaining_pending"][0].as_str(),
        Some(fixture.sha_c.as_str())
    );

    let after = snapshot_watermarks(&fixture.state.db, &fixture.repo_id);
    assert_eq!(after.last_git_sha, fixture.sha_b);
    assert_eq!(after.kv_last_git_sha.as_deref(), Some(fixture.sha_b.as_str()));
    assert!(is_commit_excluded(&fixture.state.db, &fixture.repo_id, &fixture.sha_b).unwrap());
    let receipt = fixture
        .state
        .db
        .get_state(&exclusion_receipt_key(&fixture.repo_id, &fixture.sha_b))
        .unwrap()
        .expect("exclusion receipt must be persisted");
    assert!(receipt.contains("operator_exact_skip") || receipt.contains("test_exact_skip"));

    let skip_audits = fixture
        .state
        .db
        .list_audit_log_by_action("skip_commit", 10)
        .unwrap();
    assert_eq!(skip_audits.len(), 1);

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "RS_C04_EXACT_SKIP_SELECTED_COMMIT",
            "excluded": fixture.sha_b,
            "frontier": fixture.sha_b,
            "remaining_pending": fixture.sha_c,
            "receipt_persisted": true
        })
    );
    fixture.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skip_commit_then_concurrent_tick_leaves_remaining_pending_intact() {
    let tmp = TempDir::new().unwrap();
    let fixture = exact_skip_fixture(&tmp).await;

    let db_path = tmp.path().join("reposync.db");
    let mut engine = SyncEngine::new(
        fixture.state.config.clone(),
        Database::new(&db_path).unwrap(),
        SvnClient::new("file:///nonexistent/svn", "fixture", ""),
        GitClient::new(&fixture.workdir).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    );
    engine.set_repo_id(fixture.repo_id.clone());

    let client = authed_client();
    let addr = fixture.addr;
    let repo_id = fixture.repo_id.clone();
    let sha_a = fixture.sha_a.clone();
    let sha_b = fixture.sha_b.clone();
    let sha_c = fixture.sha_c.clone();
    let skip = tokio::spawn(async move {
        post_skip(
            &client,
            addr,
            &repo_id,
            &skip_body(&sha_a, &[&sha_b], &sha_c, Some(&sha_c)),
        )
        .await
    });
    let tick = tokio::spawn(async move { engine.run_sync_cycle().await });

    let (status, _body) = skip.await.unwrap();
    assert_eq!(status, reqwest::StatusCode::OK);
    let _ = tick.await.unwrap();

    let after = snapshot_watermarks(&fixture.state.db, &fixture.repo_id);
    assert_eq!(after.last_git_sha, fixture.sha_b);
    let pending_after_tick = pending_after(&fixture.workdir, &after.last_git_sha);
    assert_eq!(pending_after_tick, vec![fixture.sha_c.clone()]);
    fixture.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_after_skip_does_not_replay_excluded_commit() {
    let tmp = TempDir::new().unwrap();
    let fixture = exact_skip_fixture(&tmp).await;
    let client = authed_client();
    let db_path = tmp.path().join("reposync.db");

    let (status, _) = post_skip(
        &client,
        fixture.addr,
        &fixture.repo_id,
        &skip_body(
            &fixture.sha_a,
            &[&fixture.sha_b],
            &fixture.sha_c,
            Some(&fixture.sha_c),
        ),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);

    fixture.server.abort();

    let reopened = Database::new(&db_path).unwrap();
    reopened.initialize().unwrap();
    assert!(is_commit_excluded(&reopened, &fixture.repo_id, &fixture.sha_b).unwrap());
    let repo = reopened.get_repository(&fixture.repo_id).unwrap().unwrap();
    assert_eq!(repo.last_git_sha, fixture.sha_b);
    let pending = pending_after(&fixture.workdir, &repo.last_git_sha);
    assert_eq!(pending, vec![fixture.sha_c.clone()]);
    assert!(!pending.contains(&fixture.sha_b));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skip_commit_refuses_cursor_mismatch_without_checkpoint_change() {
    let tmp = TempDir::new().unwrap();
    let fixture = exact_skip_fixture(&tmp).await;
    let before = snapshot_watermarks(&fixture.state.db, &fixture.repo_id);
    let client = authed_client();

    let wrong_cursor = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let (status, body) = post_skip(
        &client,
        fixture.addr,
        &fixture.repo_id,
        &skip_body(
            wrong_cursor,
            &[&fixture.sha_b],
            &fixture.sha_c,
            Some(&fixture.sha_c),
        ),
    )
    .await;

    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains(skip_reason::CURSOR_MISMATCH)
    );
    let after = snapshot_watermarks(&fixture.state.db, &fixture.repo_id);
    assert_watermarks_unchanged(&before, &after);
    fixture.server.abort();
}

#[test]
fn repo_detail_ui_binds_skip_to_observed_tips_and_selected_commits() {
    let src = include_str!("../../../web-ui/src/pages/RepoDetail.tsx");
    assert!(
        !src.contains("skip the failing commit"),
        "UI must not describe HEAD adoption as skipping one failing commit"
    );
    assert!(
        src.contains("api.skipCommit"),
        "Skip Commit must invoke exact skip API with observed tips"
    );
    assert!(
        src.contains("data-testid=\"skip-commit-button\""),
        "Skip Commit control must remain visible"
    );
    assert!(
        src.contains("does not adopt live HEAD"),
        "UI copy must state skip does not adopt live HEAD"
    );
    assert!(
        src.contains("getSkipCommitContext"),
        "UI must bind skip to observed tips via context endpoint"
    );
    assert!(
        src.contains("data-testid=\"skip-commit-modal\""),
        "skip modal must exist for selected-commit confirmation"
    );
    assert!(
        !src.contains("Skip-to-HEAD is disabled until exact per-commit skip disposition exists"),
        "disabled interim copy must be replaced by exact skip UX"
    );
}
