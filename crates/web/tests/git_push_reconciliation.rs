//! Authorized RS-C04 / #64 SVN→Git push reconciliation against a real team pair.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::Router;
use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::git_push_operations::GitPushOperationState;
use reposync_core::db::Database;
use reposync_core::errors::SyncError;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::ImportProgress;
use reposync_core::models::Repository;
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_web::api;
use reposync_web::AppState;

const TEST_TOKEN: &str = "test-session-token-for-rsc04";

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

struct GitPushFaultGuard {
    scoped_key: String,
}

impl GitPushFaultGuard {
    fn lost_reply(repo_id: &str) -> Self {
        let key = "REPOSYNC_GIT_PUSH_LOST_REPLY";
        let scoped_key = format!("{}__{}", key, repo_id);
        std::env::set_var(&scoped_key, "1");
        Self { scoped_key }
    }
}

impl Drop for GitPushFaultGuard {
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
    bridge: std::path::PathBuf,
    bare: std::path::PathBuf,
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
            name: "rsc04 pair".into(),
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

        svn_commit_file(&wc, "feature.txt", "lost reply\n", "RS-C04 lost reply");
        let remote_before = Command::new("git")
            .arg("-C")
            .arg(&bare)
            .args(["rev-parse", "refs/heads/main"])
            .output()
            .unwrap();
        let remote_before = String::from_utf8_lossy(&remote_before.stdout)
            .trim()
            .to_string();
        let mappings_before: i64 = engine
            .db()
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1 AND direction='svn_to_git'",
                [repo_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let watermark_before = engine.db().get_repo_watermark(repo_id).unwrap();

        let _fault = GitPushFaultGuard::lost_reply(repo_id);
        let result = engine.run_sync_cycle().await;
        assert!(
            matches!(
                &result,
                Err(SyncError::GitPushHeld { reason, .. }) if reason == "lost_push_reply"
            ),
            "{result:?}"
        );
        drop(_fault);
        let remote_after = Command::new("git")
            .arg("-C")
            .arg(&bare)
            .args(["rev-parse", "refs/heads/main"])
            .output()
            .unwrap();
        let remote_after = String::from_utf8_lossy(&remote_after.stdout)
            .trim()
            .to_string();
        assert_ne!(remote_before, remote_after);
        assert_eq!(
            engine
                .db()
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1 AND direction='svn_to_git'",
                    [repo_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            mappings_before
        );
        assert_eq!(
            engine.db().get_repo_watermark(repo_id).unwrap(),
            watermark_before
        );

        let op = engine
            .db()
            .active_git_push_operation(repo_id)
            .unwrap()
            .unwrap();
        assert_eq!(op.state, GitPushOperationState::ReconciliationRequired);
        drop(engine);

        let repo_git = tmp.path().join("repos").join(repo_id).join("git-repo");
        std::fs::create_dir_all(repo_git.parent().unwrap()).unwrap();
        git_cli(&bridge, &["clone", ".", repo_git.to_str().unwrap()]);

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
            bridge,
            bare,
        }
    }

    async fn reconcile(&self, token: &str) -> (reqwest::StatusCode, serde_json::Value) {
        let response = reqwest::Client::new()
            .post(format!(
                "http://{}/api/repos/{}/git-push/{}/reconcile",
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
async fn candidate_rsc04_admin_reconcile_finalizes_lost_reply() {
    let fixture = HeldPair::lost_reply("rsc04-http-reconcile").await;
    let remote_before = Command::new("git")
        .arg("-C")
        .arg(&fixture.bare)
        .args(["rev-parse", "refs/heads/main"])
        .output()
        .unwrap();
    let remote_before = String::from_utf8_lossy(&remote_before.stdout)
        .trim()
        .to_string();
    let mappings_before: i64 = fixture
        .state
        .db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1 AND direction='svn_to_git'",
            [&fixture.id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    let watermark_before = fixture.state.db.get_repo_watermark(&fixture.id).unwrap();

    let (code, body) = fixture.reconcile(TEST_TOKEN).await;
    assert_eq!(code, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["lifecycle"].as_str(), Some("completed"), "{body}");
    assert_eq!(body["checkpoint_completed"], true);
    assert_eq!(body["may_resume"], false);
    let remote_after_reconcile = String::from_utf8_lossy(
        &Command::new("git")
            .arg("-C")
            .arg(&fixture.bare)
            .args(["rev-parse", "refs/heads/main"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert_eq!(remote_after_reconcile, remote_before);
    assert_eq!(
        fixture
            .state
            .db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id=?1 AND direction='svn_to_git'",
                [&fixture.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        mappings_before + 1
    );
    assert_ne!(
        fixture.state.db.get_repo_watermark(&fixture.id).unwrap(),
        watermark_before
    );
    assert!(fixture
        .state
        .db
        .active_git_push_operation(&fixture.id)
        .unwrap()
        .is_none());
    let (repeat_code, repeat) = fixture.reconcile(TEST_TOKEN).await;
    assert_eq!(repeat_code, reqwest::StatusCode::OK);
    assert_eq!(repeat["lifecycle"].as_str(), Some("completed"));
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RSC04_HTTP_UNIQUE_MATCH",
            "operation_id":fixture.operation_id,
            "remote_before_after":remote_before,
            "idempotent":true
        })
    );
    fixture.server.abort();
}
