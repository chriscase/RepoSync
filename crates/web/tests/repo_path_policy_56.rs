//! #56 real HTTP/SQLite path-policy read/edit/save/clear regressions.
//!
//! Every test owns a temporary SQLite database and a loopback router. No SVN,
//! Git remote, scheduler or engine sync runs. Preview vectors are evaluated
//! with the engine's own `reposync_core::path_projection` functions and are
//! kept identical to `PREVIEW_VECTORS` in `web-ui/src/pathPolicy56.test.ts`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use reposync_core::config::{AppConfig, IdentityConfig};
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::ImportProgress;
use reposync_core::models::Repository;
use reposync_core::path_projection::{exclusion_message, path_is_projected};
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_web::api;
use reposync_web::AppState;
use serde_json::{json, Value};
use tempfile::TempDir;

const TEST_TOKEN: &str = "test-session-token-rs56-path-policy";

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

fn policy_repo(id: &str, allowed: Option<&str>, blocked: Option<&str>) -> Repository {
    let now = chrono::Utc::now().to_rfc3339();
    Repository {
        id: id.into(),
        name: format!("Synthetic {id}"),
        svn_url: "file:///nonexistent/svn".into(),
        svn_branch: "trunk".into(),
        svn_username: "fixture".into(),
        git_provider: "github".into(),
        git_api_url: "http://127.0.0.1:1".into(),
        git_repo: "test/repo".into(),
        git_branch: "main".into(),
        sync_mode: "direct".into(),
        poll_interval_secs: 75,
        lfs_threshold_mb: 9,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 41,
        last_git_sha: "live-git-sha".into(),
        last_sync_at: Some("2026-11-01T00:00:00Z".into()),
        sync_status: "idle".into(),
        total_syncs: 7,
        total_errors: 2,
        allowed_paths: allowed.map(Into::into),
        blocked_patterns: blocked.map(Into::into),
        consecutive_errors: 1,
        teams_webhook_url: Some("https://hooks.example.invalid/rs56".into()),
    }
}

