//! #119 real HTTP/SQLite pagination and refusal regressions; reuses the existing local web fixture.

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
        .timeout(Duration::from_secs(5))
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

async fn list_fixture(
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
        .merge(api::conflicts::routes())
        .merge(api::audit::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state, server, tmp)
}

async fn page(addr: SocketAddr, path: &str) -> serde_json::Value {
    let response = authed_client()
        .get(format!("http://{addr}{path}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{path}");
    response.json().await.unwrap()
}
fn conflict(state: &AppState, id: &str, repo: Option<&str>, status: &str, created: &str) {
    state.db.conn().execute("INSERT INTO conflicts(id,file_path,conflict_type,status,repo_id,created_at) VALUES (?1,?2,'content',?3,?4,?5)",rusqlite::params![id,format!("{id}.txt"),status,repo,created]).unwrap();
}
fn audit(state: &AppState, repo: Option<&str>, action: &str, success: bool) -> i64 {
    state
        .db
        .insert_audit_log_with_repo(reposync_core::db::queries::AuditLogInput {
            action,
            direction: None,
            svn_rev: None,
            git_sha: None,
            author: Some("fixture"),
            details: Some("retained evidence"),
            success,
            repo_id: repo,
        })
        .unwrap()
}
fn rows_snapshot(state: &AppState) -> String {
    let mut conflicts = state.db.list_conflicts(None, 1000).unwrap();
    conflicts.sort_by(|a, b| a.id.cmp(&b.id));
    format!(
        "{:?}\n{:?}",
        conflicts,
        state.db.list_audit_log(1000, 0).unwrap()
    )
}
#[tokio::test]
async fn rs119_conflicts_filter_before_pagination_reach_older_repo_and_stable_ties() {
    let (addr, state, server, _tmp) = list_fixture(&[("A119", "idle"), ("B119", "idle")]).await;
    for i in 0..45 {
        conflict(
            &state,
            &format!("A{i:03}"),
            Some("A119"),
            "detected",
            "2026-01-01T00:00:00Z",
        );
    }
    for i in 0..60 {
        conflict(
            &state,
            &format!("B{i:03}"),
            Some("B119"),
            "detected",
            "2026-02-01T00:00:00Z",
        );
    }
    for i in 0..5 {
        conflict(
            &state,
            &format!("resolved{i}"),
            Some("A119"),
            "resolved",
            "2025-01-01T00:00:00Z",
        );
    }
    for i in 0..2 {
        conflict(
            &state,
            &format!("unscoped{i}"),
            None,
            "detected",
            "2026-03-01T00:00:00Z",
        );
    }
    let before = rows_snapshot(&state);
    let mut reached = Vec::new();
    for n in 1..=3 {
        let value = page(
            addr,
            &format!("/api/conflicts?page={n}&repo_id=A119&status=detected"),
        )
        .await;
        assert_eq!(value["total"], 45);
        assert_eq!(value["total_pages"], 3);
        assert_eq!(value["per_page"], 20);
        assert_eq!(value["page"], n);
        let items = value["items"].as_array().unwrap();
        assert_eq!(items.len(), if n == 3 { 5 } else { 20 });
        for item in items {
            assert_eq!(item["repo_id"], "A119");
            assert_eq!(item["status"], "detected");
            reached.push(item["id"].as_str().unwrap().to_owned());
        }
        let reused = state
            .db
            .list_conflicts_paginated_filtered(
                Some("detected"),
                Some("A119"),
                &reposync_core::models::Pagination {
                    page: n as u32,
                    per_page: 20,
                },
            )
            .unwrap();
        assert_eq!(reused.total, 45);
        assert_eq!(
            reused
                .items
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            items
                .iter()
                .map(|c| c["id"].as_str().unwrap())
                .collect::<Vec<_>>()
        );
    }
    let expected = (0..45)
        .rev()
        .map(|i| format!("A{i:03}"))
        .collect::<Vec<_>>();
    assert_eq!(reached, expected);
    let legacy = page(addr, "/api/conflicts?repo_id=A119&status=detected").await;
    assert_eq!(legacy.as_array().unwrap().len(), 20);
    assert_eq!(legacy[0]["id"], "A044");
    let resolved = page(addr, "/api/conflicts?page=1&repo_id=A119&status=resolved").await;
    assert_eq!(resolved["total"], 5);
    let all = page(addr, "/api/conflicts?page=1&per_page=999").await;
    assert_eq!(all["per_page"], 100);
    assert_eq!(all["total"], 112);
    assert_eq!(all["items"].as_array().unwrap().len(), 100);
    let absent = page(addr, "/api/conflicts?page=99&repo_id=A119&status=detected").await;
    assert_eq!(absent["total"], 45);
    assert!(absent["items"].as_array().unwrap().is_empty());
    assert_eq!(rows_snapshot(&state), before);
    server.abort();
}
#[tokio::test]
async fn rs119_audit_combined_predicates_and_matching_totals_reach_every_filtered_row() {
    let (addr, state, server, _tmp) = list_fixture(&[("A119", "idle"), ("B119", "idle")]).await;
    let mut expected = Vec::new();
    for _ in 0..71 {
        expected.push((
            audit(&state, Some("A119"), "sync_error", false),
            Some("A119"),
            false,
            "sync_error",
        ));
    }
    for _ in 0..20 {
        expected.push((
            audit(&state, Some("A119"), "config_updated", true),
            Some("A119"),
            true,
            "config_updated",
        ));
    }
    for _ in 0..80 {
        expected.push((
            audit(&state, Some("B119"), "sync_error", false),
            Some("B119"),
            false,
            "sync_error",
        ));
    }
    for _ in 0..9 {
        expected.push((
            audit(&state, Some("A119"), "other", false),
            Some("A119"),
            false,
            "other",
        ));
    }
    for _ in 0..3 {
        expected.push((audit(&state, None, "orphan", false), None, false, "orphan"));
    }
    let before = rows_snapshot(&state);
    for (repo, success, action) in [
        (None, None, None),
        (Some("A119"), None, None),
        (Some("A119"), Some(false), Some("sync_error")),
        (None, Some(false), None),
        (None, Some(true), None),
        (Some("B119"), Some(false), Some("sync_error")),
        (Some("A119"), Some(false), Some("config_updated")),
    ] {
        let ids = expected
            .iter()
            .rev()
            .filter(|(_, r, s, a)| {
                repo.is_none_or(|x| Some(x) == *r)
                    && success.is_none_or(|x| x == *s)
                    && action.is_none_or(|x| x == *a)
            })
            .map(|(id, _, _, _)| *id)
            .collect::<Vec<_>>();
        let mut reached = Vec::new();
        let pages = ids.len().div_ceil(50);
        for n in 1..=pages.max(1) {
            let mut url = reqwest::Url::parse(&format!("http://{addr}/api/audit")).unwrap();
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("page", &n.to_string());
                if let Some(r) = repo {
                    q.append_pair("repo_id", r);
                }
                if let Some(s) = success {
                    q.append_pair("success", &s.to_string());
                }
                if let Some(a) = action {
                    q.append_pair("action", a);
                }
            }
            let response = authed_client().get(url).send().await.unwrap();
            assert_eq!(response.status(), 200);
            let value: serde_json::Value = response.json().await.unwrap();
            assert_eq!(value["total"], ids.len());
            assert_eq!(value["total_pages"], pages);
            assert_eq!(value["offset"], (n - 1) * 50);
            assert_eq!(value["page"], n);
            reached.extend(
                value["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| e["id"].as_i64().unwrap()),
            );
        }
        assert_eq!(reached, ids, "{repo:?} {success:?} {action:?}");
    }
    let second = page(
        addr,
        "/api/audit?limit=50&page=2&offset=999&repo_id=A119&success=false&action=sync_error",
    )
    .await;
    let legacy = page(
        addr,
        "/api/audit?limit=50&offset=50&repo_id=A119&success=false&action=sync_error",
    )
    .await;
    assert_eq!(second, legacy);
    assert_eq!(second["entries"].as_array().unwrap().len(), 21);
    let unaligned = page(
        addr,
        "/api/audit?limit=50&offset=1&repo_id=A119&success=false&action=sync_error",
    )
    .await;
    assert_eq!(unaligned["offset"], 1);
    assert!(unaligned["page"].is_null());
    let no_match = authed_client()
        .get(format!("http://{addr}/api/audit"))
        .query(&[("action", "' OR 1=1 --"), ("page", "1")])
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(no_match["total"], 0);
    assert!(no_match["entries"].as_array().unwrap().is_empty());
    assert_eq!(rows_snapshot(&state), before);
    server.abort();
}
#[tokio::test]
async fn rs119_read_failures_are_errors_not_empty_or_partially_dropped_rows() {
    let (addr, state, server, _tmp) = list_fixture(&[("A119", "idle"), ("B119", "idle")]).await;
    audit(&state, Some("A119"), "valid", true);
    let corrupt = audit(&state, Some("B119"), "corrupt", false);
    state
        .db
        .conn()
        .execute("UPDATE audit_log SET details=X'FF' WHERE id=?1", [corrupt])
        .unwrap();
    let response = authed_client()
        .get(format!("http://{addr}/api/audit?repo_id=B119"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 500);
    let valid = page(addr, "/api/audit?page=1&repo_id=A119").await;
    assert_eq!(valid["total"], 1);
    assert_eq!(valid["entries"].as_array().unwrap().len(), 1);
    let empty = page(addr, "/api/audit?page=1&repo_id=absent").await;
    assert_eq!(empty["total"], 0);
    assert!(empty["entries"].as_array().unwrap().is_empty());
    let bytes: Vec<u8> = state
        .db
        .conn()
        .query_row(
            "SELECT details FROM audit_log WHERE id=?1",
            [corrupt],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bytes, vec![255]);
    conflict(
        &state,
        "bad",
        Some("A119"),
        "detected",
        "2026-01-01T00:00:00Z",
    );
    state
        .db
        .conn()
        .execute("UPDATE conflicts SET file_path=X'FF' WHERE id='bad'", [])
        .unwrap();
    let response = authed_client()
        .get(format!("http://{addr}/api/conflicts?page=1&repo_id=A119"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 500);
    let empty = page(addr, "/api/conflicts?page=1&repo_id=absent").await;
    assert_eq!(empty["total"], 0);
    assert!(empty["items"].as_array().unwrap().is_empty());
    server.abort();
}
#[tokio::test]
async fn rs119_auth_invalid_page_bounds_and_large_offsets_do_not_fabricate_empty_success() {
    let (addr, _state, server, _tmp) = list_fixture(&[]).await;
    for path in ["/api/audit", "/api/conflicts?page=1"] {
        let response = reqwest::Client::new()
            .get(format!("http://{addr}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }
    for path in [
        "/api/audit?limit=0",
        "/api/audit?page=0",
        "/api/audit?repo_id=",
        "/api/conflicts?per_page=0",
        "/api/conflicts?page=0",
        "/api/conflicts?repo_id=",
    ] {
        let response = authed_client()
            .get(format!("http://{addr}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{path}");
    }
    let audit = page(addr, "/api/audit?page=4294967295&limit=500").await;
    assert_eq!(audit["offset"], 2147483647000u64);
    assert_eq!(audit["total"], 0);
    let conflict = page(addr, "/api/conflicts?page=4294967295&per_page=100").await;
    assert_eq!(conflict["page"], 4294967295u64);
    assert_eq!(conflict["total"], 0);
    server.abort();
}
