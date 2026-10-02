//! Admin skip-commit must refuse live HEAD adoption (Astra RS-C04 / RSUI-03).
//!
//! The route must not advance watermarks or checkpoints, must not write Git or
//! SVN, and a concurrent tick after the refuse must leave pending commits intact.

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
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_web::api;
use reposync_web::AppState;
use tempfile::TempDir;

const TEST_TOKEN: &str = "test-session-token-skip-commit-refuse";
const PINNED_CURSOR: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const LIVE_HEAD: &str = "cccccccccccccccccccccccccccccccccccccccc";

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

async fn skip_commit_server(
    last_git_sha: &str,
) -> (
    std::net::SocketAddr,
    Arc<AppState>,
    tokio::task::JoinHandle<()>,
    TempDir,
    String,
    PathBuf,
) {
    let tmp = TempDir::new().unwrap();
    let git_repo = tmp.path().join("git-repo");
    git2::Repository::init(&git_repo).unwrap();

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

    let db_path = tmp.path().join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let engine_db = Database::new(&db_path).unwrap();
    engine_db.initialize().unwrap();

    let engine = Arc::new(SyncEngine::new(
        config.clone(),
        engine_db,
        SvnClient::new("file:///nonexistent/svn", "fixture", ""),
        GitClient::new(&git_repo).unwrap(),
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

    let repo = fixture_repo("skip-pair", last_git_sha);
    state.db.insert_repository(&repo).unwrap();
    seed_pinned_watermarks(&state.db, &repo.id, last_git_sha);

    let app = Router::new()
        .merge(api::repos::routes())
        .merge(api::status::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, state, server, tmp, repo.id, git_repo)
}

fn linear_pending_history(tmp: &Path) -> (PathBuf, String, Vec<String>) {
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
    std::fs::write(work.join("file.txt"), "C\n").unwrap();
    git_cli(&work, &["commit", "-am", "commit C"]);
    let pending = pending_after(&work, &sha_a);
    assert_eq!(pending.len(), 2, "A→B→C must leave B and C pending");
    (work, sha_a, pending)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skip_commit_refuses_without_watermark_or_checkpoint_change() {
    let (addr, state, server, _tmp, id, _git) = skip_commit_server(PINNED_CURSOR).await;
    let before = snapshot_watermarks(&state.db, &id);
    assert_eq!(before.last_git_sha, PINNED_CURSOR);
    assert_eq!(before.sync_status, "error_paused");
    assert_eq!(before.consecutive_errors, 3);

    let client = authed_client();
    let response = client
        .post(format!("http://{addr}/api/repos/{id}/skip-commit"))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        status,
        reqwest::StatusCode::CONFLICT,
        "skip-commit must refuse with 409, got {status}: {body}"
    );
    let err = body["error"].as_str().unwrap_or("");
    assert!(
        err.contains("skip_commit_disabled"),
        "stable reason missing: {body}"
    );
    assert!(
        err.contains("pending work is preserved"),
        "pending-work guarantee missing: {body}"
    );

    let after = snapshot_watermarks(&state.db, &id);
    assert_watermarks_unchanged(&before, &after);
    assert_ne!(
        after.last_git_sha, LIVE_HEAD,
        "must not adopt a live HEAD SHA"
    );
    let skip_audits = state
        .db
        .list_audit_log_by_action("skip_commit", 10)
        .unwrap();
    assert!(
        skip_audits.is_empty(),
        "refuse must not record a skip_commit mutation: {skip_audits:?}"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "RS_C04_SKIP_COMMIT_REFUSED",
            "status": 409,
            "reason": "skip_commit_disabled",
            "watermark_unchanged": true,
            "checkpoint_unchanged": true,
            "pending_git_sha": PINNED_CURSOR
        })
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skip_commit_refuse_then_concurrent_tick_leaves_pending_intact() {
    let tmp = TempDir::new().unwrap();
    let (work, sha_a, pending_before) = linear_pending_history(tmp.path());

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

    let mut engine = SyncEngine::new(
        config.clone(),
        engine_db,
        SvnClient::new("file:///nonexistent/svn", "fixture", ""),
        GitClient::new(&work).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    );
    engine.set_repo_id("skip-pair".into());

    let (sync_tx, _) = tokio::sync::mpsc::channel(1);
    let (ws_tx, _) = tokio::sync::broadcast::channel(8);
    let state = Arc::new(AppState {
        db,
        sync_engine: Arc::new(SyncEngine::new(
            config.clone(),
            Database::new(&db_path).unwrap(),
            SvnClient::new("file:///nonexistent/svn", "fixture", ""),
            GitClient::new(&work).unwrap(),
            Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
        )),
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
    state
        .db
        .insert_repository(&fixture_repo("skip-pair", &sha_a))
        .unwrap();
    seed_pinned_watermarks(&state.db, "skip-pair", &sha_a);

    let app = Router::new()
        .merge(api::repos::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let before = snapshot_watermarks(&state.db, "skip-pair");
    let client = authed_client();
    let refuse = tokio::spawn(async move {
        client
            .post(format!("http://{addr}/api/repos/skip-pair/skip-commit"))
            .send()
            .await
            .unwrap()
    });
    let tick = tokio::spawn(async move { engine.run_sync_cycle().await });

    let refuse_resp = refuse.await.unwrap();
    assert_eq!(refuse_resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = refuse_resp.json().await.unwrap();
    assert!(body["error"]
        .as_str()
        .unwrap_or("")
        .contains("skip_commit_disabled"));
    let _ = tick.await.unwrap();

    let after = snapshot_watermarks(&state.db, "skip-pair");
    assert_eq!(after.last_git_sha, sha_a, "cursor must stay pinned at A");
    assert_eq!(after.last_svn_rev, before.last_svn_rev);
    assert_eq!(after.kv_last_git_sha.as_deref(), Some(sha_a.as_str()));
    let pending_after_tick = pending_after(&work, &after.last_git_sha);
    assert_eq!(
        pending_after_tick, pending_before,
        "B and C must remain pending after refuse + tick"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "RS_C04_SKIP_COMMIT_REFUSE_THEN_TICK",
            "reason": "skip_commit_disabled",
            "pinned_cursor": sha_a,
            "pending_after": pending_after_tick,
            "pending_intact": true
        })
    );
    server.abort();
}

#[test]
fn repo_detail_ui_disables_skip_to_head_and_does_not_claim_one_commit_skip() {
    let src = include_str!("../../../web-ui/src/pages/RepoDetail.tsx");
    assert!(
        !src.contains("skip the failing commit"),
        "UI must not describe HEAD adoption as skipping one failing commit"
    );
    assert!(
        !src.contains("api.skipCommit"),
        "Skip Commit must not invoke the unsafe shortcut"
    );
    assert!(
        src.contains("data-testid=\"skip-commit-button\""),
        "Skip Commit control must remain visible for the disabled+explanation pattern"
    );
    assert!(
        src.contains("Skip-to-HEAD is disabled until exact per-commit skip disposition exists"),
        "disabled Skip Commit must explain why it is unavailable"
    );
    assert!(
        src.contains("pending work is preserved"),
        "UI copy must state that pending work is preserved"
    );
    let button_idx = src
        .find("data-testid=\"skip-commit-button\"")
        .expect("skip-commit button");
    let window = &src[button_idx.saturating_sub(200)..button_idx + 400];
    assert!(
        window.contains("disabled"),
        "Skip Commit button must be disabled: {window}"
    );
}