async fn policy_fixture(
    repos: Vec<Repository>,
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
    for repo in &repos {
        db.insert_repository(repo).unwrap();
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
        .merge(api::repos::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, state, server, tmp)
}

async fn get_repo(client: &reqwest::Client, addr: SocketAddr, id: &str) -> Value {
    let response = client
        .get(format!("http://{addr}/api/repos/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

async fn put_repo(
    client: &reqwest::Client,
    addr: SocketAddr,
    id: &str,
    body: &Value,
) -> (u16, Value) {
    let response = client
        .put(format!("http://{addr}/api/repos/{id}"))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// A save as the editor sends it: a fresh request ID and the read `updated_at`.
fn edit(read: &Value, fields: Value) -> Value {
    let mut body = fields;
    body["save_request_id"] = json!(uuid::Uuid::new_v4().to_string());
    body["expected_updated_at"] = read["updated_at"].clone();
    body
}

fn stored(state: &AppState, id: &str) -> (Option<String>, Option<String>) {
    let repo = state.db.get_repository(id).unwrap().unwrap();
    (repo.allowed_paths, repo.blocked_patterns)
}

#[tokio::test]
async fn rs56_get_reports_unset_lists_empty_and_malformed_without_raw_columns() {
    let (addr, _state, server, _tmp) = policy_fixture(vec![
        policy_repo("unset", None, None),
        policy_repo(
            "lists",
            Some(r#"["src/","config/"]"#),
            Some(r#"["*.exe","secret-*"]"#),
        ),
        policy_repo("empty", Some("[]"), Some("[]")),
        policy_repo("broken", Some("src/"), Some(r#"{"a":1}"#)),
    ])
    .await;
    let client = authed_client();

    let unset = get_repo(&client, addr, "unset").await;
    assert_eq!(
        unset["path_policy"],
        json!({"version":1,"allowed_paths":{"state":"unset"},"blocked_patterns":{"state":"unset"},
            "enforced_direction":"git_to_svn","svn_to_git_filtering":"not_applied","path_rewriting":"none_read_only"})
    );
    let lists = get_repo(&client, addr, "lists").await;
    assert_eq!(
        lists["path_policy"]["allowed_paths"],
        json!({"state":"list","entries":["src/","config/"]})
    );
    assert_eq!(
        lists["path_policy"]["blocked_patterns"],
        json!({"state":"list","entries":["*.exe","secret-*"]})
    );
    let empty = get_repo(&client, addr, "empty").await;
    assert_eq!(
        empty["path_policy"]["allowed_paths"],
        json!({"state":"list","entries":[]})
    );
    let broken = get_repo(&client, addr, "broken").await;
    for field in ["allowed_paths", "blocked_patterns"] {
        let read = &broken["path_policy"][field];
        assert_eq!(read["state"], "malformed", "{field}");
        assert!(
            read.get("entries").is_none(),
            "malformed must not look like a list"
        );
        assert!(read["reason"].as_str().unwrap().contains("refuses to sync"));
    }
    assert_eq!(broken["path_policy"]["allowed_paths"]["stored_bytes"], 4);
    // The raw columns stay write-only; a client cannot read null as "unset".
    for repo in [&unset, &lists, &empty, &broken] {
        let object = repo.as_object().unwrap();
        assert!(!object.contains_key("allowed_paths"));
        assert!(!object.contains_key("blocked_patterns"));
    }
    server.abort();
}

#[tokio::test]
async fn rs56_get_edit_put_get_preserves_replaces_and_explicitly_clears_both_fields() {
    let allowed = r#"["src/","config/"]"#;
    let blocked = r#"["*.exe"]"#;
    let (addr, state, server, _tmp) =
        policy_fixture(vec![policy_repo("P", Some(allowed), Some(blocked))]).await;
    let client = authed_client();

    // Unrelated edit: both policy fields absent from the body are preserved byte-for-byte.
    let read = get_repo(&client, addr, "P").await;
    let (status, saved) =
        put_repo(&client, addr, "P", &edit(&read, json!({"name":"Renamed"}))).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["name"], "Renamed");
    assert_eq!(saved["path_policy"], read["path_policy"]);
    assert_eq!(
        stored(&state, "P"),
        (Some(allowed.into()), Some(blocked.into()))
    );
    let reread = get_repo(&client, addr, "P").await;
    assert_eq!(reread["path_policy"], read["path_policy"]);

    // Replace both.
    let new_allowed = r#"["team/docs","build"]"#;
    let new_blocked = r#"["secret-*","*.log","temp/"]"#;
    let (status, saved) = put_repo(
        &client,
        addr,
        "P",
        &edit(
            &reread,
            json!({"allowed_paths":new_allowed,"blocked_patterns":new_blocked}),
        ),
    )
    .await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(
        stored(&state, "P"),
        (Some(new_allowed.into()), Some(new_blocked.into()))
    );
    let replaced = get_repo(&client, addr, "P").await;
    assert_eq!(
        replaced["path_policy"]["allowed_paths"],
        json!({"state":"list","entries":["team/docs","build"]})
    );
    assert_eq!(
        replaced["path_policy"]["blocked_patterns"],
        json!({"state":"list","entries":["secret-*","*.log","temp/"]})
    );
    assert_eq!(replaced["name"], "Renamed");

    // Replace one, keep the other.
    let (status, _) = put_repo(
        &client,
        addr,
        "P",
        &edit(&replaced, json!({"blocked_patterns":r#"["*.tmp"]"#})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        stored(&state, "P"),
        (Some(new_allowed.into()), Some(r#"["*.tmp"]"#.into()))
    );

    // Explicit clear of both.
    let before_clear = get_repo(&client, addr, "P").await;
    let (status, saved) = put_repo(
        &client,
        addr,
        "P",
        &edit(
            &before_clear,
            json!({"allowed_paths":null,"blocked_patterns":null}),
        ),
    )
    .await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(stored(&state, "P"), (None, None));
    let cleared = get_repo(&client, addr, "P").await;
    assert_eq!(
        cleared["path_policy"]["allowed_paths"],
        json!({"state":"unset"})
    );
    assert_eq!(
        cleared["path_policy"]["blocked_patterns"],
        json!({"state":"unset"})
    );
    assert_eq!(cleared["name"], "Renamed");
    server.abort();
}

#[tokio::test]
async fn rs56_malformed_stored_policy_is_not_cleared_by_unrelated_saves() {
    let (addr, state, server, _tmp) =
        policy_fixture(vec![policy_repo("M", Some("src/"), Some("not json"))]).await;
    let client = authed_client();

    let read = get_repo(&client, addr, "M").await;
    let (status, _) = put_repo(
        &client,
        addr,
        "M",
        &edit(&read, json!({"poll_interval_secs":90})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        stored(&state, "M"),
        (Some("src/".into()), Some("not json".into()))
    );
    let after = get_repo(&client, addr, "M").await;
    assert_eq!(after["path_policy"]["allowed_paths"]["state"], "malformed");
    assert_eq!(
        after["path_policy"]["blocked_patterns"]["state"],
        "malformed"
    );
    assert_eq!(after["poll_interval_secs"], 90);

    // Only an explicit replacement or clear changes a malformed field.
    let (status, _) = put_repo(
        &client,
        addr,
        "M",
        &edit(&after, json!({"allowed_paths":r#"["src"]"#})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        stored(&state, "M"),
        (Some(r#"["src"]"#.into()), Some("not json".into()))
    );
    let replaced = get_repo(&client, addr, "M").await;
    let (status, _) = put_repo(
        &client,
        addr,
        "M",
        &edit(&replaced, json!({"blocked_patterns":null})),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(stored(&state, "M"), (Some(r#"["src"]"#.into()), None));
    server.abort();
}

#[tokio::test]
async fn rs56_invalid_supplied_lists_are_rejected_before_receipt_cas_or_write() {
    let allowed = r#"["src/"]"#;
    let blocked = r#"["*.exe"]"#;
    let (addr, state, server, _tmp) =
        policy_fixture(vec![policy_repo("V", Some(allowed), Some(blocked))]).await;
    let client = authed_client();
    let read = get_repo(&client, addr, "V").await;
    // Any repository write or save-receipt write attempt would now fail as 500.
    state
        .db
        .conn()
        .execute_batch(
            "CREATE TRIGGER rs56_no_repo_write BEFORE UPDATE ON repositories \
             BEGIN SELECT RAISE(ABORT,'rs56 write attempted'); END;",
        )
        .unwrap();
    let many = serde_json::to_string(&vec!["a"; 257]).unwrap();
    let cases: Vec<(&str, String, &str)> = vec![
        ("allowed_paths", "not json".into(), "not_array"),
        ("allowed_paths", "{}".into(), "not_array"),
        ("allowed_paths", "[1]".into(), "not_array"),
        ("allowed_paths", "\"src\"".into(), "not_array"),
        ("allowed_paths", r#"[""]"#.into(), "empty_entry"),
        ("allowed_paths", r#"[" src"]"#.into(), "surrounding_space"),
        (
            "allowed_paths",
            "[\"a\\u0007b\"]".into(),
            "control_character",
        ),
        ("allowed_paths", r#"["/"]"#.into(), "names_no_path"),
        ("allowed_paths", r#"["a//b"]"#.into(), "empty_component"),
        ("allowed_paths", r#"["../outside"]"#.into(), "dot_component"),
        (
            "allowed_paths",
            r#"["src/*"]"#.into(),
            "wildcard_in_allowed",
        ),
        ("blocked_patterns", many, "too_many_entries"),
        (
            "blocked_patterns",
            r#"["ok","x/./y"]"#.into(),
            "dot_component",
        ),
    ];
    let mut request_ids = Vec::new();
    for (field, raw, code) in cases {
        // A valid unrelated change rides along; the whole request must be refused.
        let body = edit(&read, json!({"name":"Must not apply", field: raw}));
        request_ids.push(body["save_request_id"].as_str().unwrap().to_string());
        let (status, error) = put_repo(&client, addr, "V", &body).await;
        assert_eq!(status, 400, "{field} {raw}: {error}");
        let message = error["error"].as_str().unwrap();
        assert!(message.starts_with(field), "{message}");
        assert!(message.contains(code), "{message}");
        assert!(message.contains("nothing was saved"), "{message}");
    }
    state
        .db
        .conn()
        .execute_batch("DROP TRIGGER rs56_no_repo_write")
        .unwrap();
    let repo = state.db.get_repository("V").unwrap().unwrap();
    assert_eq!(repo.name, "Synthetic V");
    assert_eq!(
        stored(&state, "V"),
        (Some(allowed.into()), Some(blocked.into()))
    );
    for request in request_ids {
        let lookup = client
            .get(format!(
                "http://{addr}/api/repository-save-requests/{request}"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(
            lookup.status(),
            404,
            "no receipt may exist for a refused save"
        );
    }
    // Positive control: the same lookup does find a receipt for an accepted save.
    let valid = edit(&read, json!({"allowed_paths":r#"["src","lib"]"#}));
    assert_eq!(put_repo(&client, addr, "V", &valid).await.0, 200);
    let found = client
        .get(format!(
            "http://{addr}/api/repository-save-requests/{}",
            valid["save_request_id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(found.status(), 200);
    server.abort();
}

#[tokio::test]
async fn rs56_policy_save_preserves_credentials_unrelated_settings_and_runtime_progress() {
    let (addr, state, server, _tmp) =
        policy_fixture(vec![policy_repo("C", Some(r#"["src"]"#), None)]).await;
    let client = authed_client();
    let read = get_repo(&client, addr, "C").await;
    let credentials = json!({"save_request_id":uuid::Uuid::new_v4().to_string(),
        "expected_updated_at":read["updated_at"],"svn_password":"rs56-synthetic-svn","git_token":"rs56-synthetic-git"});
    let response = client
        .post(format!("http://{addr}/api/repos/C/credentials"))
        .json(&credentials)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let before = state.db.get_repository("C").unwrap().unwrap();

    let reread = get_repo(&client, addr, "C").await;
    let (status, saved) = put_repo(
        &client,
        addr,
        "C",
        &edit(
            &reread,
            json!({"allowed_paths":r#"["src","docs"]"#,"blocked_patterns":r#"["*.bak"]"#}),
        ),
    )
    .await;
    assert_eq!(status, 200, "{saved}");
    assert!(!saved.to_string().contains("rs56-synthetic"));
    let after = state.db.get_repository("C").unwrap().unwrap();
    assert_eq!(after.allowed_paths.as_deref(), Some(r#"["src","docs"]"#));
    assert_eq!(after.blocked_patterns.as_deref(), Some(r#"["*.bak"]"#));
    // Unrelated configuration, runtime progress and per-repo settings are untouched.
    assert_eq!(
        (
            &after.name,
            &after.svn_url,
            &after.svn_branch,
            &after.git_repo,
            &after.git_branch
        ),
        (
            &before.name,
            &before.svn_url,
            &before.svn_branch,
            &before.git_repo,
            &before.git_branch
        )
    );
    assert_eq!(
        (
            after.poll_interval_secs,
            after.lfs_threshold_mb,
            after.auto_merge,
            after.enabled
        ),
        (
            before.poll_interval_secs,
            before.lfs_threshold_mb,
            before.auto_merge,
            before.enabled
        )
    );
    assert_eq!(
        (after.last_svn_rev, &after.last_git_sha, &after.last_sync_at),
        (
            before.last_svn_rev,
            &before.last_git_sha,
            &before.last_sync_at
        )
    );
    assert_eq!(
        (
            after.total_syncs,
            after.total_errors,
            after.consecutive_errors
        ),
        (
            before.total_syncs,
            before.total_errors,
            before.consecutive_errors
        )
    );
    assert_eq!(after.teams_webhook_url, before.teams_webhook_url);
    assert_eq!(
        state
            .db
            .get_state("secret_svn_password_C")
            .unwrap()
            .as_deref(),
        Some("rs56-synthetic-svn")
    );
    assert_eq!(
        state.db.get_state("secret_git_token_C").unwrap().as_deref(),
        Some("rs56-synthetic-git")
    );
    server.abort();
}

#[tokio::test]
async fn rs56_policy_save_receipt_is_stable_and_stale_editor_cannot_overwrite() {
    let (addr, state, server, _tmp) =
        policy_fixture(vec![policy_repo("R", Some(r#"["src"]"#), None)]).await;
    let client = authed_client();
    let read = get_repo(&client, addr, "R").await;
    let body = edit(
        &read,
        json!({"allowed_paths":r#"["lib"]"#,"blocked_patterns":null}),
    );
    let (status, first) = put_repo(&client, addr, "R", &body).await;
    assert_eq!(status, 200);
    let receipt = first["save_receipt"].clone();
    assert_eq!(receipt["request_id"], body["save_request_id"]);
    assert_eq!(receipt["kind"], "configuration");

    // A newer administrator change must survive a lost-reply replay of the old request.
    let mut newer = state.db.get_repository("R").unwrap().unwrap();
    newer.allowed_paths = Some(r#"["newer"]"#.into());
    newer.updated_at = (chrono::Utc::now() + chrono::Duration::seconds(1)).to_rfc3339();
    state.db.update_repository(&newer).unwrap();
    let (status, replay) = put_repo(&client, addr, "R", &body).await;
    assert_eq!(status, 200);
    assert_eq!(replay["save_receipt"], receipt);
    assert_eq!(stored(&state, "R").0.as_deref(), Some(r#"["newer"]"#));

    // The same request ID with a different policy payload is a conflict, not a write.
    let mut changed = body.clone();
    changed["allowed_paths"] = json!(r#"["other"]"#);
    assert_eq!(put_repo(&client, addr, "R", &changed).await.0, 409);
    // A save prepared from an older read (stale editor) cannot overwrite newer policy.
    let stale = edit(&read, json!({"allowed_paths":r#"["stale"]"#}));
    assert_eq!(put_repo(&client, addr, "R", &stale).await.0, 409);
    assert_eq!(stored(&state, "R"), (Some(r#"["newer"]"#.into()), None));
    server.abort();
}

/// (allowed, blocked, path, expected). `None` = included in Git→SVN;
/// `Some("")` = excluded because not under an allowed prefix;
/// `Some(pattern)` = excluded by that first matching blocked pattern.
type Vector = (
    &'static [&'static str],
    &'static [&'static str],
    &'static str,
    Option<&'static str>,
);

const PREVIEW_VECTORS: &[Vector] = &[
    (&[], &[], "anything/at/all.txt", None),
    (&["team"], &[], "team", None),
    (&["team"], &[], "team/foo.txt", None),
    (&["team"], &[], "team-other/x.txt", Some("")),
    (&["team"], &[], "Team/x.txt", Some("")),
    (&["team/"], &[], "team/a.txt", None),
    (&["/team"], &[], "team/a.txt", None),
    (&["team\\docs"], &[], "team/docs/a.md", None),
    (&["team"], &[], "/team/x.txt", None),
    (&["src"], &[], "src\\x.rs", None),
    (&[], &["*.exe"], "bin/tool.exe", Some("*.exe")),
    (&[], &["*.exe"], "bin/tool.exe.txt", None),
    (&[], &["secret-*"], "secret-note.txt", Some("secret-*")),
    (
        &[],
        &["secret-*"],
        "secret-note/child.txt",
        Some("secret-*"),
    ),
    (&[], &["secret-*"], "a/secret-x.txt", None),
    (&[], &["secret"], "secret/leak.txt", Some("secret")),
    (&[], &["secret"], "secret", Some("secret")),
    (&[], &["secret"], "secrets/x.txt", None),
    (&[], &["temp/"], "temp/x", Some("temp/")),
    (&[], &["temp/"], "tempfile", None),
    (&[], &["*"], "a", Some("*")),
    (&[], &["a*b"], "a*b/c", Some("a*b")),
    (&[], &["a*b"], "axb", None),
    (&["src"], &["src/gen"], "src/main.rs", None),
    (&["src"], &["src/gen"], "src/gen/x.rs", Some("src/gen")),
    (&["src"], &["src/gen"], "docs/a.md", Some("")),
    (&["src"], &["*.exe"], "docs/x.exe", Some("")),
    (&[], &["*.log", "logs"], "logs/a.log", Some("*.log")),
];

#[test]
fn rs56_preview_vectors_match_the_engine_projection() {
    for (allowed, blocked, path, expected) in PREVIEW_VECTORS {
        let allowed: Vec<String> = allowed.iter().map(|s| s.to_string()).collect();
        let blocked: Vec<String> = blocked.iter().map(|s| s.to_string()).collect();
        let included = path_is_projected(path, &allowed, &blocked);
        assert_eq!(
            included,
            expected.is_none(),
            "{allowed:?} {blocked:?} {path}"
        );
        if let Some(reason) = expected {
            let message = exclusion_message(path, &allowed, &blocked);
            if reason.is_empty() {
                assert!(message.contains("not under allowed paths"), "{message}");
            } else {
                assert!(
                    message.ends_with(&format!("matches blocked pattern '{reason}'")),
                    "{message}"
                );
            }
        }
    }
}
