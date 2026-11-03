//! Integration tests proving the web server does not freeze when the sync
//! engine's database mutex is held (the root cause of the 60-second hang bug).
//!
//! Each test targets a different aspect of the fix:
//!   1. Concurrent web requests complete even when the sync engine DB is locked.
//!   2. Saturating the `spawn_blocking` pool does not block pure-async tasks.
//!   3. Two `Database` instances on the same file have independent Rust mutexes.
//!   4. The health-check endpoint responds within 100 ms under extreme load.
//!   5. Sustained polling + periodic sync DB locks over 15 seconds.
//!   6. LDAP auth timeout with concurrent logins falls back to local bcrypt.
//!   7. WAL-mode SQLite: concurrent writer + readers on same file.
//!   8. Saturating spawn_blocking pool while server serves requests.
//!   9. Concurrent requests while web DB mutex is held.
//!  10. No file-descriptor or memory leaks after 1 000 requests.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// A fixed test token pre-seeded into every test server's session map.
const TEST_TOKEN: &str = "test-session-token-for-freeze-tests";

/// Build a minimal `AppConfig` suitable for testing.
///
/// Sets an admin password so auth doesn't require setup wizard completion.
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

/// Spin up an Axum server on a random port and return everything needed to
/// drive tests against it.
///
/// Returns `(addr, shared_state, server_handle, _tmpdir)`.  The caller must
/// keep `_tmpdir` alive for the duration of the test so the git repo and data
/// directory are not deleted.
async fn build_test_server() -> (
    SocketAddr,
    Arc<AppState>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let git_repo_path = tmp.path().join("git-repo");
    git2::Repository::init(&git_repo_path).expect("git init");

    let config = minimal_config(tmp.path());

    // Two separate Database instances → two independent Rust mutexes.
    let web_db = Database::in_memory().expect("web db");
    web_db.initialize().expect("web db init");

    let engine_db = Database::in_memory().expect("engine db");
    engine_db.initialize().expect("engine db init");

    let svn_client = SvnClient::new(
        "https://svn.test.invalid/repo",
        "testuser",
        "",
    );
    let git_client = GitClient::new(&git_repo_path).expect("git client");
    let identity_mapper = Arc::new(
        IdentityMapper::new(&IdentityConfig::default()).expect("identity mapper"),
    );

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
    });

    let app = Router::new()
        .merge(api::status::routes())
        .merge(api::auth::routes())
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let handle = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.ok();
    });

    // Pre-seed a test session so authenticated endpoints work.
    {
        let mut sessions = state.sessions.write().await;
        sessions.insert(
            TEST_TOKEN.to_string(),
            chrono::Utc::now() + chrono::Duration::hours(24),
        );
    }

    // Give the server a moment to start accepting connections.
    tokio::time::sleep(Duration::from_millis(50)).await;

    (addr, state, handle, tmp)
}

/// Build a `reqwest::Client` with the test auth token as a default header.
fn authed_client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", TEST_TOKEN)).unwrap(),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap()
}

/// Like `build_test_server` but merges more route modules (repos, audit)
/// so we can exercise a wider surface area under load.
async fn build_test_server_full() -> (
    SocketAddr,
    Arc<AppState>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let git_repo_path = tmp.path().join("git-repo");
    git2::Repository::init(&git_repo_path).expect("git init");

    let config = minimal_config(tmp.path());

    let web_db = Database::in_memory().expect("web db");
    web_db.initialize().expect("web db init");

    let engine_db = Database::in_memory().expect("engine db");
    engine_db.initialize().expect("engine db init");

    let svn_client = SvnClient::new(
        "https://svn.test.invalid/repo",
        "testuser",
        "",
    );
    let git_client = GitClient::new(&git_repo_path).expect("git client");
    let identity_mapper = Arc::new(
        IdentityMapper::new(&IdentityConfig::default()).expect("identity mapper"),
    );

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
    });

    let app = Router::new()
        .merge(api::status::routes())
        .merge(api::auth::routes())
        .merge(api::repos::routes())
        .merge(api::audit::routes())
        .merge(api::sync_history::routes())
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let handle = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.ok();
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

