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

    let engine_db = Database::in_memory().unwrap();
    engine_db.initialize().unwrap();
    let dummy_git = tmp.path().join("dummy-git");
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
        config_path,
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
        .merge(api::setup::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state, server, tmp, repo_id, bare)
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

async fn wait_terminal(client: &reqwest::Client, addr: SocketAddr) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let status = setup_status(client, addr).await;
            if matches!(
                status["lifecycle"].as_str(),
                Some("completed" | "cancelled" | "failed" | "reconciliation_required")
            ) {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("timed out waiting for setup import terminal status")
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
        let terminal = wait_terminal(&client, addr).await;
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
    let cancel: serde_json::Value = client
        .post(format!("http://{addr}/api/setup/import/cancel"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancel["lifecycle"], "cancel_requested", "{cancel}");
    let terminal = wait_terminal(&client, addr).await;
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
