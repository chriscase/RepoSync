//! GitHub `forced` is a hint that triggers the same polling inspection.
//! It is never authority to reset the Git checkout.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::ImportProgress;
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_web::api;
use reposync_web::AppState;
use tempfile::TempDir;

fn webhook_state(tmp: &TempDir) -> (Arc<AppState>, tokio::sync::mpsc::Receiver<()>) {
    let git_path = tmp.path().join("git-repo");
    git2::Repository::init(&git_path).unwrap();
    let toml = format!(
        "[daemon]\ndata_dir = {:?}\n[svn]\nurl = 'https://svn.test.invalid/repo'\nusername = 'fixture'\npassword_env = ''\n[github]\nrepo = 'test/repo'\ntoken_env = ''\n",
        tmp.path().display().to_string()
    );
    let config: AppConfig = toml::from_str(&toml).unwrap();
    let engine = SyncEngine::new(
        config.clone(),
        Database::in_memory().unwrap(),
        SvnClient::new("https://svn.test.invalid/repo", "fixture", ""),
        GitClient::new(&git_path).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    );
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let (broadcast, _) = tokio::sync::broadcast::channel(8);
    let state = Arc::new(AppState {
        db: Database::in_memory().unwrap(),
        sync_engine: Arc::new(engine),
        config,
        sync_trigger: tx,
        ws_broadcast: broadcast,
        sessions: tokio::sync::RwLock::new(HashMap::new()),
        import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        config_path: tmp.path().join("config.toml"),
        prev_net_snapshot: std::sync::Mutex::new(None),
        repo_import_progress: tokio::sync::RwLock::new(HashMap::new()),
        login_attempts: std::sync::Mutex::new(HashMap::new()),
        import_handles: tokio::sync::Mutex::new(Vec::new()),
    });
    (state, rx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r09_webhook_forced_is_hint_not_reset() {
    let tmp = TempDir::new().unwrap();
    let (state, mut rx) = webhook_state(&tmp);
    let app = Router::new()
        .merge(api::webhooks::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let forced = client
        .post(format!("http://{addr}/webhook/github"))
        .header("X-GitHub-Event", "push")
        .json(&serde_json::json!({
            "ref": "refs/heads/main",
            "forced": true,
            "commits": [{"id": "abc", "message": "rewrite", "author": {"name": "dev", "email": "dev@example.invalid"}}],
            "repository": {"full_name": "test/repo"}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(forced.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = forced.json().await.unwrap();
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["forced_hint"], true, "{body}");
    assert_eq!(body["inspection"], "polling_safety_gate", "{body}");
    let message = body["message"].as_str().unwrap_or("");
    assert!(
        message.contains("hint"),
        "forced push must be described as a hint: {message}"
    );
    assert!(
        !message.to_ascii_lowercase().contains("reset"),
        "forced must not authorize reset: {message}"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("forced webhook must trigger the same sync channel")
        .expect("sync trigger closed");

    let ordinary = client
        .post(format!("http://{addr}/webhook/github"))
        .header("X-GitHub-Event", "push")
        .json(&serde_json::json!({
            "ref": "refs/heads/main",
            "forced": false,
            "commits": [{"id": "def", "message": "ff", "author": {"name": "dev", "email": "dev@example.invalid"}}],
            "repository": {"full_name": "test/repo"}
        }))
        .send()
        .await
        .unwrap();
    let ordinary_body: serde_json::Value = ordinary.json().await.unwrap();
    assert_eq!(ordinary_body["forced_hint"], false, "{ordinary_body}");
    assert_eq!(ordinary_body["inspection"], "polling_safety_gate");
    tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("unforced webhook uses the same inspection trigger")
        .expect("sync trigger closed");

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R09_WEBHOOK_FORCED_HINT",
            "forced_hint":true,
            "inspection":"polling_safety_gate",
            "reset_authorized":false,
            "same_channel_as_unforced":true
        })
    );
    server.abort();
}
