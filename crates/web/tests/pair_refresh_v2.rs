//! v2 transport boundaries use a current database actor and the actual engine.
use std::sync::Arc;

use reposync_core::config::AppConfig;
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::ImportProgress;
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_web::{api, AppState, WebServer};

struct Fixture {
    state: Arc<AppState>,
    url: String,
    server: tokio::task::JoinHandle<()>,
    _temp: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn engine(config: &AppConfig, db: Database, git: &std::path::Path) -> Arc<SyncEngine> {
    Arc::new(SyncEngine::new(
        config.clone(),
        db,
        SvnClient::new("file:///unused", "", ""),
        GitClient::new(git).unwrap(),
        Arc::new(IdentityMapper::new(&Default::default()).unwrap()),
    ))
}

async fn fixture(wrong_engine: bool) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let db = Database::new(root.join("reposync.db")).unwrap();
    db.initialize().unwrap();
    let mut config: AppConfig = toml::from_str(
        r#"
        [daemon]
        data_dir = "/unused"
        [svn]
        url = "file:///unused"
        username = ""
        password_env = ""
        [github]
        repo = "fixture/repo"
        token_env = ""
    "#,
    )
    .unwrap();
    config.daemon.data_dir = root.clone();
    db.set_state("encryption_key", &"51".repeat(32)).unwrap();
    let now = chrono::Utc::now();
    for (id, role, enabled) in [
        ("admin", "admin", true),
        ("user", "user", true),
        ("disabled", "admin", false),
    ] {
        db.insert_user(&reposync_core::models::User {
            id: id.into(),
            username: id.into(),
            display_name: id.into(),
            email: format!("{id}@example.invalid"),
            password_hash: "fixture".into(),
            role: role.into(),
            enabled,
            created_at: (now - chrono::Duration::minutes(2)).to_rfc3339(),
            updated_at: now.to_rfc3339(),
        })
        .unwrap();
        db.insert_session(&reposync_core::models::Session {
            token: format!("token-{id}"),
            user_id: id.into(),
            created_at: (now - chrono::Duration::minutes(1)).to_rfc3339(),
            expires_at: (now + chrono::Duration::hours(1)).to_rfc3339(),
        })
        .unwrap();
    }
    db.insert_session(&reposync_core::models::Session {
        token: "token-expired".into(),
        user_id: "admin".into(),
        created_at: (now - chrono::Duration::hours(2)).to_rfc3339(),
        expires_at: (now - chrono::Duration::hours(1)).to_rfc3339(),
    })
    .unwrap();
    let git = root.join("git");
    git2::Repository::init(&git).unwrap();
    let actual = engine(
        &config,
        Database::new(root.join("reposync.db")).unwrap(),
        &git,
    );
    let routed = if wrong_engine {
        engine(
            &config,
            Database::new(root.join("reposync.db")).unwrap(),
            &git,
        )
    } else {
        actual.clone()
    };
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let state = WebServer::new(
        config,
        db,
        actual,
        tx,
        root.join("config.toml"),
        Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
    )
    .app_state();
    state.sessions.write().await.insert(
        "legacy-memory-token".into(),
        now + chrono::Duration::hours(1),
    );
    let router = api::pair_refresh::routes(routed).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Fixture {
        state,
        url,
        server,
        _temp: temp,
    }
}

fn state(f: &Fixture) -> Vec<(String, String, String)> {
    f.state
        .db
        .conn()
        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[tokio::test]
async fn v2_requires_current_enabled_database_administrator_before_preparation() {
    let f = fixture(false).await;
    let saved = state(&f);
    let client = reqwest::Client::new();
    for token in [
        "token-user",
        "token-disabled",
        "token-expired",
        "legacy-memory-token",
        "unknown",
    ] {
        let response = client
            .post(format!("{}/api/v2/repos/child/refresh/prepare", f.url))
            .bearer_auth(token)
            .json(&serde_json::json!({"request_id":"request"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "{token}"
        );
        assert_eq!(state(&f), saved);
    }
    let response = client
        .post(format!("{}/api/v2/repos/child/refresh/prepare", f.url))
        .json(&serde_json::json!({"request_id":"request"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(state(&f), saved);
}

#[tokio::test]
async fn v2_rejects_unknown_fields_reanchor_empty_request_and_fabricated_handle() {
    let f = fixture(false).await;
    let saved = state(&f);
    let client = reqwest::Client::new();
    let prepare = format!("{}/api/v2/repos/child/refresh/prepare", f.url);
    for body in [
        serde_json::json!({"request_id":"request","execute":true}),
        serde_json::json!({"request_id":"request","operation":"reanchor"}),
    ] {
        let response = client
            .post(&prepare)
            .bearer_auth("token-admin")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    }
    let response = client
        .post(&prepare)
        .bearer_auth("token-admin")
        .json(&serde_json::json!({"request_id":""}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let action = format!("{}/api/v2/repos/child/refresh/action", f.url);
    let response = client.post(&action).bearer_auth("token-admin").json(&serde_json::json!({
        "request_id":"request","preparation_id":uuid::Uuid::new_v4(),"approval_digest":"fake","action":"execute"
    })).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let response = client.post(&action).bearer_auth("token-admin").json(&serde_json::json!({
        "request_id":"request","preparation_id":uuid::Uuid::new_v4(),"approval_digest":"fake","action":"reanchor"
    })).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(state(&f), saved);
}

#[tokio::test]
async fn v2_refuses_an_equivalent_but_different_engine_instance() {
    let f = fixture(true).await;
    let saved = state(&f);
    let response = reqwest::Client::new()
        .post(format!("{}/api/v2/repos/child/refresh/prepare", f.url))
        .bearer_auth("token-admin")
        .json(&serde_json::json!({"request_id":"request"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(state(&f), saved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_server_preserves_journals_and_exposes_only_authenticated_recovery() {
    let f = fixture(false).await;
    f.state.db.ensure_repository_scope_schema_upgrade().unwrap();
    f.state
        .db
        .set_state(
            "pair_refresh_execution_v3:authority:retained",
            "corrupt-retained-evidence",
        )
        .unwrap();
    let saved = state(&f);
    let root = &f.state.config.daemon.data_dir;
    let lock = Arc::new(reposync_core::data_dir::acquire(root).unwrap());
    let db = Database::open_existing_for_pair_refresh_recovery(root.join("reposync.db")).unwrap();
    let server = WebServer::for_pair_refresh_recovery(
        f.state.config.clone(),
        db,
        f.state.sync_engine.clone(),
        root.join("config.toml"),
        lock,
    )
    .unwrap();
    assert_eq!(state(&f), saved);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    let url = format!("http://{addr}");
    let task = tokio::spawn(async move {
        server.start(&addr).await.unwrap();
    });
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if client
            .get(format!("{url}/api/auth/info"))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline && !task.is_finished());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    for path in [
        "/api/v2/repos/child/refresh/prepare",
        "/api/repos",
        "/api/config",
        "/api/sync",
        "/",
    ] {
        let response = client
            .post(format!("{url}{path}"))
            .bearer_auth("token-admin")
            .json(&serde_json::json!({"request_id":"new"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND, "{path}");
    }
    let endpoint = format!("{url}/api/v2/repos/child/refresh/recovery/prepare");
    let response = client
        .post(&endpoint)
        .json(&serde_json::json!({"request_id":"held"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let response = client
        .post(&endpoint)
        .bearer_auth("token-admin")
        .json(&serde_json::json!({"request_id":"held"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(state(&f), saved);
    task.abort();
    let _ = task.await;
}