/// Build a test server with LDAP enabled and a local user provisioned.
///
/// LDAP points to an unreachable TEST-NET address so it will timeout/fail,
/// exercising the "LDAP fail → local bcrypt fallback" code path.
async fn build_test_server_with_ldap(
    test_password: &str,
) -> (
    SocketAddr,
    Arc<AppState>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let git_repo_path = tmp.path().join("git-repo");
    git2::Repository::init(&git_repo_path).expect("git init");

    let config = minimal_config(tmp.path());

    let web_db = Database::in_memory().expect("web db");
    web_db.initialize().expect("web db init");

    // Insert a local user with known bcrypt password hash.
    let password_hash = reposync_core::crypto::hash_password(test_password)
        .expect("hash_password");
    let now = chrono::Utc::now().to_rfc3339();
    let user = reposync_core::models::User {
        id: "test-user-1".to_string(),
        username: "testuser".to_string(),
        display_name: "Test User".to_string(),
        email: "test@example.com".to_string(),
        password_hash,
        role: "admin".to_string(),
        enabled: true,
        created_at: now.clone(),
        updated_at: now,
    };
    web_db.insert_user(&user).expect("insert_user");

    // Save LDAP config pointing to an unreachable address (RFC 5737 TEST-NET).
    let ldap_config = reposync_core::ldap_auth::LdapConfig {
        url: "ldaps://192.0.2.1:636".to_string(),
        base_dn: "dc=test,dc=invalid".to_string(),
        search_filter: "(sAMAccountName={0})".to_string(),
        display_name_attr: "displayName".to_string(),
        email_attr: "mail".to_string(),
        group_attr: "memberOf".to_string(),
        bind_dn: None,
        bind_password: None,
        tls_verify: true,
    };
    web_db
        .save_ldap_config(&ldap_config, true)
        .expect("save_ldap_config");

    let engine_db = Database::in_memory().expect("engine db");
    engine_db.initialize().expect("engine db init");

    let svn_client = SvnClient::new(
        "https://svn.test.invalid/repo",
        "testuser",
        "",
    );
    let git_client = GitClient::new(&git_repo_path).expect("git client");
    let identity_mapper = Arc::new(
        IdentityMapper::new(&IdentityConfig::default()).expect("identity mapper"),
    );

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
    });

    let app = Router::new()
        .merge(api::status::routes())
        .merge(api::auth::routes())
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let handle = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.ok();
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

// ---------------------------------------------------------------------------
// Test 1 — Concurrent web requests are NOT blocked by sync engine DB lock
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_web_requests_not_blocked_by_sync_engine_db() {
    let (addr, state, _server, _tmp) = build_test_server().await;
    let base_url = format!("http://{}", addr);

    // Simulate a long sync cycle by holding the sync engine's DB mutex.
    let sync_engine = state.sync_engine.clone();
    let blocker = tokio::task::spawn_blocking(move || {
        let _guard = sync_engine.db().conn();
        std::thread::sleep(Duration::from_secs(5));
    });

    // Ensure the blocker has acquired the lock before firing requests.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Fire 20 concurrent requests to health + status endpoints.
    let client = authed_client();

    let mut handles = Vec::new();
    for i in 0..20 {
        let c = client.clone();
        let url = if i % 2 == 0 {
            format!("{}/api/status/health", base_url)
        } else {
            format!("{}/api/status", base_url)
        };
        handles.push(tokio::spawn(async move {
            let start = Instant::now();
            let resp = c.get(&url).send().await;
            (url, resp, start.elapsed())
        }));
    }

    // Every single request must complete within the 3-second window.
    let deadline = tokio::time::timeout(Duration::from_secs(3), async {
        let mut results = Vec::new();
        for h in handles {
            results.push(h.await.expect("join"));
        }
        results
    })
    .await
    .expect("requests timed out — web server blocked by sync engine DB lock");

    for (url, resp, elapsed) in &deadline {
        let resp = resp.as_ref().expect("HTTP error");
        assert!(
            resp.status().is_success(),
            "{} returned {}",
            url,
            resp.status()
        );
        assert!(
            *elapsed < Duration::from_secs(3),
            "{} took {:?}",
            url,
            elapsed
        );
    }

    blocker.await.ok();
}

