//! #114 real router, session, SQLite and two-repository acknowledgement regressions.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

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

async fn ack_fixture(
    repos: &[(&str, &str)],
) -> (
    SocketAddr,
    Arc<AppState>,
    tokio::task::JoinHandle<()>,
    TempDir,
) {
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
        scheduler_service: std::sync::Mutex::new(reposync_web::SchedulerServiceState::running()),
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
    (addr, state, server, tmp)
}

fn add_session(state: &AppState, token: &str, role: &str, enabled: bool, expired: bool) {
    let now = chrono::Utc::now().to_rfc3339();
    state
        .db
        .insert_user(&reposync_core::models::User {
            id: token.into(),
            username: token.into(),
            display_name: token.into(),
            email: "fixture@example.invalid".into(),
            password_hash: "unused".into(),
            role: role.into(),
            enabled,
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .unwrap();
    state
        .db
        .insert_session(&reposync_core::models::Session {
            token: token.into(),
            user_id: token.into(),
            created_at: now,
            expires_at: (chrono::Utc::now()
                + chrono::Duration::hours(if expired { -1 } else { 1 }))
            .to_rfc3339(),
        })
        .unwrap();
}

fn failure(state: &AppState, repo: Option<&str>) {
    state
        .db
        .insert_audit_log_with_repo(reposync_core::db::queries::AuditLogInput {
            action: "sync_failed",
            direction: Some("svn_to_git"),
            svn_rev: Some(42),
            git_sha: Some("exact-hash"),
            author: Some("original-author"),
            details: Some("operation-id=123; original failure"),
            success: false,
            repo_id: repo,
        })
        .unwrap();
}

async fn observation(client: &reqwest::Client, addr: SocketAddr, query: &str) -> serde_json::Value {
    let response = client
        .get(format!("http://{addr}/api/status{query}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json::<serde_json::Value>().await.unwrap()["error_observation"].clone()
}

#[tokio::test]
async fn rs114_http_two_repository_scope_idempotence_and_preserved_audit() {
    let (addr, state, server, _tmp) = ack_fixture(&[("a", "idle"), ("b", "idle")]).await;
    add_session(&state, TEST_TOKEN, "admin", true, false);
    let client = authed_client();
    failure(&state, Some("a"));
    failure(&state, Some("b"));
    failure(&state, None);
    let originals = state
        .db
        .list_audit_log(100, 0)
        .unwrap()
        .iter()
        .map(|e| format!("{e:?}"))
        .collect::<Vec<_>>();
    let observed = observation(&client, addr, "?repo_id=a").await;
    let request =
        serde_json::json!({"scope":{"kind":"repository","repo_id":"a"},"observation":observed});
    let response = client
        .post(format!("http://{addr}/api/status/acknowledge-errors"))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["acknowledged"],
        1
    );
    assert_eq!(state.db.count_errors_for_repo("a").unwrap(), 0);
    assert_eq!(state.db.count_errors_for_repo("b").unwrap(), 1);
    assert_eq!(state.db.count_errors().unwrap(), 2);
    let failures = state
        .db
        .list_audit_log(100, 0)
        .unwrap()
        .into_iter()
        .filter(|e| !e.success)
        .map(|e| format!("{e:?}"))
        .collect::<Vec<_>>();
    assert_eq!(originals, failures);
    let receipts = state
        .db
        .list_audit_log_by_action("errors_acknowledged", 100)
        .unwrap();
    assert_eq!(receipts[0].author.as_deref(), Some(TEST_TOKEN));
    assert_eq!(receipts[0].repo_id.as_deref(), Some("a"));
    failure(&state, Some("a"));
    let retry = client
        .post(format!("http://{addr}/api/status/acknowledge-errors"))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(retry.status(), 200);
    assert_eq!(
        retry.json::<serde_json::Value>().await.unwrap()["acknowledged"],
        0
    );
    assert_eq!(state.db.count_errors_for_repo("a").unwrap(), 1);
    assert_eq!(
        state
            .db
            .list_audit_log_by_action("errors_acknowledged", 100)
            .unwrap()
            .len(),
        1
    );
    let all = observation(&client, addr, "").await;
    let response = client
        .post(format!("http://{addr}/api/status/acknowledge-errors"))
        .json(&serde_json::json!({"scope":{"kind":"all"},"observation":all}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["acknowledged"],
        3
    );
    assert_eq!(state.db.count_errors().unwrap(), 0);
    assert_eq!(
        state
            .db
            .list_audit_log(100, 0)
            .unwrap()
            .iter()
            .filter(|e| !e.success)
            .count(),
        4
    );
    server.abort();
}

#[tokio::test]
async fn rs114_http_authentication_authorization_and_explicit_scope() {
    let (addr, state, server, _tmp) = ack_fixture(&[("a", "idle")]).await;
    add_session(&state, TEST_TOKEN, "admin", true, false);
    add_session(&state, "viewer", "user", true, false);
    add_session(&state, "expired", "admin", true, true);
    add_session(&state, "disabled", "admin", false, false);
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/api/status/acknowledge-errors");
    for token in [None, Some("invalid"), Some("expired"), Some("disabled")] {
        let mut r = client.post(&url).json(&serde_json::json!({}));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        assert_eq!(r.send().await.unwrap().status(), 401);
    }
    assert_eq!(
        client
            .post(&url)
            .bearer_auth("viewer")
            .body("malformed")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    failure(&state, Some("a"));
    let admin = authed_client();
    let observed = observation(&admin, addr, "?repo_id=a").await;
    for scope in [
        serde_json::json!(null),
        serde_json::json!({}),
        serde_json::json!({"kind":"purge"}),
        serde_json::json!({"kind":"all","purge":true}),
    ] {
        assert_eq!(
            admin
                .post(&url)
                .json(&serde_json::json!({"scope":scope,"observation":observed}))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    assert_eq!(admin.post(&url).json(&serde_json::json!({"scope":{"kind":"repository","repo_id":"missing"},"observation":observed})).send().await.unwrap().status(),404);
    for legacy in [
        "/api/status/reset-errors",
        "/api/status/reset-errors?repo_id=a",
    ] {
        assert_eq!(
            admin
                .post(format!("http://{addr}{legacy}"))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    assert_eq!(admin.delete(&url).send().await.unwrap().status(), 405);
    assert_eq!(state.db.count_errors_for_repo("a").unwrap(), 1);
    // A session's stored role is not a lasting grant: current DB role controls each request.
    state
        .db
        .conn()
        .execute("UPDATE users SET role='user' WHERE id=?1", [TEST_TOKEN])
        .unwrap();
    assert_eq!(
        admin
            .post(&url)
            .json(&serde_json::json!({"scope":{"kind":"all"},"observation":observed}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    server.abort();
}

#[tokio::test]
async fn rs114_http_write_failure_is_visible_and_does_not_dismiss_errors() {
    let (addr, state, server, _tmp) = ack_fixture(&[("a", "idle")]).await;
    // The existing authenticated legacy administrator path remains supported.
    failure(&state, Some("a"));
    let client = authed_client();
    let observed = observation(&client, addr, "?repo_id=a").await;
    state.db.conn().execute_batch("CREATE TRIGGER reject_ack BEFORE INSERT ON audit_log WHEN NEW.action='errors_acknowledged' BEGIN SELECT RAISE(ABORT,'private storage failure'); END;").unwrap();
    let response=client.post(format!("http://{addr}/api/status/acknowledge-errors")).json(&serde_json::json!({"scope":{"kind":"repository","repo_id":"a"},"observation":observed})).send().await.unwrap();
    assert_eq!(response.status(), 500);
    let text = response.text().await.unwrap();
    assert!(!text.contains("private storage failure"));
    assert_eq!(state.db.count_errors_for_repo("a").unwrap(), 1);
    assert!(state
        .db
        .list_audit_log_by_action("errors_acknowledged", 100)
        .unwrap()
        .is_empty());
    state
        .db
        .conn()
        .execute_batch("DROP TRIGGER reject_ack")
        .unwrap();
    let retry=client.post(format!("http://{addr}/api/status/acknowledge-errors")).json(&serde_json::json!({"scope":{"kind":"repository","repo_id":"a"},"observation":observed})).send().await.unwrap();
    assert_eq!(retry.status(), 200);
    assert_eq!(state.db.count_errors().unwrap(), 0);
    server.abort();
}
