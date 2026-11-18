//! Setup-wizard busy slot: durable `import_operation_v1` + process-wide busy
//! admission on `/api/setup/import` start/status/cancel.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::import_operations::ImportOperationState;
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::{ImportPhase, ImportProgress};
use reposync_core::models::Repository;
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_web::api;
use reposync_web::AppState;

const TEST_TOKEN: &str = "test-session-token-for-setup-wizard-busy";

fn svn_available() -> bool {
    Command::new("svnadmin")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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

fn insert_wizard_repo(db: &Database, svn_url: &str, tmp: &Path) -> String {
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();
    db.insert_repository(&Repository {
        id: id.clone(),
        name: "history".into(),
        svn_url: svn_url.into(),
        svn_branch: "trunk".into(),
        svn_username: "fixture".into(),
        git_provider: "gitea".into(),
        git_api_url: format!("file://{}", tmp.display()),
        git_repo: "local/history".into(),
        git_branch: "main".into(),
        sync_mode: "direct".into(),
        poll_interval_secs: 60,
        lfs_threshold_mb: 0,
        auto_merge: true,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
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
    })
    .unwrap();
    id
}

async fn setup_wizard_fixture() -> (
    SocketAddr,
    Arc<AppState>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
    String,
    std::path::PathBuf,
) {
    let tmp = tempfile::tempdir().unwrap();
    let svn_repo = tmp.path().join("svn-repo");
    let bare = tmp.path().join("local").join("history.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    assert!(Command::new("svnadmin")
        .args(["create", svn_repo.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let svn_url = format!("file://{}", svn_repo.display());
    assert!(Command::new("svn")
        .args([
            "mkdir",
            &format!("{svn_url}/trunk"),
            "-m",
            "trunk",
            "--username",
            "fixture",
        ])
        .status()
        .unwrap()
        .success());
    let checkout = tmp.path().join("svn-wc");
    assert!(Command::new("svn")
        .args([
            "checkout",
            &format!("{svn_url}/trunk"),
            checkout.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    std::fs::write(checkout.join("history.txt"), "first\n").unwrap();
    assert!(Command::new("svn")
        .args(["add", "history.txt"])
        .current_dir(&checkout)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("svn")
        .args(["commit", "-m", "first", "--username", "fixture"])
        .current_dir(&checkout)
        .status()
        .unwrap()
        .success());
    std::fs::write(checkout.join("history.txt"), "second\n").unwrap();
    assert!(Command::new("svn")
        .args(["commit", "-m", "second", "--username", "fixture"])
        .current_dir(&checkout)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["init", "--bare", bare.to_str().unwrap()])
        .status()
        .unwrap()
        .success());

    let data_dir = tmp.path().display().to_string().replace('\\', "/");
    let config_toml = format!(
        r#"
[daemon]
data_dir = "{data_dir}"

[svn]
url = "{svn_url}"
username = "fixture"
password_env = ""

[github]
repo = "local/history"
api_url = "file://{data_dir}"
git_base_url = "file://{data_dir}"
default_branch = "main"
token_env = ""
"#
    );
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let mut config: AppConfig = toml::from_str(&config_toml).unwrap();
    config.web.admin_password = Some("test-admin-pass".into());

    let db = Database::new(tmp.path().join("reposync.db")).unwrap();
    db.initialize().unwrap();
    let repo_id = insert_wizard_repo(&db, &svn_url, tmp.path());

    let (addr, state, server) = serve_wizard_fixture(tmp.path(), config, db).await;
    (addr, state, server, tmp, repo_id, bare)
}

async fn serve_wizard_fixture(
    dir: &Path,
    config: AppConfig,
    db: Database,
) -> (SocketAddr, Arc<AppState>, tokio::task::JoinHandle<()>) {
    let engine_db = Database::in_memory().unwrap();
    engine_db.initialize().unwrap();
    let dummy_git = dir.join("dummy-git");
    git2::Repository::init(&dummy_git).unwrap();
    let engine = Arc::new(SyncEngine::new(
        config.clone(),
        engine_db,
        SvnClient::new("file:///nonexistent", "fixture", ""),
        GitClient::new(&dummy_git).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    ));
    let (sync_tx, _) = tokio::sync::mpsc::channel(1);
    let (ws_tx, _) = tokio::sync::broadcast::channel(32);
    let state = Arc::new(AppState {
        db,
        sync_engine: engine,
        config,
        sync_trigger: sync_tx,
        ws_broadcast: ws_tx,
        sessions: tokio::sync::RwLock::new(HashMap::new()),
        import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        config_path: dir.join("config.toml"),
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
        .merge(api::setup::routes())
        .merge(api::repos::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state, server)
}

async fn setup_status(client: &reqwest::Client, addr: SocketAddr) -> serde_json::Value {
    client
        .get(format!("http://{addr}/api/setup/import/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn cancel_setup(client: &reqwest::Client, addr: SocketAddr) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = client
                .post(format!("http://{addr}/api/setup/import/cancel"))
                .send()
                .await
                .unwrap();
            if response.status().is_success() {
                return response.json().await.unwrap();
            }
            let status = setup_status(client, addr).await;
            if matches!(
                status["lifecycle"].as_str(),
                Some("completed" | "cancelled" | "failed" | "reconciliation_required")
            ) {
                return serde_json::json!({
                    "ok": true,
                    "message": "terminal outcome retained; published history is not undone",
                    "operation_id": status["operation_id"],
                    "lifecycle": status["lifecycle"],
                });
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed out cancelling setup import")
}

// A durable terminal outcome can precede worker cleanup and BusyGuard release.
// These callers require both that outcome and actual idle for the same operation.
async fn wait_terminal(
    client: &reqwest::Client,
    addr: SocketAddr,
    operation_id: &str,
    repo_id: &str,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let status = setup_status(client, addr).await;
            assert_eq!(status["operation_id"], operation_id, "{status}");
            if matches!(
                status["lifecycle"].as_str(),
                Some("completed" | "cancelled" | "failed" | "reconciliation_required")
            ) && status["busy"] == false
                && !reposync_core::busy::is_busy(repo_id)
            {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed out waiting for setup import terminal and idle status")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64c07_setup_wizard_busy_duplicate_cancel_idle_after_terminal() {
    if !svn_available() {
        eprintln!("SKIP: svnadmin not available");
        return;
    }
    let (addr, _state, server, tmp, repo_id, bare) = setup_wizard_fixture().await;
    let client = authed_client();
    let started = client
        .post(format!("http://{addr}/api/setup/import"))
        .header("x-request-id", "setup-busy-1")
        .send()
        .await
        .unwrap();
    assert!(
        started.status().is_success(),
        "{}",
        started.text().await.unwrap()
    );
    let started: serde_json::Value = started.json().await.unwrap();
    assert_eq!(started["ok"], true, "{started}");
    let op_id = started["operation_id"].as_str().unwrap().to_string();

    let status = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = setup_status(&client, addr).await;
            if status["operation_id"] == op_id {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("status never showed the setup operation id");
    assert_eq!(status["operation_id"], op_id);
    if matches!(
        status["lifecycle"].as_str(),
        Some("queued" | "running" | "cancel_requested" | "cancelling")
    ) {
        assert_eq!(status["busy"], true, "{status}");
        assert!(reposync_core::busy::is_busy(&repo_id), "{status}");
        let duplicate: serde_json::Value = client
            .post(format!("http://{addr}/api/setup/import"))
            .header("x-request-id", "setup-busy-2")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(duplicate["ok"], false, "{duplicate}");
        assert_eq!(duplicate["operation_id"], op_id);
        let idempotent: serde_json::Value = client
            .post(format!("http://{addr}/api/setup/import"))
            .header("x-request-id", "setup-busy-1")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(idempotent["ok"], true, "{idempotent}");
        assert_eq!(idempotent["operation_id"], op_id);

        let remote_before = Command::new("git")
            .args(["--git-dir", bare.to_str().unwrap(), "show-ref"])
            .output()
            .unwrap()
            .stdout;
        let cancel = cancel_setup(&client, addr).await;
        assert_eq!(cancel["ok"], true, "{cancel}");
        assert!(
            cancel["message"]
                .as_str()
                .unwrap_or("")
                .contains("not undone"),
            "{cancel}"
        );
        assert!(
            cancel["lifecycle"] == "cancel_requested"
                || cancel["lifecycle"] == "cancelled"
                || cancel["lifecycle"] == "completed",
            "{cancel}"
        );
        let terminal = wait_terminal(&client, addr, &op_id, &repo_id).await;
        assert_eq!(terminal["operation_id"], op_id);
        assert_eq!(terminal["busy"], false, "{terminal}");
        assert!(!reposync_core::busy::is_busy(&repo_id));
        let remote_after = Command::new("git")
            .args(["--git-dir", bare.to_str().unwrap(), "show-ref"])
            .output()
            .unwrap()
            .stdout;
        if terminal["lifecycle"] == "cancelled" {
            assert_eq!(
                remote_after, remote_before,
                "cancel must not undo or invent published history"
            );
        }
    } else {
        let status = wait_terminal(&client, addr, &op_id, &repo_id).await;
        assert_eq!(status["busy"], false, "{status}");
        assert!(!reposync_core::busy::is_busy(&repo_id));
    }

    let restart: serde_json::Value = client
        .post(format!("http://{addr}/api/setup/import"))
        .header("x-request-id", "setup-busy-restart")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restart["ok"], false, "{restart}");
    let reopened = Database::new(tmp.path().join("reposync.db")).unwrap();
    let latest = reopened.latest_import_operation(&repo_id).unwrap().unwrap();
    assert_eq!(latest.id, op_id);
    assert!(
        latest.state.is_terminal()
            || matches!(
                latest.state,
                ImportOperationState::Queued
                    | ImportOperationState::Running
                    | ImportOperationState::CancelRequested
            )
    );
    server.abort();
}

async fn cancel_selected(
    client: &reqwest::Client,
    addr: SocketAddr,
    id: &str,
) -> serde_json::Value {
    client
        .post(format!("http://{addr}/api/setup/import/cancel"))
        .query(&[("operation_id", id)])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn rs19_status_keeps_legacy_checkpoints_read_only_and_reports_storage_errors() {
    let (addr, state, server, _tmp, repo_id, _bare) = setup_wizard_fixture().await;
    let client = authed_client();
    let idle = setup_status(&client, addr).await;
    assert_eq!(idle["can_start"], true, "{idle}");
    assert_eq!(idle["can_cancel"], false);
    state
        .db
        .conn()
        .execute(
            "UPDATE repositories SET last_svn_rev=4, last_git_sha='legacy-tip' WHERE id=?1",
            [&repo_id],
        )
        .unwrap();
    let key = format!("last_svn_rev_{repo_id}");
    assert!(state.db.get_state(&key).unwrap().is_none());
    assert_eq!(setup_status(&client, addr).await["can_start"], false);
    assert!(
        state.db.get_state(&key).unwrap().is_none(),
        "polling wrote a checkpoint"
    );
    // The request still authenticates normally; corrupt only this fixture's
    // operation document so a failed read cannot be reported as idle.
    let op = state
        .db
        .create_import_operation(&repo_id, "fixture", "broken", "fixture")
        .unwrap();
    state
        .db
        .set_state(
            &format!("import_operation_v1:document:{}", op.id),
            "malformed",
        )
        .unwrap();
    let response = client
        .get(format!("http://{addr}/api/setup/import/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    server.abort();
}

fn complete_fixture_setup(db: &Database, repo_id: &str, op_id: &str) {
    db.note_import_local(repo_id, op_id, 1, "fixture-tip", 1, 1)
        .unwrap();
    db.begin_import_publication(repo_id, op_id, "refs/heads/main", "fixture-tip")
        .unwrap();
    db.confirm_import_publication(repo_id, op_id, "fixture-tip")
        .unwrap();
    db.complete_import_operation(repo_id, op_id, 1, "fixture-tip")
        .unwrap();
}

#[tokio::test]
async fn rs19_completed_setup_history_and_new_start_display_their_own_repository() {
    let (addr, state, server, _tmp, id, _) = setup_wizard_fixture().await;
    let mut other = state.db.get_repository(&id).unwrap().unwrap();
    other.id = "next-start-target".into();
    other.name = "AAA pending".into();
    let old = state
        .db
        .create_setup_import_operation(&id, "fixture", "finished", "setup-target")
        .unwrap();
    state.db.start_import_operation(&id, &old.id).unwrap();
    complete_fixture_setup(&state.db, &id, &old.id);
    state.db.insert_repository(&other).unwrap();
    let observed = setup_status(&authed_client(), addr).await;
    assert_eq!(observed["operation_id"], old.id, "{observed}");
    assert_eq!(observed["operation_repository_id"], id);
    assert_eq!(observed["lifecycle"], "completed");
    assert_eq!(observed["can_start"], true);
    assert_eq!(observed["start_repository_id"], other.id);
    assert_eq!(observed["start_repository_name"], other.name);
    let mut newer = other.clone();
    newer.id = "target-after-reordering".into();
    newer.name = "000 now first".into();
    state.db.insert_repository(&newer).unwrap();
    let refused: serde_json::Value = authed_client()
        .post(format!("http://{addr}/api/setup/import"))
        .query(&[("expected_repository_id", &other.id)])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(refused["message"]
        .as_str()
        .unwrap()
        .contains("selection changed"));
    assert!(state
        .db
        .latest_import_operation(&newer.id)
        .unwrap()
        .is_none());
    assert!(state
        .db
        .latest_import_operation(&other.id)
        .unwrap()
        .is_none());
    assert_eq!(
        state.db.setup_import_owner().unwrap().unwrap().operation.id,
        old.id
    );
    server.abort();
}

#[tokio::test]
async fn rs19_stale_stop_never_cancels_a_successor_or_signals_its_worker() {
    let (addr, state, server, tmp, repo_id, _bare) = setup_wizard_fixture().await;
    let repo = state.db.get_repository(&repo_id).unwrap().unwrap();
    let fingerprint = reposync_core::db::import_operations::import_target_fingerprint(
        &repo,
        &tmp.path().join("git-repo"),
    );
    let old = state
        .db
        .create_setup_import_operation(&repo_id, "fixture", "first", &fingerprint)
        .unwrap();
    state.db.start_import_operation(&repo_id, &old.id).unwrap();
    let client = authed_client();
    let mut slot = state.import_progress.write().await;
    slot.setup_operation_id = Some(old.id.clone());
    slot.phase = ImportPhase::Importing;
    let mut request = Box::pin(cancel_selected(&client, addr, &old.id));
    // Deliver the HTTP request while the worker slot is held. It must validate
    // both the durable identity and the owning signal after the slot changes.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut request)
            .await
            .is_err()
    );
    complete_fixture_setup(&state.db, &repo_id, &old.id);
    let next = state
        .db
        .create_setup_import_operation(&repo_id, "fixture", "second", &fingerprint)
        .unwrap();
    *slot = ImportProgress::default();
    slot.setup_operation_id = Some(next.id.clone());
    slot.phase = ImportPhase::Connecting;
    let next_signal = slot.cancel_signal.clone();
    drop(slot);
    let stale = request.await;
    assert_eq!(stale["ok"], false, "{stale}");
    assert!(stale.get("operation_id").is_none());
    assert_eq!(cancel_selected(&client, addr, "unknown").await["ok"], false);
    assert!(!next_signal.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(
        state
            .db
            .get_import_operation(&repo_id, &next.id)
            .unwrap()
            .unwrap(),
        next
    );
    let current = setup_status(&client, addr).await;
    assert_eq!(current["operation_id"], next.id);
    assert_eq!(current["has_worker_progress"], true);
    assert_eq!(current["can_start"], false);
    assert_eq!(current["can_cancel"], true);
    let stop = cancel_selected(&client, addr, &next.id).await;
    assert_eq!(stop["ok"], true, "{stop}");
    assert_eq!(stop["lifecycle"], "cancel_requested");
    assert_eq!(stop["operation_id"], next.id);
    assert!(next_signal.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(setup_status(&client, addr).await["can_cancel"], false);
    assert_eq!(
        cancel_selected(&client, addr, &next.id).await["lifecycle"],
        "cancel_requested"
    );
    server.abort();
}

#[tokio::test]
async fn rs19_setup_does_not_expose_or_cancel_managed_full_or_snapshot_imports() {
    let (addr, state, server, tmp, repo_id, _bare) = setup_wizard_fixture().await;
    let repo = state.db.get_repository(&repo_id).unwrap().unwrap();
    let fingerprint = reposync_core::db::import_operations::import_target_fingerprint(
        &repo,
        &tmp.path().join("repos").join(&repo_id).join("git-repo"),
    );
    let op = state
        .db
        .create_import_operation(&repo_id, "fixture", "managed", &fingerprint)
        .unwrap();
    state.db.start_import_operation(&repo_id, &op.id).unwrap();
    state
        .db
        .note_import_local(&repo_id, &op.id, 3, "foreign-tip", 1, 1)
        .unwrap();
    let client = authed_client();
    let signal = {
        let mut p = state.import_progress.write().await;
        p.setup_operation_id = Some("unrelated-setup-worker".into());
        p.phase = ImportPhase::Importing;
        p.commits_created = 91;
        p.errors.push("unrelated worker detail".into());
        p.cancel_signal.clone()
    };
    for snapshot in [false, true] {
        if snapshot {
            state
                .db
                .mark_snapshot_import_request(&repo_id, &op.id)
                .unwrap();
        }
        let before = state
            .db
            .get_import_operation(&repo_id, &op.id)
            .unwrap()
            .unwrap();
        let status = setup_status(&client, addr).await;
        assert_eq!(status["busy"], true);
        assert_eq!(status["can_cancel"], false);
        assert_eq!(status["can_start"], false);
        assert_eq!(status["has_worker_progress"], false);
        assert!(status.get("operation_id").is_none());
        assert!(status.get("last_local_svn_rev").is_none());
        assert!(status.get("last_local_git_sha").is_none());
        assert_eq!(status["commits_created"], 0);
        assert_eq!(status["errors"], serde_json::json!([]));
        assert_eq!(cancel_selected(&client, addr, &op.id).await["ok"], false);
        assert_eq!(cancel_setup(&client, addr).await["ok"], false);
        assert_eq!(
            state
                .db
                .get_import_operation(&repo_id, &op.id)
                .unwrap()
                .unwrap(),
            before
        );
        assert!(!signal.load(std::sync::atomic::Ordering::Acquire));
    }
    server.abort();
}

#[tokio::test]
async fn rs19_reopened_wizard_identifies_terminal_operation_after_settings_and_order_change() {
    let (_addr, state, server, tmp, id, _) = setup_wizard_fixture().await;
    let mut original = state.db.get_repository(&id).unwrap().unwrap();
    let fingerprint = reposync_core::db::import_operations::import_target_fingerprint(
        &original,
        &tmp.path().join("git-repo"),
    );
    let op = state
        .db
        .create_setup_import_operation(&id, "fixture", "setup", &fingerprint)
        .unwrap();
    state.db.start_import_operation(&id, &op.id).unwrap();
    state
        .db
        .note_import_local(&id, &op.id, 4, "original-checkpoint", 1, 1)
        .unwrap();
    state
        .db
        .finish_import_operation(
            &id,
            &op.id,
            ImportOperationState::Cancelled,
            "held at original checkpoint",
        )
        .unwrap();
    original.name = "ZZ renamed".into();
    original.allowed_paths = Some("[\"modified/\"]".into());
    original.lfs_threshold_mb += 1;
    original.auto_merge = !original.auto_merge;
    state.db.update_repository(&original).unwrap();
    assert_ne!(
        fingerprint,
        reposync_core::db::import_operations::import_target_fingerprint(
            &original,
            &tmp.path().join("git-repo")
        )
    );
    let mut other = original.clone();
    other.id = "other".into();
    other.name = "AA new registration".into();
    state.db.insert_repository(&other).unwrap();
    let foreign = state
        .db
        .create_import_operation(&other.id, "fixture", "managed", "managed-target")
        .unwrap();
    let config = state.config.clone();
    server.abort();
    let _ = server.await;
    drop(state);
    let db = Database::new(tmp.path().join("reposync.db")).unwrap();
    let (addr, fresh, server) = serve_wizard_fixture(tmp.path(), config, db).await;
    let client = authed_client();
    assert_eq!(fresh.db.list_repositories().unwrap()[0].id, other.id);
    let observed = setup_status(&client, addr).await;
    assert_eq!(observed["operation_id"], op.id, "{observed}");
    assert_eq!(observed["lifecycle"], "cancelled");
    assert_eq!(observed["can_start"], false);
    assert_eq!(observed["can_cancel"], false);
    assert_eq!(observed["has_worker_progress"], false);
    assert_eq!(observed["last_local_svn_rev"], 4);
    assert_eq!(observed["last_local_git_sha"], "original-checkpoint");
    assert_eq!(
        cancel_selected(&client, addr, &op.id).await["lifecycle"],
        "cancelled"
    );
    assert_eq!(
        cancel_selected(&client, addr, &foreign.id).await["ok"],
        false
    );
    assert_eq!(
        fresh
            .db
            .get_import_operation(&other.id, &foreign.id)
            .unwrap()
            .unwrap(),
        foreign
    );
    server.abort();
}

#[tokio::test]
async fn rs19_only_one_setup_admission_across_two_repositories_and_connections() {
    let (_addr, state, server, tmp, id, _) = setup_wizard_fixture().await;
    let mut other = state.db.get_repository(&id).unwrap().unwrap();
    other.id = "other".into();
    other.name = "other".into();
    state.db.insert_repository(&other).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let contenders: Vec<_> = [id.clone(), other.id.clone()]
        .into_iter()
        .map(|candidate| {
            let db = Database::new(tmp.path().join("reposync.db")).unwrap();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                db.create_setup_import_operation(&candidate, "fixture", "new", "setup-target")
                    .is_ok()
            })
        })
        .collect();
    let results: Vec<_> = contenders
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|won| **won).count(), 1);
    let owner = state.db.setup_import_owner().unwrap().unwrap();
    assert!(owner.holds_slot);
    let other_id = if owner.operation.repo_id == id {
        &other.id
    } else {
        &id
    };
    assert!(state
        .db
        .create_setup_import_operation(other_id, "fixture", "retry", "setup-target")
        .is_err());
    assert!(state
        .db
        .latest_import_operation(other_id)
        .unwrap()
        .is_none());
    state
        .db
        .create_import_operation(other_id, "fixture", "managed", "managed-target")
        .unwrap();
    assert_eq!(
        state.db.setup_import_owner().unwrap().unwrap().operation.id,
        owner.operation.id
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64c07_setup_wizard_held_operation_refuses_duplicate_start() {
    if !svn_available() {
        eprintln!("SKIP: svnadmin not available");
        return;
    }
    let (addr, state, server, tmp, repo_id, _bare) = setup_wizard_fixture().await;
    let workdir = tmp.path().join("git-repo");
    let fingerprint = reposync_core::db::import_operations::import_target_fingerprint(
        &state.db.get_repository(&repo_id).unwrap().unwrap(),
        &workdir,
    );
    let held = state
        .db
        .create_import_operation(&repo_id, "legacy", "held-request", &fingerprint)
        .unwrap();
    state
        .db
        .finish_import_operation(
            &repo_id,
            &held.id,
            ImportOperationState::Cancelled,
            "held for inspection; published history is not undone",
        )
        .unwrap();
    assert!(!reposync_core::busy::is_busy(&repo_id));

    let client = authed_client();
    let status = setup_status(&client, addr).await;
    assert_eq!(status["operation_id"], held.id);
    assert_eq!(status["lifecycle"], "cancelled");
    assert_eq!(status["can_start"], false);
    assert_eq!(status["can_cancel"], false);
    assert_eq!(status["has_worker_progress"], false);
    assert_eq!(status["busy"], false, "{status}");

    let start: serde_json::Value = client
        .post(format!("http://{addr}/api/setup/import"))
        .header("x-request-id", "must-not-double-start")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(start["ok"], false, "{start}");
    assert_eq!(start["operation_id"], held.id);

    let cancel: serde_json::Value = client
        .post(format!("http://{addr}/api/setup/import/cancel"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancel["ok"], true, "{cancel}");
    assert_eq!(cancel["lifecycle"], "cancelled");
    assert!(
        cancel["message"]
            .as_str()
            .unwrap_or("")
            .contains("not undone"),
        "{cancel}"
    );
    assert_eq!(
        state
            .db
            .active_import_operation(&repo_id)
            .unwrap()
            .unwrap()
            .id,
        held.id
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64c07_setup_wizard_failed_clone_refuses_replacement_and_holds() {
    if !svn_available() {
        eprintln!("SKIP: svnadmin not available");
        return;
    }
    let (addr, state, server, tmp, repo_id, bare) = setup_wizard_fixture().await;
    // Clone target is gone: GitClient::clone_repo must fail. The old path then
    // git-inited a replacement, added origin, and started a full import.
    assert!(std::fs::remove_dir_all(&bare).is_ok());
    let workdir = tmp.path().join("git-repo");
    assert!(!workdir.join(".git").exists());

    let client = authed_client();
    let started = client
        .post(format!("http://{addr}/api/setup/import"))
        .header("x-request-id", "setup-clone-fail")
        .send()
        .await
        .unwrap();
    let status = started.status();
    let body: serde_json::Value = started.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{status} {body}");
    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("could not be cloned") && error.contains("held"),
        "{body}"
    );
    assert_ne!(body["ok"], true, "{body}");

    let held = state
        .db
        .active_import_operation(&repo_id)
        .unwrap()
        .expect("enrolled operation must remain held");
    assert_eq!(held.state, ImportOperationState::ReconciliationRequired);
    assert_eq!(held.request_id, "setup-clone-fail");
    assert_eq!(
        state
            .db
            .get_repository(&repo_id)
            .unwrap()
            .unwrap()
            .last_svn_rev,
        0
    );
    assert!(!reposync_core::busy::is_busy(&repo_id));

    let progress = state.import_progress.read().await.clone();
    assert_eq!(progress.phase, ImportPhase::Failed);
    assert_eq!(progress.commits_created, 0);

    // Failed clone must not become a replacement that the importer then fills.
    // A leftover dest from the clone attempt itself is not a successful import.
    if workdir.join(".git").exists() {
        let commits = Command::new("git")
            .args([
                "-C",
                workdir.to_str().unwrap(),
                "rev-list",
                "--all",
                "--count",
            ])
            .output()
            .unwrap();
        let count = String::from_utf8_lossy(&commits.stdout).trim().to_string();
        assert!(
            count == "0" || !commits.status.success(),
            "importer must not run on a replacement: {count}"
        );
    }
    assert!(!workdir.join("history.txt").exists());

    let retry: serde_json::Value = client
        .post(format!("http://{addr}/api/setup/import"))
        .header("x-request-id", "setup-clone-fail-retry")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry["ok"], false, "{retry}");
    assert_eq!(retry["operation_id"], held.id);
    server.abort();
}

fn exec_witness(label: &str, path: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map(|meta| format!("{:o}", meta.permissions().mode()))
        .unwrap_or_else(|error| format!("metadata {error}"));
    let test_x = Command::new("test")
        .arg("-x")
        .arg(path)
        .status()
        .map(|status| {
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".into())
        })
        .unwrap_or_else(|error| format!("spawn {error}"));
    format!(
        "{label} path={} mode={mode} test_x={test_x}",
        path.display()
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64c07_setup_wizard_reset_reimport_failed_clone_holds() {
    if !svn_available() {
        eprintln!("SKIP: svnadmin not available");
        return;
    }
    let (addr, state, server, tmp, repo_id, bare) = setup_wizard_fixture().await;
    // Clone target is gone: after enrollment, spawn_import_task must fail
    // the same way start_import does, not leave Queued with no worker.
    assert!(std::fs::remove_dir_all(&bare).is_ok());
    let workdir = tmp.path().join("git-repo");
    assert!(!workdir.join(".git").exists());

    // Force-push is out of scope. Succeed that git push so enrollment and
    // the spawn_import_task clone run against the missing target.
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real_git = String::from_utf8(real_git.stdout)
        .unwrap()
        .trim()
        .to_string();
    assert!(!real_git.is_empty(), "git binary required");
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"push\" ]; then\n  exit 0\nfi\nexec {real_git} \"$@\"\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&wrapper).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&wrapper, perms).unwrap();
    }
    let prev_path = std::env::var("PATH").unwrap();
    std::env::set_var("PATH", format!("{}:{prev_path}", bin.display()));
    let resolved = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .map(|output| {
            format!(
                "status {:?} stdout {} stderr {}",
                output.status,
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })
        .unwrap_or_else(|error| format!("spawn {error}"));
    let shim_probe = Command::new(&wrapper).arg("version").output();
    let shim_probe = match shim_probe {
        Err(error) => format!(
            "errno kind {:?} raw {:?}",
            error.kind(),
            error.raw_os_error()
        ),
        Ok(output) => format!(
            "status {:?} stderr {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    };
    let shim_exec = exec_witness("shim", &wrapper);
    std::env::set_var("GIT_AUTHOR_NAME", "RepoSync Test");
    std::env::set_var("GIT_AUTHOR_EMAIL", "reposync@localhost");
    std::env::set_var("GIT_COMMITTER_NAME", "RepoSync Test");
    std::env::set_var("GIT_COMMITTER_EMAIL", "reposync@localhost");

    let client = authed_client();
    let started = client
        .post(format!("http://{addr}/api/setup/reset-reimport"))
        .send()
        .await
        .unwrap();
    std::env::set_var("PATH", prev_path);
    let status = started.status();
    let body: serde_json::Value = started.json().await.unwrap();
    let text = body["message"]
        .as_str()
        .or_else(|| body["error"].as_str())
        .unwrap_or("");
    let phase = if text.contains("git push failed") {
        "push"
    } else if text.contains("could not be cloned") {
        "clone"
    } else {
        "other"
    };
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_REQUEST,
        "phase={phase} resolved={resolved} shim_probe={shim_probe} {shim_exec} {status} {body}"
    );
    let error = body["error"].as_str().unwrap_or("");
    assert!(
        error.contains("could not be cloned") && error.contains("held"),
        "{body}"
    );
    assert_ne!(body["ok"], true, "{body}");

    let held = state
        .db
        .active_import_operation(&repo_id)
        .unwrap()
        .expect("enrolled reset-reimport operation must remain held");
    assert_eq!(held.state, ImportOperationState::ReconciliationRequired);
    assert!(held.request_id.starts_with("setup-reset-"), "{held:?}");
    assert!(!reposync_core::busy::is_busy(&repo_id));

    let progress = state.import_progress.read().await.clone();
    assert_eq!(progress.phase, ImportPhase::Failed);
    assert_eq!(progress.commits_created, 0);

    if workdir.join(".git").exists() {
        let commits = Command::new("git")
            .args([
                "-C",
                workdir.to_str().unwrap(),
                "rev-list",
                "--all",
                "--count",
            ])
            .output()
            .unwrap();
        let count = String::from_utf8_lossy(&commits.stdout).trim().to_string();
        assert!(
            count == "0" || !commits.status.success(),
            "importer must not run on a replacement: {count}"
        );
    }
    assert!(!workdir.join("history.txt").exists());

    let retry_start: serde_json::Value = client
        .post(format!("http://{addr}/api/setup/import"))
        .header("x-request-id", "setup-reset-clone-fail-retry")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry_start["ok"], false, "{retry_start}");
    assert_eq!(retry_start["operation_id"], held.id);

    let retry_reset: serde_json::Value = client
        .post(format!("http://{addr}/api/setup/reset-reimport"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry_reset["ok"], false, "{retry_reset}");
    assert_eq!(retry_reset["operation_id"], held.id);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64_setup_wizard_preparation_hold_persist_failure_is_not_acknowledged() {
    if !svn_available() {
        eprintln!("SKIP: svnadmin not available");
        return;
    }
    let (addr, state, server, _tmp, repo_id, bare) = setup_wizard_fixture().await;
    assert!(std::fs::remove_dir_all(&bare).is_ok());
    state
        .db
        .conn()
        .execute_batch(
            "CREATE TRIGGER reject_setup_preparation_finish
        BEFORE UPDATE OF value ON kv_state
        WHEN NEW.key LIKE 'import_operation_v1:document:%'
          AND json_extract(NEW.value, '$.state') = 'reconciliation_required'
        BEGIN SELECT RAISE(FAIL, 'fixture preparation finish failure'); END;",
        )
        .unwrap();

    let client = authed_client();
    let started = client
        .post(format!("http://{addr}/api/setup/import"))
        .header("x-request-id", "setup-prep-hold-persist-fail")
        .send()
        .await
        .unwrap();
    let status = started.status();
    let body: serde_json::Value = started.json().await.unwrap();
    assert_eq!(
        status,
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        "{status} {body}"
    );
    assert_eq!(
        body["error"].as_str().unwrap_or(""),
        "internal server error",
        "persistence failure must surface as operator-visible 500, not a held clone error: {body}"
    );

    let held = state
        .db
        .active_import_operation(&repo_id)
        .unwrap()
        .expect("active import pointer must remain while hold is unconfirmed");
    assert_eq!(held.state, ImportOperationState::Queued);
    assert_eq!(held.request_id, "setup-prep-hold-persist-fail");
    assert!(!reposync_core::busy::is_busy(&repo_id));

    let progress = state.import_progress.read().await.clone();
    assert_eq!(progress.phase, ImportPhase::Failed);

    let retry: serde_json::Value = client
        .post(format!("http://{addr}/api/setup/import"))
        .header("x-request-id", "setup-prep-hold-persist-fail-retry")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry["ok"], false, "{retry}");
    assert_eq!(retry["operation_id"], held.id);
    server.abort();
}

#[cfg(feature = "reliability-fixture")]
async fn wait_for_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {}", path.display()));
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64c07_setup_wizard_connecting_barrier_busy_and_cancel() {
    if !svn_available() {
        eprintln!("SKIP: svnadmin not available");
        return;
    }
    let (addr, state, server, tmp, repo_id, bare) = setup_wizard_fixture().await;
    let barrier = tmp.path().join(&repo_id);
    std::fs::create_dir(&barrier).unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
    std::env::set_var("REPOSYNC_IMPORT_BARRIER_DIR", &barrier);
    let client = authed_client();
    let started: serde_json::Value = client
        .post(format!("http://{addr}/api/setup/import"))
        .query(&[("expected_repository_id", &repo_id)])
        .header("x-request-id", "setup-connecting")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(started["ok"], true, "{started}");
    let op_id = started["operation_id"].as_str().unwrap();
    wait_for_file(&barrier.join("connecting.ready")).await;
    assert!(reposync_core::busy::is_busy(&repo_id));
    // A name-sorted repository read no longer puts the enrolled worker first.
    let mut other = state.db.get_repository(&repo_id).unwrap().unwrap();
    other.id = "another-registration".into();
    other.name = "AAA comes first".into();
    state.db.insert_repository(&other).unwrap();
    assert_eq!(state.db.list_repositories().unwrap()[0].id, other.id);
    let status = setup_status(&client, addr).await;
    assert_eq!(status["busy"], true, "{status}");
    assert_eq!(status["operation_id"], op_id);
    assert!(
        status["lifecycle"] == "queued" || status["lifecycle"] == "running",
        "{status}"
    );
    let duplicate: serde_json::Value = client
        .post(format!("http://{addr}/api/setup/import"))
        .header("x-request-id", "setup-connecting-other")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(duplicate["ok"], false, "{duplicate}");
    assert_eq!(status["can_cancel"], true);
    let cancel = cancel_selected(&client, addr, op_id).await;
    assert_eq!(cancel["lifecycle"], "cancel_requested", "{cancel}");
    let terminal = wait_terminal(&client, addr, op_id, &repo_id).await;
    assert_eq!(terminal["lifecycle"], "cancelled", "{terminal}");
    assert_eq!(terminal["busy"], false, "{terminal}");
    assert!(!reposync_core::busy::is_busy(&repo_id));
    assert!(terminal["last_local_svn_rev"].is_null() || terminal["last_local_svn_rev"] == 0);
    assert!(!Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show-ref",
            "--verify",
            "refs/heads/main"
        ])
        .status()
        .unwrap()
        .success());
    let retry: serde_json::Value = client
        .post(format!("http://{addr}/api/setup/import"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retry["ok"], false, "{retry}");
    assert!(state
        .db
        .active_import_operation(&repo_id)
        .unwrap()
        .is_some());
    for key in ["REPOSYNC_IMPORT_BARRIER_DIR", "REPOSYNC_FIXTURE_ROOT"] {
        std::env::remove_var(key);
    }
    server.abort();
}

fn git_origin_url(repo_path: &Path) -> String {
    String::from_utf8_lossy(
        &Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(repo_path)
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string()
}

fn prepare_config_git_repo(workdir: &Path, bare: &Path, origin_url: &str) {
    if workdir.exists() {
        std::fs::remove_dir_all(workdir).ok();
    }
    assert!(Command::new("git")
        .args(["clone", bare.to_str().unwrap(), workdir.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["remote", "set-url", "origin", origin_url])
        .current_dir(workdir)
        .status()
        .unwrap()
        .success());
}

/// RS-11 / #63: setup import must not embed the first managed repo's git token
/// on the legacy config remote at `data_dir/git-repo`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_rs11_setup_import_config_remote_ignores_managed_chain() {
    if !svn_available() {
        eprintln!("SKIP: svnadmin not available");
        return;
    }
    let (addr, state, server, tmp, repo_id, bare) = setup_wizard_fixture().await;
    let managed_token = "managed-setup-import-token";
    state
        .db
        .set_state(&format!("secret_git_token_{}", repo_id), managed_token)
        .unwrap();

    let workdir = tmp.path().join("git-repo");
    let config_origin = "http://x-access-token:configtok@git.test.invalid/local/history.git";
    prepare_config_git_repo(&workdir, &bare, config_origin);

    let client = authed_client();
    let started = client
        .post(format!("http://{addr}/api/setup/import"))
        .header("x-request-id", "rs11-setup-import-managed-chain")
        .send()
        .await
        .unwrap();
    assert!(
        started.status().is_success(),
        "{}",
        started.text().await.unwrap()
    );

    let url = git_origin_url(&workdir);
    assert_eq!(url, config_origin);
    assert!(
        url.contains("configtok"),
        "setup import must preserve config remote userinfo: {url}"
    );
    assert!(
        !url.contains(managed_token),
        "setup import must not embed managed repo token on config remote: {url}"
    );

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS11_SETUP_IMPORT_CONFIG_REMOTE_IGNORES_MANAGED_CHAIN",
            "config_remote_unchanged":true,
            "foreign_token_embedded":false
        })
    );
    server.abort();
}

/// RS-11 / #63: setup import must not strip the config remote when the managed
/// repo chain carries an explicit empty git-token key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_rs11_setup_import_config_remote_keeps_origin_on_revocation() {
    if !svn_available() {
        eprintln!("SKIP: svnadmin not available");
        return;
    }
    let (addr, state, server, tmp, repo_id, bare) = setup_wizard_fixture().await;
    state
        .db
        .set_state(&format!("secret_git_token_{}", repo_id), "")
        .unwrap();

    let workdir = tmp.path().join("git-repo");
    let config_origin = "http://x-access-token:configtok@git.test.invalid/local/history.git";
    prepare_config_git_repo(&workdir, &bare, config_origin);

    let client = authed_client();
    let started = client
        .post(format!("http://{addr}/api/setup/import"))
        .header("x-request-id", "rs11-setup-import-revocation")
        .send()
        .await
        .unwrap();
    assert!(
        started.status().is_success(),
        "{}",
        started.text().await.unwrap()
    );

    let url = git_origin_url(&workdir);
    assert_eq!(
        url, config_origin,
        "explicit managed-repo revocation must not strip config remote origin"
    );
    assert!(
        url.contains("configtok"),
        "explicit managed-repo revocation must not strip config remote userinfo: {url}"
    );

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS11_SETUP_IMPORT_CONFIG_REMOTE_KEEPS_ORIGIN_ON_REVOCATION",
            "config_remote_unchanged":true,
            "origin_stripped":false
        })
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs113_setup_probe_rejects_embedded_credentials_without_public_disclosure() {
    let (addr, _state, server, _tmp, _repo_id, _bare) = setup_wizard_fixture().await;
    let client = authed_client();
    // An open listener is a spawn/connection canary. URL validation must refuse
    // before invoking SVN; the response must not echo either userinfo spelling.
    let canary = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    for userinfo in ["alice:synthetic-secret", "alice%3Asynthetic%2Dsecret"] {
        let result = client.post(format!("http://{addr}/api/setup/test-svn"))
            .json(&serde_json::json!({"url":format!("http://{userinfo}@{}/repo",canary.local_addr().unwrap()),"username":"","password":""}))
            .send().await.unwrap();
        assert!(result.status().is_success());
        let body: serde_json::Value = result.json().await.unwrap();
        assert_eq!(body["ok"], false);
        let text = body["message"].as_str().unwrap();
        assert!(text.contains("unsupported SVN URL authority"), "{text}");
        assert!(text.contains("REDACTED"), "{text}");
        assert!(
            !text.contains("synthetic-secret") && !text.contains("synthetic%2Dsecret"),
            "{text}"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), canary.accept())
                .await
                .is_err(),
            "rejected URL must not open an SVN connection"
        );
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rs113_repo_probe_and_full_import_use_the_same_literal_branch() {
    let (addr, state, server, tmp, repo_id, bare) = setup_wizard_fixture().await;
    let client = authed_client();
    let mut repo = state.db.get_repository(&repo_id).unwrap().unwrap();
    let root = SvnClient::new(&repo.svn_url, "fixture", "");
    let intended_url =
        reposync_core::svn::operand::configured_branch_url(&repo.svn_url, "branches/a%40b 雪#?")
            .unwrap();
    let decoy_url =
        reposync_core::svn::operand::configured_branch_url(&repo.svn_url, "branches/a@b 雪#?")
            .unwrap();
    // Independent nodes have no copy-history path relocation. That separate
    // full-history-import limitation is not hidden by this operand regression.
    for (index, (url, content)) in [
        (intended_url, "literal import\n"),
        (decoy_url.clone(), "decoy import\n"),
    ]
    .iter()
    .enumerate()
    {
        let target = reposync_core::svn::operand::literal_url(url, None).unwrap();
        let made = Command::new("svn")
            .args([
                "mkdir",
                "--parents",
                "-m",
                "independent branch layout",
                "--",
                &target,
            ])
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        let svn = SvnClient::new(url, "fixture", "");
        let wc = tmp.path().join(format!("branch-{index}"));
        svn.checkout_head(&wc).await.unwrap();
        std::fs::write(wc.join("history.txt"), content).unwrap();
        svn.add(&wc, &["history.txt"]).await.unwrap();
        svn.commit(&wc, "distinct branch payload", "fixture")
            .await
            .unwrap();
    }
    repo.svn_branch = "branches/a%40b 雪#?".into();
    state.db.update_repository(&repo).unwrap();
    let revision = root.info().await.unwrap().latest_rev;
    let probe: serde_json::Value = client
        .post(format!("http://{addr}/api/repos/{repo_id}/test-svn"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(probe["ok"], true, "{probe}");
    let started: serde_json::Value = client
        .post(format!("http://{addr}/api/repos/{repo_id}/import"))
        .header("x-request-id", "rs113-literal-import")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(started["ok"], true, "{started}");
    let completed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let status: serde_json::Value = client
                .get(format!("http://{addr}/api/repos/{repo_id}/import/status"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if matches!(
                status["lifecycle"].as_str(),
                Some("completed" | "failed" | "reconciliation_required")
            ) && !reposync_core::busy::is_busy(&repo_id)
            {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("literal import must finish with observed idle");
    assert_eq!(completed["lifecycle"], "completed", "{completed}");
    let git = git2::Repository::open_bare(&bare).unwrap();
    let commit = git
        .find_reference("refs/heads/main")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    let tree = commit.tree().unwrap();
    let entry = tree.get_path(Path::new("history.txt")).unwrap();
    assert_eq!(
        git.find_blob(entry.id()).unwrap().content(),
        b"literal import\n"
    );
    let checkpoint = state.db.get_repo_watermark(&repo_id).unwrap();
    assert_eq!(checkpoint.1, commit.id().to_string());
    assert!(checkpoint.0 >= 3 && checkpoint.0 <= revision);
    let mapped: i64 = state.db.conn().query_row("SELECT COUNT(*) FROM sync_records WHERE repo_id=?1 AND direction='svn_to_git' AND git_sha=?2 AND status='applied'",rusqlite::params![&repo_id,checkpoint.1],|r|r.get(0)).unwrap();
    assert_eq!(mapped, 1);
    assert_eq!(
        root.info().await.unwrap().latest_rev,
        revision,
        "import must not mutate SVN or decoy"
    );
    assert_eq!(
        SvnClient::new(decoy_url, "fixture", "")
            .cat("history.txt", revision)
            .await
            .unwrap(),
        "decoy import\n"
    );
    server.abort();
    let _ = server.await;
}
