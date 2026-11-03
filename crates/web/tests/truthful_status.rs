//! #116 truthful readonly status projection regressions using the existing local HTTP/SQLite fixture.

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
    tokio::time::sleep(Duration::from_millis(20)).await;
    (addr, state, server, tmp)
}

fn kv_snapshot(state: &AppState) -> Vec<(String, String, String)> {
    state
        .db
        .conn()
        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}
async fn status(addr: SocketAddr, id: Option<&str>) -> serde_json::Value {
    let suffix = id.map(|id| format!("?repo_id={id}")).unwrap_or_default();
    let response = authed_client()
        .get(format!("http://{addr}/api/status{suffix}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}
#[tokio::test]
async fn rs116_http_explicit_configuration_health_and_unknown_have_no_write_or_freshness_fallback()
{
    let (addr, state, server, _tmp) =
        status_fixture(&[("a", "error_paused"), ("b", "unexpected-new-state")]).await;
    for (id, health) in [("a", "paused"), ("b", "unknown")] {
        let before = kv_snapshot(&state);
        let value = status(addr, Some(id)).await;
        assert_eq!(value["observation"]["configured"], "enabled");
        assert_eq!(value["observation"]["health"], health);
        assert_eq!(value["observation"]["data_freshness"], "never_recorded");
        assert!(value["observation"]["last_successful_sync_at"].is_null());
        assert_eq!(
            value["observation"]["remote_freshness"],
            "not_verified_by_status"
        );
        assert_eq!(value["observation"]["uptime_available"], false);
        assert_eq!(kv_snapshot(&state), before);
    }
    state.db.update_repo_sync_status("a", "idle").unwrap();
    assert_eq!(
        status(addr, Some("a")).await["observation"]["health"],
        "initializing"
    );
    let mut repo = state.db.get_repository("a").unwrap().unwrap();
    repo.enabled = false;
    state.db.update_repository(&repo).unwrap();
    let value = status(addr, Some("a")).await;
    assert_eq!(value["observation"]["configured"], "disabled");
    assert_eq!(value["observation"]["health"], "disabled");
    server.abort();
}
#[tokio::test]
async fn rs116_http_real_import_lifecycle_cancelled_does_not_release_busy_or_durable_hold() {
    use reposync_core::db::import_operations::ImportOperationState;
    let (addr, state, server, _tmp) = status_fixture(&[("lifecycle", "idle")]).await;
    let op = state
        .db
        .create_import_operation("lifecycle", "legacy-admin", "request-a", "fixture-target")
        .unwrap();
    assert_eq!(
        status(addr, Some("lifecycle")).await["observation"]["health"],
        "running"
    );
    state
        .db
        .start_import_operation("lifecycle", &op.id)
        .unwrap();
    let guard = reposync_core::busy::try_acquire("lifecycle").unwrap();
    assert_eq!(
        status(addr, Some("lifecycle")).await["observation"]["health"],
        "busy"
    );
    state.db.request_import_cancel("lifecycle", &op.id).unwrap();
    let value = status(addr, Some("lifecycle")).await;
    assert_eq!(
        value["observation"]["operations"][0]["state"],
        "cancel_requested"
    );
    state
        .db
        .finish_import_operation(
            "lifecycle",
            &op.id,
            ImportOperationState::Cancelled,
            "User cancelled; collected result not established",
        )
        .unwrap();
    let before = kv_snapshot(&state);
    let value = status(addr, Some("lifecycle")).await;
    assert_eq!(value["observation"]["registry_busy"], true);
    assert_eq!(value["observation"]["health"], "held");
    assert_eq!(value["observation"]["operations"][0]["state"], "cancelled");
    assert_eq!(value["observation"]["operations"][0]["held"], true);
    assert_eq!(kv_snapshot(&state), before);
    drop(guard);
    assert_eq!(
        status(addr, Some("lifecycle")).await["observation"]["health"],
        "held"
    );
    server.abort();
}
#[tokio::test]
async fn rs116_http_publication_uncertainty_preserves_operation_ref_and_safe_action() {
    use reposync_core::db::import_operations::ImportOperationState;
    let (addr, state, server, _tmp) = status_fixture(&[("hold", "idle")]).await;
    let op = state
        .db
        .create_import_operation("hold", "legacy-admin", "request-hold", "fixture-target")
        .unwrap();
    state.db.start_import_operation("hold", &op.id).unwrap();
    let sha = "a".repeat(40);
    state
        .db
        .note_import_local("hold", &op.id, 8, &sha, 1, 1)
        .unwrap();
    state
        .db
        .begin_import_publication("hold", &op.id, "refs/heads/main", &sha)
        .unwrap();
    state
        .db
        .finish_import_operation(
            "hold",
            &op.id,
            ImportOperationState::ReconciliationRequired,
            "Publication response lost; outcome uncertain",
        )
        .unwrap();
    let before = kv_snapshot(&state);
    let value = status(addr, Some("hold")).await;
    let observation = &value["observation"];
    assert_eq!(observation["health"], "held");
    assert_eq!(observation["operations"][0]["operation_id"], op.id);
    assert_eq!(
        observation["operations"][0]["target_ref"],
        "refs/heads/main"
    );
    assert_eq!(observation["operations"][0]["intended_git_sha"], sha);
    assert!(observation["safe_next_action"]
        .as_str()
        .unwrap()
        .contains("do not prove reconciliation"));
    assert_eq!(kv_snapshot(&state), before);
    server.abort();
}
#[tokio::test]
async fn rs116_http_durable_rewritten_history_evidence_is_read_not_repaired_or_reproven() {
    use reposync_core::history_inspect::{
        history_block_key_for_repo, persist_history_block, HistoryInspectReject,
        REASON_OBSERVED_REMOTE_REWRITE,
    };
    let (addr, state, server, _tmp) = status_fixture(&[("history", "idle")]).await;
    let old = "a".repeat(40);
    let remote = "b".repeat(40);
    let key = history_block_key_for_repo(&state.db, "history").unwrap();
    persist_history_block(
        &state.db,
        &key,
        Some("history"),
        &HistoryInspectReject {
            reason: REASON_OBSERVED_REMOTE_REWRITE.into(),
            detail: "Stored observation: remote rewritten; no automatic repair".into(),
            o: Some(old.clone()),
            r: Some(remote.clone()),
            l: None,
        },
        Some(&old),
    )
    .unwrap();
    let before = kv_snapshot(&state);
    let value = status(addr, Some("history")).await;
    assert_eq!(value["observation"]["health"], "held");
    let evidence = value["observation"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|op| op["kind"] == "history")
        .unwrap();
    assert!(evidence["reason"]
        .as_str()
        .unwrap()
        .contains(REASON_OBSERVED_REMOTE_REWRITE));
    assert_eq!(evidence["history_evidence"]["handled_checkpoint"], old);
    assert_eq!(
        evidence["history_evidence"]["fresh_observed_remote_tip"],
        remote
    );
    assert_eq!(kv_snapshot(&state), before);
    server.abort();
}
#[tokio::test]
async fn rs116_http_missing_and_corrupt_evidence_and_failed_metrics_never_return_zero_health() {
    let (addr, state, server, _tmp) = status_fixture(&[("a", "idle")]).await;
    state
        .db
        .set_state("import_operation_v1:active:a", "orphan-operation")
        .unwrap();
    let before = kv_snapshot(&state);
    let value = status(addr, Some("a")).await;
    assert_eq!(value["observation"]["health"], "unknown");
    assert_eq!(value["observation"]["evidence_complete"], false);
    assert_eq!(kv_snapshot(&state), before);
    state
        .db
        .set_state(
            "import_operation_v1:document:orphan-operation",
            "{malformed",
        )
        .unwrap();
    assert_eq!(
        status(addr, Some("a")).await["observation"]["health"],
        "unknown"
    );
    let missing = authed_client()
        .get(format!("http://{addr}/api/status?repo_id=missing"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    let body: serde_json::Value = missing.json().await.unwrap();
    assert!(body.get("total_syncs").is_none());
    let no_auth = reqwest::get(format!("http://{addr}/api/status?repo_id=a"))
        .await
        .unwrap();
    assert_eq!(no_auth.status(), 401);
    state
        .db
        .conn()
        .execute_batch("DROP TABLE conflicts")
        .unwrap();
    let failure = authed_client()
        .get(format!("http://{addr}/api/status?repo_id=a"))
        .send()
        .await
        .unwrap();
    assert_eq!(failure.status(), 500);
    assert!(failure
        .json::<serde_json::Value>()
        .await
        .unwrap()
        .get("active_conflicts")
        .is_none());
    server.abort();
}
#[tokio::test]
async fn rs116_http_scoped_checkpoint_missing_copy_does_not_backfill_and_aggregate_keeps_unknown() {
    let (addr, state, server, _tmp) = status_fixture(&[("a", "idle"), ("b", "error_paused")]).await;
    let mut repo = state.db.get_repository("a").unwrap().unwrap();
    repo.last_svn_rev = 9;
    repo.last_git_sha = "a".repeat(40);
    state.db.update_repository(&repo).unwrap();
    let before = kv_snapshot(&state);
    let value = status(addr, Some("a")).await;
    assert_eq!(value["observation"]["health"], "unknown");
    assert!(state.db.get_state("last_svn_rev_a").unwrap().is_none());
    assert_eq!(kv_snapshot(&state), before);
    let global = status(addr, None).await;
    assert_eq!(global["observation"]["health"], "unknown");
    assert_eq!(
        global["observation"]["data_freshness"],
        "multiple_scoped_records"
    );
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
async fn rs116_http_restricted_details_do_not_mask_a_dangling_active_pointer_or_leak_intent() {
    let (addr, state, server, _tmp) = status_fixture(&[("private", "idle")]).await;
    let op = state
        .db
        .create_import_operation("private", "actor-owner", "private-request", "fixture")
        .unwrap();
    state.db.start_import_operation("private", &op.id).unwrap();
    let sha = "d".repeat(40);
    state
        .db
        .note_import_local("private", &op.id, 4, &sha, 1, 1)
        .unwrap();
    state
        .db
        .begin_import_publication("private", &op.id, "refs/heads/private-intent", &sha)
        .unwrap();
    actor_session(&state, "readonly-viewer", "user");
    let read = || {
        reqwest::Client::new()
            .get(format!("http://{addr}/api/status?repo_id=private"))
            .bearer_auth("readonly-viewer")
    };
    let before = kv_snapshot(&state);
    let response = read().send().await.unwrap();
    assert_eq!(response.status(), 200);
    let value: serde_json::Value = response.json().await.unwrap();
    assert_eq!(value["observation"]["health"], "running");
    assert_eq!(
        value["observation"]["operations"].as_array().unwrap().len(),
        1
    );
    let projected = &value["observation"]["operations"][0];
    assert_eq!(projected["details_visible"], false);
    assert!(projected["operation_id"].is_null());
    assert!(projected["target_ref"].is_null());
    assert!(projected["intended_git_sha"].is_null());
    assert!(projected["confirmed_svn_revision"].is_null());
    assert_eq!(kv_snapshot(&state), before);
    state
        .db
        .set_state(
            "import_operation_v1:active:private",
            "missing-different-operation",
        )
        .unwrap();
    let before = kv_snapshot(&state);
    let value: serde_json::Value = read().send().await.unwrap().json().await.unwrap();
    assert_eq!(value["observation"]["health"], "unknown");
    assert_eq!(value["observation"]["evidence_complete"], false);
    assert_eq!(kv_snapshot(&state), before);
    server.abort();
}
#[tokio::test]
async fn rs116_http_late_pair_active_and_reconciliation_are_not_hidden_by_idle_registration() {
    use reposync_core::db::late_pair_publish_operations::LatePairPublishState;
    let (addr, state, server, _tmp) = status_fixture(&[("late-parent", "idle")]).await;
    let mut op = state
        .db
        .create_late_pair_publish_operation(
            "late-parent",
            "legacy-admin",
            "late-request",
            "fixture",
            "feature",
            "branches/feature",
            &"a".repeat(40),
            &"b".repeat(40),
            5,
            5,
            "late_pair_publish_v1",
        )
        .unwrap();
    let before = kv_snapshot(&state);
    let running = status(addr, Some("late-parent")).await;
    assert_eq!(running["observation"]["health"], "running");
    assert_eq!(kv_snapshot(&state), before);
    op.state = LatePairPublishState::ReconciliationRequired;
    op.outcome_detail = Some("SVN copy result is uncertain".into());
    let op = state.db.update_late_pair_publish_operation(op).unwrap();
    let before = kv_snapshot(&state);
    let held = status(addr, Some("late-parent")).await;
    assert_eq!(held["observation"]["health"], "held");
    let evidence = held["observation"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["kind"] == "late_pair")
        .unwrap();
    assert_eq!(evidence["operation_id"], op.id);
    assert_eq!(evidence["state"], "reconciliation_required");
    assert_eq!(kv_snapshot(&state), before);
    server.abort();
}
