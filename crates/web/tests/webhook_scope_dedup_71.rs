//! #71 webhook hardening: authenticity before anything else, repository and
//! branch scope, delivery-ID dedup, burst coalescing, and stale or
//! out-of-order deliveries that only prompt an inspection.
//!
//! The delivery log is process-wide, so every test uses its own delivery IDs.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use hmac::{Hmac, Mac};
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
use sha2::Sha256;
use tempfile::TempDir;
use tokio::sync::mpsc::Receiver;

const GIT_SECRET: &str = "git-fixture-secret";
const SVN_SECRET: &str = "svn-fixture-secret";

struct Harness {
    _tmp: TempDir,
    state: Arc<AppState>,
    rx: Option<Receiver<()>>,
    addr: SocketAddr,
    server: tokio::task::JoinHandle<()>,
    client: reqwest::Client,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn harness(secrets: bool, capacity: usize) -> Harness {
    let tmp = TempDir::new().unwrap();
    let git_path = tmp.path().join("git-repo");
    git2::Repository::init(&git_path).unwrap();
    let toml = format!(
        "[daemon]\ndata_dir = {:?}\n[svn]\nurl = 'https://svn.test.invalid/repo'\nusername = 'fixture'\npassword_env = ''\n[github]\nrepo = 'test/repo'\ntoken_env = ''\n",
        tmp.path().display().to_string()
    );
    let mut config: AppConfig = toml::from_str(&toml).unwrap();
    if secrets {
        config.github.webhook_secret = Some(GIT_SECRET.into());
        config.svn.webhook_secret = Some(SVN_SECRET.into());
    }
    let engine = SyncEngine::new(
        config.clone(),
        Database::in_memory().unwrap(),
        SvnClient::new("https://svn.test.invalid/repo", "fixture", ""),
        GitClient::new(&git_path).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    );
    let (tx, rx) = tokio::sync::mpsc::channel(capacity);
    let (broadcast, _) = tokio::sync::broadcast::channel(8);
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    let state = Arc::new(AppState {
        db,
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
        scheduler_service: std::sync::Mutex::new(reposync_web::SchedulerServiceState::running()),
    });
    let app = Router::new()
        .merge(api::webhooks::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Harness {
        _tmp: tmp,
        state,
        rx: Some(rx),
        addr,
        server,
        client: reqwest::Client::new(),
    }
}

fn push(repo: &str, git_ref: &str, before: &str, after: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "ref": git_ref,
        "before": before,
        "after": after,
        "forced": false,
        "commits": [{"id": after, "message": "m", "author": {"name": "dev", "email": "dev@example.invalid"}}],
        "repository": {"full_name": repo}
    }))
    .unwrap()
}