// ---------------------------------------------------------------------------
// Test 2 — spawn_blocking pool exhaustion does NOT block async tasks
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_spawn_blocking_exhaustion_does_not_block_async_tasks() {
    let mutex = Arc::new(std::sync::Mutex::new(()));

    // Hold the mutex so every spawn_blocking task blocks.
    let guard = mutex.lock().unwrap();

    let mut blocking_handles = Vec::new();
    for _ in 0..600 {
        let m = mutex.clone();
        blocking_handles.push(tokio::task::spawn_blocking(move || {
            let _lock = m.lock().unwrap();
        }));
    }

    // Let the runtime schedule some of those blocking tasks.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // A pure async task must still complete promptly — this is the same
    // execution model as the health_check handler.
    let result = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        42u32
    })
    .await;

    assert!(
        result.is_ok(),
        "pure async task blocked despite spawn_blocking pool being saturated"
    );
    assert_eq!(result.unwrap(), 42);

    // Unblock all the waiting tasks.
    drop(guard);
    for h in blocking_handles {
        h.await.ok();
    }
}

// ---------------------------------------------------------------------------
// Test 3 — Separate Database instances have independent Rust mutexes
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_separate_database_instances_no_mutex_contention() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("shared.db");

    let db1 = Database::new(&db_path).expect("db1");
    db1.initialize().expect("db1 init");

    let db2 = Database::new(&db_path).expect("db2");

    // Hold db1's Rust mutex for 5 seconds.
    let blocker = tokio::task::spawn_blocking(move || {
        {
            let _guard = db1.conn();
            std::thread::sleep(Duration::from_secs(5));
        } // _guard dropped here, releasing the mutex
        db1 // keep db1 alive so it's not dropped early
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // db2 should acquire its *own* Rust mutex instantly — it is a separate
    // Mutex<Connection>.  SQLite WAL mode allows concurrent readers on the
    // same file.
    let reader = tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        let _conn = db2.conn();
        start.elapsed()
    });

    let read_elapsed = tokio::time::timeout(Duration::from_secs(2), reader)
        .await
        .expect("reader timed out — separate DB instances may share Rust mutex")
        .expect("join");

    assert!(
        read_elapsed < Duration::from_secs(1),
        "reader took {:?}, expected near-instant for separate Mutex",
        read_elapsed
    );

    blocker.await.ok();
}

// ---------------------------------------------------------------------------
// Test 4 — Health check responds even under spawn_blocking saturation
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_health_check_responds_under_spawn_blocking_saturation() {
    let (addr, _state, _server, _tmp) = build_test_server().await;
    let base_url = format!("http://{}", addr);

    // Saturate the spawn_blocking pool with tasks that block on a held mutex.
    let mutex = Arc::new(std::sync::Mutex::new(()));
    let guard = mutex.lock().unwrap();

    let mut blockers = Vec::new();
    for _ in 0..600 {
        let m = mutex.clone();
        blockers.push(tokio::task::spawn_blocking(move || {
            let _lock = m.lock().unwrap();
        }));
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    // health_check is a pure async handler — no spawn_blocking, no DB access.
    // It must respond within 500 ms even with the blocking pool exhausted.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();

    for _ in 0..5 {
        let start = Instant::now();
        let resp = tokio::time::timeout(
            Duration::from_millis(500),
            client.get(format!("{}/api/status/health", base_url)).send(),
        )
        .await
        .expect("health check timed out under spawn_blocking saturation")
        .expect("HTTP error");

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);

        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "health check took {:?}, expected < 500ms",
            elapsed
        );
    }

    // Clean up.
    drop(guard);
    for h in blockers {
        h.await.ok();
    }
}

