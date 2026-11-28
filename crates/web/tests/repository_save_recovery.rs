//! #115 real HTTP/SQLite repository-save and credential recovery regressions.

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

async fn save_fixture(
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
        .merge(api::repos::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state, server, tmp)
}

fn create_body(request: &str) -> serde_json::Value {
    serde_json::json!({"name":"Synthetic recovery","svn_url":"svn://127.0.0.1/nonexistent", "svn_branch":"trunk",
        "git_repo":"test/repo","git_branch":"main","save_request_id":request})
}
async fn create(
    client: &reqwest::Client,
    addr: SocketAddr,
    body: &serde_json::Value,
) -> serde_json::Value {
    let response = client
        .post(format!("http://{addr}/api/repos"))
        .json(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

#[tokio::test]
async fn rs115_http_partial_credential_failure_keeps_created_id_and_retries_atomically() {
    let (addr, state, server, _tmp) = save_fixture(&[]).await;
    let client = authed_client();
    let request = uuid::Uuid::new_v4().to_string();
    let body = create_body(&request);
    let created = create(&client, addr, &body).await;
    let id = created["id"].as_str().unwrap();
    let credential_id = uuid::Uuid::new_v4().to_string();
    let credentials = serde_json::json!({"save_request_id":credential_id,
        "expected_updated_at":created["save_receipt"]["configuration_updated_at"],"svn_password":"synthetic-private-svn","git_token":"synthetic-private-git"});
    state.db.conn().execute_batch("CREATE TRIGGER reject_git BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'secret_git_token_%' BEGIN SELECT RAISE(ABORT,'private storage failure'); END;").unwrap();
    let response = client
        .post(format!("http://{addr}/api/repos/{id}/credentials"))
        .json(&credentials)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 500);
    let message = response.text().await.unwrap();
    assert!(!message.contains("synthetic-private"));
    assert!(!message.contains("private storage failure"));
    assert!(state.db.get_repository(id).unwrap().is_some());
    assert_eq!(state.db.list_repositories().unwrap().len(), 1);
    assert!(state
        .db
        .get_state(&format!("secret_svn_password_{id}"))
        .unwrap()
        .is_none());
    assert!(state
        .db
        .get_state(&format!("secret_git_token_{id}"))
        .unwrap()
        .is_none());
    let missing = client
        .get(format!(
            "http://{addr}/api/repos/{id}/save-requests/{credential_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    state
        .db
        .conn()
        .execute_batch("DROP TRIGGER reject_git")
        .unwrap();
    let repeated = create(&client, addr, &body).await;
    assert_eq!(repeated["id"], created["id"]);
    let response = client
        .post(format!("http://{addr}/api/repos/{id}/credentials"))
        .json(&credentials)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let result: serde_json::Value = response.json().await.unwrap();
    assert_eq!(result["save_receipt"]["request_id"], credential_id);
    assert_eq!(
        state
            .db
            .get_state(&format!("secret_svn_password_{id}"))
            .unwrap()
            .as_deref(),
        Some("synthetic-private-svn")
    );
    assert_eq!(
        state
            .db
            .get_state(&format!("secret_git_token_{id}"))
            .unwrap()
            .as_deref(),
        Some("synthetic-private-git")
    );
    assert_eq!(state.db.list_repositories().unwrap().len(), 1);
    server.abort();
}

#[tokio::test]
async fn rs115_http_lost_create_and_credential_responses_do_not_duplicate_or_replay_newer_values() {
    let (addr, state, server, _tmp) = save_fixture(&[]).await;
    let client = authed_client();
    let request = uuid::Uuid::new_v4().to_string();
    let body = create_body(&request);
    let first = create(&client, addr, &body).await;
    let id = first["id"].as_str().unwrap();
    let (a, b) = tokio::join!(create(&client, addr, &body), create(&client, addr, &body));
    assert_eq!(a["id"], first["id"]);
    assert_eq!(b["id"], first["id"]);
    assert_eq!(state.db.list_repositories().unwrap().len(), 1);
    let mut changed = body.clone();
    changed["name"] = "Different content".into();
    assert_eq!(
        client
            .post(format!("http://{addr}/api/repos"))
            .json(&changed)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let config_lookup = client
        .get(format!(
            "http://{addr}/api/repository-save-requests/{request}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(config_lookup.status(), 200);
    assert_eq!(
        config_lookup.json::<serde_json::Value>().await.unwrap()["repo_id"],
        id
    );
    let old_id = uuid::Uuid::new_v4().to_string();
    let old = serde_json::json!({"save_request_id":old_id,"svn_password":"old-secret-秘密"});
    let endpoint = format!("http://{addr}/api/repos/{id}/credentials");
    assert_eq!(
        client
            .post(&endpoint)
            .json(&old)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let new = serde_json::json!({"save_request_id":uuid::Uuid::new_v4().to_string(),"svn_password":"newer-secret"});
    assert_eq!(
        client
            .post(&endpoint)
            .json(&new)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(&endpoint)
            .json(&old)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        state
            .db
            .get_state(&format!("secret_svn_password_{id}"))
            .unwrap()
            .as_deref(),
        Some("newer-secret")
    );
    let mut mismatch = old.clone();
    mismatch["svn_password"] = "changed-secret".into();
    assert_eq!(
        client
            .post(&endpoint)
            .json(&mismatch)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let receipt = client
        .get(format!(
            "http://{addr}/api/repos/{id}/save-requests/{old_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(receipt.status(), 200);
    let text = receipt.text().await.unwrap();
    assert!(!text.contains("old-secret"));
    assert!(!text.contains("payload_mac"));
    assert!(!text.contains("scope_uuid"));
    server.abort();
}

#[tokio::test]
async fn rs115_http_update_receipt_cas_and_explicit_policy_clear_preserve_runtime_progress() {
    let (addr, state, server, _tmp) = save_fixture(&[]).await;
    let client = authed_client();
    let created = create(
        &client,
        addr,
        &create_body(&uuid::Uuid::new_v4().to_string()),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let mut model = state.db.get_repository(id).unwrap().unwrap();
    model.allowed_paths = Some("[\"old\"]".into());
    model.blocked_patterns = Some("[\"old\"]".into());
    state.db.update_repository(&model).unwrap();
    state.db.conn().execute("UPDATE repositories SET last_svn_rev=99,last_git_sha='live',total_syncs=8,total_errors=4 WHERE id=?1",[id]).unwrap();
    let request = uuid::Uuid::new_v4().to_string();
    let body = serde_json::json!({"save_request_id":request,"expected_updated_at":created["updated_at"],"name":"Edited", "allowed_paths":null,"blocked_patterns":null});
    let endpoint = format!("http://{addr}/api/repos/{id}");
    let response = client.put(&endpoint).json(&body).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let edited: serde_json::Value = response.json().await.unwrap();
    let actual = state.db.get_repository(id).unwrap().unwrap();
    assert_eq!(actual.name, "Edited");
    assert_eq!(actual.last_svn_rev, 99);
    assert_eq!(actual.total_errors, 4);
    assert!(actual.allowed_paths.is_none());
    assert!(actual.blocked_patterns.is_none());
    let mut newer = actual;
    newer.name = "Newer administrator settings".into();
    newer.updated_at = (chrono::Utc::now() + chrono::Duration::seconds(1)).to_rfc3339();
    state.db.update_repository(&newer).unwrap();
    // A known receipt remains recoverable without re-applying its old configuration.
    assert_eq!(
        client
            .put(&endpoint)
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        state.db.get_repository(id).unwrap().unwrap().name,
        "Newer administrator settings"
    );
    let stale = serde_json::json!({"save_request_id":uuid::Uuid::new_v4().to_string(),"expected_updated_at":created["updated_at"],"name":"Stale"});
    assert_eq!(
        client
            .put(&endpoint)
            .json(&stale)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let credentials = serde_json::json!({"save_request_id":uuid::Uuid::new_v4().to_string(),"expected_updated_at":edited["save_receipt"]["configuration_updated_at"],"git_token":"must-not-write"});
    assert_eq!(
        client
            .post(format!("{endpoint}/credentials"))
            .json(&credentials)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert!(state
        .db
        .get_state(&format!("secret_git_token_{id}"))
        .unwrap()
        .is_none());
    server.abort();
}

fn actor_session(state: &AppState, token: &str, role: &str) {
    let now = chrono::Utc::now().to_rfc3339();
    let id = format!("actor-{token}");
    state
        .db
        .insert_user(&reposync_core::models::User {
            id: id.clone(),
            username: id.clone(),
            display_name: id.clone(),
            email: "fixture@example.invalid".into(),
            password_hash: "unused".into(),
            role: role.into(),
            enabled: true,
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .unwrap();
    state
        .db
        .insert_session(&reposync_core::models::Session {
            token: token.into(),
            user_id: id,
            created_at: now,
            expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
        })
        .unwrap();
}

#[tokio::test]
async fn rs115_http_receipts_and_writes_are_actor_bound_and_unknown_credentials_refuse() {
    let (addr, state, server, _tmp) = save_fixture(&[]).await;
    actor_session(&state, "admin-a", "admin");
    actor_session(&state, "admin-b", "admin");
    actor_session(&state, "viewer", "user");
    let client = reqwest::Client::new();
    let request = uuid::Uuid::new_v4().to_string();
    let body = create_body(&request);
    let base = format!("http://{addr}/api/repos");
    assert_eq!(
        client
            .post(&base)
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&base)
            .bearer_auth("viewer")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = client
        .post(&base)
        .bearer_auth("admin-a")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let created: serde_json::Value = response.json().await.unwrap();
    let id = created["id"].as_str().unwrap();
    let lookup = format!("http://{addr}/api/repository-save-requests/{request}");
    assert_eq!(
        client
            .get(&lookup)
            .bearer_auth("admin-b")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client
            .get(&lookup)
            .bearer_auth("viewer")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(&lookup)
            .bearer_auth("admin-a")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let credential_url = format!("{base}/{id}/credentials");
    assert_eq!(client.post(&credential_url).bearer_auth("admin-a").json(&serde_json::json!({"save_request_id":uuid::Uuid::new_v4().to_string(),"unknown_password":"not-stored"})).send().await.unwrap().status(),422);
    assert_eq!(
        client
            .post(&credential_url)
            .bearer_auth("admin-a")
            .json(&serde_json::json!({"save_request_id":uuid::Uuid::new_v4().to_string()}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert!(state
        .db
        .get_state(&format!("secret_svn_password_{id}"))
        .unwrap()
        .is_none());
    server.abort();
}
