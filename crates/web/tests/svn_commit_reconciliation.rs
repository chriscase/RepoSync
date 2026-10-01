//! Authorized #64-C Git→SVN commit reconciliation against a real team pair.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::Router;
use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::svn_commit_operations::SvnCommitOperationState;
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::ImportProgress;
use reposync_core::models::{Repository, Session, User};
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_web::api;
use reposync_web::AppState;

const TEST_TOKEN: &str = "test-session-token-for-64c";

fn svn_available() -> bool {
    Command::new("svn")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && Command::new("svnadmin")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn create_svn_repo(dir: &Path) -> String {
    let repo_dir = dir.join("svn_repo");
    assert!(Command::new("svnadmin")
        .args(["create", repo_dir.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let hook = repo_dir.join("hooks/pre-revprop-change");
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!("file://{}", repo_dir.display())
}

fn svn_commit_file(wc: &Path, name: &str, content: &str, message: &str) {
    std::fs::write(wc.join(name), content).unwrap();
    let status = Command::new("svn")
        .args(["status", wc.join(name).to_str().unwrap()])
        .output()
        .unwrap();
    if String::from_utf8_lossy(&status.stdout).contains('?') {
        assert!(Command::new("svn")
            .args(["add", wc.join(name).to_str().unwrap()])
            .status()
            .unwrap()
            .success());
    }
    assert!(Command::new("svn")
        .args([
            "commit",
            "-m",
            message,
            wc.to_str().unwrap(),
            "--username",
            "fixture",
            "--non-interactive",
        ])
        .status()
        .unwrap()
        .success());
}

struct SvnCommitFaultGuard {
    scoped_key: String,
}

impl SvnCommitFaultGuard {
    fn lost_reply(repo_id: &str) -> Self {
        let key = "REPOSYNC_SVN_COMMIT_LOST_REPLY";
        let scoped_key = format!("{}__{}", key, repo_id);
        std::env::set_var(&scoped_key, "1");
        Self { scoped_key }
    }
}

impl Drop for SvnCommitFaultGuard {
    fn drop(&mut self) {
        std::env::remove_var(&self.scoped_key);
    }
}

struct HeldPair {
    addr: std::net::SocketAddr,
    state: Arc<AppState>,
    server: tokio::task::JoinHandle<()>,
    _tmp: tempfile::TempDir,
    id: String,
    operation_id: String,
    svn_url: String,
}

impl HeldPair {
    async fn lost_reply(repo_id: &str) -> Self {
        assert!(svn_available());
        let tmp = tempfile::tempdir().unwrap();
        let svn_url = create_svn_repo(tmp.path());
        let wc = tmp.path().join("wc");
        assert!(Command::new("svn")
            .args([
                "checkout",
                &svn_url,
                wc.to_str().unwrap(),
                "--non-interactive"
            ])
            .status()
            .unwrap()
            .success());
        svn_commit_file(&wc, ".gitkeep", "", "anchor");
        svn_commit_file(&wc, "origin.txt", "SVN origin\n", "origin");

        let bridge = tmp.path().join("bridge");
        let bare = tmp.path().join("origin.git");
        git2::Repository::init(&bridge).unwrap();
        git_cli(&bridge, &["config", "user.name", "Fixture"]);
        git_cli(
            &bridge,
            &["config", "user.email", "fixture@example.invalid"],
        );
        std::fs::write(bridge.join("README"), "seed\n").unwrap();
        git_cli(&bridge, &["add", "README"]);
        git_cli(&bridge, &["commit", "-m", "seed"]);
        git_cli(&bridge, &["branch", "-M", "main"]);
        Command::new("git")
            .args([
                "clone",
                "--bare",
                bridge.to_str().unwrap(),
                bare.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        git_cli(
            &bridge,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        git_cli(&bridge, &["push", "-u", "origin", "main"]);
        let initial = Command::new("git")
            .arg("-C")
            .arg(&bridge)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let initial = String::from_utf8_lossy(&initial.stdout).trim().to_string();

        let db_path = tmp.path().join("sync.db");
        let db = Database::new(&db_path).unwrap();
        db.initialize().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        db.insert_repository(&Repository {
            id: repo_id.into(),
            name: "64c pair".into(),
            svn_url: svn_url.clone(),
            svn_branch: "".into(),
            svn_username: "fixture".into(),
            git_provider: "local".into(),
            git_api_url: "".into(),
            git_repo: bare.to_string_lossy().to_string(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 5,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now,
            last_svn_rev: 1,
            last_git_sha: initial,
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

        let toml = format!(
            r#"
[daemon]
data_dir = "{}"
[svn]
url = "{svn_url}"
username = ""
password_env = ""
[github]
repo = "test/test-repo"
token_env = ""
"#,
            tmp.path().display()
        );
        let mut config: AppConfig = toml::from_str(&toml).unwrap();
        config.web.admin_password = Some("test-admin-pass".into());
        config.svn.password = Some(String::new());
        config.github.token = Some(String::new());
        config.svn.layout = reposync_core::config::SvnLayout::Custom;
        config.github.default_branch = "main".into();

        let git = GitClient::new(&bridge).unwrap();
        let mut engine = SyncEngine::new(
            config.clone(),
            Database::new(&db_path).unwrap(),
            SvnClient::new(&svn_url, "", ""),
            git,
            Arc::new(
                IdentityMapper::new(&IdentityConfig {
                    email_domain: Some("example.com".into()),
                    ..Default::default()
                })
                .unwrap(),
            ),
        );
        engine.set_repo_id(repo_id.into());
        engine.db().initialize().unwrap();
        assert_eq!(engine.run_sync_cycle().await.unwrap().svn_to_git_count, 1);

        let developer = tmp.path().join("developer");
        assert!(Command::new("git")
            .args([
                "clone",
                "-b",
                "main",
                bare.to_str().unwrap(),
                developer.to_str().unwrap()
            ])
            .status()
            .unwrap()
            .success());
        std::fs::write(developer.join("feature.txt"), "one\n").unwrap();
        git_cli(&developer, &["add", "feature.txt"]);
        git_cli(&developer, &["commit", "-m", "Git change"]);
        git_cli(&developer, &["push", "origin", "main"]);

        let _fault = SvnCommitFaultGuard::lost_reply(repo_id);
        let _ = engine.run_sync_cycle().await;
        drop(_fault);
        let op = engine
            .db()
            .active_svn_commit_operation(repo_id)
            .unwrap()
            .unwrap();
        assert_eq!(op.state, SvnCommitOperationState::ReconciliationRequired);
        drop(engine);

        let web_db = Database::new(&db_path).unwrap();
        web_db.initialize().unwrap();
        let placeholder_git = tmp.path().join("placeholder-git");
        git2::Repository::init(&placeholder_git).unwrap();
        let placeholder_engine = SyncEngine::new(
            config.clone(),
            Database::in_memory().unwrap(),
            SvnClient::new(&svn_url, "", ""),
            GitClient::new(&placeholder_git).unwrap(),
            Arc::new(
                IdentityMapper::new(&IdentityConfig {
                    email_domain: Some("example.com".into()),
                    ..Default::default()
                })
                .unwrap(),
            ),
        );
        let (sync_tx, _sync_rx) = tokio::sync::mpsc::channel(1);
        let (ws_tx, _) = tokio::sync::broadcast::channel(8);
        let state = Arc::new(AppState {
            db: web_db,
            sync_engine: Arc::new(placeholder_engine),
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
        {
            let mut sessions = state.sessions.write().await;
            sessions.insert(
                TEST_TOKEN.into(),
                chrono::Utc::now() + chrono::Duration::hours(24),
            );
        }
        let app = Router::new()
            .merge(api::repos::routes())
            .merge(api::auth::routes())
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            addr,
            state,
            server,
            _tmp: tmp,
            id: repo_id.into(),
            operation_id: op.id,
            svn_url,
        }
    }

    async fn reconcile(&self, token: &str) -> (reqwest::StatusCode, serde_json::Value) {
        let response = reqwest::Client::new()
            .post(format!(
                "http://{}/api/repos/{}/svn-commit/{}/reconcile",
                self.addr, self.id, self.operation_id
            ))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = response.json().await.unwrap_or(serde_json::json!({}));
        (status, body)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64c_admin_reconcile_finalizes_lost_reply() {
    let fixture = HeldPair::lost_reply("64c-http-reconcile").await;
    let svn_before = {
        let repo = fixture.svn_url.strip_prefix("file://").unwrap();
        String::from_utf8_lossy(
            &Command::new("svnlook")
                .args(["youngest", repo])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string()
    };
    let (code, body) = fixture.reconcile(TEST_TOKEN).await;
    assert_eq!(code, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["lifecycle"], "completed", "{body}");
    assert_eq!(body["checkpoint_completed"], true);
    assert_eq!(body["may_resume"], false);
    let svn_after = {
        let repo = fixture.svn_url.strip_prefix("file://").unwrap();
        String::from_utf8_lossy(
            &Command::new("svnlook")
                .args(["youngest", repo])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string()
    };
    assert_eq!(svn_after, svn_before);
    assert!(fixture
        .state
        .db
        .active_svn_commit_operation(&fixture.id)
        .unwrap()
        .is_none());
    let (repeat_code, repeat) = fixture.reconcile(TEST_TOKEN).await;
    assert_eq!(repeat_code, reqwest::StatusCode::OK);
    assert_eq!(repeat["lifecycle"], "completed");
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"64C_HTTP_UNIQUE_MATCH",
            "operation_id":fixture.operation_id,
            "svn_before_after":svn_before,
            "idempotent":true
        })
    );
    fixture.server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64c_named_admin_only_without_legacy_fallback() {
    let fixture = HeldPair::lost_reply("64c-http-auth").await;
    let now = chrono::Utc::now();
    for (id, role, enabled) in [
        ("admin-64c", "admin", true),
        ("viewer-64c", "viewer", true),
        ("disabled-64c", "admin", false),
    ] {
        fixture
            .state
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
    }
    for (token, user, expiry) in [
        (
            "admin-64c-token",
            "admin-64c",
            now + chrono::Duration::hours(1),
        ),
        (
            "viewer-64c-token",
            "viewer-64c",
            now + chrono::Duration::hours(1),
        ),
        (
            "disabled-64c-token",
            "disabled-64c",
            now + chrono::Duration::hours(1),
        ),
        (
            "expired-64c-token",
            "admin-64c",
            now - chrono::Duration::hours(1),
        ),
    ] {
        fixture
            .state
            .db
            .insert_session(&Session {
                token: token.into(),
                user_id: user.into(),
                expires_at: expiry.to_rfc3339(),
                created_at: now.to_rfc3339(),
            })
            .unwrap();
    }
    for token in [
        TEST_TOKEN,
        "viewer-64c-token",
        "disabled-64c-token",
        "expired-64c-token",
    ] {
        let (status, _) = fixture.reconcile(token).await;
        assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED, "{token}");
        assert!(fixture
            .state
            .db
            .active_svn_commit_operation(&fixture.id)
            .unwrap()
            .is_some());
    }
    let (status, body) = fixture.reconcile("admin-64c-token").await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["lifecycle"], "completed");
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"64C_AUTH","denied":4,"admin_completed":true})
    );
    fixture.server.abort();
}