// ---------------------------------------------------------------------------
// Test 5 — Sustained polling load while sync engine DB is periodically locked
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_sustained_load_under_sync_cycles() {
    let (addr, state, _server, _tmp) = build_test_server_full().await;
    let base_url = format!("http://{}", addr);

    let test_duration = Duration::from_secs(15);
    let start = Instant::now();

    // Track the worst-case latencies seen by each poller.
    let max_health_latency = Arc::new(AtomicU64::new(0));
    let max_other_latency = Arc::new(AtomicU64::new(0));
    let total_requests = Arc::new(AtomicU64::new(0));
    let failed_requests = Arc::new(AtomicU64::new(0));

    // --- Sync simulation: lock the sync engine DB for 2 s every 5 s ----------
    let sync_engine = state.sync_engine.clone();
    let sync_task = {
        let start = start;
        tokio::spawn(async move {
            while start.elapsed() < test_duration {
                let se = sync_engine.clone();
                tokio::task::spawn_blocking(move || {
                    let _guard = se.db().conn();
                    std::thread::sleep(Duration::from_secs(2));
                })
                .await
                .ok();
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        })
    };

    // --- Poller helper -------------------------------------------------------
    let spawn_poller = |url: String,
                        interval: Duration,
                        is_health: bool,
                        max_lat: Arc<AtomicU64>,
                        total: Arc<AtomicU64>,
                        failed: Arc<AtomicU64>| {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", TEST_TOKEN)).unwrap(),
        );
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .default_headers(headers)
            .build()
            .unwrap();
        let start = start;
        tokio::spawn(async move {
            while start.elapsed() < test_duration {
                let req_start = Instant::now();
                let resp = client.get(&url).send().await;
                let elapsed_ms = req_start.elapsed().as_millis() as u64;
                total.fetch_add(1, Ordering::Relaxed);

                match resp {
                    Ok(r) if r.status().is_success() => {}
                    _ => {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                }

                // Update max latency.
                let _ = max_lat.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                    if elapsed_ms > cur {
                        Some(elapsed_ms)
                    } else {
                        None
                    }
                });

                if is_health {
                    // Immediate assertion for health: must be fast.
                    assert!(
                        elapsed_ms < 200,
                        "health check took {}ms, expected < 200ms",
                        elapsed_ms
                    );
                }

                tokio::time::sleep(interval).await;
            }
        })
    };

    let pollers = vec![
        // Health check every 1 s
        spawn_poller(
            format!("{}/api/status/health", base_url),
            Duration::from_secs(1),
            true,
            max_health_latency.clone(),
            total_requests.clone(),
            failed_requests.clone(),
        ),
        // GET /api/status every 3 s
        spawn_poller(
            format!("{}/api/status", base_url),
            Duration::from_secs(3),
            false,
            max_other_latency.clone(),
            total_requests.clone(),
            failed_requests.clone(),
        ),
        // GET /api/auth/info every 5 s (unauthenticated)
        spawn_poller(
            format!("{}/api/auth/info", base_url),
            Duration::from_secs(5),
            false,
            max_other_latency.clone(),
            total_requests.clone(),
            failed_requests.clone(),
        ),
        // GET /api/repos every 5 s
        spawn_poller(
            format!("{}/api/repos", base_url),
            Duration::from_secs(5),
            false,
            max_other_latency.clone(),
            total_requests.clone(),
            failed_requests.clone(),
        ),
    ];

    // Wait for all pollers to finish.
    for p in pollers {
        p.await.ok();
    }
    sync_task.await.ok();

    let total = total_requests.load(Ordering::Relaxed);
    let failed = failed_requests.load(Ordering::Relaxed);
    let worst_health = max_health_latency.load(Ordering::Relaxed);
    let worst_other = max_other_latency.load(Ordering::Relaxed);

    assert!(
        total > 10,
        "expected >10 total requests, got {}",
        total
    );
    assert_eq!(
        failed, 0,
        "{} out of {} requests failed",
        failed, total
    );
    assert!(
        worst_health < 200,
        "worst health latency {}ms >= 200ms",
        worst_health
    );
    assert!(
        worst_other < 3000,
        "worst endpoint latency {}ms >= 3000ms",
        worst_other
    );
}

