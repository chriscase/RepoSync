//! Synthetic HTTP authorization boundaries; no external services or import workers.
use axum::Router;
use reposync_core::{
    config::{AppConfig, IdentityConfig},
    db::Database,
    git::GitClient,
    identity::IdentityMapper,
    import::ImportProgress,
    models::{Session, User},
    svn::SvnClient,
    sync_engine::SyncEngine,
};
use reposync_web::{api, AppState};
use serde_json::{json, Value};
use std::{collections::HashMap, net::SocketAddr, path::Path, sync::Arc, time::Duration};
const TEST_TOKEN: &str = "legacy-admin";
fn minimal_config(data_dir: &Path) -> AppConfig {
    let toml_str = format!(
        r#"
[daemon]
data_dir = "{}"

[svn]
url = "https://svn.test.invalid/repo"
username = "testuser"
password_env = ""

[github]
repo = "test/repo"
token_env = ""
"#,
        data_dir.display().to_string().replace('\\', "/")
    );
    let mut config: AppConfig =
        toml::from_str(&toml_str).expect("failed to parse minimal test config");
    // admin_password is #[serde(skip)] so we must set it directly.
    config.web.admin_password = Some("test-admin-pass".to_string());
    config
}

async fn fixture(
    password: bool,
) -> (
    SocketAddr,
    Arc<AppState>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let git_repo_path = tmp.path().join("git-repo");
    git2::Repository::init(&git_repo_path).expect("git init");

    let mut config = minimal_config(tmp.path());
    if !password {
        config.web.admin_password = None;
    }

    let web_db = Database::in_memory().expect("web db");
    web_db.initialize().expect("web db init");

    let engine_db = Database::in_memory().expect("engine db");
    engine_db.initialize().expect("engine db init");

    let svn_client = SvnClient::new("https://svn.test.invalid/repo", "testuser", "");
    let git_client = GitClient::new(&git_repo_path).expect("git client");
    let identity_mapper =
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).expect("identity mapper"));

    let sync_engine = Arc::new(SyncEngine::new(
        config.clone(),
        engine_db,
        svn_client,
        git_client,
        identity_mapper,
    ));

    let (sync_tx, _sync_rx) = tokio::sync::mpsc::channel(1);
    let (ws_tx, _) = tokio::sync::broadcast::channel(256);

    let state = Arc::new(AppState {
        db: web_db,
        sync_engine,
        config,
        sync_trigger: sync_tx,
        ws_broadcast: ws_tx,
        sessions: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        config_path: tmp.path().join("config.toml"),
        prev_net_snapshot: std::sync::Mutex::new(None),
        repo_import_progress: tokio::sync::RwLock::new(HashMap::new()),
        login_attempts: std::sync::Mutex::new(HashMap::new()),
        import_handles: tokio::sync::Mutex::new(Vec::new()),
        scheduler_service: std::sync::Mutex::new(reposync_web::SchedulerServiceState::running()),
    });

    let app = Router::new()
        .merge(api::auth::routes())
        .merge(api::setup::routes())
        .merge(api::config::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });

    // Pre-seed a test session so authenticated endpoints work.
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(
            TEST_TOKEN.to_string(),
            chrono::Utc::now() + chrono::Duration::hours(24),
        );
    }

    tokio::time::sleep(Duration::from_millis(50)).await;

    (addr, state, handle, tmp)
}

