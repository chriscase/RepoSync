//! Unscoped `/api/status` aggregation for team-mode managed repositories.

use std::collections::HashMap;
use std::net::SocketAddr;
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

const TEST_TOKEN: &str = "test-session-token-unscoped-status";

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

fn fixture_repo(id: &str, sync_status: &str) -> Repository {
    let now = chrono::Utc::now().to_rfc3339();
    Repository {
        id: id.into(),
        name: id.into(),
        svn_url: "file:///nonexistent/svn".into(),
        svn_branch: "trunk".into(),
        svn_username: "fixture".into(),
        git_provider: "github".into(),
        git_api_url: "http://127.0.0.1:1".into(),
        git_repo: "test/repo".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 60,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: sync_status.into(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    }
}

async fn status_fixture(
    repos: &[(&str, &str)],
) -> (SocketAddr, Arc<AppState>, tokio::task::JoinHandle<()>) {
    let tmp = TempDir::new().unwrap();
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
    db.set_state("sync_state", "idle").unwrap();

    for (id, status) in repos {
        db.insert_repository(&fixture_repo(id, status)).unwrap();
    }

    let engine_db = Database::new(&db_path).unwrap();
    engine_db.initialize().unwrap();
    let git_dir = tmp.path().join("git");
    std::process::Command::new("git")
        .args(["init", git_dir.to_str().unwrap()])
        .status()
        .unwrap();
    let engine = Arc::new(SyncEngine::new(
        config.clone(),
        engine_db,
        SvnClient::new("file:///nonexistent/svn", "fixture", ""),
        GitClient::new(&git_dir).unwrap(),
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

    let app = Router::new()
        .merge(api::status::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, state, server)
}

#[tokio::test]
async fn candidate_rs05_web_unscoped_status_aggregates_worst_repo_state() {
    let (addr, _state, server) =
        status_fixture(&[("healthy", "idle"), ("blocked", "reconciliation_required")]).await;
    let client = authed_client();
    let response = client
        .get(format!("http://{addr}/api/status"))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(status.is_success(), "{body}");
    assert_eq!(
        body["state"], "reconciliation_required",
        "unscoped /api/status must aggregate managed repos, not stale global idle"
    );

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS05_WEB_UNSCOPED_STATUS_AGGREGATION",
            "state":body["state"],
            "global_sync_state":"idle"
        })
    );

    server.abort();
}

#[tokio::test]
async fn candidate_rs05_web_unscoped_status_omits_global_git_tip_with_multiple_repos() {
    let (addr, state, server) = status_fixture(&[("alpha", "idle"), ("beta", "idle")]).await;
    let stale_global = "dddddddddddddddddddddddddddddddddddddddd";
    let alpha_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let beta_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    state.db.set_state("last_git_hash", stale_global).unwrap();
    state
        .db
        .conn()
        .execute(
            "INSERT INTO commit_map (git_sha, svn_rev, direction, synced_at) VALUES (?1, 9, 'svn_to_git', '2020-01-01T00:00:00Z')",
            ["eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"],
        )
        .unwrap();
    state
        .db
        .conn()
        .execute(
            "UPDATE repositories SET last_git_sha = ?1 WHERE id = 'alpha'",
            [alpha_sha],
        )
        .unwrap();
    state
        .db
        .conn()
        .execute(
            "UPDATE repositories SET last_git_sha = ?1 WHERE id = 'beta'",
            [beta_sha],
        )
        .unwrap();

    let client = authed_client();
    let response = client
        .get(format!("http://{addr}/api/status"))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(status.is_success(), "{body}");
    assert!(
        body["last_git_hash"].is_null(),
        "unscoped /api/status must not report a misleading global git tip across managed repos: {body}"
    );

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS05_WEB_UNSCOPED_STATUS_NO_GLOBAL_GIT_TIP",
            "last_git_hash":body["last_git_hash"],
            "stale_global":stale_global,
            "alpha_sha":alpha_sha,
            "beta_sha":beta_sha
        })
    );

    server.abort();
}

#[tokio::test]
async fn candidate_rs05_web_unscoped_status_reports_legacy_git_tip() {
    let (addr, state, server) = status_fixture(&[]).await;
    let legacy_tip = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    state.db.set_state("last_git_hash", legacy_tip).unwrap();

    let client = authed_client();
    let response = client
        .get(format!("http://{addr}/api/status"))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    assert!(status.is_success(), "{body}");
    assert_eq!(
        body["last_git_hash"].as_str(),
        Some(legacy_tip),
        "legacy single-repo installs must keep reporting the global git tip"
    );

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS05_LEGACY_UNSCOPED_STATUS_GIT_TIP",
            "last_git_hash":body["last_git_hash"]
        })
    );

    server.abort();
}

#[tokio::test]
async fn candidate_rs05_web_unscoped_status_reports_error_paused() {
    let (addr, _state, server) =
        status_fixture(&[("paused", "error_paused"), ("healthy", "idle")]).await;
    let client = authed_client();
    let response = client
        .get(format!("http://{addr}/api/status"))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        body["state"], "error_paused",
        "error_paused must round-trip through unscoped /api/status"
    );

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS05_ERROR_PAUSED_STATUS_ROUNDTRIP",
            "state":body["state"]
        })
    );

    server.abort();
}