// ---------------------------------------------------------------------------
// Test 6 — LDAP auth timeout: concurrent logins fall back to local bcrypt
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_ldap_auth_timeout_under_load() {
    let test_password = "correct-horse-battery-staple";
    let (addr, _state, _server, _tmp) =
        build_test_server_with_ldap(test_password).await;
    let base_url = format!("http://{}", addr);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    // Fire 5 concurrent login requests.  Each will try LDAP (unreachable →
    // timeout/error) then fall back to local bcrypt.
    let mut handles = Vec::new();
    for _ in 0..5 {
        let c = client.clone();
        let url = format!("{}/api/auth/login", base_url);
        let pw = test_password.to_string();
        handles.push(tokio::spawn(async move {
            let start = Instant::now();
            let resp = c
                .post(&url)
                .json(&serde_json::json!({
                    "username": "testuser",
                    "password": pw,
                }))
                .send()
                .await;
            (resp, start.elapsed())
        }));
    }

    // Also verify health check still responds during LDAP timeouts.
    let health_handle = {
        let c = client.clone();
        let url = format!("{}/api/status/health", base_url);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let start = Instant::now();
            let resp = c.get(&url).send().await;
            (resp, start.elapsed())
        })
    };

    // Collect login results — all must complete within 15 s total.
    let all_logins = tokio::time::timeout(Duration::from_secs(15), async {
        let mut results = Vec::new();
        for h in handles {
            results.push(h.await.expect("join"));
        }
        results
    })
    .await
    .expect("login requests timed out — possible serial LDAP blocking");

    for (i, (resp, elapsed)) in all_logins.iter().enumerate() {
        let r = resp.as_ref().expect("HTTP error on login");
        assert!(
            r.status().is_success(),
            "login {} returned {}, elapsed {:?}",
            i,
            r.status(),
            elapsed
        );
    }

    // Health check must have responded promptly.
    let (health_resp, health_elapsed) = health_handle.await.expect("join");
    let health_resp = health_resp.expect("HTTP error on health");
    assert_eq!(health_resp.status(), 200);
    assert!(
        health_elapsed < Duration::from_secs(1),
        "health check during LDAP timeouts took {:?}",
        health_elapsed
    );
}

// ---------------------------------------------------------------------------
// Test 7 — Database WAL contention: writer + readers on same file
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_database_wal_contention() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("wal-test.db");

    let db_writer = Database::new(&db_path).expect("db_writer");
    db_writer.initialize().expect("db_writer init");

    let db_reader1 = Database::new(&db_path).expect("db_reader1");
    let db_reader2 = Database::new(&db_path).expect("db_reader2");
    let db_reader3 = Database::new(&db_path).expect("db_reader3");

    let writer_done = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Writer: insert 100 audit_log entries as fast as possible.
    let wd = writer_done.clone();
    let writer = tokio::task::spawn_blocking(move || {
        for i in 0..100 {
            db_writer
                .insert_audit_log(
                    &format!("test-action-{}", i),
                    Some("svn_to_git"),
                    Some(i),
                    Some("abc123"),
                    Some("tester"),
                    Some("test details"),
                    true,
                )
                .expect("insert_audit_log");
        }
        wd.store(true, Ordering::Release);
    });

    // Give the writer a head start.
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Reader helper: repeatedly call a DB method, track max latency.
    let spawn_reader = |db: Database,
                        done: Arc<std::sync::atomic::AtomicBool>,
                        op: fn(&Database)| {
        tokio::task::spawn_blocking(move || {
            let mut max_ms: u64 = 0;
            let mut count: u64 = 0;
            while !done.load(Ordering::Acquire) || count < 10 {
                let t = Instant::now();
                op(&db);
                let elapsed_ms = t.elapsed().as_millis() as u64;
                if elapsed_ms > max_ms {
                    max_ms = elapsed_ms;
                }
                count += 1;
                if count > 500 {
                    break; // safety valve
                }
            }
            (max_ms, count)
        })
    };

    fn read_count_errors(db: &Database) {
        let _ = db.count_errors();
    }
    fn read_list_audit(db: &Database) {
        let _ = db.list_audit_log(10, 0);
    }
    fn read_get_state(db: &Database) {
        let _ = db.get_state("nonexistent_key");
    }

    let r1 = spawn_reader(db_reader1, writer_done.clone(), read_count_errors);
    let r2 = spawn_reader(db_reader2, writer_done.clone(), read_list_audit);
    let r3 = spawn_reader(db_reader3, writer_done.clone(), read_get_state);

    writer.await.expect("writer");

    let results = tokio::time::timeout(Duration::from_secs(10), async {
        let r1 = r1.await.expect("reader1");
        let r2 = r2.await.expect("reader2");
        let r3 = r3.await.expect("reader3");
        vec![
            ("count_errors", r1),
            ("list_audit_log", r2),
            ("get_state", r3),
        ]
    })
    .await
    .expect("readers timed out — possible WAL deadlock");

    for (name, (max_ms, count)) in &results {
        assert!(
            *count > 0,
            "{} did not complete any reads",
            name
        );
        assert!(
            *max_ms < 100,
            "{} worst read latency was {}ms (expected < 100ms, {} reads)",
            name,
            max_ms,
            count
        );
    }
}