fn sign(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(GIT_SECRET.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

impl Harness {
    async fn github(
        &self,
        body: Vec<u8>,
        delivery: Option<&str>,
        signature: Option<String>,
    ) -> (reqwest::StatusCode, serde_json::Value) {
        let mut req = self
            .client
            .post(format!("http://{}/webhook/github", self.addr))
            .header("X-GitHub-Event", "push")
            .header("Content-Type", "application/json");
        if let Some(id) = delivery {
            req = req.header("X-GitHub-Delivery", id);
        }
        if let Some(sig) = signature {
            req = req.header("X-Hub-Signature-256", sig);
        }
        let resp = tokio::time::timeout(Duration::from_secs(5), req.body(body).send())
            .await
            .expect("webhook must answer without waiting on the scheduler")
            .unwrap();
        let status = resp.status();
        let json = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    async fn svn(
        &self,
        body: &str,
        delivery: Option<&str>,
        secret: Option<&str>,
    ) -> (reqwest::StatusCode, serde_json::Value) {
        let mut req = self
            .client
            .post(format!("http://{}/webhook/svn", self.addr))
            .header("Content-Type", "application/json");
        if let Some(id) = delivery {
            req = req.header("X-Webhook-Delivery", id);
        }
        if let Some(s) = secret {
            req = req.header("X-Webhook-Secret", s);
        }
        let resp = req.body(body.to_string()).send().await.unwrap();
        let status = resp.status();
        let json = resp.json().await.unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    /// Drain queued triggers and return how many there were.
    fn drain(&mut self) -> usize {
        let rx = self.rx.as_mut().unwrap();
        let mut n = 0;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        n
    }
}

fn paired_repo(id: &str, git_repo: &str, enabled: bool) -> Repository {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": id,
        "svn_url": "https://svn.test.invalid/repo",
        "svn_branch": "trunk",
        "svn_username": "fixture",
        "git_provider": "github",
        "git_api_url": "https://api.github.invalid",
        "git_repo": git_repo,
        "git_branch": "main",
        "sync_mode": "team",
        "poll_interval_secs": 60,
        "lfs_threshold_mb": 50,
        "auto_merge": false,
        "enabled": enabled,
        "created_by": null,
        "created_at": "2026-11-27T00:00:00Z",
        "updated_at": "2026-11-27T00:00:00Z",
        "last_svn_rev": 7,
        "last_git_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_71_signature_is_checked_before_scope_or_dedup() {
    let mut h = harness(true, 8).await;
    let body = push("test/repo", "refs/heads/main", "1", "2");

    // Missing and tampered signatures are refused and trigger nothing.
    let (status, _) = h.github(body.clone(), Some("auth-1"), None).await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    let tampered = sign(&push("test/repo", "refs/heads/main", "1", "3"));
    let (status, _) = h.github(body.clone(), Some("auth-1"), Some(tampered)).await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    // An unsigned body is refused before its JSON is read.
    let (status, _) = h.github(b"not json".to_vec(), Some("auth-x"), None).await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(h.drain(), 0, "refused deliveries must not trigger");

    // A refused delivery did not reserve its ID: the real one still triggers.
    let (status, json) = h
        .github(body.clone(), Some("auth-1"), Some(sign(&body)))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{json}");
    assert_eq!(json["triggered"], true, "{json}");
    assert_eq!(h.drain(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_71_only_synced_repositories_and_branches_trigger() {
    let mut h = harness(true, 8).await;
    h.state
        .db
        .insert_repository(&paired_repo(
            "pair-a",
            "https://github.invalid/Team/Paired.git",
            true,
        ))
        .unwrap();
    h.state
        .db
        .insert_repository(&paired_repo("pair-b", "team/disabled", false))
        .unwrap();

    for (repo, git_ref, id) in [
        ("someone/else", "refs/heads/main", "scope-1"),
        ("team/disabled", "refs/heads/main", "scope-2"),
        ("test/repo", "refs/tags/v1.0.0", "scope-3"),
    ] {
        let body = push(repo, git_ref, "1", "2");
        let (status, json) = h.github(body.clone(), Some(id), Some(sign(&body))).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{json}");
        assert_eq!(json["ok"], true, "{json}");
        assert_eq!(json["triggered"], false, "{repo} {git_ref}: {json}");
        assert!(
            json["message"].as_str().unwrap().contains("ignored"),
            "{json}"
        );
    }
    let no_repo = serde_json::to_vec(&serde_json::json!({"ref": "refs/heads/main"})).unwrap();
    let (_, json) = h
        .github(no_repo.clone(), Some("scope-4"), Some(sign(&no_repo)))
        .await;
    assert_eq!(json["triggered"], false, "{json}");
    assert_eq!(h.drain(), 0, "out-of-scope pushes must not trigger");

    // The configured repository and an enabled pair's repository both trigger,
    // matched without regard to case or URL form.
    let body = push("TEST/Repo", "refs/heads/main", "1", "2");
    let (_, json) = h
        .github(body.clone(), Some("scope-5"), Some(sign(&body)))
        .await;
    assert_eq!(json["triggered"], true, "{json}");
    let body = push("team/paired", "refs/heads/main", "1", "2");
    let (_, json) = h
        .github(body.clone(), Some("scope-6"), Some(sign(&body)))
        .await;
    assert_eq!(json["triggered"], true, "{json}");
    assert_eq!(h.drain(), 2);

    // An out-of-scope delivery does not reserve its ID either.
    let body = push("test/repo", "refs/heads/main", "1", "2");
    let (_, json) = h
        .github(body.clone(), Some("scope-1"), Some(sign(&body)))
        .await;
    assert_eq!(json["triggered"], true, "{json}");
    assert_eq!(h.drain(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_71_only_the_paired_branch_of_a_synced_repository_triggers() {
    let mut h = harness(true, 8).await;
    let mut child = paired_repo("pair-child", "team/paired", true);
    child.git_branch = "refs/heads/release/2.x".into();
    h.state.db.insert_repository(&child).unwrap();
    h.state
        .db
        .insert_repository(&paired_repo("pair-main", "team/paired", true))
        .unwrap();
    let mut disabled = paired_repo("pair-off", "team/paired", false);
    disabled.git_branch = "hotfix".into();
    h.state.db.insert_repository(&disabled).unwrap();

    // Branches nobody syncs, on synced repositories, are ignored and say so.
    for (repo, git_ref, id) in [
        ("team/paired", "refs/heads/feature", "branch-1"),
        ("team/paired", "refs/heads/hotfix", "branch-2"),
        ("team/paired", "refs/heads/Main", "branch-3"),
        ("team/paired", "refs/heads/release/2.x/extra", "branch-4"),
        ("test/repo", "refs/heads/feature", "branch-5"),
    ] {
        let body = push(repo, git_ref, "1", "2");
        let (status, json) = h.github(body.clone(), Some(id), Some(sign(&body))).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{json}");
        assert_eq!(json["triggered"], false, "{repo} {git_ref}: {json}");
        let message = json["message"].as_str().unwrap();
        assert!(
            message.contains("branch") && message.contains("is not synced"),
            "{json}"
        );
    }
    assert_eq!(h.drain(), 0, "pushes to unpaired branches must not trigger");

    // Each enabled pair's own branch triggers, including a stored
    // `refs/heads/` form, as does the configured default branch.
    for (repo, git_ref, id) in [
        ("team/paired", "refs/heads/main", "branch-6"),
        ("Team/Paired", "refs/heads/release/2.x", "branch-7"),
        ("test/repo", "refs/heads/main", "branch-8"),
    ] {
        let body = push(repo, git_ref, "1", "2");
        let (_, json) = h.github(body.clone(), Some(id), Some(sign(&body))).await;
        assert_eq!(json["triggered"], true, "{repo} {git_ref}: {json}");
        assert_eq!(h.drain(), 1);
    }

    // An ignored delivery did not reserve its ID.
    let body = push("team/paired", "refs/heads/main", "1", "2");
    let (_, json) = h
        .github(body.clone(), Some("branch-1"), Some(sign(&body)))
        .await;
    assert_eq!(json["triggered"], true, "{json}");
    assert_eq!(h.drain(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_71_redelivery_is_deduplicated_and_stale_events_change_nothing() {
    let mut h = harness(true, 8).await;
    h.state
        .db
        .insert_repository(&paired_repo("pair-s", "test/repo", true))
        .unwrap();
    let before = serde_json::to_value(h.state.db.list_repositories().unwrap()).unwrap();

    let newer = push(
        "test/repo",
        "refs/heads/main",
        "b".repeat(40).as_str(),
        "c".repeat(40).as_str(),
    );
    let (_, json) = h
        .github(newer.clone(), Some("dedup-1"), Some(sign(&newer)))
        .await;
    assert_eq!(json["triggered"], true, "{json}");

    // Redelivery of the same ID: acknowledged, not acted on again.
    let (status, json) = h
        .github(newer.clone(), Some("dedup-1"), Some(sign(&newer)))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(json["triggered"], false, "{json}");
    assert_eq!(json["message"], "duplicate delivery ignored", "{json}");
    assert_eq!(
        h.drain(),
        1,
        "a redelivery must not queue a second inspection"
    );

    // An older event arriving late, and one without a delivery ID, only ask for
    // an inspection. Their before/after values are never applied.
    let stale = push(
        "test/repo",
        "refs/heads/main",
        "0".repeat(40).as_str(),
        "b".repeat(40).as_str(),
    );
    let (_, json) = h
        .github(stale.clone(), Some("dedup-0"), Some(sign(&stale)))
        .await;
    assert_eq!(json["triggered"], true, "{json}");
    assert_eq!(json["inspection"], "polling_safety_gate", "{json}");
    let (_, json) = h.github(stale.clone(), None, Some(sign(&stale))).await;
    assert_eq!(json["triggered"], true, "{json}");
    assert_eq!(h.drain(), 2);

    let after = serde_json::to_value(h.state.db.list_repositories().unwrap()).unwrap();
    assert_eq!(
        before, after,
        "a webhook must not move any pair's watermark"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_71_bursts_fold_into_a_pending_inspection() {
    let mut h = harness(false, 1).await;
    let mut queued = 0;
    for i in 0..20 {
        let (status, json) = h
            .github(
                push("test/repo", "refs/heads/main", "1", "2"),
                Some(&format!("burst-{i}")),
                None,
            )
            .await;
        assert_eq!(status, reqwest::StatusCode::OK, "{json}");
        if json["triggered"] == true {
            queued += 1;
        } else {
            assert!(
                json["message"]
                    .as_str()
                    .unwrap()
                    .contains("already pending"),
                "{json}"
            );
        }
    }
    assert_eq!(queued, 1, "a full queue folds later deliveries into it");
    assert_eq!(h.drain(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_71_unreachable_scheduler_fails_visibly_and_allows_redelivery() {
    let mut h = harness(true, 4).await;
    drop(h.rx.take());
    let body = push("test/repo", "refs/heads/main", "1", "2");
    let (status, _) = h
        .github(body.clone(), Some("closed-1"), Some(sign(&body)))
        .await;
    assert_eq!(status, reqwest::StatusCode::INTERNAL_SERVER_ERROR);

    // The failed delivery was forgotten, so its redelivery is acted on.
    let mut live = harness(true, 4).await;
    let (_, json) = live
        .github(body.clone(), Some("closed-1"), Some(sign(&body)))
        .await;
    assert_eq!(json["triggered"], true, "{json}");
    assert_eq!(live.drain(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_71_svn_secret_is_checked_before_parsing_and_deliveries_dedup() {
    let mut h = harness(true, 8).await;
    let (status, _) = h.svn("not json", Some("svn-1"), None).await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    let (status, _) = h.svn("not json", Some("svn-1"), Some("wrong")).await;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    let (status, _) = h.svn("not json", Some("svn-1"), Some(SVN_SECRET)).await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(h.drain(), 0);

    let body = r#"{"revision": 42, "author": "dev", "message": "m"}"#;
    let (_, json) = h.svn(body, Some("svn-1"), Some(SVN_SECRET)).await;
    assert_eq!(json["triggered"], true, "{json}");
    let (_, json) = h.svn(body, Some("svn-1"), Some(SVN_SECRET)).await;
    assert_eq!(json["triggered"], false, "{json}");
    assert_eq!(json["message"], "duplicate delivery ignored");
    // SVN and Git delivery IDs do not collide.
    let git = push("test/repo", "refs/heads/main", "1", "2");
    let (_, json) = h.github(git.clone(), Some("svn-1"), Some(sign(&git))).await;
    assert_eq!(json["triggered"], true, "{json}");
    assert_eq!(h.drain(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_71_documented_svn_hook_payload_is_accepted() {
    // docs/getting-started.md sends only the repository path and revision.
    let mut h = harness(false, 8).await;
    let (status, json) = h
        .svn(
            r#"{"repository": "/srv/svn/repo", "revision": 43}"#,
            None,
            None,
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{json}");
    assert_eq!(json["triggered"], true, "{json}");
    assert_eq!(h.drain(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_71_unauthenticated_delivery_ids_are_not_recorded() {
    // Without a secret anyone can send any ID, so IDs are not trusted: a
    // pre-recorded ID must not suppress the real delivery.
    let mut h = harness(false, 8).await;
    let body = push("test/repo", "refs/heads/main", "1", "2");
    let (_, json) = h.github(body.clone(), Some("unauth-1"), None).await;
    assert_eq!(json["triggered"], true, "{json}");
    assert_eq!(h.drain(), 1);
    let (_, json) = h.github(body, Some("unauth-1"), None).await;
    assert_eq!(json["triggered"], true, "{json}");
    let svn = r#"{"revision": 44}"#;
    let (_, json) = h.svn(svn, Some("/srv/svn/repo@44"), None).await;
    assert_eq!(json["triggered"], true, "{json}");
    assert_eq!(h.drain(), 2, "neither repeat was dropped as a duplicate");
}