fn add_user(state: &AppState, id: &str, role: &str, enabled: bool, expired: bool) {
    let now = chrono::Utc::now();
    state
        .db
        .insert_user(&User {
            id: id.into(),
            username: id.into(),
            display_name: id.into(),
            email: format!("{id}@example.invalid"),
            password_hash: "fixture".into(),
            role: role.into(),
            enabled,
            created_at: now.to_rfc3339(),
            updated_at: now.to_rfc3339(),
        })
        .unwrap();
    state
        .db
        .insert_session(&Session {
            token: id.into(),
            user_id: id.into(),
            created_at: now.to_rfc3339(),
            expires_at: (now + chrono::Duration::hours(if expired { -1 } else { 1 })).to_rfc3339(),
        })
        .unwrap();
}
fn routes(url: &str) -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("GET", "/api/setup/config", json!(null)),
        (
            "POST",
            "/api/setup/test-svn",
            json!({"url":url,"username":"fixture"}),
        ),
        (
            "POST",
            "/api/setup/test-git",
            json!({"api_url":url,"repo":"fixture/repo","provider":"gitea"}),
        ),
        (
            "POST",
            "/api/setup/apply",
            json!({"svn_url":url,"svn_username":"fixture","git_api_url":url,"git_repo":"fixture/repo","identity_mappings":[{"svn_username":"fixture","name":"Fixture","email":"fixture@example.invalid"}]}),
        ),
        ("POST", "/api/setup/import", json!(null)),
        ("GET", "/api/setup/import/status", json!(null)),
        ("POST", "/api/setup/import/cancel", json!(null)),
        ("POST", "/api/setup/reset-reimport", json!(null)),
        ("GET", "/api/config", json!(null)),
        ("GET", "/api/config/identity", json!(null)),
        ("PUT", "/api/config/identity", json!({"mappings":[]})),
        ("GET", "/api/config/notifications", json!(null)),
        (
            "POST",
            "/api/config/notifications",
            json!({"teams_webhook_url":url}),
        ),
        ("POST", "/api/config/notifications/test", json!(null)),
    ]
}
fn snapshot(state: &AppState) -> Vec<String> {
    let conn = state.db.conn();
    let mut names = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap();
    let tables = names
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut out = vec![];
    for table in tables {
        let mut query = conn
            .prepare(&format!(
                "SELECT * FROM \"{}\" ORDER BY rowid",
                table.replace('"', "\"\"")
            ))
            .unwrap();
        let count = query.column_count();
        let mut rows = query.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            out.push(format!(
                "{}:{:?}",
                table,
                (0..count)
                    .map(|i| format!("{:?}", row.get_ref(i).unwrap()))
                    .collect::<Vec<_>>()
            ));
        }
    }
    out
}
async fn call(
    addr: SocketAddr,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Value,
) -> reqwest::Response {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let mut request = client.request(method.parse().unwrap(), format!("http://{addr}{path}"));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request.json(&body).send().await.unwrap()
}
#[tokio::test]
async fn all_fourteen_routes_reject_before_side_effects() {
    // A listening but never served owned sink proves refused probes cannot connect.
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", sink.local_addr().unwrap());
    let (addr, state, server, tmp) = fixture(true).await;
    add_user(&state, "user", "user", true, false);
    add_user(&state, "disabled", "admin", false, false);
    add_user(&state, "expired", "admin", true, true);
    state.db.set_state("teams_webhook_url", &url).unwrap();
    std::fs::write(
        state
            .config_path
            .parent()
            .unwrap()
            .join("identity-mappings.toml"),
        "sentinel",
    )
    .unwrap();
    std::fs::write(&state.config_path, "config sentinel").unwrap();
    let progress_before = serde_json::to_value(&*state.import_progress.read().await).unwrap();
    let before = snapshot(&state);
    for (token, expected) in [
        (Some("user"), 403),
        (None, 401),
        (Some("invalid"), 401),
        (Some("disabled"), 401),
        (Some("expired"), 401),
    ] {
        for (method, path, body) in routes(&url) {
            let response = call(addr, token, method, path, body).await;
            assert_eq!(
                response.status().as_u16(),
                expected,
                "{method} {path} {token:?}"
            );
        }
    }
    assert_eq!(snapshot(&state), before);
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("identity-mappings.toml")).unwrap(),
        "sentinel"
    );
    assert_eq!(
        std::fs::read_to_string(&state.config_path).unwrap(),
        "config sentinel"
    );
    let progress = state.import_progress.read().await;
    assert_eq!(serde_json::to_value(&*progress).unwrap(), progress_before);
    assert!(!progress.cancel_requested);
    assert!(!progress
        .cancel_signal
        .load(std::sync::atomic::Ordering::Acquire));
    drop(progress);
    assert!(state.import_handles.lock().await.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), sink.accept())
            .await
            .is_err()
    );
    server.abort();
}
#[tokio::test]
async fn admin_and_bootstrap_matrix_preserves_existing_admission() {
    for (password, named) in [(false, false), (true, false), (false, true), (true, true)] {
        let (addr, state, server, _tmp) = fixture(password).await;
        if named {
            add_user(&state, "admin", "admin", true, false);
        }
        let token = if named {
            Some("admin")
        } else if password {
            Some(TEST_TOKEN)
        } else {
            None
        };
        // Empty probes are positive authorization checks with no outbound operation.
        for (path, body) in [
            ("/api/setup/test-svn", json!({"url":"","username":""})),
            (
                "/api/setup/test-git",
                json!({"api_url":"","repo":"","provider":"gitea"}),
            ),
        ] {
            assert_eq!(
                call(addr, token, "POST", path, body.clone()).await.status(),
                200
            );
            if password || named {
                assert_eq!(call(addr, None, "POST", path, body).await.status(), 401);
            }
        }
        for path in [
            "/api/setup/config",
            "/api/config",
            "/api/config/identity",
            "/api/config/notifications",
        ] {
            assert_eq!(
                call(addr, token, "GET", path, json!(null))
                    .await
                    .status()
                    .as_u16(),
                if !password && !named { 403 } else { 200 },
                "{path}"
            );
        }
        // No repository exists: these must reach existing domain validation, not a worker.
        for (method, path) in [
            ("POST", "/api/setup/import"),
            ("GET", "/api/setup/import/status"),
            ("POST", "/api/setup/import/cancel"),
            ("POST", "/api/setup/reset-reimport"),
        ] {
            let status = call(addr, token, method, path, json!(null))
                .await
                .status()
                .as_u16();
            if !password && !named && path.ends_with("reset-reimport") {
                assert_eq!(status, 403);
            } else {
                assert_ne!(status, 401);
                assert_ne!(status, 403);
            }
        }
        if password || named {
            assert_eq!(
                call(
                    addr,
                    token,
                    "PUT",
                    "/api/config/identity",
                    json!({"mappings":[]})
                )
                .await
                .status(),
                200
            );
            assert_eq!(
                call(
                    addr,
                    token,
                    "POST",
                    "/api/config/notifications",
                    json!({"teams_webhook_url":""})
                )
                .await
                .status(),
                200
            );
            assert_eq!(
                call(
                    addr,
                    token,
                    "POST",
                    "/api/config/notifications/test",
                    json!(null)
                )
                .await
                .status(),
                400
            );
        }
        // Apply only after no-repository import checks, with all optional side effects off.
        assert_eq!(
            call(
                addr,
                token,
                "POST",
                "/api/setup/apply",
                json!({
                    "svn_url":"", "svn_username":"", "git_api_url":"", "git_repo":""
                })
            )
            .await
            .status(),
            200
        );
        assert!(state.import_handles.lock().await.is_empty());
        server.abort();
    }
}

#[tokio::test]
async fn page_identity_observes_the_current_session_mode() {
    let (addr, state, server, _tmp) = fixture(true).await;
    let me = "/api/auth/me";
    assert_eq!(
        call(addr, Some(TEST_TOKEN), "GET", me, Value::Null)
            .await
            .status(),
        200
    );
    add_user(&state, "named", "admin", true, false);
    add_user(&state, "disabled", "admin", false, false);
    for (token, expected) in [("named", 200), ("disabled", 401), (TEST_TOKEN, 401)] {
        assert_eq!(
            call(addr, Some(token), "GET", me, Value::Null)
                .await
                .status(),
            expected
        );
    }
    server.abort();
}

#[tokio::test]
async fn unavailable_user_count_cannot_enable_setup_bootstrap() {
    let (addr, state, server, _tmp) = fixture(false).await;
    state
        .db
        .conn()
        .execute("ALTER TABLE users RENAME TO unavailable_users", [])
        .unwrap();
    let response = call(addr, None, "GET", "/api/setup/import/status", Value::Null).await;
    assert_eq!(response.status(), 500);
    server.abort();
}