// ---------------------------------------------------------------------------
// Test 8 — spawn_blocking pool pressure: server still serves while pool full
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_spawn_blocking_pool_pressure_with_server() {
    let (addr, _state, _server, _tmp) = build_test_server().await;
    let base_url = format!("http://{}", addr);

    // Saturate the spawn_blocking pool.
    let mutex = Arc::new(std::sync::Mutex::new(()));
    let guard = mutex.lock().unwrap();

    let mut blockers = Vec::new();
    for _ in 0..512 {
        let m = mutex.clone();
        blockers.push(tokio::task::spawn_blocking(move || {
            let _lock = m.lock().unwrap();
        }));
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    // 1. Pure async tasks must still run.
    let async_result = tokio::time::timeout(Duration::from_millis(100), async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        true
    })
    .await;
    assert!(
        async_result.is_ok(),
        "pure async task blocked by spawn_blocking saturation"
    );

    // 2. Health check (pure async) must respond within 200 ms.
    let client = authed_client();

    let start = Instant::now();
    let resp = tokio::time::timeout(
        Duration::from_millis(200),
        client.get(format!("{}/api/status/health", base_url)).send(),
    )
    .await
    .expect("health check timed out under spawn_blocking pool pressure")
    .expect("HTTP error");
    assert_eq!(resp.status(), 200);
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "health check took {:?}",
        start.elapsed()
    );

    // 3. Status endpoint also responds (it reads from web DB which is not
    //    behind spawn_blocking, but validate_session does a DB call).
    //    With auth bypassed (no users, no admin_password) this should be fast.
    let start = Instant::now();
    let resp = tokio::time::timeout(
        Duration::from_secs(2),
        client.get(format!("{}/api/status", base_url)).send(),
    )
    .await
    .expect("/api/status timed out under spawn_blocking pool pressure")
    .expect("HTTP error");
    assert!(
        resp.status().is_success(),
        "/api/status returned {}",
        resp.status()
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "/api/status took {:?}",
        start.elapsed()
    );

    // Clean up.
    drop(guard);
    for h in blockers {
        h.await.ok();
    }
}

// ---------------------------------------------------------------------------
// Test 9 — Concurrent auth + DB access under web DB mutex contention
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_auth_and_db_access() {
    let (addr, state, _server, _tmp) = build_test_server_full().await;
    let base_url = format!("http://{}", addr);

    // Periodically hold the web DB mutex for 500 ms to simulate contention.
    let db_blocker_state = state.clone();
    let blocker = tokio::spawn(async move {
        for _ in 0..3 {
            let s = db_blocker_state.clone();
            tokio::task::spawn_blocking(move || {
                let _guard = s.db.conn();
                std::thread::sleep(Duration::from_millis(500));
            })
            .await
            .ok();
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });

    // Give the first lock a moment to acquire.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Fire 20 concurrent requests to /api/status (requires validate_session → DB).
    let client = authed_client();

    let mut handles = Vec::new();
    for i in 0..20 {
        let c = client.clone();
        let url = format!("{}/api/status", base_url);
        handles.push(tokio::spawn(async move {
            let start = Instant::now();
            let resp = c.get(&url).send().await;
            (i, resp, start.elapsed())
        }));
    }

    // All 20 must complete within 10 seconds (accounting for serial mutex access).
    let results = tokio::time::timeout(Duration::from_secs(10), async {
        let mut out = Vec::new();
        for h in handles {
            out.push(h.await.expect("join"));
        }
        out
    })
    .await
    .expect("requests timed out — web DB mutex caused indefinite blocking");

    for (i, resp, elapsed) in &results {
        let r = resp.as_ref().expect("HTTP error");
        assert!(
            r.status().is_success(),
            "request {} returned {}, took {:?}",
            i,
            r.status(),
            elapsed
        );
    }

    blocker.await.ok();
}

// ---------------------------------------------------------------------------
// Test 10 — No file-descriptor or memory leak after many requests
// ---------------------------------------------------------------------------

/// Count open file descriptors for the current process (Linux only).
#[cfg(target_os = "linux")]
fn count_open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|entries| entries.count())
        .unwrap_or(0)
}

/// Read VmRSS (resident set size) in kilobytes from /proc/self/status (Linux).
#[cfg(target_os = "linux")]
fn rss_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                return parts[1].parse().unwrap_or(0);
            }
        }
    }
    0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_no_resource_leak_after_many_requests() {
    let (addr, _state, _server, _tmp) = build_test_server_full().await;
    let base_url = format!("http://{}", addr);

    let client = authed_client();

    // Warm up: make a few requests so any lazy initialization is done.
    for _ in 0..10 {
        client
            .get(format!("{}/api/status/health", base_url))
            .send()
            .await
            .ok();
    }

    #[cfg(target_os = "linux")]
    let fds_before = count_open_fds();
    #[cfg(target_os = "linux")]
    let rss_before = rss_kb();

    // Make 1000 requests across various endpoints.
    let endpoints = [
        "/api/status/health",
        "/api/status",
        "/api/auth/info",
        "/api/repos",
    ];

    for i in 0..1000 {
        let ep = endpoints[i % endpoints.len()];
        let resp = client
            .get(format!("{}{}", base_url, ep))
            .send()
            .await;
        match resp {
            Ok(r) => assert!(
                r.status().is_success(),
                "request {} to {} returned {}",
                i,
                ep,
                r.status()
            ),
            Err(e) => panic!("request {} to {} failed: {}", i, ep, e),
        }
    }

    // Give the runtime a moment to clean up connections.
    tokio::time::sleep(Duration::from_millis(500)).await;

    #[cfg(target_os = "linux")]
    {
        let fds_after = count_open_fds();
        let rss_after = rss_kb();

        let fd_growth = fds_after.saturating_sub(fds_before);
        let rss_growth_kb = rss_after.saturating_sub(rss_before);

        assert!(
            fd_growth < 50,
            "file descriptor leak: grew by {} (before={}, after={})",
            fd_growth,
            fds_before,
            fds_after
        );

        // 50 MB = 51200 KB
        assert!(
            rss_growth_kb < 51200,
            "memory leak: RSS grew by {} KB ({:.1} MB) after 1000 requests",
            rss_growth_kb,
            rss_growth_kb as f64 / 1024.0
        );
    }
}

/// Regression test for the re-entrant Mutex deadlock in `list_commit_map`.
///
/// Root cause: the handler called `db.conn()` (holding the MutexGuard) then
/// called `db.list_commit_map()` in the `else` branch (no repo_id), which
/// internally called `self.conn()` on the same non-reentrant std::sync::Mutex.
/// This permanently deadlocked the thread, and because all authenticated
/// requests share the same mutex via validate_session, the entire server froze.
///
/// This test reproduces the exact frontend request pattern that triggers it:
/// the React RepoDetail page fires /api/commit-map?limit=15 WITHOUT repo_id
/// concurrently with 5 other endpoints.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_commit_map_no_repo_id_does_not_deadlock() {
    let (addr, _state, _server, _tmp) = build_test_server_full().await;
    let base_url = format!("http://{}", addr);

    let client = authed_client();

    // Run 10 rounds of 6 concurrent requests matching the exact frontend
    // pattern. Before the fix, the FIRST round deadlocked the server.
    for round in 0..10 {
        let mut handles = Vec::new();

        // The critical request: /api/commit-map WITHOUT repo_id hits the
        // else branch that previously called db.list_commit_map() while
        // holding a MutexGuard from db.conn().
        let c = client.clone();
        let u = base_url.clone();
        handles.push(tokio::spawn(async move {
            c.get(format!("{}/api/commit-map?limit=15", u))
                .send()
                .await
        }));

        // The other 5 endpoints the frontend fires concurrently.
        for endpoint in &[
            "/api/status/health",
            "/api/status",
            "/api/sync-records?limit=20",
            "/api/audit?limit=10",
            "/api/status/system",
        ] {
            let c = client.clone();
            let url = format!("{}{}", base_url, endpoint);
            handles.push(tokio::spawn(async move { c.get(&url).send().await }));
        }

        for (i, h) in handles.into_iter().enumerate() {
            let result = h.await.expect("task panicked");
            assert!(
                result.is_ok(),
                "round {} request {} timed out or failed: {:?} — \
                 server likely deadlocked on re-entrant db.conn() mutex",
                round,
                i,
                result.err()
            );
            let resp = result.unwrap();
            assert_eq!(
                resp.status().as_u16(),
                200,
                "round {} request {} returned {} — expected 200",
                round,
                i,
                resp.status()
            );
        }
    }
}
