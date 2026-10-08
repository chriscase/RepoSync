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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
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

    let svn_client = SvnClient::new("https://svn.test.invalid/repo", "testuser", "");
    let git_client = GitClient::new(&git_repo_path).expect("git client");
    let identity_mapper =
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).expect("identity mapper"));

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
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
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

    let svn_client = SvnClient::new("https://svn.test.invalid/repo", "testuser", "");
    let git_client = GitClient::new(&git_repo_path).expect("git client");
    let identity_mapper =
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).expect("identity mapper"));

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
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
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
    let password_hash = reposync_core::crypto::hash_password(test_password).expect("hash_password");
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

    let svn_client = SvnClient::new("https://svn.test.invalid/repo", "testuser", "");
    let git_client = GitClient::new(&git_repo_path).expect("git client");
    let identity_mapper =
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).expect("identity mapper"));

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
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
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
#[allow(clippy::await_holding_lock)]
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
#[allow(clippy::await_holding_lock)]
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
    let sync_task = tokio::spawn(async move {
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
    });

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
                let _ = max_lat.fetch_max(elapsed_ms, Ordering::Relaxed);

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

    assert!(total > 10, "expected >10 total requests, got {}", total);
    assert_eq!(failed, 0, "{} out of {} requests failed", failed, total);
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
    let (addr, _state, _server, _tmp) = build_test_server_with_ldap(test_password).await;
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
    let spawn_reader =
        |db: Database, done: Arc<std::sync::atomic::AtomicBool>, op: fn(&Database)| {
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
        assert!(*count > 0, "{} did not complete any reads", name);
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
#[allow(clippy::await_holding_lock)]
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
        let resp = client.get(format!("{}{}", base_url, ep)).send().await;
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
            c.get(format!("{}/api/commit-map?limit=15", u)).send().await
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

/// R02/R03 baseline API diagnostic with real disposable SVN and Git remotes.
/// The import progress represents the actual per-repository route's in-memory
/// state; the historical test name is retained for matched-base comparison.
/// Its old 404 observation is archived in the initial Phase 0 report.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostic_r02_r03_root_delete_disables_and_per_repo_cancel_is_missing() {
    use reposync_core::import::ImportPhase;
    use std::process::Command;
    assert!(
        Command::new("svnadmin").arg("--version").output().is_ok(),
        "svnadmin required"
    );
    let (addr, state, server, tmp) = build_test_server_full().await;
    if let Ok(root) = std::env::var("REPOSYNC_FIXTURE_ROOT") {
        let root = std::path::Path::new(&root).canonicalize().unwrap();
        let target = tmp.path().canonicalize().unwrap();
        assert!(
            target.starts_with(root),
            "API fixture target escaped owned root"
        );
    }
    let svn_repo = tmp.path().join("svn-fixture");
    let created = Command::new("svnadmin")
        .args(["create", svn_repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(created.status.success());
    let svn_url = format!("file://{}", svn_repo.display());
    let created = Command::new("svn")
        .args([
            "mkdir",
            &format!("{svn_url}/trunk"),
            "-m",
            "SVN fixture",
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let git_work = tmp.path().join("git-fixture");
    let git_bare = tmp.path().join("git-origin.git");
    assert!(Command::new("git")
        .args(["init", "--bare", git_bare.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["init", "-b", "main", git_work.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    std::fs::write(git_work.join("fixture.txt"), "preserve remote history\n").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&git_work)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["add", "fixture.txt"]);
    git(&["commit", "-m", "Fixture"]);
    git(&["remote", "add", "origin", git_bare.to_str().unwrap()]);
    git(&["push", "origin", "main"]);
    let git_before = Command::new("git")
        .args([
            "--git-dir",
            git_bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    assert!(git_before.status.success());

    // The provider is a loopback-only HTTP fixture. Its SHA is read from the
    // actual disposable bare Git ref rather than a made-up response value.
    git(&["checkout", "-b", "feature"]);
    std::fs::write(git_work.join("feature.txt"), "step one\n").unwrap();
    git(&["add", "feature.txt"]);
    git(&["commit", "-m", "Feature step one"]);
    std::fs::write(git_work.join("feature.txt"), "step two\n").unwrap();
    git(&["commit", "-am", "Feature step two"]);
    git(&["push", "origin", "feature"]);
    let feature_tip = Command::new("git")
        .args([
            "--git-dir",
            git_bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/feature",
        ])
        .output()
        .unwrap();
    assert!(feature_tip.status.success());
    let feature_tip = String::from_utf8_lossy(&feature_tip.stdout)
        .trim()
        .to_string();
    let branches_url = format!("{svn_url}/branches");
    let target_url = format!("{branches_url}/feature");
    let out = Command::new("svn")
        .args([
            "mkdir",
            &branches_url,
            "-m",
            "Branches",
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("svn")
        .args([
            "copy",
            &format!("{svn_url}/trunk"),
            &target_url,
            "-m",
            "Feature target",
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let svn_before = Command::new("svnlook")
        .args(["youngest", svn_repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(svn_before.status.success());
    let provider_sha = feature_tip.clone();
    let provider = axum::Router::new().route(
        "/api/v1/repos/local/fixture/branches/feature",
        axum::routing::get(move || {
            let sha = provider_sha.clone();
            async move { axum::Json(serde_json::json!({"commit":{"id":sha}})) }
        }),
    );
    let provider_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_addr = provider_listener.local_addr().unwrap();
    assert!(
        provider_addr.ip().is_loopback(),
        "provider endpoint must be enrolled loopback"
    );
    let provider_handle = tokio::spawn(async move {
        axum::serve(provider_listener, provider).await.unwrap();
    });

    let client = authed_client();
    let base = format!("http://{addr}");
    let response = client.post(format!("{base}/api/repos"))
        .json(&serde_json::json!({"name":"fixture", "svn_url":svn_url, "svn_branch":"trunk", "git_provider":"gitea", "git_api_url":format!("http://{provider_addr}/api/v1"), "git_repo":"local/fixture", "git_branch":"main"}))
        .send().await.unwrap();
    assert!(
        response.status().is_success(),
        "create: {}",
        response.status()
    );
    let body: serde_json::Value = response.json().await.unwrap();
    let id = body["id"].as_str().unwrap();

    let pair = client.post(format!("{base}/api/repos/{id}/branches"))
        .json(&serde_json::json!({"svn_branch":"branches/feature", "git_branch":"feature", "skip_import":true, "auto_create_svn_branch":false, "auto_create_git_branch":false}))
        .send().await.unwrap();
    let pair_status = pair.status();
    let pair: serde_json::Value = pair.json().await.unwrap();
    assert_eq!(
        pair_status,
        reqwest::StatusCode::BAD_REQUEST,
        "late pair of unproven Git-first history must refuse: {pair}"
    );
    let err = pair["error"].as_str().unwrap_or("");
    assert!(
        err.contains("missing_baseline") || err.contains("unrelated_git_first"),
        "expected lineage refusal, got {pair}"
    );
    assert!(state.db.list_child_repositories(id).unwrap().is_empty());
    assert!(state.db.list_commit_map(100).unwrap().is_empty());
    let svn_tree = Command::new("svn")
        .args(["list", &target_url, "--non-interactive"])
        .output()
        .unwrap();
    assert!(svn_tree.status.success());
    assert!(!String::from_utf8_lossy(&svn_tree.stdout).contains("feature.txt"));
    eprintln!(
        "R08 API: skip_import of Git-first feature was refused before watermark or child row"
    );

    let progress = state.get_repo_import_progress(id).await;
    progress.write().await.phase = ImportPhase::Importing;
    let cancel = client
        .post(format!("{base}/api/repos/{id}/import/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        cancel.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "cancellation requires an exact durable operation ID"
    );
    let status: serde_json::Value = client
        .get(format!("{base}/api/repos/{id}/import/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["phase"], "importing");
    assert!(status.get("operation_id").is_none());
    assert!(!progress.read().await.cancel_requested);
    assert!(!progress
        .read()
        .await
        .cancel_signal
        .load(std::sync::atomic::Ordering::Acquire));
    assert!(state.db.active_import_operation(id).unwrap().is_none());
    state
        .db
        .conn()
        .execute(
            "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
             VALUES (1, 'abc123', 'svn_to_git', 't', 'svn', 'git', ?1)",
            [id],
        )
        .unwrap();
    state
        .db
        .set_state(&format!("secret_svn_password_{id}"), "keep-secret")
        .unwrap();

    let delete = client
        .delete(format!("{base}/api/repos/{id}"))
        .send()
        .await
        .unwrap();

    assert!(delete.status().is_success());
    let delete_body: serde_json::Value = delete.json().await.unwrap();
    assert_eq!(delete_body["message"], "repository disabled");
    assert_eq!(delete_body["action"], "disable");
    assert!(!state.db.get_repository(id).unwrap().unwrap().enabled);
    assert!(state.db.get_repository(id).unwrap().is_some());
    let maps: i64 = state
        .db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM commit_map WHERE repo_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(maps, 1);
    assert_eq!(
        state
            .db
            .get_state(&format!("secret_svn_password_{id}"))
            .unwrap()
            .as_deref(),
        Some("keep-secret")
    );
    assert!(state.db.managed_removal(id).unwrap().is_none());
    assert!(state
        .db
        .list_repositories()
        .unwrap()
        .iter()
        .any(|repo| repo.id == id));
    let svn_after = Command::new("svnlook")
        .args(["youngest", svn_repo.to_str().unwrap()])
        .output()
        .unwrap();
    let git_after = Command::new("git")
        .args([
            "--git-dir",
            git_bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    assert_eq!(svn_before.stdout, svn_after.stdout);
    assert_eq!(git_before.stdout, git_after.stdout);
    eprintln!("CANDIDATE R02/R03: root DELETE retained disabled registration; missing operation ID rejected without changing import state. Remote SVN r{} and Git ref {} unchanged.", String::from_utf8_lossy(&svn_after.stdout).trim(), String::from_utf8_lossy(&git_after.stdout).trim());
    server.abort();
    provider_handle.abort();
}

/// Disposable real SVN history, local bare Git target, v12 file database and
/// the actual repository routes. No provider or production credentials.
async fn import_fixture() -> (
    SocketAddr,
    Arc<AppState>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
    String,
    std::path::PathBuf,
) {
    use std::process::Command;
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

    let config = minimal_config(tmp.path());
    let db = Database::new(tmp.path().join("reposync.db")).unwrap();
    db.initialize().unwrap();
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
        config_path: tmp.path().join("config.toml"),
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
        .merge(api::repos::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = authed_client();
    let response = client
        .post(format!("http://{addr}/api/repos"))
        .json(&serde_json::json!({
            "name":"history", "svn_url":svn_url, "svn_branch":"trunk",
            "svn_username":"fixture", "git_provider":"gitea",
            "git_api_url":format!("file://{}", tmp.path().display()),
            "git_repo":"local/history", "git_branch":"main",
        }))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let created: serde_json::Value = response.json().await.unwrap();
    (
        addr,
        state,
        server,
        tmp,
        created["id"].as_str().unwrap().to_string(),
        bare,
    )
}

#[cfg(feature = "reliability-browser")]
async fn run_import_card_browser(
    addr: SocketAddr,
    id: &str,
    barrier: &Path,
    mode: &str,
) -> (tokio::process::Child, std::process::Child) {
    use std::process::{Command, Stdio};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ui = root.join("web-ui");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let vite = Command::new("node")
        .arg(ui.join("node_modules/vite/bin/vite.js"))
        .args(["--host", "127.0.0.1", "--strictPort", "--port"])
        .arg(port.to_string())
        .env("REPOSYNC_TEST_API_URL", format!("http://{addr}"))
        .current_dir(&ui)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}/reliability-import.html?repo={id}");
    let client = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if client
                .get(&url)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    let artifacts = root.join("target/reliability-ui-artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let browser = tokio::process::Command::new("node")
        .arg(root.join("scripts/reliability-import-browser.mjs"))
        .env("REPOSYNC_UI_URL", url)
        .env("REPOSYNC_UI_MODE", mode)
        .env("REPOSYNC_UI_BARRIER", barrier)
        .env("REPOSYNC_UI_ARTIFACTS", &artifacts)
        .env("REPOSYNC_UI_TOKEN", TEST_TOKEN)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    (browser, vite)
}

#[cfg(feature = "reliability-browser")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_74_mounted_import_card_real_api_browser_journey() {
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let barrier = tmp.path().join(&id);
    std::fs::create_dir(&barrier).unwrap();
    std::fs::write(barrier.join("connecting.release"), b"go").unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
    std::env::set_var("REPOSYNC_IMPORT_BARRIER_DIR", &barrier);
    std::env::set_var("REPOSYNC_IMPORT_CANCEL_OBSERVE", "1");
    let (browser, mut vite) = run_import_card_browser(addr, &id, &barrier, "cancel").await;
    let mut browser = Some(browser);
    // The browser child starts asynchronously; wait for it to POST /import before
    // expecting worker fixture barriers so Chrome/Vite slowness cannot consume the
    // after_first_local budget.
    wait_for_file_timeout(
        &barrier.join("import_started.ready"),
        Duration::from_secs(60),
    )
    .await;
    wait_for_file(&barrier.join("after_first_local.ready")).await;
    state
        .db
        .conn()
        .execute_batch(
            "CREATE TRIGGER reject_cancel_receipt
         BEFORE UPDATE OF value ON kv_state
         WHEN NEW.key LIKE 'import_operation_v1:document:%' AND json_extract(NEW.value, '$.cancel_requested') = 1
         BEGIN SELECT RAISE(FAIL, 'fixture receipt failure'); END;",
        )
        .unwrap();
    std::fs::write(barrier.join("failure_ready"), b"ready").unwrap();
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            if barrier.join("failure_seen").exists() {
                break;
            }
            if browser.as_mut().unwrap().try_wait().unwrap().is_some() {
                let output = browser.take().unwrap().wait_with_output().await.unwrap();
                panic!(
                    "browser exited before failed cancellation inspection: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let active = state.db.active_import_operation(&id).unwrap().unwrap();
    assert!(
        !active.cancel_requested,
        "failed durable write acknowledged cancellation"
    );
    state
        .db
        .conn()
        .execute_batch("DROP TRIGGER reject_cancel_receipt")
        .unwrap();
    std::fs::write(barrier.join("failure_release"), b"go").unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(45),
        browser.take().unwrap().wait_with_output(),
    )
    .await
    .unwrap()
    .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "browser: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    vite.kill().unwrap();
    vite.wait().unwrap();
    let status = terminal_import_status(
        &authed_client(),
        &format!("http://{addr}/api/repos/{id}/import"),
    )
    .await;
    assert_eq!(status["lifecycle"], "cancelled");
    assert_eq!(status["last_local_svn_rev"], 1);
    assert!(status["last_confirmed_svn_rev"].is_null());
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    assert!(state.db.active_import_operation(&id).unwrap().is_some());
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
    server.abort();
    std::env::remove_var("REPOSYNC_IMPORT_BARRIER_DIR");
    std::env::remove_var("REPOSYNC_IMPORT_CANCEL_OBSERVE");
    std::env::remove_var("REPOSYNC_FIXTURE_ROOT");

    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let barrier = tmp.path().join(&id);
    std::fs::create_dir(&barrier).unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
    std::env::set_var("REPOSYNC_IMPORT_LOST_PUSH_REPLY", &id);
    let (browser, mut vite) = run_import_card_browser(addr, &id, &barrier, "uncertain").await;
    let output = tokio::time::timeout(Duration::from_secs(50), browser.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "browser: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    vite.kill().unwrap();
    vite.wait().unwrap();
    let status = terminal_import_status(
        &authed_client(),
        &format!("http://{addr}/api/repos/{id}/import"),
    )
    .await;
    assert_eq!(status["lifecycle"], "reconciliation_required");
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    assert!(state.db.active_import_operation(&id).unwrap().is_some());
    assert!(Command::new("git")
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
    server.abort();
    std::env::remove_var("REPOSYNC_IMPORT_LOST_PUSH_REPLY");
    std::env::remove_var("REPOSYNC_FIXTURE_ROOT");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64a_real_import_completes_with_confirmed_ref_and_cursors() {
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client
        .post(&base)
        .header("x-request-id", "ordinary-request")
        .send()
        .await
        .unwrap();
    assert!(
        start.status().is_success(),
        "{}",
        start.text().await.unwrap()
    );
    let started: serde_json::Value = start.json().await.unwrap();
    let op_id = started["operation_id"].as_str().unwrap();
    let mut final_status = serde_json::Value::Null;
    for _ in 0..100 {
        final_status = client
            .get(format!("{base}/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if final_status["lifecycle"] == "completed" {
            break;
        }
        if final_status["lifecycle"] == "failed"
            || final_status["lifecycle"] == "reconciliation_required"
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(final_status["lifecycle"], "completed", "{final_status}");
    assert_eq!(final_status["operation_id"], op_id);
    let op = state.db.get_import_operation(&id, op_id).unwrap().unwrap();
    let (rev, sha) = state.db.get_repo_watermark(&id).unwrap();
    assert_eq!(rev, 3);
    assert_eq!(op.last_confirmed_svn_rev, Some(rev));
    assert_eq!(op.last_confirmed_git_sha.as_deref(), Some(sha.as_str()));
    assert!(state.db.active_import_operation(&id).unwrap().is_none());
    let out = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), sha);
    let tree = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show",
            "main:history.txt",
        ])
        .output()
        .unwrap();
    assert_eq!(tree.stdout, b"second\n");
    let history = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-list",
            "--reverse",
            "main",
        ])
        .output()
        .unwrap();
    assert!(history.status.success());
    let commits: Vec<_> = String::from_utf8_lossy(&history.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(commits.len(), 3);
    for (position, expected) in [(1usize, b"first\n".as_slice()), (2, b"second\n".as_slice())] {
        let path = format!("{}:history.txt", commits[position]);
        let git_tree = Command::new("git")
            .args(["--git-dir", bare.to_str().unwrap(), "show", &path])
            .output()
            .unwrap();
        assert_eq!(git_tree.stdout, expected);
        let svn_rev = (position + 1).to_string();
        let svn_tree = Command::new("svn")
            .args([
                "cat",
                "-r",
                &svn_rev,
                &format!(
                    "file://{}/trunk/history.txt",
                    tmp.path().join("svn-repo").display()
                ),
            ])
            .output()
            .unwrap();
        assert_eq!(svn_tree.stdout, expected);
    }
    let late_cancel: serde_json::Value = client
        .post(format!("{base}/{op_id}/cancel"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(late_cancel["lifecycle"], "completed");
    assert_eq!(
        state.db.get_repo_watermark(&id).unwrap(),
        (rev, sha.clone())
    );
    let repeated: serde_json::Value = client
        .post(&base)
        .header("x-request-id", "ordinary-request")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(repeated["operation_id"], op_id);
    assert_eq!(repeated["lifecycle"], "completed");
    assert_eq!(
        client.post(&base).send().await.unwrap().status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let checkout = tmp.path().join("svn-wc");
    std::fs::write(checkout.join("history.txt"), "third\n").unwrap();
    let commit = Command::new("svn")
        .args(["commit", "-m", "after import", "--username", "fixture"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let mut sync_config = state.config.clone();
    sync_config.svn.trunk_path = String::new();
    sync_config.svn.layout = reposync_core::config::SvnLayout::Custom;
    sync_config.github.default_branch = "main".into();
    sync_config.identity.email_domain = Some("example.invalid".into());
    let repo = state.db.get_repository(&id).unwrap().unwrap();
    let local = tmp.path().join("repos").join(&id).join("git-repo");
    let mut engine = SyncEngine::new(
        sync_config,
        Database::new(tmp.path().join("reposync.db")).unwrap(),
        SvnClient::new(format!("{}/trunk", repo.svn_url), "fixture", ""),
        GitClient::new(&local).unwrap(),
        Arc::new(
            IdentityMapper::new(&IdentityConfig {
                email_domain: Some("example.invalid".into()),
                ..Default::default()
            })
            .unwrap(),
        ),
    );
    engine.set_repo_id(id.clone());
    engine.run_sync_cycle().await.unwrap();
    let after_sync = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show",
            "main:history.txt",
        ])
        .output()
        .unwrap();
    assert_eq!(after_sync.stdout, b"third\n");
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 4);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"64A_ORDINARY","operation_id":op_id,
        "svn_rev":rev,"remote_sha":sha,"remote_tree_content":"second\\n","later_sync_rev":4,
        "later_remote_tree_content":"third\\n","active_pointer":false})
    );
    server.abort();
}

#[cfg(feature = "reliability-fixture")]
async fn wait_for_file(path: &Path) {
    wait_for_file_timeout(path, Duration::from_secs(30)).await;
}

#[cfg(feature = "reliability-fixture")]
async fn wait_for_file_timeout(path: &Path, timeout: Duration) {
    let waited = tokio::time::timeout(timeout, async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if waited.is_err() {
        let listing = path.parent().map(|dir| {
            std::fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .filter_map(|entry| {
                            entry
                                .ok()
                                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        });
        panic!(
            "timed out waiting for {}; barrier dir contents: {listing:?}",
            path.display()
        );
    }
}

#[cfg(feature = "reliability-fixture")]
async fn terminal_import_status(client: &reqwest::Client, base: &str) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status: serde_json::Value = client
                .get(format!("{base}/status"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
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
    .unwrap()
}

#[cfg(feature = "reliability-fixture")]
fn set_import_cleanup_fault(root: &Path, stage: &str) -> std::path::PathBuf {
    let dir = root.join("cleanup-fault");
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", root);
    std::env::set_var("REPOSYNC_IMPORT_FAULT_DIR", &dir);
    std::env::set_var("REPOSYNC_IMPORT_FAULT_STAGE", stage);
    dir
}

#[cfg(feature = "reliability-fixture")]
fn clear_import_cleanup_fault() {
    for key in [
        "REPOSYNC_IMPORT_FAULT_STAGE",
        "REPOSYNC_IMPORT_FAULT_DIR",
        "REPOSYNC_FIXTURE_ROOT",
    ] {
        std::env::remove_var(key);
    }
}

#[cfg(feature = "reliability-fixture")]
fn import_fault_commands(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("commands.log"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[cfg(feature = "reliability-fixture")]
async fn actual_import_unknown_cleanup(stage: &str, request_cancel: bool, expected_local_rev: i64) {
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let fault = set_import_cleanup_fault(tmp.path(), stage);
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let started = client.post(&base).send().await.unwrap();
    assert!(started.status().is_success());
    let started: serde_json::Value = started.json().await.unwrap();
    let operation_id = started["operation_id"].as_str().unwrap();
    wait_for_file(&fault.join("fault.ready")).await;
    assert_eq!(
        std::fs::read_to_string(fault.join("fault.ready")).unwrap(),
        stage
    );
    let local = tmp.path().join("repos").join(&id).join("git-repo");
    let local_snapshot = |args: &[&str]| {
        let result = Command::new("git")
            .args(args)
            .current_dir(&local)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    };
    let tree_before = local_snapshot(&["rev-parse", "HEAD^{tree}"]);
    let head_before = local_snapshot(&["rev-parse", "HEAD"]);
    let index_before = local_snapshot(&["ls-files", "--stage"]);
    let index_workdir_before = local_snapshot(&["status", "--porcelain", "-uall"]);
    let remote_refs = || {
        Command::new("git")
            .args(["--git-dir", bare.to_str().unwrap(), "show-ref"])
            .output()
            .unwrap()
            .stdout
    };
    let remote_refs_before = remote_refs();
    let map_before: i64 = state
        .db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM commit_map WHERE repo_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(map_before, 0);
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    if request_cancel {
        let cancel = client
            .post(format!("{base}/{operation_id}/cancel"))
            .send()
            .await
            .unwrap();
        assert!(cancel.status().is_success());
        assert_eq!(
            cancel.json::<serde_json::Value>().await.unwrap()["lifecycle"],
            "cancel_requested"
        );
    }
    std::fs::write(fault.join("fault.release"), b"release").unwrap();
    let terminal = terminal_import_status(&client, &base).await;
    assert_eq!(terminal["operation_id"], operation_id);
    assert!(
        terminal["lifecycle"] == "failed" || terminal["lifecycle"] == "reconciliation_required",
        "{terminal}"
    );
    assert!(
        terminal.to_string().contains("cleanup unconfirmed"),
        "{terminal}"
    );
    assert_eq!(terminal["last_local_svn_rev"], expected_local_rev);
    assert!(terminal["last_confirmed_svn_rev"].is_null());
    let trace = import_fault_commands(&fault);
    assert_eq!(trace.last().map(String::as_str), Some(stage), "{trace:?}");
    assert_eq!(
        trace.iter().filter(|s| s.as_str() == "svn-export").count(),
        1
    );
    assert!(!trace
        .iter()
        .any(|s| matches!(s.as_str(), "git-push" | "git-commit")));
    assert_eq!(local_snapshot(&["rev-parse", "HEAD^{tree}"]), tree_before);
    assert_eq!(local_snapshot(&["rev-parse", "HEAD"]), head_before);
    assert_eq!(local_snapshot(&["ls-files", "--stage"]), index_before);
    assert_eq!(
        local_snapshot(&["status", "--porcelain", "-uall"]),
        index_workdir_before
    );
    assert_eq!(remote_refs(), remote_refs_before);
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    let map_after: i64 = state
        .db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM commit_map WHERE repo_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(map_after, map_before);
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
    let reopened = Database::new(tmp.path().join("reposync.db")).unwrap();
    let held = reopened.active_import_operation(&id).unwrap().unwrap();
    assert_eq!(held.id, operation_id);
    assert_ne!(
        held.state,
        reposync_core::db::import_operations::ImportOperationState::Cancelled
    );
    let retry = client.post(&base).send().await.unwrap();
    assert_eq!(retry.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        terminal_import_status(&client, &base).await["operation_id"],
        operation_id
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":if request_cancel {"74_UNCERTAIN_SVN_CANCEL"} else {"74_UNCERTAIN_APPLY_TIMEOUT"},
            "operation_id":operation_id,"fault_stage":stage,"commands":trace,
            "checkpoint":0,"local_rev":expected_local_rev,"map_rows":map_after,
            "tree_index_workdir_preserved":true,"all_remote_refs_preserved":true,"remote_ref_present":false,"restart_held":true
        })
    );
    server.abort();
    clear_import_cleanup_fault();
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_74_apply_timeout_cleanup_unknown_holds_without_export_or_next_write() {
    actual_import_unknown_cleanup("git-apply", false, 1).await;
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_74_svn_cancel_cleanup_unknown_holds_without_next_revision() {
    actual_import_unknown_cleanup("svn-diff", true, 1).await;
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_74_svn_early_cleanup_unknown_never_starts_revision_work() {
    use std::process::Command;
    for stage in ["svn-info", "svn-log", "svn-export"] {
        let (addr, state, server, tmp, id, bare) = import_fixture().await;
        let fault = set_import_cleanup_fault(tmp.path(), stage);
        let client = authed_client();
        let base = format!("http://{addr}/api/repos/{id}/import");
        let started = client.post(&base).send().await.unwrap();
        assert!(started.status().is_success());
        let started: serde_json::Value = started.json().await.unwrap();
        let operation_id = started["operation_id"].as_str().unwrap();
        wait_for_file(&fault.join("fault.ready")).await;
        assert!(client
            .post(format!("{base}/{operation_id}/cancel"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
        std::fs::write(fault.join("fault.release"), b"release").unwrap();
        let terminal = terminal_import_status(&client, &base).await;
        assert!(
            terminal["lifecycle"] == "failed" || terminal["lifecycle"] == "reconciliation_required",
            "{stage}: {terminal}"
        );
        assert!(
            terminal.to_string().contains("cleanup unconfirmed"),
            "{stage}: {terminal}"
        );
        let trace = import_fault_commands(&fault);
        assert_eq!(trace.last().map(String::as_str), Some(stage));
        assert!(!trace
            .iter()
            .any(|s| matches!(s.as_str(), "git-apply" | "git-commit" | "git-push")));
        assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
        assert_eq!(
            state
                .db
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM commit_map WHERE repo_id=?1",
                    [&id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
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
        assert_eq!(
            Database::new(tmp.path().join("reposync.db"))
                .unwrap()
                .active_import_operation(&id)
                .unwrap()
                .unwrap()
                .id,
            operation_id
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"74_UNCERTAIN_SVN_EARLY","stage":stage,"operation_id":operation_id,"commands":trace,"checkpoint":0,"mapping_rows":0,"remote_ref_present":false,"restart_held":true})
        );
        server.abort();
        clear_import_cleanup_fault();
    }
}

#[cfg(all(feature = "reliability-fixture", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_74_finished_failed_patch_still_uses_export_and_completes() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let fault = set_import_cleanup_fault(tmp.path(), "none");
    let wrapper = tmp.path().join("git-finished-apply-failure");
    std::fs::write(
        &wrapper,
        b"#!/bin/sh\nif [ \"$1\" = apply ]; then exit 1; fi\nexec git \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("REPOSYNC_IMPORT_GIT_BINARY", &wrapper);
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    assert!(client
        .post(&base)
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    let terminal = terminal_import_status(&client, &base).await;
    assert_eq!(terminal["lifecycle"], "completed", "{terminal}");
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 3);
    let trace = import_fault_commands(&fault);
    assert!(trace.iter().any(|s| s == "git-apply"));
    assert!(
        trace.iter().filter(|s| s.as_str() == "svn-export").count() >= 2,
        "{trace:?}"
    );
    let remote = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show",
            "main:history.txt",
        ])
        .output()
        .unwrap();
    assert!(remote.status.success());
    assert_eq!(remote.stdout, b"second\n");
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"74_FINISHED_PATCH_FALLBACK","commands":trace,"checkpoint":3,"remote_history":"second","completed":true})
    );
    server.abort();
    std::env::remove_var("REPOSYNC_IMPORT_GIT_BINARY");
    clear_import_cleanup_fault();
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_74_preparation_cancel_cleanup_unknown_never_starts_worker() {
    use std::process::Command;
    for stage in ["git-clone", "git-ls-remote"] {
        let (addr, state, server, tmp, id, bare) = import_fixture().await;
        let fault = set_import_cleanup_fault(tmp.path(), stage);
        let client = authed_client();
        let base = format!("http://{addr}/api/repos/{id}/import");
        let pending = tokio::spawn({
            let client = client.clone();
            let base = base.clone();
            async move { client.post(base).send().await.unwrap() }
        });
        wait_for_file(&fault.join("fault.ready")).await;
        let op = state.db.active_import_operation(&id).unwrap().unwrap();
        let cancel = client
            .post(format!("{base}/{}/cancel", op.id))
            .send()
            .await
            .unwrap();
        assert!(cancel.status().is_success());
        std::fs::write(fault.join("fault.release"), b"release").unwrap();
        assert!(!pending.await.unwrap().status().is_success());
        let terminal = terminal_import_status(&client, &base).await;
        assert_eq!(terminal["operation_id"], op.id);
        assert_eq!(
            terminal["lifecycle"], "reconciliation_required",
            "{terminal}"
        );
        assert!(
            terminal.to_string().contains("cleanup unconfirmed"),
            "{terminal}"
        );
        let trace = import_fault_commands(&fault);
        assert_eq!(trace.last().map(String::as_str), Some(stage));
        assert!(!trace.iter().any(|s| s.starts_with("svn-")
            || matches!(s.as_str(), "git-apply" | "git-commit" | "git-push")));
        assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
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
        let reopened = Database::new(tmp.path().join("reposync.db")).unwrap();
        assert_eq!(
            reopened.active_import_operation(&id).unwrap().unwrap().id,
            op.id
        );
        assert_eq!(
            client.post(&base).send().await.unwrap().status(),
            reqwest::StatusCode::BAD_REQUEST
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({
                "case":"74_UNCERTAIN_PREPARATION_CANCEL","operation_id":op.id,
                "stage":stage,"commands":trace,"worker_started":false,"checkpoint":0,
                "remote_ref_present":false,"restart_held":true
            })
        );
        server.abort();
        clear_import_cleanup_fault();
    }
}

#[cfg(all(feature = "reliability-fixture", unix))]
fn fixture_process_stopped(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } != 0 {
        return true;
    }
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        return stat
            .rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with("Z ") || rest.starts_with("X "));
    }
    false
}

#[cfg(all(feature = "reliability-fixture", unix))]
async fn wait_for_stopped(pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture_process_stopped(pid) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64a_mid_import_cancel_holds_local_work_without_publication_after_restart() {
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let barrier = tmp.path().join(&id);
    std::fs::create_dir(&barrier).unwrap();
    std::fs::write(barrier.join("connecting.release"), b"go").unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
    std::env::set_var("REPOSYNC_IMPORT_BARRIER_DIR", &barrier);
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client.post(&base).send().await.unwrap();
    assert!(start.status().is_success());
    let started: serde_json::Value = start.json().await.unwrap();
    let op_id = started["operation_id"].as_str().unwrap();
    wait_for_file(&barrier.join("after_first_local.ready")).await;
    assert_eq!(
        state
            .db
            .get_import_operation(&id, op_id)
            .unwrap()
            .unwrap()
            .last_local_svn_rev,
        Some(1)
    );
    let cancel = client
        .post(format!("{base}/{op_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert!(cancel.status().is_success());
    let accepted: serde_json::Value = cancel.json().await.unwrap();
    assert_eq!(accepted["lifecycle"], "cancel_requested");
    let duplicate: serde_json::Value = client
        .post(format!("{base}/{op_id}/cancel"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(duplicate["lifecycle"] == "cancel_requested" || duplicate["lifecycle"] == "cancelled");
    let stale = client
        .post(format!(
            "{base}/00000000-0000-0000-0000-000000000000/cancel"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::NOT_FOUND);
    let status = terminal_import_status(&client, &base).await;
    assert_eq!(status["lifecycle"], "cancelled", "{status}");
    assert_eq!(status["last_local_svn_rev"], 1);
    assert!(status["last_confirmed_svn_rev"].is_null());
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    assert!(state.db.active_import_operation(&id).unwrap().is_some());
    let remote = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show-ref",
            "--verify",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    assert!(
        !remote.status.success(),
        "cancelled import published unexpectedly"
    );
    let local = tmp.path().join("repos").join(&id).join("git-repo");
    let local_head = Command::new("git")
        .args(["-C", local.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(
        local_head.status.success(),
        "local partial work must remain identifiable"
    );
    let repeated_start = client.post(&base).send().await.unwrap();
    assert_eq!(repeated_start.status(), reqwest::StatusCode::BAD_REQUEST);

    server.abort();
    let reopened = Database::new(tmp.path().join("reposync.db")).unwrap();
    reopened.initialize().unwrap();
    let restarted = reposync_web::WebServer::new(
        state.config.clone(),
        reopened,
        state.sync_engine.clone(),
        state.sync_trigger.clone(),
        tmp.path().join("config.toml"),
        Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
    )
    .app_state();
    restarted.sessions.write().await.insert(
        TEST_TOKEN.into(),
        chrono::Utc::now() + chrono::Duration::hours(1),
    );
    let app = Router::new()
        .merge(api::repos::routes())
        .with_state(restarted.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let restart_addr = listener.local_addr().unwrap();
    let restart_server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let restart_base = format!("http://{restart_addr}/api/repos/{id}/import");
    let restart_status: serde_json::Value = client
        .get(format!("{restart_base}/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restart_status["lifecycle"], "cancelled");
    assert_eq!(restart_status["operation_id"], op_id);
    assert_eq!(
        client.post(&restart_base).send().await.unwrap().status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    assert!(restarted.db.active_import_operation(&id).unwrap().is_some());
    assert_eq!(restarted.db.get_repo_watermark(&id).unwrap().0, 0);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"64A_MID_IMPORT","operation_id":op_id,
        "local_rev":1,"local_sha":String::from_utf8_lossy(&local_head.stdout).trim(),
        "remote_ref_present":false,"checkpoint":0,"restart_lifecycle":"cancelled","held":true})
    );
    restart_server.abort();
    std::env::remove_var("REPOSYNC_IMPORT_BARRIER_DIR");
    std::env::remove_var("REPOSYNC_FIXTURE_ROOT");
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64a_lost_push_reply_retains_intent_and_blocks_replay() {
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
    std::env::set_var("REPOSYNC_IMPORT_LOST_PUSH_REPLY", &id);
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client.post(&base).send().await.unwrap();
    assert!(start.status().is_success());
    let started: serde_json::Value = start.json().await.unwrap();
    let op_id = started["operation_id"].as_str().unwrap();
    let status = terminal_import_status(&client, &base).await;
    assert_eq!(status["lifecycle"], "reconciliation_required", "{status}");
    let op = state.db.get_import_operation(&id, op_id).unwrap().unwrap();
    let intended = op.intended_git_sha.unwrap();
    assert_eq!(op.intended_ref.as_deref(), Some("refs/heads/main"));
    assert!(op.last_confirmed_git_sha.is_none());
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    let remote = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    assert!(remote.status.success());
    assert_eq!(String::from_utf8_lossy(&remote.stdout).trim(), intended);
    assert_eq!(
        client.post(&base).send().await.unwrap().status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    server.abort();
    let reopened = Database::new(tmp.path().join("reposync.db")).unwrap();
    reopened.initialize().unwrap();
    reopened.hold_interrupted_imports().unwrap();
    assert!(reopened.active_import_operation(&id).unwrap().is_some());
    assert_eq!(reopened.get_repo_watermark(&id).unwrap().0, 0);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"64A_LOST_PUSH","operation_id":op_id,
        "intended_sha":intended,"observed_remote_sha":String::from_utf8_lossy(&remote.stdout).trim(),
        "checkpoint":0,"held":true})
    );
    std::env::remove_var("REPOSYNC_IMPORT_LOST_PUSH_REPLY");
    std::env::remove_var("REPOSYNC_FIXTURE_ROOT");
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64a_connecting_cancel_rejects_duplicate_and_keeps_other_repo_independent() {
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let barrier = tmp.path().join(&id);
    std::fs::create_dir(&barrier).unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
    std::env::set_var("REPOSYNC_IMPORT_BARRIER_DIR", &barrier);
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client
        .post(&base)
        .header("x-request-id", "connecting-request")
        .send()
        .await
        .unwrap();
    assert!(start.status().is_success());
    let started: serde_json::Value = start.json().await.unwrap();
    let op_id = started["operation_id"].as_str().unwrap();
    wait_for_file(&barrier.join("connecting.ready")).await;
    let repeated: serde_json::Value = client
        .post(&base)
        .header("x-request-id", "connecting-request")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(repeated["operation_id"], op_id);
    assert_eq!(
        client.post(&base).send().await.unwrap().status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .get(format!("{base}/status"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        reqwest::Client::new()
            .post(format!("{base}/{op_id}/cancel"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert!(
        !state
            .db
            .get_import_operation(&id, op_id)
            .unwrap()
            .unwrap()
            .cancel_requested
    );
    let cancel: serde_json::Value = client
        .post(format!("{base}/{op_id}/cancel"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancel["lifecycle"], "cancel_requested");
    let status = terminal_import_status(&client, &base).await;
    assert_eq!(status["lifecycle"], "cancelled");
    assert!(status["last_local_svn_rev"].is_null());
    assert!(state.db.active_import_operation(&id).unwrap().is_some());
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
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

    let second_bare = tmp.path().join("local").join("second.git");
    assert!(Command::new("git")
        .args(["init", "--bare", second_bare.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let source = state.db.get_repository(&id).unwrap().unwrap();
    let second = client
        .post(format!("http://{addr}/api/repos"))
        .json(
            &serde_json::json!({"name":"second", "svn_url":source.svn_url, "svn_branch":"trunk",
            "svn_username":"fixture", "git_provider":"gitea",
            "git_api_url":format!("file://{}", tmp.path().display()),
            "git_repo":"local/second", "git_branch":"main"}),
        )
        .send()
        .await
        .unwrap();
    assert!(second.status().is_success());
    let second: serde_json::Value = second.json().await.unwrap();
    let second_id = second["id"].as_str().unwrap();
    let second_base = format!("http://{addr}/api/repos/{second_id}/import");
    assert!(client
        .post(&second_base)
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    let second_status = terminal_import_status(&client, &second_base).await;
    assert_eq!(second_status["lifecycle"], "completed", "{second_status}");
    assert_eq!(state.db.get_repo_watermark(second_id).unwrap().0, 3);
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    assert!(Command::new("git")
        .args([
            "--git-dir",
            second_bare.to_str().unwrap(),
            "show-ref",
            "--verify",
            "refs/heads/main"
        ])
        .status()
        .unwrap()
        .success());
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"64A_CONNECTING","stopped_repo":id,
        "stopped_checkpoint":0,"independent_repo":second_id,"independent_checkpoint":3,"equal_svn_numbers_isolated":true})
    );
    server.abort();
    std::env::remove_var("REPOSYNC_IMPORT_BARRIER_DIR");
    std::env::remove_var("REPOSYNC_FIXTURE_ROOT");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64a_failed_finalizer_keeps_confirmed_ref_held_without_checkpoint() {
    use std::process::Command;
    let (addr, state, server, _tmp, id, bare) = import_fixture().await;
    state
        .db
        .conn()
        .execute_batch(
            "CREATE TRIGGER reject_import_finalization
        BEFORE UPDATE OF last_svn_rev ON repositories
        BEGIN SELECT RAISE(FAIL, 'fixture finalizer failure'); END;",
        )
        .unwrap();
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client.post(&base).send().await.unwrap();
    assert!(start.status().is_success());
    let started: serde_json::Value = start.json().await.unwrap();
    let op_id = started["operation_id"].as_str().unwrap();
    let status = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let value: serde_json::Value = client
                .get(format!("{base}/status"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if value["lifecycle"] == "reconciliation_required" {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(status["last_confirmed_svn_rev"], 3);
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    assert!(state.db.active_import_operation(&id).unwrap().is_some());
    let remote = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    assert!(remote.status.success());
    assert_eq!(
        String::from_utf8_lossy(&remote.stdout).trim(),
        status["last_confirmed_git_sha"].as_str().unwrap()
    );
    assert_eq!(
        state
            .db
            .get_import_operation(&id, op_id)
            .unwrap()
            .unwrap()
            .state,
        reposync_core::db::import_operations::ImportOperationState::ReconciliationRequired
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"64A_FINALIZER","operation_id":op_id,
        "remote_sha":String::from_utf8_lossy(&remote.stdout).trim(),"checkpoint":0,"held":true})
    );
    server.abort();
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64a_failed_cancel_receipt_is_not_acknowledged_or_signalled() {
    let (addr, state, server, tmp, id, _bare) = import_fixture().await;
    let barrier = tmp.path().join(&id);
    std::fs::create_dir(&barrier).unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
    std::env::set_var("REPOSYNC_IMPORT_BARRIER_DIR", &barrier);
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client.post(&base).send().await.unwrap();
    assert!(start.status().is_success());
    let started: serde_json::Value = start.json().await.unwrap();
    let op_id = started["operation_id"].as_str().unwrap();
    wait_for_file(&barrier.join("connecting.ready")).await;
    state
        .db
        .conn()
        .execute_batch(
            "CREATE TRIGGER reject_cancel_receipt
        BEFORE UPDATE OF value ON kv_state
        WHEN NEW.key LIKE 'import_operation_v1:document:%' AND NEW.value LIKE '%cancel_requested%'
        BEGIN SELECT RAISE(FAIL, 'fixture receipt failure'); END;",
        )
        .unwrap();
    let failed = client
        .post(format!("{base}/{op_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        !state
            .db
            .get_import_operation(&id, op_id)
            .unwrap()
            .unwrap()
            .cancel_requested
    );
    let progress = state.get_repo_import_progress(&id).await;
    assert!(!progress.read().await.cancel_signal.load(Ordering::Acquire));
    state
        .db
        .conn()
        .execute_batch("DROP TRIGGER reject_cancel_receipt")
        .unwrap();
    let accepted = client
        .post(format!("{base}/{op_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert!(accepted.status().is_success());
    assert_eq!(
        terminal_import_status(&client, &base).await["lifecycle"],
        "cancelled"
    );
    server.abort();
    std::env::remove_var("REPOSYNC_IMPORT_BARRIER_DIR");
    std::env::remove_var("REPOSYNC_FIXTURE_ROOT");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64a_named_disabled_expired_and_legacy_fallback_cannot_cancel() {
    use reposync_core::models::{Session, User};
    let (addr, state, server, _tmp, id, _bare) = import_fixture().await;
    let now = chrono::Utc::now();
    for (user_id, role, enabled) in [
        ("active-admin", "admin", true),
        ("disabled-admin", "admin", false),
        ("viewer", "viewer", true),
    ] {
        state
            .db
            .insert_user(&User {
                id: user_id.into(),
                username: user_id.into(),
                display_name: user_id.into(),
                email: format!("{user_id}@example.invalid"),
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
            "active-token",
            "active-admin",
            now + chrono::Duration::hours(1),
        ),
        (
            "disabled-token",
            "disabled-admin",
            now + chrono::Duration::hours(1),
        ),
        (
            "expired-token",
            "active-admin",
            now - chrono::Duration::hours(1),
        ),
        ("viewer-token", "viewer", now + chrono::Duration::hours(1)),
    ] {
        state
            .db
            .insert_session(&Session {
                token: token.into(),
                user_id: user.into(),
                expires_at: expiry.to_rfc3339(),
                created_at: now.to_rfc3339(),
            })
            .unwrap();
    }
    let op = state
        .db
        .create_import_operation(&id, "active-admin", "auth-fixture", "target")
        .unwrap();
    let url = format!("http://{addr}/api/repos/{id}/import/{}/cancel", op.id);
    for token in [
        "disabled-token",
        "expired-token",
        TEST_TOKEN,
        "viewer-token",
    ] {
        let status = reqwest::Client::new()
            .post(&url)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(
            status,
            reqwest::StatusCode::UNAUTHORIZED,
            "token {token} gained cancellation access"
        );
        assert!(
            !state
                .db
                .get_import_operation(&id, &op.id)
                .unwrap()
                .unwrap()
                .cancel_requested
        );
    }
    let accepted = reqwest::Client::new()
        .post(&url)
        .bearer_auth("active-token")
        .send()
        .await
        .unwrap();
    assert!(accepted.status().is_success());
    assert!(
        state
            .db
            .get_import_operation(&id, &op.id)
            .unwrap()
            .unwrap()
            .cancel_requested
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64a_existing_git_target_is_preserved_before_replay() {
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let work = tmp.path().join("existing-target");
    assert!(Command::new("git")
        .args(["init", "-b", "main", work.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    std::fs::write(work.join("keep.txt"), "existing independent history\n").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&work)
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["add", "keep.txt"]);
    git(&["commit", "-m", "preexisting"]);
    git(&["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&["push", "origin", "main"]);
    let before = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let rejected = client.post(&base).send().await.unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::BAD_REQUEST);
    let status: serde_json::Value = client
        .get(format!("{base}/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["lifecycle"], "reconciliation_required");
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    let after = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    assert_eq!(before.stdout, after.stdout);
    let tree = Command::new("git")
        .args(["--git-dir", bare.to_str().unwrap(), "show", "main:keep.txt"])
        .output()
        .unwrap();
    assert_eq!(tree.stdout, b"existing independent history\n");
    assert!(state.db.active_import_operation(&id).unwrap().is_some());
    server.abort();
}

#[cfg(all(feature = "reliability-fixture", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_74_incremental_apply_completes_through_supervised_command() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let wrapper = tmp.path().join("git-apply-positive-wrapper");
    let outcomes = tmp.path().join("git-apply-exits.txt");
    std::fs::write(
        &wrapper,
        b"#!/bin/sh\nif [ \"$1\" = apply ]; then\n git \"$@\"\n result=$?\n echo $result >> \"$GIT_APPLY_EXITS\"\n exit $result\nfi\nexec git \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
    std::env::set_var("REPOSYNC_IMPORT_GIT_BINARY", &wrapper);
    std::env::set_var("GIT_APPLY_EXITS", &outcomes);
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client.post(&base).send().await.unwrap();
    assert!(start.status().is_success());
    let started: serde_json::Value = start.json().await.unwrap();
    let operation_id = started["operation_id"].as_str().unwrap();
    let terminal = terminal_import_status(&client, &base).await;
    assert_eq!(terminal["lifecycle"], "completed", "{terminal}");
    let exits = std::fs::read_to_string(&outcomes).unwrap();
    assert!(
        exits.lines().any(|line| line == "0"),
        "actual git apply never succeeded: {exits}"
    );
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 3);
    let tree = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show",
            "main:history.txt",
        ])
        .output()
        .unwrap();
    assert_eq!(tree.stdout, b"second\n");
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"74_APPLY_POSITIVE","operation_id":operation_id,
            "successful_git_apply_calls":exits.lines().filter(|line| *line == "0").count(),
            "remote_tree_content":"second\\n","checkpoint":3
        })
    );
    server.abort();
    for name in [
        "REPOSYNC_IMPORT_GIT_BINARY",
        "REPOSYNC_FIXTURE_ROOT",
        "GIT_APPLY_EXITS",
    ] {
        std::env::remove_var(name);
    }
}

#[cfg(all(feature = "reliability-fixture", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_74_git_apply_child_and_descendant_stop_on_exact_cancel() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let wrapper = tmp.path().join("git-apply-stall-wrapper");
    let parent_file = tmp.path().join("git-apply-parent.pid");
    let descendant_file = tmp.path().join("git-apply-descendant.pid");
    std::fs::write(
        &wrapper,
        b"#!/bin/sh\nif [ \"$1\" = apply ]; then\n echo $$ > \"$GIT_STALL_PARENT\"\n sleep 60 & echo $! > \"$GIT_STALL_DESCENDANT\"\n wait\nfi\nexec git \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
    std::env::set_var("REPOSYNC_IMPORT_GIT_BINARY", &wrapper);
    std::env::set_var("GIT_STALL_PARENT", &parent_file);
    std::env::set_var("GIT_STALL_DESCENDANT", &descendant_file);

    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client.post(&base).send().await.unwrap();
    assert!(start.status().is_success());
    let started: serde_json::Value = start.json().await.unwrap();
    let operation_id = started["operation_id"].as_str().unwrap();
    wait_for_file(&descendant_file).await;
    let parent: i32 = std::fs::read_to_string(&parent_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let descendant: i32 = std::fs::read_to_string(&descendant_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(parent, 0) }, 0);
    assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
    let before = tokio::time::timeout(
        Duration::from_secs(2),
        client.get(format!("{base}/status")).send(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(before.status().is_success());
    let before: serde_json::Value = before.json().await.unwrap();
    assert_eq!(before["operation_id"], operation_id);
    assert_eq!(before["last_local_svn_rev"], 1);

    let cancel = client
        .post(format!("{base}/{operation_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert!(cancel.status().is_success());
    let terminal = terminal_import_status(&client, &base).await;
    assert_eq!(terminal["lifecycle"], "cancelled", "{terminal}");
    assert_eq!(terminal["last_local_svn_rev"], 1);
    assert!(terminal["last_confirmed_svn_rev"].is_null());
    wait_for_stopped(parent).await;
    wait_for_stopped(descendant).await;
    let op = state
        .db
        .get_import_operation(&id, operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(op.processed_revisions, 1);
    assert_eq!(op.local_commits, 1);
    assert_eq!(op.last_local_svn_rev, Some(1));
    assert!(op.last_confirmed_svn_rev.is_none());
    assert!(state.db.active_import_operation(&id).unwrap().is_some());
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    let remote = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show-ref",
            "--verify",
            "refs/heads/main",
        ])
        .status()
        .unwrap();
    assert!(!remote.success());
    let local = tmp.path().join("repos").join(&id).join("git-repo");
    let local_count = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(&local)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&local_count.stdout).trim(), "1");
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"74_GIT_APPLY_STOP","operation_id":operation_id,
            "parent_stopped":true,"descendant_stopped":true,
            "local_rev":1,"local_commits":1,"remote_ref_present":false,
            "checkpoint":0,"held":true
        })
    );
    server.abort();
    for name in [
        "REPOSYNC_IMPORT_GIT_BINARY",
        "REPOSYNC_FIXTURE_ROOT",
        "GIT_STALL_PARENT",
        "GIT_STALL_DESCENDANT",
    ] {
        std::env::remove_var(name);
    }
}

#[cfg(all(feature = "reliability-fixture", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_74_lfs_enabled_import_completes_with_pointer_publication() {
    use std::process::Command;

    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    state
        .db
        .conn()
        .execute(
            "UPDATE repositories SET lfs_threshold_mb=1 WHERE id=?1",
            [&id],
        )
        .unwrap();
    let checkout = tmp.path().join("svn-wc");
    let large = vec![0u8; 1_100_000];
    std::fs::write(checkout.join("large.bin"), &large).unwrap();
    assert!(Command::new("svn")
        .args(["add", "large.bin"])
        .current_dir(&checkout)
        .status()
        .unwrap()
        .success());
    let commit = Command::new("svn")
        .args(["commit", "-m", "large binary", "--username", "fixture"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client.post(&base).send().await.unwrap();
    assert!(start.status().is_success());
    let started: serde_json::Value = start.json().await.unwrap();
    let operation_id = started["operation_id"].as_str().unwrap();
    let terminal = terminal_import_status(&client, &base).await;
    assert_eq!(terminal["lifecycle"], "completed", "{terminal}");
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 4);
    assert!(terminal["log_lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| {
            line.as_str()
                .unwrap_or("")
                .contains("Git LFS installed in repo")
        }));
    let pointer = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show",
            "main:large.bin",
        ])
        .output()
        .unwrap();
    assert!(pointer.status.success());
    assert!(pointer
        .stdout
        .starts_with(b"version https://git-lfs.github.com/spec/v1\n"));
    assert!(String::from_utf8_lossy(&pointer.stdout).contains("size 1100000"));
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"74_LFS_POSITIVE","operation_id":operation_id,
            "checkpoint":4,"pointer_published":true,"source_bytes":large.len(),
            "remote_pointer_bytes":pointer.stdout.len()
        })
    );
    server.abort();
}

#[cfg(all(feature = "reliability-fixture", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_74_lfs_preflight_child_and_descendant_stop() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    state
        .db
        .conn()
        .execute(
            "UPDATE repositories SET lfs_threshold_mb=1 WHERE id=?1",
            [&id],
        )
        .unwrap();
    let wrapper = tmp.path().join("git-lfs-stall-wrapper");
    let parent_file = tmp.path().join("git-lfs-parent.pid");
    let descendant_file = tmp.path().join("git-lfs-descendant.pid");
    std::fs::write(
        &wrapper,
        b"#!/bin/sh\nif [ \"$1\" = lfs ] && [ \"$2\" = version ]; then\n echo $$ > \"$GIT_STALL_PARENT\"\n sleep 60 & echo $! > \"$GIT_STALL_DESCENDANT\"\n wait\nfi\nexec git \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
    std::env::set_var("REPOSYNC_IMPORT_GIT_BINARY", &wrapper);
    std::env::set_var("GIT_STALL_PARENT", &parent_file);
    std::env::set_var("GIT_STALL_DESCENDANT", &descendant_file);

    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client.post(&base).send().await.unwrap();
    assert!(start.status().is_success());
    let started: serde_json::Value = start.json().await.unwrap();
    let operation_id = started["operation_id"].as_str().unwrap();
    wait_for_file(&descendant_file).await;
    let parent: i32 = std::fs::read_to_string(&parent_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let descendant: i32 = std::fs::read_to_string(&descendant_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(parent, 0) }, 0);
    assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
    let responsive = tokio::time::timeout(
        Duration::from_secs(2),
        client.get(format!("{base}/status")).send(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(responsive.status().is_success());
    let cancel = client
        .post(format!("{base}/{operation_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert!(cancel.status().is_success());
    let terminal = terminal_import_status(&client, &base).await;
    assert_eq!(terminal["lifecycle"], "cancelled", "{terminal}");
    wait_for_stopped(parent).await;
    wait_for_stopped(descendant).await;
    let op = state
        .db
        .get_import_operation(&id, operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(op.processed_revisions, 0);
    assert!(op.last_local_svn_rev.is_none());
    assert!(op.last_confirmed_svn_rev.is_none());
    assert!(state.db.active_import_operation(&id).unwrap().is_some());
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    let remote = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show-ref",
            "--verify",
            "refs/heads/main",
        ])
        .status()
        .unwrap();
    assert!(!remote.success());
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"74_LFS_STOP","operation_id":operation_id,
            "parent_stopped":true,"descendant_stopped":true,
            "processed_revisions":0,"remote_ref_present":false,"checkpoint":0,"held":true
        })
    );
    server.abort();
    for name in [
        "REPOSYNC_IMPORT_GIT_BINARY",
        "REPOSYNC_FIXTURE_ROOT",
        "GIT_STALL_PARENT",
        "GIT_STALL_DESCENDANT",
    ] {
        std::env::remove_var(name);
    }
}

#[cfg(all(feature = "reliability-fixture", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_74_reset_refusal_preserves_published_installation() {
    use sha2::{Digest, Sha256};
    use std::process::Command;

    fn domain_hash(db: &Database) -> String {
        let conn = db.conn();
        let mut hash = Sha256::new();
        for table in ["repositories", "kv_state", "commit_map", "sync_records"] {
            hash.update(table.as_bytes());
            let mut statement = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            let mut rows = statement.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                for column in 0..columns {
                    let value = format!("{:?}", row.get_ref(column).unwrap());
                    hash.update((value.len() as u64).to_le_bytes());
                    hash.update(value.as_bytes());
                }
            }
        }
        format!("{:x}", hash.finalize())
    }
    fn git_output(cwd: &Path, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client.post(&base).send().await.unwrap();
    assert!(start.status().is_success());
    let started: serde_json::Value = start.json().await.unwrap();
    let operation_id = started["operation_id"].as_str().unwrap();
    assert_eq!(
        terminal_import_status(&client, &base).await["lifecycle"],
        "completed"
    );
    state
        .db
        .set_state(
            &format!("secret_svn_password_{id}"),
            "synthetic-reset-preservation",
        )
        .unwrap();
    let local = tmp.path().join("repos").join(&id).join("git-repo");
    let sentinel = local.join("untracked-preservation.fixture");
    std::fs::write(&sentinel, b"keep this local work\n").unwrap();
    let before_db = domain_hash(&state.db);
    let before_remote_refs = git_output(&bare, &["show-ref"]);
    let before_remote_tree = git_output(&bare, &["rev-parse", "main^{tree}"]);
    let before_local_refs = git_output(&local, &["show-ref"]);
    let before_local_tree = git_output(&local, &["rev-parse", "HEAD^{tree}"]);
    let before_local_status = git_output(&local, &["status", "--porcelain", "-uall"]);
    let before_watermark = state.db.get_repo_watermark(&id).unwrap();
    assert_eq!(before_watermark.0, 3);
    assert!(state.db.active_import_operation(&id).unwrap().is_none());

    let refusal = client
        .post(format!("{base}?reset=true"))
        .send()
        .await
        .unwrap();
    assert_eq!(refusal.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(refusal
        .text()
        .await
        .unwrap()
        .contains("Reset & Reimport is unavailable"));
    assert_eq!(domain_hash(&state.db), before_db);
    assert_eq!(git_output(&bare, &["show-ref"]), before_remote_refs);
    assert_eq!(
        git_output(&bare, &["rev-parse", "main^{tree}"]),
        before_remote_tree
    );
    assert_eq!(git_output(&local, &["show-ref"]), before_local_refs);
    assert_eq!(
        git_output(&local, &["rev-parse", "HEAD^{tree}"]),
        before_local_tree
    );
    assert_eq!(
        git_output(&local, &["status", "--porcelain", "-uall"]),
        before_local_status
    );
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep this local work\n");
    assert_eq!(state.db.get_repo_watermark(&id).unwrap(), before_watermark);
    assert!(state.db.active_import_operation(&id).unwrap().is_none());
    assert_eq!(
        state.db.latest_import_operation(&id).unwrap().unwrap().id,
        operation_id
    );

    // The deliberately untracked preservation sentinel would independently
    // block sync as a dirty worktree, so remove it before the healthy control.
    std::fs::remove_file(&sentinel).unwrap();
    // A new SVN revision remains synchronizable after the refused reset.
    let checkout = tmp.path().join("svn-wc");
    std::fs::write(checkout.join("history.txt"), "after refused reset\n").unwrap();
    let svn_commit = Command::new("svn")
        .args([
            "commit",
            "-m",
            "after refused reset",
            "--username",
            "fixture",
        ])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(svn_commit.status.success());
    let mut config = state.config.clone();
    config.svn.trunk_path = String::new();
    config.svn.layout = reposync_core::config::SvnLayout::Custom;
    config.github.default_branch = "main".into();
    config.identity.email_domain = Some("example.invalid".into());
    let repo = state.db.get_repository(&id).unwrap().unwrap();
    let mut engine = SyncEngine::new(
        config,
        Database::new(tmp.path().join("reposync.db")).unwrap(),
        SvnClient::new(format!("{}/trunk", repo.svn_url), "fixture", ""),
        GitClient::new(&local).unwrap(),
        Arc::new(
            IdentityMapper::new(&IdentityConfig {
                email_domain: Some("example.invalid".into()),
                ..Default::default()
            })
            .unwrap(),
        ),
    );
    engine.set_repo_id(id.clone());
    engine.run_sync_cycle().await.unwrap();
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 4);
    assert_eq!(
        git_output(&bare, &["show", "main:history.txt"]),
        b"after refused reset\n"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"74_RESET_REFUSAL", "operation_id":operation_id,
            "before_db_sha256":before_db, "remote_refs_unchanged":true,
            "remote_tree_unchanged":true,"local_refs_tree_index_workdir_unchanged":true,
            "checkpoint_before":3,"healthy_sync_after":4,"active_hold":false
        })
    );
    server.abort();
}

#[cfg(all(feature = "reliability-fixture", unix))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_64a_stalled_svn_info_child_and_descendant_are_stopped() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let descendant_stopped = |pid: i32| {
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        #[cfg(target_os = "linux")]
        {
            // The isolated container's PID 1 may defer reaping an orphaned
            // grandchild. A zombie has stopped and cannot perform SVN work.
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                return stat
                    .rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with("Z ") || rest.starts_with("X "));
            }
        }
        false
    };
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let wrapper = tmp.path().join("svn-stall-wrapper");
    let pid_file = tmp.path().join("svn-descendant.pid");
    std::fs::write(&wrapper, b"#!/bin/sh\nif [ \"$1\" = info ]; then\n sleep 60 & echo $! > \"$CHILD_PID_FILE\"\n wait\nfi\nexec svn \"$@\"\n").unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
    std::env::set_var("REPOSYNC_IMPORT_SVN_BINARY", &wrapper);
    std::env::set_var("CHILD_PID_FILE", &pid_file);
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client.post(&base).send().await.unwrap();
    assert!(start.status().is_success());
    let started: serde_json::Value = start.json().await.unwrap();
    let op_id = started["operation_id"].as_str().unwrap();
    if tokio::time::timeout(Duration::from_secs(10), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_err()
    {
        let status: serde_json::Value = client
            .get(format!("{base}/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        panic!("stalled SVN child marker missing; durable status: {status}");
    }
    let descendant: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(descendant, 0) }, 0);
    let status: serde_json::Value = tokio::time::timeout(Duration::from_secs(2), async {
        client
            .get(format!("{base}/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(status["operation_id"], op_id);
    let cancel = client
        .post(format!("{base}/{op_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert!(cancel.status().is_success());
    assert_eq!(
        terminal_import_status(&client, &base).await["lifecycle"],
        "cancelled"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !descendant_stopped(descendant) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    assert!(state.db.active_import_operation(&id).unwrap().is_some());
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
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"64A_SVN_CHILD","operation_id":op_id,
        "descendant_pid":descendant,"descendant_stopped":true,"checkpoint":0,"remote_ref_present":false})
    );
    server.abort();
    std::env::remove_var("REPOSYNC_IMPORT_SVN_BINARY");
    std::env::remove_var("CHILD_PID_FILE");
    std::env::remove_var("REPOSYNC_FIXTURE_ROOT");
}

#[cfg(feature = "reliability-fixture")]
mod import_reconciliation_tests {
    use super::*;
    use std::process::Command;

    struct HeldFixture {
        addr: SocketAddr,
        state: Arc<AppState>,
        server: tokio::task::JoinHandle<()>,
        tmp: tempfile::TempDir,
        id: String,
        bare: std::path::PathBuf,
        operation_id: String,
    }

    impl HeldFixture {
        async fn lost_reply() -> Self {
            Self::lost_reply_with_extra_revisions(0).await
        }

        async fn lost_reply_with_extra_revisions(extra: usize) -> Self {
            let (addr, state, server, tmp, id, bare) = import_fixture().await;
            for n in 0..extra {
                std::fs::write(
                    tmp.path().join("svn-wc/history.txt"),
                    format!("extra {n}\n"),
                )
                .unwrap();
                let output = Command::new("svn")
                    .args(["commit", "-m", "extra"])
                    .current_dir(tmp.path().join("svn-wc"))
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::env::set_var("REPOSYNC_FIXTURE_ROOT", tmp.path());
            std::env::set_var("REPOSYNC_IMPORT_LOST_PUSH_REPLY", &id);
            let client = authed_client();
            let base = format!("http://{addr}/api/repos/{id}/import");
            let start: serde_json::Value = client
                .post(&base)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let operation_id = start["operation_id"].as_str().unwrap().to_owned();
            let status = terminal_import_status(&client, &base).await;
            assert_eq!(status["lifecycle"], "reconciliation_required", "{status}");
            std::env::remove_var("REPOSYNC_IMPORT_LOST_PUSH_REPLY");
            Self {
                addr,
                state,
                server,
                tmp,
                id,
                bare,
                operation_id,
            }
        }

        async fn failed_finalizer() -> Self {
            let (addr, state, server, tmp, id, bare) = import_fixture().await;
            state.db.conn().execute_batch(
                "CREATE TRIGGER reject_import_finalization BEFORE UPDATE OF last_svn_rev ON repositories
                 BEGIN SELECT RAISE(FAIL, 'fixture finalizer failure'); END;"
            ).unwrap();
            let client = authed_client();
            let base = format!("http://{addr}/api/repos/{id}/import");
            let start: serde_json::Value = client
                .post(&base)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let operation_id = start["operation_id"].as_str().unwrap().to_owned();
            let status = terminal_import_status(&client, &base).await;
            assert_eq!(status["lifecycle"], "reconciliation_required", "{status}");
            assert!(status["intended_git_sha"].is_null());
            state
                .db
                .conn()
                .execute_batch("DROP TRIGGER reject_import_finalization")
                .unwrap();
            Self {
                addr,
                state,
                server,
                tmp,
                id,
                bare,
                operation_id,
            }
        }

        async fn restart(self) -> Self {
            let Self {
                state,
                server,
                tmp,
                id,
                bare,
                operation_id,
                ..
            } = self;
            let config = state.config.clone();
            let engine = state.sync_engine.clone();
            let sync_trigger = state.sync_trigger.clone();
            server.abort();
            let _ = server.await;
            drop(state);
            let db = Database::new(tmp.path().join("reposync.db")).unwrap();
            db.initialize().unwrap();
            let state = reposync_web::WebServer::new(
                config,
                db,
                engine,
                sync_trigger,
                tmp.path().join("config.toml"),
                Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
            )
            .app_state();
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
            Self {
                addr,
                state,
                server,
                tmp,
                id,
                bare,
                operation_id,
            }
        }

        async fn reconcile(&self) -> (reqwest::StatusCode, serde_json::Value) {
            let response = authed_client()
                .post(format!(
                    "http://{}/api/repos/{}/import/{}/reconcile",
                    self.addr, self.id, self.operation_id,
                ))
                .send()
                .await
                .unwrap();
            let status = response.status();
            (status, response.json().await.unwrap())
        }

        fn operation(&self) -> reposync_core::db::import_operations::ImportOperation {
            self.state
                .db
                .get_import_operation(&self.id, &self.operation_id)
                .unwrap()
                .unwrap()
        }

        fn remote(&self) -> (String, String, String) {
            let read = |args: &[&str]| -> String {
                let output = Command::new("git")
                    .arg("--git-dir")
                    .arg(&self.bare)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                String::from_utf8_lossy(&output.stdout).trim().to_owned()
            };
            (
                read(&["rev-parse", "refs/heads/main"]),
                read(&["rev-parse", "refs/heads/main^{tree}"]),
                read(&["rev-list", "--count", "refs/heads/main"]),
            )
        }

        fn svn_head(&self) -> String {
            let output = Command::new("svnlook")
                .arg("youngest")
                .arg(self.tmp.path().join("svn-repo"))
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }

        fn trace_remote_inspection(&self) {
            let script = self.tmp.path().join("inspection-only.sh");
            std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$REPOSYNC_RECONCILE_TRACE\"\n[ \"$1\" = ls-remote ] || exit 97\n[ \"${REPOSYNC_RECONCILE_FAIL:-0}\" = 1 ] && exit 128\nexec git \"$@\"\n").unwrap();
            std::env::set_var("REPOSYNC_FIXTURE_ROOT", self.tmp.path());
            std::env::set_var("REPOSYNC_IMPORT_GIT_BINARY", script);
            std::env::set_var(
                "REPOSYNC_RECONCILE_TRACE",
                self.tmp.path().join("reconcile-commands.log"),
            );
        }

        fn trace(&self) -> String {
            std::fs::read_to_string(self.tmp.path().join("reconcile-commands.log"))
                .unwrap_or_default()
        }

        fn clear_trace() {
            std::env::remove_var("REPOSYNC_IMPORT_GIT_BINARY");
            std::env::remove_var("REPOSYNC_RECONCILE_TRACE");
            std::env::remove_var("REPOSYNC_RECONCILE_FAIL");
            std::env::remove_var("REPOSYNC_FIXTURE_ROOT");
        }

        fn git_workdir(&self) -> std::path::PathBuf {
            self.state
                .config
                .daemon
                .data_dir
                .join("repos")
                .join(&self.id)
                .join("git-repo")
        }

        async fn auto_reconcile(&self) -> reposync_core::auto_reconcile::AutoReconcileResult {
            let repo = self.state.db.get_repository(&self.id).unwrap().unwrap();
            let svn = reposync_core::svn::SvnClient::new("file:///nonexistent", "fixture", "");
            reposync_core::auto_reconcile::reconcile_held_external_writes(
                &self.state.db,
                &repo,
                &svn,
                Some(self.git_workdir().as_path()),
            )
            .await
            .unwrap()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64e_import_lost_reply_auto_finalizes_without_second_publication() {
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        let checkpoint_before = fixture.state.db.get_repo_watermark(&fixture.id).unwrap();
        let op = fixture.operation();
        fixture.trace_remote_inspection();
        let result = fixture.auto_reconcile().await;
        let attempt = result
            .attempts
            .iter()
            .find(|a| {
                a.kind == reposync_core::auto_reconcile::HeldExternalWriteKind::ImportOperation
            })
            .expect("import auto-reconcile attempt");
        assert!(attempt.finalized, "{attempt:?}");
        assert!(!attempt.resume_authorized);
        assert_eq!(fixture.remote(), before);
        assert_ne!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap(),
            checkpoint_before
        );
        assert!(fixture
            .state
            .db
            .active_import_operation(&fixture.id)
            .unwrap()
            .is_none());
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({
                "case":"64E_IMPORT_AUTO_COMPLETE",
                "operation_id":op.id,
                "remote_before_after":before,
                "finalized":true
            })
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64e_import_mismatch_auto_reconcile_stays_held_without_mutation() {
        let fixture = HeldFixture::lost_reply().await;
        let checkpoint = fixture.state.db.get_repo_watermark(&fixture.id).unwrap();
        let parent = Command::new("git")
            .arg("--git-dir")
            .arg(&fixture.bare)
            .args(["rev-parse", "refs/heads/main^"])
            .output()
            .unwrap();
        assert!(parent.status.success());
        let parent = String::from_utf8_lossy(&parent.stdout).trim().to_owned();
        assert!(Command::new("git")
            .arg("--git-dir")
            .arg(&fixture.bare)
            .args(["update-ref", "refs/heads/main", &parent])
            .status()
            .unwrap()
            .success());
        fixture.trace_remote_inspection();
        let result = fixture.auto_reconcile().await;
        let attempt = result
            .attempts
            .iter()
            .find(|a| {
                a.kind == reposync_core::auto_reconcile::HeldExternalWriteKind::ImportOperation
            })
            .expect("import auto-reconcile attempt");
        assert!(!attempt.finalized);
        assert!(!attempt.resume_authorized);
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap(),
            checkpoint
        );
        assert_eq!(
            fixture.operation().state,
            reposync_core::db::import_operations::ImportOperationState::ReconciliationRequired
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({
                "case":"64E_IMPORT_MISMATCH_HELD",
                "finalized":false,
                "watermark_unchanged":true
            })
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64e_import_restart_still_auto_reconciles() {
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        let op = fixture.operation();
        let fixture = fixture.restart().await;
        fixture.state.db.hold_interrupted_imports().unwrap();
        assert_eq!(
            fixture.operation().state,
            reposync_core::db::import_operations::ImportOperationState::ReconciliationRequired
        );
        fixture.trace_remote_inspection();
        let result = fixture.auto_reconcile().await;
        let attempt = result
            .attempts
            .iter()
            .find(|a| {
                a.kind == reposync_core::auto_reconcile::HeldExternalWriteKind::ImportOperation
            })
            .expect("import auto-reconcile attempt");
        assert!(attempt.finalized, "{attempt:?}");
        assert_eq!(fixture.remote(), before);
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({
                "case":"64E_IMPORT_RESTART_AUTO",
                "operation_id":op.id,
                "held_until_auto_reconcile":true,
                "finalized":true
            })
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_lost_reply_complete_after_restart_without_second_push() {
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        let svn_before = fixture.svn_head();
        let op = fixture.operation();
        assert_eq!(op.intended_git_sha.as_deref(), Some(before.0.as_str()));
        assert!(op.last_confirmed_git_sha.is_none());
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
            0
        );
        let mappings = fixture.state.db.list_commit_map(100).unwrap().len();
        let fixture = fixture.restart().await;
        fixture.trace_remote_inspection();
        let (code, result) = fixture.reconcile().await;
        assert_eq!(code, reqwest::StatusCode::OK, "{result}");
        assert_eq!(result["lifecycle"], "completed", "{result}");
        assert_eq!(result["publication_proved"], true);
        assert_eq!(result["publication_receipt_recorded"], true);
        assert_eq!(result["checkpoint_completed"], true);
        assert_eq!(result["observed_remote_git_sha"], before.0);
        assert_eq!(fixture.remote(), before);
        assert_eq!(fixture.svn_head(), svn_before);
        assert_eq!(
            fixture.state.db.list_commit_map(100).unwrap().len(),
            mappings
        );
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap(),
            (3, before.0.clone())
        );
        assert_eq!(
            fixture
                .state
                .db
                .get_state(&format!("last_svn_rev_{}", fixture.id))
                .unwrap()
                .as_deref(),
            Some("3")
        );
        assert_eq!(
            fixture
                .state
                .db
                .get_state(&format!("last_git_sha_{}", fixture.id))
                .unwrap()
                .as_deref(),
            Some(before.0.as_str())
        );
        assert!(fixture
            .state
            .db
            .active_import_operation(&fixture.id)
            .unwrap()
            .is_none());
        assert_eq!(fixture.operation().confirmed_batches, 1);
        let (repeat_code, repeat) = fixture.reconcile().await;
        assert_eq!(repeat_code, reqwest::StatusCode::OK);
        assert_eq!(repeat["lifecycle"], "completed");
        assert_eq!(fixture.operation().confirmed_batches, 1);
        assert_eq!(
            fixture.trace().lines().collect::<Vec<_>>(),
            ["ls-remote --exit-code origin refs/heads/main"]
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_LOST_REPLY_COMPLETE",
            "operation_id":fixture.operation_id,"remote_before_after":before,"checkpoint":3,
            "mappings_before_after":mappings,"confirmed_batches":1,"command_trace":fixture.trace(),
            "restart":true,"idempotent":true})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_finalizer_recovery_after_restart_without_remote_write() {
        let fixture = HeldFixture::failed_finalizer().await;
        let before = fixture.remote();
        let svn_before = fixture.svn_head();
        let op = fixture.operation();
        assert!(op.intended_git_sha.is_none());
        assert_eq!(
            op.last_confirmed_git_sha.as_deref(),
            Some(before.0.as_str())
        );
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
            0
        );
        let mappings = fixture.state.db.list_commit_map(100).unwrap().len();
        let fixture = fixture.restart().await;
        fixture.trace_remote_inspection();
        let (code, result) = fixture.reconcile().await;
        assert_eq!(code, reqwest::StatusCode::OK, "{result}");
        assert_eq!(result["lifecycle"], "completed", "{result}");
        assert_eq!(result["publication_proved"], true);
        assert_eq!(result["publication_receipt_recorded"], false);
        assert_eq!(result["checkpoint_completed"], true);
        assert_eq!(fixture.remote(), before);
        assert_eq!(fixture.svn_head(), svn_before);
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap(),
            (3, before.0.clone())
        );
        assert_eq!(
            fixture.state.db.list_commit_map(100).unwrap().len(),
            mappings
        );
        assert_eq!(fixture.operation().confirmed_batches, op.confirmed_batches);
        assert!(fixture
            .state
            .db
            .active_import_operation(&fixture.id)
            .unwrap()
            .is_none());
        assert_eq!(
            fixture.trace().lines().collect::<Vec<_>>(),
            ["ls-remote --exit-code origin refs/heads/main"]
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_FINALIZER_RECOVERY",
            "remote_before_after":before,"checkpoint":3,"mappings_before_after":mappings,
            "command_trace":fixture.trace(),"restart":true})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    async fn remote_negative(kind: &str) {
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        let intended = fixture.operation().intended_git_sha.unwrap();
        let observed = match kind {
            "missing" => {
                let output = Command::new("git")
                    .arg("--git-dir")
                    .arg(&fixture.bare)
                    .args(["update-ref", "-d", "refs/heads/main"])
                    .output()
                    .unwrap();
                assert!(output.status.success());
                None
            }
            "mismatch" => {
                let parent = Command::new("git")
                    .arg("--git-dir")
                    .arg(&fixture.bare)
                    .args(["rev-parse", "refs/heads/main^"])
                    .output()
                    .unwrap();
                assert!(parent.status.success());
                let parent = String::from_utf8_lossy(&parent.stdout).trim().to_owned();
                let update = Command::new("git")
                    .arg("--git-dir")
                    .arg(&fixture.bare)
                    .args(["update-ref", "refs/heads/main", &parent])
                    .output()
                    .unwrap();
                assert!(update.status.success());
                Some(parent)
            }
            "advanced" => {
                let child = Command::new("git")
                    .arg("--git-dir")
                    .arg(&fixture.bare)
                    .args([
                        "commit-tree",
                        &before.1,
                        "-p",
                        &before.0,
                        "-m",
                        "external successor",
                    ])
                    .env("GIT_AUTHOR_NAME", "external")
                    .env("GIT_AUTHOR_EMAIL", "external@example.invalid")
                    .env("GIT_COMMITTER_NAME", "external")
                    .env("GIT_COMMITTER_EMAIL", "external@example.invalid")
                    .output()
                    .unwrap();
                assert!(
                    child.status.success(),
                    "{}",
                    String::from_utf8_lossy(&child.stderr)
                );
                let child = String::from_utf8_lossy(&child.stdout).trim().to_owned();
                let update = Command::new("git")
                    .arg("--git-dir")
                    .arg(&fixture.bare)
                    .args(["update-ref", "refs/heads/main", &child])
                    .output()
                    .unwrap();
                assert!(update.status.success());
                let ancestor = Command::new("git")
                    .arg("--git-dir")
                    .arg(&fixture.bare)
                    .args(["merge-base", "--is-ancestor", &before.0, &child])
                    .status()
                    .unwrap();
                assert!(ancestor.success());
                Some(child)
            }
            _ => panic!("unknown negative case"),
        };
        let checkpoint = fixture.state.db.get_repo_watermark(&fixture.id).unwrap();
        fixture.trace_remote_inspection();
        let (code, result) = fixture.reconcile().await;
        assert_eq!(code, reqwest::StatusCode::OK, "{result}");
        assert_eq!(result["lifecycle"], "reconciliation_required", "{result}");
        assert_eq!(result["publication_proved"], false);
        assert_eq!(result["checkpoint_completed"], false);
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap(),
            checkpoint
        );
        assert_eq!(
            fixture.operation().intended_git_sha.as_deref(),
            Some(intended.as_str())
        );
        assert_eq!(fixture.operation().confirmed_batches, 0);
        assert!(fixture
            .state
            .db
            .active_import_operation(&fixture.id)
            .unwrap()
            .is_some());
        assert_eq!(
            fixture.trace().lines().collect::<Vec<_>>(),
            ["ls-remote --exit-code origin refs/heads/main"]
        );
        match observed.as_deref() {
            Some(sha) => {
                assert_eq!(fixture.remote().0, sha);
                assert_eq!(result["observed_remote_git_sha"], sha);
            }
            None => {
                let absent = Command::new("git")
                    .arg("--git-dir")
                    .arg(&fixture.bare)
                    .args(["show-ref", "--verify", "refs/heads/main"])
                    .status()
                    .unwrap();
                assert!(!absent.success());
                assert!(result["observed_remote_git_sha"].is_null());
            }
        }
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":format!("64B_REMOTE_{}", kind.to_uppercase()),
            "intended":intended,"observed":observed,"checkpoint":checkpoint,"held":true,
            "command_trace":fixture.trace()})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_remote_missing_keeps_intent_and_checkpoint() {
        remote_negative("missing").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_remote_mismatch_keeps_intent_and_checkpoint() {
        remote_negative("mismatch").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_remote_advanced_is_not_exact_publication_proof() {
        remote_negative("advanced").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_inspection_failure_is_not_remote_absence() {
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        let intended = fixture.operation().intended_git_sha.clone();
        fixture.trace_remote_inspection();
        std::env::set_var("REPOSYNC_RECONCILE_FAIL", "1");
        let (code, result) = fixture.reconcile().await;
        assert_eq!(code, reqwest::StatusCode::OK);
        assert_eq!(result["lifecycle"], "reconciliation_required");
        assert!(result["remaining_reason"]
            .as_str()
            .unwrap()
            .contains("unavailable"));
        assert_eq!(fixture.remote(), before);
        assert_eq!(fixture.operation().intended_git_sha, intended);
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
            0
        );
        assert_eq!(
            fixture.trace().lines().collect::<Vec<_>>(),
            ["ls-remote --exit-code origin refs/heads/main"]
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_INSPECTION_FAILURE",
            "remote_before_after":before,"checkpoint":0,"held":true,"command_trace":fixture.trace()})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_config_changed_blocks_verified_remote() {
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        let intended = fixture.operation().intended_git_sha.clone();
        fixture.state.db.conn().execute(
            "UPDATE repositories SET git_repo='local/changed',updated_at=datetime('now') WHERE id=?1",
            [&fixture.id],
        ).unwrap();
        fixture.trace_remote_inspection();
        let (code, result) = fixture.reconcile().await;
        assert_eq!(code, reqwest::StatusCode::OK);
        assert_eq!(result["lifecycle"], "reconciliation_required");
        assert!(result["remaining_reason"]
            .as_str()
            .unwrap()
            .contains("configuration changed"));
        assert_eq!(fixture.remote(), before);
        assert_eq!(fixture.operation().intended_git_sha, intended);
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
            0
        );
        assert!(
            fixture.trace().is_empty(),
            "configuration must be checked before remote access"
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_CONFIG_CHANGED",
            "remote_before_after":before,"checkpoint":0,"held":true,"command_trace":fixture.trace()})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_missing_local_object_blocks_remote_confirmation() {
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        let intended = fixture.operation().intended_git_sha.unwrap();
        let object = fixture
            .tmp
            .path()
            .join("repos")
            .join(&fixture.id)
            .join("git-repo")
            .join(".git/objects")
            .join(&intended[..2])
            .join(&intended[2..]);
        assert!(
            object.exists(),
            "fixture commit should be a loose local object"
        );
        std::fs::remove_file(object).unwrap();
        fixture.trace_remote_inspection();
        let (code, result) = fixture.reconcile().await;
        assert_eq!(code, reqwest::StatusCode::OK, "{result}");
        assert_eq!(result["lifecycle"], "reconciliation_required");
        assert!(result["remaining_reason"]
            .as_str()
            .unwrap()
            .contains("local Git object"));
        assert_eq!(
            fixture.operation().intended_git_sha.as_deref(),
            Some(intended.as_str())
        );
        assert_eq!(fixture.remote(), before);
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
            0
        );
        assert!(fixture.trace().is_empty());
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_LOCAL_MISSING",
            "remote_before_after":before,"checkpoint":0,"intent_retained":true,"command_trace":fixture.trace()})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_unsupported_v1_document_is_preserved() {
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        let key = format!("import_operation_v1:document:{}", fixture.operation_id);
        let mut record: serde_json::Value =
            serde_json::from_str(&fixture.state.db.get_state(&key).unwrap().unwrap()).unwrap();
        record["version"] = serde_json::json!(2);
        let raw = record.to_string();
        fixture
            .state
            .db
            .conn()
            .execute(
                "UPDATE kv_state SET value=?1 WHERE key=?2",
                rusqlite::params![raw, key],
            )
            .unwrap();
        fixture.trace_remote_inspection();
        let (code, _) = fixture.reconcile().await;
        assert_eq!(code, reqwest::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(fixture.state.db.get_state(&key).unwrap().unwrap(), raw);
        assert_eq!(fixture.remote(), before);
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
            0
        );
        assert!(fixture.trace().is_empty());
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_MALFORMED",
            "unsupported_record_preserved":true,"checkpoint":0,"command_trace":fixture.trace()})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_finalization_failure_rolls_back_publication_receipt() {
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        fixture.state.db.conn().execute_batch(
            "CREATE TRIGGER reject_reconciliation_completion BEFORE UPDATE OF last_svn_rev ON repositories
             BEGIN SELECT RAISE(FAIL, 'fixture completion failure'); END;"
        ).unwrap();
        fixture.trace_remote_inspection();
        let (code, _) = fixture.reconcile().await;
        assert_eq!(code, reqwest::StatusCode::INTERNAL_SERVER_ERROR);
        let held = fixture.operation();
        assert_eq!(
            held.state,
            reposync_core::db::import_operations::ImportOperationState::ReconciliationRequired
        );
        assert!(held.intended_git_sha.is_some());
        assert!(held.last_confirmed_git_sha.is_none());
        assert_eq!(held.confirmed_batches, 0);
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
            0
        );
        assert_eq!(fixture.remote(), before);
        fixture
            .state
            .db
            .conn()
            .execute_batch("DROP TRIGGER reject_reconciliation_completion")
            .unwrap();
        let (retry_code, retry) = fixture.reconcile().await;
        assert_eq!(retry_code, reqwest::StatusCode::OK, "{retry}");
        assert_eq!(retry["lifecycle"], "completed");
        assert_eq!(fixture.operation().confirmed_batches, 1);
        assert_eq!(fixture.remote(), before);
        assert_eq!(fixture.trace().lines().count(), 2);
        assert!(fixture
            .trace()
            .lines()
            .all(|line| line == "ls-remote --exit-code origin refs/heads/main"));
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_ATOMIC_FAILURE",
            "first":"rolled_back_held","second":"completed_once","remote_before_after":before,
            "command_trace":fixture.trace()})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_conflicting_legacy_cursor_is_not_overwritten() {
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        let cursor = format!("last_svn_rev_{}", fixture.id);
        fixture.state.db.set_state(&cursor, "99").unwrap();
        let intended = fixture.operation().intended_git_sha.clone();
        fixture.trace_remote_inspection();
        let (code, result) = fixture.reconcile().await;
        assert_eq!(code, reqwest::StatusCode::OK, "{result}");
        assert_eq!(result["lifecycle"], "reconciliation_required");
        assert!(result["remaining_reason"]
            .as_str()
            .unwrap()
            .contains("checkpoint changed"));
        assert_eq!(
            fixture.state.db.get_state(&cursor).unwrap().as_deref(),
            Some("99")
        );
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
            0
        );
        assert_eq!(fixture.operation().intended_git_sha, intended);
        assert_eq!(fixture.remote(), before);
        assert_eq!(fixture.trace().lines().count(), 1);
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_CURSOR_CONFLICT",
            "legacy_cursor":99,"repository_checkpoint":0,"intent_retained":true,
            "remote_before_after":before,"command_trace":fixture.trace()})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_stale_id_cannot_reconcile_new_active_operation() {
        let fixture = HeldFixture::lost_reply().await;
        let old = fixture.operation();
        let active_key = format!("import_operation_v1:active:{}", fixture.id);
        fixture
            .state
            .db
            .conn()
            .execute("DELETE FROM kv_state WHERE key=?1", [&active_key])
            .unwrap();
        let newer = fixture
            .state
            .db
            .create_import_operation(
                &fixture.id,
                "legacy",
                "newer-fixture-request",
                &old.target_fingerprint,
            )
            .unwrap();
        fixture.trace_remote_inspection();
        let (code, _result) = fixture.reconcile().await;
        assert_eq!(code, reqwest::StatusCode::BAD_REQUEST);
        assert_eq!(
            fixture
                .state
                .db
                .active_import_operation(&fixture.id)
                .unwrap()
                .unwrap()
                .id,
            newer.id
        );
        assert_eq!(fixture.operation().intended_git_sha, old.intended_git_sha);
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
            0
        );
        assert!(fixture.trace().is_empty());
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_STALE_ID",
            "old_id":fixture.operation_id,"new_id":newer.id,"checkpoint":0,"command_trace":fixture.trace()})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_partial_publication_confirmed_but_stays_held() {
        let fixture = HeldFixture::lost_reply_with_extra_revisions(49).await;
        let before = fixture.remote();
        let original = fixture.operation();
        assert_eq!(original.processed_revisions, 50);
        assert_eq!(original.total_revisions, Some(52));
        fixture.trace_remote_inspection();
        let (code, result) = fixture.reconcile().await;
        assert_eq!(code, reqwest::StatusCode::OK, "{result}");
        assert_eq!(result["lifecycle"], "reconciliation_required");
        assert_eq!(result["publication_proved"], true);
        assert_eq!(result["publication_receipt_recorded"], true);
        assert_eq!(result["checkpoint_completed"], false);
        assert!(result["remaining_reason"]
            .as_str()
            .unwrap()
            .contains("partial"));
        assert_eq!(result["may_resume"], true);
        assert_eq!(result["resume_authorized"], true);
        assert_eq!(fixture.operation().confirmed_batches, 1);
        assert!(fixture.operation().intended_git_sha.is_none());
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
            0
        );
        assert!(fixture
            .state
            .db
            .active_import_operation(&fixture.id)
            .unwrap()
            .is_some());
        let (repeat_code, repeat) = fixture.reconcile().await;
        assert_eq!(repeat_code, reqwest::StatusCode::OK);
        assert_eq!(repeat["lifecycle"], "reconciliation_required");
        assert_eq!(repeat["publication_receipt_recorded"], false);
        assert_eq!(fixture.operation().confirmed_batches, 1);
        assert_eq!(fixture.remote(), before);
        assert_eq!(fixture.trace().lines().count(), 2);
        assert!(fixture
            .trace()
            .lines()
            .all(|line| line == "ls-remote --exit-code origin refs/heads/main"));
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_PARTIAL_CONFIRMED",
            "processed":50,"total":52,"checkpoint":0,"confirmed_batches":1,
            "remote_before_after":before,"command_trace":fixture.trace(),"held":true})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_named_admin_only_without_legacy_fallback() {
        use reposync_core::models::{Session, User};
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        let now = chrono::Utc::now();
        for (id, role, enabled) in [
            ("admin-64b", "admin", true),
            ("viewer-64b", "viewer", true),
            ("disabled-64b", "admin", false),
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
                "admin-64b-token",
                "admin-64b",
                now + chrono::Duration::hours(1),
            ),
            (
                "viewer-64b-token",
                "viewer-64b",
                now + chrono::Duration::hours(1),
            ),
            (
                "disabled-64b-token",
                "disabled-64b",
                now + chrono::Duration::hours(1),
            ),
            (
                "expired-64b-token",
                "admin-64b",
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
        fixture.trace_remote_inspection();
        let url = format!(
            "http://{}/api/repos/{}/import/{}/reconcile",
            fixture.addr, fixture.id, fixture.operation_id
        );
        for token in [
            TEST_TOKEN,
            "viewer-64b-token",
            "disabled-64b-token",
            "expired-64b-token",
        ] {
            let denied = reqwest::Client::new()
                .post(&url)
                .bearer_auth(token)
                .send()
                .await
                .unwrap();
            assert_eq!(
                denied.status(),
                reqwest::StatusCode::UNAUTHORIZED,
                "{token}"
            );
            assert_eq!(
                fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
                0
            );
            assert!(fixture.trace().is_empty());
        }
        let accepted = reqwest::Client::new()
            .post(&url)
            .bearer_auth("admin-64b-token")
            .send()
            .await
            .unwrap();
        assert!(accepted.status().is_success());
        let body: serde_json::Value = accepted.json().await.unwrap();
        assert_eq!(body["lifecycle"], "completed");
        assert_eq!(fixture.remote(), before);
        assert_eq!(fixture.trace().lines().count(), 1);
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_AUTH",
            "denied":4,"admin_completed":true,"checkpoint":3,"command_trace":fixture.trace()})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_reconcile_serializes_against_manual_writer() {
        let fixture = HeldFixture::lost_reply().await;
        let before = fixture.remote();
        fixture.trace_remote_inspection();
        let guard = reposync_core::busy::try_acquire(&fixture.id).unwrap();
        let url = format!(
            "http://{}/api/repos/{}/import/{}/reconcile",
            fixture.addr, fixture.id, fixture.operation_id
        );
        let first_url = url.clone();
        let first =
            tokio::spawn(async move { authed_client().post(first_url).send().await.unwrap() });
        let second = tokio::spawn(async move { authed_client().post(url).send().await.unwrap() });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!first.is_finished() && !second.is_finished());
        let client = authed_client();
        let denied = client
            .post(format!(
                "http://{}/api/repos/{}/sync",
                fixture.addr, fixture.id
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), reqwest::StatusCode::BAD_REQUEST);
        assert_eq!(
            fixture.state.db.get_repo_watermark(&fixture.id).unwrap().0,
            0
        );
        let repo = fixture
            .state
            .db
            .get_repository(&fixture.id)
            .unwrap()
            .unwrap();
        let other = client
            .post(format!("http://{}/api/repos", fixture.addr))
            .json(&serde_json::json!({"name":"other", "svn_url":repo.svn_url,
                "svn_branch":"trunk", "git_provider":"gitea", "git_api_url":repo.git_api_url,
                "git_repo":"local/other", "git_branch":"main"}))
            .send()
            .await
            .unwrap();
        assert!(other.status().is_success());
        let other: serde_json::Value = other.json().await.unwrap();
        let unrelated = client
            .post(format!(
                "http://{}/api/repos/{}/sync",
                fixture.addr,
                other["id"].as_str().unwrap()
            ))
            .send()
            .await
            .unwrap();
        assert!(unrelated.status().is_success());
        drop(guard);
        let a = first.await.unwrap();
        let b = second.await.unwrap();
        assert!(a.status().is_success() && b.status().is_success());
        let a: serde_json::Value = a.json().await.unwrap();
        let b: serde_json::Value = b.json().await.unwrap();
        assert_eq!(a["lifecycle"], "completed");
        assert_eq!(b["lifecycle"], "completed");
        assert_eq!(fixture.operation().confirmed_batches, 1);
        assert_eq!(fixture.remote(), before);
        assert_eq!(fixture.trace().lines().count(), 1);
        let ordinary = client
            .post(format!(
                "http://{}/api/repos/{}/sync",
                fixture.addr, fixture.id
            ))
            .send()
            .await
            .unwrap();
        assert!(ordinary.status().is_success());
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_RACE",
            "two_requests":"serialized_completed","manual_during":"held",
            "unrelated_repo":"usable","manual_after":"allowed","checkpoint":3,
            "command_trace":fixture.trace()})
        );
        HeldFixture::clear_trace();
        fixture.server.abort();
    }

    #[cfg(feature = "reliability-browser")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64b_mounted_import_card_verifies_complete_mismatch_and_partial() {
        async fn browse(fixture: &HeldFixture, mode: &str) {
            let (browser, mut vite) =
                run_import_card_browser(fixture.addr, &fixture.id, fixture.tmp.path(), mode).await;
            let output = tokio::time::timeout(Duration::from_secs(60), browser.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            eprintln!("{}", String::from_utf8_lossy(&output.stdout));
            assert!(
                output.status.success(),
                "browser {mode}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            vite.kill().unwrap();
            vite.wait().unwrap();
        }

        let complete = HeldFixture::lost_reply().await;
        complete.trace_remote_inspection();
        browse(&complete, "reconcile-complete").await;
        assert_eq!(
            complete.operation().state,
            reposync_core::db::import_operations::ImportOperationState::Completed
        );
        assert_eq!(
            complete
                .state
                .db
                .get_repo_watermark(&complete.id)
                .unwrap()
                .0,
            3
        );
        assert_eq!(complete.trace().lines().count(), 1);
        complete.server.abort();
        HeldFixture::clear_trace();

        let mismatch = HeldFixture::lost_reply().await;
        let parent = Command::new("git")
            .arg("--git-dir")
            .arg(&mismatch.bare)
            .args(["rev-parse", "refs/heads/main^"])
            .output()
            .unwrap();
        assert!(parent.status.success());
        let parent = String::from_utf8_lossy(&parent.stdout).trim().to_owned();
        assert!(Command::new("git")
            .arg("--git-dir")
            .arg(&mismatch.bare)
            .args(["update-ref", "refs/heads/main", &parent])
            .status()
            .unwrap()
            .success());
        mismatch.trace_remote_inspection();
        browse(&mismatch, "reconcile-mismatch").await;
        assert_eq!(
            mismatch.operation().state,
            reposync_core::db::import_operations::ImportOperationState::ReconciliationRequired
        );
        assert_eq!(
            mismatch
                .state
                .db
                .get_repo_watermark(&mismatch.id)
                .unwrap()
                .0,
            0
        );
        assert_eq!(mismatch.trace().lines().count(), 1);
        mismatch.server.abort();
        HeldFixture::clear_trace();

        let partial = HeldFixture::lost_reply_with_extra_revisions(49).await;
        partial.trace_remote_inspection();
        browse(&partial, "reconcile-partial").await;
        assert_eq!(
            partial.operation().state,
            reposync_core::db::import_operations::ImportOperationState::ReconciliationRequired
        );
        assert_eq!(
            partial.state.db.get_repo_watermark(&partial.id).unwrap().0,
            0
        );
        assert_eq!(partial.operation().confirmed_batches, 1);
        assert_eq!(partial.trace().lines().count(), 1);
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64B_UI",
            "journeys":["complete","mismatch","partial"],
            "command_trace_each":"ls-remote only","reload":true})
        );
        partial.server.abort();
        HeldFixture::clear_trace();
    }
}

fn disposable_remote_tips(
    root: &std::path::Path,
) -> (std::path::PathBuf, String, std::path::PathBuf) {
    use std::process::Command;
    let svn_repo = root.join("svn-remote");
    let created = Command::new("svnadmin")
        .args(["create", svn_repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let svn_url = format!("file://{}", svn_repo.display());
    let created = Command::new("svn")
        .args([
            "mkdir",
            &format!("{svn_url}/trunk"),
            "-m",
            "fixture",
            "--non-interactive",
        ])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let git_bare = root.join("git-origin.git");
    assert!(Command::new("git")
        .args(["init", "--bare", git_bare.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let git_work = root.join("git-work");
    assert!(Command::new("git")
        .args(["init", "-b", "main", git_work.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    std::fs::write(git_work.join("fixture.txt"), "preserve remote history\n").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(&git_work)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["add", "fixture.txt"]);
    git(&["commit", "-m", "Fixture"]);
    git(&["remote", "add", "origin", git_bare.to_str().unwrap()]);
    git(&["push", "origin", "main"]);
    (svn_repo, svn_url, git_bare)
}

fn remote_snapshot(svn_repo: &std::path::Path, git_bare: &std::path::Path) -> (Vec<u8>, Vec<u8>) {
    use std::process::Command;
    let svn = Command::new("svnlook")
        .args(["youngest", svn_repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(svn.status.success());
    let git = Command::new("git")
        .args([
            "--git-dir",
            git_bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    assert!(git.status.success());
    (svn.stdout, git.stdout)
}

async fn create_fixture_repo(base: &str, client: &reqwest::Client, svn_url: &str) -> String {
    let response = client
        .post(format!("{base}/api/repos"))
        .json(&serde_json::json!({
            "name": format!("fixture-{}", uuid::Uuid::new_v4()),
            "svn_url": svn_url,
            "svn_branch": "trunk",
            "git_provider": "gitea",
            "git_api_url": "http://127.0.0.1:9/api/v1",
            "git_repo": "local/fixture",
            "git_branch": "main"
        }))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
    let body: serde_json::Value = response.json().await.unwrap();
    body["id"].as_str().unwrap().to_string()
}

/// R02: legacy root DELETE disables only. Registration, mappings, secrets, and remotes stay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r02_legacy_delete_keeps_registration_mappings_and_remotes() {
    let (addr, state, server, tmp) = build_test_server_full().await;
    let (svn_repo, svn_url, git_bare) = disposable_remote_tips(tmp.path());
    let before = remote_snapshot(&svn_repo, &git_bare);
    let client = authed_client();
    let base = format!("http://{addr}");
    let id = create_fixture_repo(&base, &client, &svn_url).await;
    state
        .db
        .conn()
        .execute(
            "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
             VALUES (2, 'def456', 'svn_to_git', 't', 'svn', 'git', ?1)",
            [&id],
        )
        .unwrap();
    state
        .db
        .set_state(&format!("secret_svn_password_{id}"), "owned")
        .unwrap();
    let delete = client
        .delete(format!("{base}/api/repos/{id}"))
        .send()
        .await
        .unwrap();
    assert!(delete.status().is_success());
    let body: serde_json::Value = delete.json().await.unwrap();
    assert_eq!(body["message"], "repository disabled");
    assert_eq!(body["action"], "disable");
    assert_ne!(body["action"], "managed_remove");
    let again = client
        .delete(format!("{base}/api/repos/{id}"))
        .send()
        .await
        .unwrap();
    assert!(again.status().is_success());
    let repo = state.db.get_repository(&id).unwrap().unwrap();
    assert!(!repo.enabled);
    let maps: i64 = state
        .db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM commit_map WHERE repo_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(maps, 1);
    assert_eq!(
        state
            .db
            .get_state(&format!("secret_svn_password_{id}"))
            .unwrap()
            .as_deref(),
        Some("owned")
    );
    assert!(state.db.managed_removal(&id).unwrap().is_none());
    assert!(state.db.removal_tombstone(&id).unwrap().is_none());
    let listed: Vec<serde_json::Value> = client
        .get(format!("{base}/api/repos"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed
        .iter()
        .any(|repo| repo["id"] == id && repo["enabled"] == false));
    assert_eq!(remote_snapshot(&svn_repo, &git_bare), before);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"R02_LEGACY_DELETE","action":"disable","registration":true,"mappings":1,"remote":"unchanged"})
    );
    server.abort();
}

/// R02: additive managed removal cleans owned local data only and leaves remotes unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r02_managed_remove_preserves_remotes_and_owned_local_only() {
    let (addr, state, server, tmp) = build_test_server_full().await;
    let (svn_repo, svn_url, git_bare) = disposable_remote_tips(tmp.path());
    let before = remote_snapshot(&svn_repo, &git_bare);
    let client = authed_client();
    let base = format!("http://{addr}");
    let id = create_fixture_repo(&base, &client, &svn_url).await;
    let saved = state.db.get_repository(&id).unwrap().unwrap();
    state
        .db
        .set_state(&format!("secret_svn_password_{id}"), "owned-secret")
        .unwrap();
    state
        .db
        .set_state("secret_svn_password", "global-secret")
        .unwrap();
    state
        .db
        .set_state("secret_svn_password_sibling", "sibling-secret")
        .unwrap();
    state
        .db
        .set_state(&format!("last_svn_rev_{id}"), "4")
        .unwrap();
    state
        .db
        .conn()
        .execute(
            "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
             VALUES (4, 'abc', 'svn_to_git', 't', 'svn', 'git', ?1)",
            [&id],
        )
        .unwrap();
    let data = state.config.daemon.data_dir.clone();
    let owned = data.join("repos").join(&id);
    std::fs::create_dir_all(owned.join("git-repo")).unwrap();
    std::fs::write(owned.join("git-repo").join("owned.txt"), "owned\n").unwrap();
    let outside = tmp.path().join("outside-target");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "do-not-delete\n").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.txt"), owned.join("escape")).unwrap();
    let sibling = data.join("repos").join("sibling-dir");
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("keep.txt"), "sibling\n").unwrap();

    let mut child = saved.clone();
    child.id = format!("{id}-child");
    child.name = format!("{}-child", saved.name);
    child.parent_id = Some(id.clone());
    child.git_branch = "feature".into();
    state.db.insert_repository(&child).unwrap();
    let blocked = client
        .post(format!("{base}/api/repos/{id}/remove"))
        .send()
        .await
        .unwrap();
    let blocked_status = blocked.status();
    let blocked_body: serde_json::Value = blocked.json().await.unwrap();
    assert_eq!(
        blocked_status,
        reqwest::StatusCode::CONFLICT,
        "{blocked_body}"
    );
    assert_eq!(blocked_body["state"], "blocked");
    assert_eq!(blocked_body["parent_removal_blocked"], true);
    assert!(blocked_body["dependency_preview"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == child.id));
    assert!(owned.join("git-repo").join("owned.txt").exists());
    assert_eq!(remote_snapshot(&svn_repo, &git_bare), before);
    state
        .db
        .conn()
        .execute("DELETE FROM repositories WHERE id=?1", [&child.id])
        .unwrap();

    let removed = client
        .post(format!("{base}/api/repos/{id}/remove"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        removed.status(),
        reqwest::StatusCode::OK,
        "{}",
        removed.text().await.unwrap()
    );
    let body: serde_json::Value = removed.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["action"], "managed_remove");
    assert_eq!(body["state"], "completed");
    assert_eq!(body["remote_git"], "untouched");
    assert_eq!(body["remote_svn"], "untouched");
    assert_eq!(body["restore_supported"], true);
    assert_eq!(body["registration_listed"], false);
    assert!(!owned.exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "do-not-delete\n"
    );
    assert_eq!(
        std::fs::read_to_string(sibling.join("keep.txt")).unwrap(),
        "sibling\n"
    );
    assert!(state.db.get_repository(&id).unwrap().is_none());
    assert!(state
        .db
        .get_state(&format!("secret_svn_password_{id}"))
        .unwrap()
        .is_none());
    assert_eq!(
        state
            .db
            .get_state("secret_svn_password")
            .unwrap()
            .as_deref(),
        Some("global-secret")
    );
    assert_eq!(
        state
            .db
            .get_state("secret_svn_password_sibling")
            .unwrap()
            .as_deref(),
        Some("sibling-secret")
    );
    assert_eq!(
        state
            .db
            .get_state(&format!("last_svn_rev_{id}"))
            .unwrap()
            .as_deref(),
        Some("4")
    );
    let maps: i64 = state
        .db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM commit_map WHERE repo_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(maps, 1);
    let tombstone = state.db.removal_tombstone(&id).unwrap().unwrap();
    assert!(tombstone.restore_supported);
    assert_eq!(tombstone.remote_git, "untouched");
    let listed: Vec<serde_json::Value> = client
        .get(format!("{base}/api/repos"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed.iter().all(|repo| repo["id"] != id));
    let missing = client
        .get(format!("{base}/api/repos/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    let status = client
        .get(format!("{base}/api/repos/{id}/removal"))
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), reqwest::StatusCode::OK);
    let repeated = client
        .post(format!("{base}/api/repos/{id}/remove"))
        .send()
        .await
        .unwrap();
    assert_eq!(repeated.status(), reqwest::StatusCode::OK);
    let repeated: serde_json::Value = repeated.json().await.unwrap();
    assert_eq!(repeated["state"], "completed");
    assert_eq!(repeated["ok"], true);
    assert!(state
        .db
        .insert_repository(&saved)
        .unwrap_err()
        .to_string()
        .contains("cannot recreate"));
    assert_eq!(remote_snapshot(&svn_repo, &git_bare), before);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"R02_MANAGED_REMOVE","state":"completed","remote":"unchanged","owned_local":"removed","sibling":"kept","restore_supported":true})
    );
    server.abort();
}

/// R02: busy or failed path cleanup does not report success and does not touch remotes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r02_managed_remove_waits_and_retries_without_touching_remotes() {
    let (addr, state, server, tmp) = build_test_server_full().await;
    let (svn_repo, svn_url, git_bare) = disposable_remote_tips(tmp.path());
    let before = remote_snapshot(&svn_repo, &git_bare);
    let client = authed_client();
    let base = format!("http://{addr}");
    let id = create_fixture_repo(&base, &client, &svn_url).await;
    let data = state.config.daemon.data_dir.clone();
    let outside = tmp.path().join("linked-outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "do-not-delete\n").unwrap();
    let link = data.join("repos").join(&id);
    if link.exists() {
        std::fs::remove_dir_all(&link).unwrap();
    }
    std::fs::create_dir_all(data.join("repos")).unwrap();
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    let failed = client
        .post(format!("{base}/api/repos/{id}/remove"))
        .send()
        .await
        .unwrap();
    let failed_status = failed.status();
    let failed_body: serde_json::Value = failed.json().await.unwrap();
    assert_eq!(
        failed_status,
        reqwest::StatusCode::CONFLICT,
        "{failed_body}"
    );
    assert_eq!(failed_body["ok"], false);
    assert_eq!(failed_body["state"], "failed");
    assert_eq!(failed_body["retryable"], true);
    assert_eq!(failed_body["action"], "managed_remove");
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "do-not-delete\n"
    );
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert!(state.db.get_repository(&id).unwrap().is_some());
    assert_eq!(remote_snapshot(&svn_repo, &git_bare), before);

    std::fs::remove_file(&link).unwrap();
    std::fs::create_dir_all(link.join("git-repo")).unwrap();
    std::fs::write(link.join("owned.txt"), "owned\n").unwrap();
    let guard = reposync_core::busy::try_acquire(&id).unwrap();
    let waiting = client
        .post(format!("{base}/api/repos/{id}/remove"))
        .send()
        .await
        .unwrap();
    let waiting_status = waiting.status();
    let waiting_body: serde_json::Value = waiting.json().await.unwrap();
    assert_eq!(
        waiting_status,
        reqwest::StatusCode::ACCEPTED,
        "{waiting_body}"
    );
    assert_eq!(waiting_body["ok"], false);
    assert_eq!(waiting_body["state"], "cancelling");
    assert!(link.join("owned.txt").exists());
    assert_eq!(remote_snapshot(&svn_repo, &git_bare), before);
    drop(guard);

    let done = client
        .post(format!("{base}/api/repos/{id}/remove"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        done.status(),
        reqwest::StatusCode::OK,
        "{}",
        done.text().await.unwrap()
    );
    let done: serde_json::Value = done.json().await.unwrap();
    assert_eq!(done["state"], "completed");
    assert_eq!(done["ok"], true);
    assert!(!link.exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "do-not-delete\n"
    );
    assert_eq!(remote_snapshot(&svn_repo, &git_bare), before);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"R02_REMOVE_RETRY","failed_then_completed":true,"busy_wait":"cancelling","remote":"unchanged"})
    );
    server.abort();
}

async fn wait_import_terminal(client: &reqwest::Client, base: &str) -> serde_json::Value {
    let mut last = serde_json::Value::Null;
    // Snapshot workers record verification before `git commit`. A 10s budget
    // expired while that commit was still running under a parallel workspace
    // test load, so the waiter covers a slower commit without treating
    // `verifying` as terminal.
    for _ in 0..600 {
        last = client
            .get(format!("{base}/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if matches!(
            last["lifecycle"].as_str(),
            Some("completed" | "cancelled" | "failed" | "reconciliation_required")
        ) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    last
}

fn git_show(bare: &std::path::Path, spec: &str) -> Vec<u8> {
    use std::process::Command;
    let out = Command::new("git")
        .args(["--git-dir", bare.to_str().unwrap(), "show", spec])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn git_rev_list_count(bare: &std::path::Path) -> usize {
    use std::process::Command;
    let out = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-list",
            "--count",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

fn svn_commit_extra(tmp: &std::path::Path, name: &str, body: &str) {
    use std::process::Command;
    let svn_url = format!("file://{}", tmp.join("svn-repo").display());
    let checkout = tmp.join(format!("svn-wc-{name}"));
    assert!(Command::new("svn")
        .args([
            "checkout",
            &format!("{svn_url}/trunk"),
            checkout.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    std::fs::write(checkout.join(name), body).unwrap();
    assert!(Command::new("svn")
        .args(["add", name])
        .current_dir(&checkout)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("svn")
        .args(["commit", "-m", "advance after pin", "--username", "fixture"])
        .current_dir(&checkout)
        .status()
        .unwrap()
        .success());
}

/// R11: snapshot at a selected numeric revision materializes that tree only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r11_snapshot_at_fixed_revision() {
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client
        .post(&base)
        .json(&serde_json::json!({"import_mode":"snapshot","svn_revision":"2"}))
        .send()
        .await
        .unwrap();
    assert!(
        start.status().is_success(),
        "{}",
        start.text().await.unwrap()
    );
    let started: serde_json::Value = start.json().await.unwrap();
    assert_eq!(started["import_mode"], "snapshot");
    assert_eq!(started["starting_revision"], 2);
    assert!(started["history_boundary"]
        .as_str()
        .unwrap()
        .contains("before r2"));
    let status = wait_import_terminal(&client, &base).await;
    assert_eq!(status["lifecycle"], "completed", "{status}");
    assert_eq!(status["import_mode"], "snapshot");
    assert_eq!(status["starting_revision"], 2);
    assert_eq!(status["earlier_history_imported"], false);
    assert_eq!(status["snapshot_pin"]["operative_rev"], 2);
    assert_eq!(status["snapshot_pin"]["peg_rev"], 2);
    assert_eq!(git_rev_list_count(&bare), 1);
    assert_eq!(git_show(&bare, "main:history.txt"), b"first\n");
    let (rev, sha) = state.db.get_repo_watermark(&id).unwrap();
    assert_eq!(rev, 2);
    assert_eq!(status["last_confirmed_svn_rev"], 2);
    assert_eq!(status["last_confirmed_git_sha"].as_str().unwrap(), sha);
    let repo: serde_json::Value = client
        .get(format!("http://{addr}/api/repos/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(repo["import_mode"], "snapshot");
    assert_eq!(repo["starting_revision"], 2);
    assert_eq!(repo["initializing"], false);
    assert!(!repo["history_boundary"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("full history"));
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R11_SNAPSHOT_FIXED_R",
            "starting_revision":2,
            "git_commits":1,
            "history_txt":"first",
            "watermark":rev,
            "earlier_history_imported":false
        })
    );
    let _ = tmp;
    server.abort();
}

/// R11: pin is sticky after SVN advances past the selected revision.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r11_pin_holds_when_svn_advances() {
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client
        .post(&base)
        .json(&serde_json::json!({"import_mode":"snapshot","svn_revision":"HEAD"}))
        .send()
        .await
        .unwrap();
    assert!(
        start.status().is_success(),
        "{}",
        start.text().await.unwrap()
    );
    let started: serde_json::Value = start.json().await.unwrap();
    assert_eq!(started["starting_revision"], 3);
    svn_commit_extra(tmp.path(), "later.txt", "after-pin\n");
    let status = wait_import_terminal(&client, &base).await;
    assert_eq!(status["lifecycle"], "completed", "{status}");
    assert_eq!(status["starting_revision"], 3);
    assert_eq!(git_rev_list_count(&bare), 1);
    assert_eq!(git_show(&bare, "main:history.txt"), b"second\n");
    let later = std::process::Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "cat-file",
            "-e",
            "main:later.txt",
        ])
        .status()
        .unwrap();
    assert!(
        !later.success(),
        "later SVN revision must not appear in snapshot"
    );
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 3);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R11_PIN_HOLDS_AFTER_ADVANCE",
            "pinned":3,
            "svn_after_pin":4,
            "git_has_later_txt":false
        })
    );
    server.abort();
}

/// R12: omitted import_mode stays full-history (upgrade-compatible default).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r12_omitted_mode_is_full_history() {
    let (addr, state, server, _tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client.post(&base).send().await.unwrap();
    assert!(
        start.status().is_success(),
        "{}",
        start.text().await.unwrap()
    );
    let started: serde_json::Value = start.json().await.unwrap();
    assert_eq!(started["import_mode"], "full");
    assert!(started["starting_revision"].is_null());
    let status = wait_import_terminal(&client, &base).await;
    assert_eq!(status["lifecycle"], "completed", "{status}");
    assert_eq!(status["import_mode"], "full");
    assert_eq!(status["earlier_history_imported"], true);
    assert_eq!(git_rev_list_count(&bare), 3);
    assert_eq!(git_show(&bare, "main:history.txt"), b"second\n");
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 3);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"R12_FULL_DEFAULT","import_mode":"full","git_commits":3})
    );
    server.abort();
}

/// R11: mismatched existing Git target is refused without reset or overwrite.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r11_mismatched_target_is_refused() {
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let work = tmp.path().join("existing-target");
    assert!(Command::new("git")
        .args(["init", "-b", "main", work.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    std::fs::write(work.join("keep.txt"), "existing independent history\n").unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&work)
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["add", "keep.txt"]);
    git(&["commit", "-m", "preexisting"]);
    git(&["remote", "add", "origin", bare.to_str().unwrap()]);
    git(&["push", "origin", "main"]);
    let before = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let rejected = client
        .post(&base)
        .json(&serde_json::json!({"import_mode":"snapshot","svn_revision":"2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = rejected.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("already exists")
            || body["error"].as_str().unwrap().contains("new target"),
        "{body}"
    );
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    let after = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ])
        .output()
        .unwrap();
    assert_eq!(before.stdout, after.stdout);
    assert_eq!(
        git_show(&bare, "main:keep.txt"),
        b"existing independent history\n"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"R11_MISMATCHED_TARGET","refused":true,"remote":"unchanged"})
    );
    server.abort();
}

/// R11: invalid / inaccessible revision fails closed without a baseline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r11_invalid_revision_is_refused() {
    use std::process::Command;
    let (addr, state, server, _tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let rejected = client
        .post(&base)
        .json(&serde_json::json!({"import_mode":"snapshot","svn_revision":"99"}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = rejected.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("revision"),
        "{body}"
    );
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
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
    let status: serde_json::Value = client
        .get(format!("{base}/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["lifecycle"], "failed");
    assert_eq!(status["import_mode"], "snapshot");
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"R11_INVALID_REV","refused":true,"watermark":0})
    );
    server.abort();
}

/// R11: snapshot import with an LFS threshold publishes a pointer, not a fat blob.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r11_snapshot_lfs_pointer_publication() {
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    state
        .db
        .conn()
        .execute(
            "UPDATE repositories SET lfs_threshold_mb=1 WHERE id=?1",
            [&id],
        )
        .unwrap();
    let checkout = tmp.path().join("svn-wc");
    let large = vec![0u8; 1_100_000];
    std::fs::write(checkout.join("large.bin"), &large).unwrap();
    assert!(Command::new("svn")
        .args(["add", "large.bin"])
        .current_dir(&checkout)
        .status()
        .unwrap()
        .success());
    let commit = Command::new("svn")
        .args(["commit", "-m", "large binary", "--username", "fixture"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client
        .post(&base)
        .json(&serde_json::json!({"import_mode":"snapshot","svn_revision":"HEAD"}))
        .send()
        .await
        .unwrap();
    assert!(
        start.status().is_success(),
        "{}",
        start.text().await.unwrap()
    );
    let status = wait_import_terminal(&client, &base).await;
    assert_eq!(status["lifecycle"], "completed", "{status}");
    assert_eq!(status["import_mode"], "snapshot");
    assert_eq!(status["starting_revision"], 4);
    assert_eq!(status["earlier_history_imported"], false);
    assert_eq!(git_rev_list_count(&bare), 1);
    assert_eq!(git_show(&bare, "main:history.txt"), b"second\n");
    let pointer = git_show(&bare, "main:large.bin");
    assert!(pointer.starts_with(b"version https://git-lfs.github.com/spec/v1\n"));
    assert!(String::from_utf8_lossy(&pointer).contains("size 1100000"));
    assert!(status["log_lines"].as_array().unwrap().iter().any(|line| {
        line.as_str()
            .unwrap_or("")
            .contains("Git LFS installed in repo")
    }));
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 4);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R11_SNAPSHOT_LFS",
            "starting_revision":4,
            "git_commits":1,
            "pointer_published":true,
            "source_bytes":large.len()
        })
    );
    server.abort();
}

/// R11: a snapshot held after publication can finish only when the remote SHA matches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r11_snapshot_reconcile_finishes_exact_baseline() {
    use std::process::Command;
    let (addr, state, server, _tmp, id, bare) = import_fixture().await;
    state
        .db
        .conn()
        .execute_batch(
            "CREATE TRIGGER reject_import_finalization BEFORE UPDATE OF last_svn_rev ON repositories
             BEGIN SELECT RAISE(FAIL, 'fixture finalizer failure'); END;",
        )
        .unwrap();
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client
        .post(&base)
        .json(&serde_json::json!({"import_mode":"snapshot","svn_revision":"2"}))
        .send()
        .await
        .unwrap();
    assert!(
        start.status().is_success(),
        "{}",
        start.text().await.unwrap()
    );
    let started: serde_json::Value = start.json().await.unwrap();
    let operation_id = started["operation_id"].as_str().unwrap();
    let status = wait_import_terminal(&client, &base).await;
    assert_eq!(status["lifecycle"], "reconciliation_required", "{status}");
    state
        .db
        .conn()
        .execute_batch("DROP TRIGGER reject_import_finalization")
        .unwrap();
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);
    assert_eq!(git_rev_list_count(&bare), 1);
    assert_eq!(git_show(&bare, "main:history.txt"), b"first\n");
    let op = state
        .db
        .get_import_operation(&id, operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(op.operation_type, "snapshot_import");
    assert_eq!(op.snapshot_pin.as_ref().unwrap().operative_rev, 2);
    let intended = op.last_confirmed_git_sha.clone().unwrap();

    let tree = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main^{tree}",
        ])
        .output()
        .unwrap();
    assert!(tree.status.success());
    let tree = String::from_utf8_lossy(&tree.stdout).trim().to_owned();
    let child = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "commit-tree",
            &tree,
            "-m",
            "unrelated",
        ])
        .env("GIT_AUTHOR_NAME", "external")
        .env("GIT_AUTHOR_EMAIL", "external@example.invalid")
        .env("GIT_COMMITTER_NAME", "external")
        .env("GIT_COMMITTER_EMAIL", "external@example.invalid")
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let child = String::from_utf8_lossy(&child.stdout).trim().to_owned();
    assert!(Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "update-ref",
            "refs/heads/main",
            &child,
        ])
        .status()
        .unwrap()
        .success());
    let mismatch = client
        .post(format!("{base}/{operation_id}/reconcile"))
        .send()
        .await
        .unwrap();
    assert!(mismatch.status().is_success());
    let mismatch: serde_json::Value = mismatch.json().await.unwrap();
    assert_eq!(
        mismatch["lifecycle"], "reconciliation_required",
        "{mismatch}"
    );
    assert_eq!(mismatch["checkpoint_completed"], false);
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 0);

    assert!(Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "update-ref",
            "refs/heads/main",
            &intended,
        ])
        .status()
        .unwrap()
        .success());
    let done = client
        .post(format!("{base}/{operation_id}/reconcile"))
        .send()
        .await
        .unwrap();
    assert!(done.status().is_success(), "{}", done.text().await.unwrap());
    let done: serde_json::Value = done.json().await.unwrap();
    assert_eq!(done["lifecycle"], "completed", "{done}");
    assert_eq!(done["checkpoint_completed"], true);
    assert_eq!(done["publication_proved"], true);
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 2);
    assert_eq!(git_rev_list_count(&bare), 1);
    assert_eq!(git_show(&bare, "main:history.txt"), b"first\n");
    let again = client
        .post(format!("{base}/{operation_id}/reconcile"))
        .send()
        .await
        .unwrap();
    assert!(again.status().is_success());
    let again: serde_json::Value = again.json().await.unwrap();
    assert_eq!(again["lifecycle"], "completed");
    assert_eq!(state.db.get_repo_watermark(&id).unwrap().0, 2);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R11_SNAPSHOT_RECONCILE",
            "mismatch_held":true,
            "watermark":2,
            "git_commits":1
        })
    );
    server.abort();
}

fn git_cmd(dir: &std::path::Path, args: &[&str]) {
    use std::process::Command;
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{} {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn clone_work(tmp: &std::path::Path, bare: &std::path::Path, name: &str) -> std::path::PathBuf {
    let work = tmp.join(name);
    git_cmd(
        tmp,
        &["clone", bare.to_str().unwrap(), work.to_str().unwrap()],
    );
    work
}

fn svn_youngest(svn_repo: &std::path::Path) -> i64 {
    use std::process::Command;
    let out = Command::new("svnlook")
        .args(["youngest", svn_repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

async fn snapshot_imported_parent() -> (
    std::net::SocketAddr,
    std::sync::Arc<AppState>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
    String,
    std::path::PathBuf,
) {
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let client = authed_client();
    let base = format!("http://{addr}/api/repos/{id}/import");
    let start = client
        .post(&base)
        .json(&serde_json::json!({"import_mode":"snapshot","svn_revision":"2"}))
        .send()
        .await
        .unwrap();
    assert!(
        start.status().is_success(),
        "{}",
        start.text().await.unwrap()
    );
    let status = wait_import_terminal(&client, &base).await;
    assert_eq!(status["lifecycle"], "completed", "{status}");
    assert!(std::process::Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ])
        .status()
        .unwrap()
        .success());
    (addr, state, server, tmp, id, bare)
}

fn push_feature_commits(tmp: &std::path::Path, bare: &std::path::Path, n: usize) -> String {
    let work = clone_work(tmp, bare, "feature-work");
    // Bare fixtures often keep unborn `master` as HEAD while the snapshot lives
    // on `refs/heads/main`. Branch from that imported parent, not a new root.
    git_cmd(
        &work,
        &[
            "fetch",
            "--",
            "origin",
            "refs/heads/main:refs/remotes/origin/main",
        ],
    );
    git_cmd(&work, &["checkout", "-B", "feature", "origin/main"]);
    for i in 1..=n {
        std::fs::write(work.join("feature.txt"), format!("step {i}\n")).unwrap();
        if i == 1 {
            git_cmd(&work, &["add", "feature.txt"]);
            git_cmd(&work, &["commit", "-m", &format!("Feature step {i}")]);
        } else {
            git_cmd(&work, &["commit", "-am", &format!("Feature step {i}")]);
        }
    }
    git_cmd(&work, &["push", "-u", "origin", "feature"]);
    let tip = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&work)
        .output()
        .unwrap();
    String::from_utf8_lossy(&tip.stdout).trim().to_string()
}

fn push_orphan_branch(tmp: &std::path::Path, bare: &std::path::Path) {
    let work = clone_work(tmp, bare, "orphan-work");
    git_cmd(&work, &["checkout", "--orphan", "unrelated"]);
    let _ = std::process::Command::new("git")
        .args(["rm", "-rf", "--ignore-unmatch", "."])
        .current_dir(&work)
        .output();
    std::fs::write(work.join("git-first.txt"), "unrelated git-first root\n").unwrap();
    git_cmd(&work, &["add", "git-first.txt"]);
    git_cmd(&work, &["commit", "-m", "Unrelated Git-first root"]);
    git_cmd(&work, &["push", "-u", "origin", "unrelated"]);
}

/// R06: SVN-derived Git feature commits are admitted in preview; tips are pinned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_svn_derived_preview_pins_tips() {
    let (addr, state, server, tmp, id, bare) = snapshot_imported_parent().await;
    let feature_tip = push_feature_commits(tmp.path(), &bare, 2);
    let svn_repo = tmp.path().join("svn-repo");
    let before = svn_youngest(&svn_repo);
    let children_before = state.db.list_child_repositories(&id).unwrap().len();
    let client = authed_client();
    let response = client
        .post(format!("http://{addr}/api/repos/{id}/branches"))
        .json(&serde_json::json!({
            "svn_branch":"branches/feature",
            "git_branch":"feature",
            "skip_import":false,
            "dry_run":true,
            "auto_create_svn_branch":true,
            "auto_create_git_branch":true
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let plan: serde_json::Value = response.json().await.unwrap();
    assert!(status.is_success(), "{plan}");
    assert_eq!(plan["mode"], "preview");
    assert_eq!(plan["admitted"], true);
    assert_eq!(plan["published"], false);
    assert_eq!(plan["scheduler_active"], false);
    assert_eq!(plan["pair_state"], "preparing");
    assert_eq!(plan["policy_version"], "late_pair_admission_v1");
    assert_eq!(plan["git_tip"], feature_tip);
    assert_eq!(plan["svn_source_revision"], 2);
    assert_eq!(plan["proposed_svn_copy_source_revision"], 2);
    assert_eq!(plan["pending_git"]["count"], 2);
    assert_eq!(plan["skip_import_applied"], false);
    assert_eq!(plan["existing_svn_target"]["equivalent"], false);
    assert_eq!(
        state.db.list_child_repositories(&id).unwrap().len(),
        children_before
    );
    assert_eq!(svn_youngest(&svn_repo), before);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R06_SVN_DERIVED_PREVIEW",
            "admitted":true,
            "pending_git":2,
            "svn_source_revision":2,
            "published":false,
            "child_rows":0
        })
    );
    server.abort();
}

/// R06/R11: snapshot-bounded ancestry is enough for late-pair admission.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_snapshot_baseline_late_pair() {
    let (addr, state, server, tmp, id, bare) = snapshot_imported_parent().await;
    assert_eq!(git_rev_list_count(&bare), 1);
    let _ = push_feature_commits(tmp.path(), &bare, 1);
    let client = authed_client();
    let response = client
        .post(format!("http://{addr}/api/repos/{id}/branches"))
        .json(&serde_json::json!({
            "svn_branch":"branches/feature",
            "git_branch":"feature",
            "skip_import":false,
            "preview":true
        }))
        .send()
        .await
        .unwrap();
    let plan: serde_json::Value = response.json().await.unwrap();
    assert_eq!(plan["admitted"], true, "{plan}");
    assert_eq!(plan["verified_baseline"]["svn_revision"], 2);
    assert_eq!(
        plan["verified_baseline"]["evidence"],
        "snapshot_import_confirmed"
    );
    assert_eq!(plan["pending_git"]["count"], 1);
    assert!(state.db.list_child_repositories(&id).unwrap().is_empty());
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R06_SNAPSHOT_BASELINE",
            "parent_git_commits":1,
            "pending_git":1,
            "admitted":true
        })
    );
    server.abort();
}

/// R08: unrelated Git-first / orphan history is refused before remote mutation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r08_refuse_unrelated_git_first() {
    let (addr, state, server, tmp, id, bare) = snapshot_imported_parent().await;
    push_orphan_branch(tmp.path(), &bare);
    let svn_repo = tmp.path().join("svn-repo");
    let before = svn_youngest(&svn_repo);
    let client = authed_client();
    let response = client
        .post(format!("http://{addr}/api/repos/{id}/branches"))
        .json(&serde_json::json!({
            "svn_branch":"branches/feature",
            "git_branch":"unrelated",
            "skip_import":false,
            "dry_run":true
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("unrelated_git_first"),
        "{body}"
    );
    assert!(state.db.list_child_repositories(&id).unwrap().is_empty());
    assert_eq!(svn_youngest(&svn_repo), before);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R08_REFUSE_GIT_FIRST",
            "refused":true,
            "reason":"unrelated_git_first",
            "svn_unchanged":true,
            "child_rows":0
        })
    );
    server.abort();
}

/// R06: unsafe skip_import is refused and does not set watermarks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_unsafe_skip_import_blocked() {
    let (addr, state, server, tmp, id, bare) = snapshot_imported_parent().await;
    let _ = push_feature_commits(tmp.path(), &bare, 2);
    let parent_mark = state.db.get_repo_watermark(&id).unwrap();
    let client = authed_client();
    let response = client
        .post(format!("http://{addr}/api/repos/{id}/branches"))
        .json(&serde_json::json!({
            "svn_branch":"branches/feature",
            "git_branch":"feature",
            "skip_import":true,
            "dry_run":true
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("unsafe_skip_import"),
        "{body}"
    );
    assert!(state.db.list_child_repositories(&id).unwrap().is_empty());
    assert_eq!(state.db.get_repo_watermark(&id).unwrap(), parent_mark);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R06_SKIP_IMPORT_BLOCKED",
            "refused":true,
            "watermark_unchanged":true
        })
    );
    server.abort();
}

/// R07 partial: an existing SVN target is reported as not equivalent; no copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_existing_target_not_equivalent() {
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = snapshot_imported_parent().await;
    let _ = push_feature_commits(tmp.path(), &bare, 1);
    let svn_url = format!("file://{}", tmp.path().join("svn-repo").display());
    let svn_repo = tmp.path().join("svn-repo");
    assert!(Command::new("svn")
        .args([
            "mkdir",
            &format!("{svn_url}/branches"),
            "-m",
            "branches",
            "--non-interactive"
        ])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("svn")
        .args([
            "copy",
            &format!("{svn_url}/trunk"),
            &format!("{svn_url}/branches/feature"),
            "-m",
            "existing target",
            "--non-interactive",
        ])
        .status()
        .unwrap()
        .success());
    let before = svn_youngest(&svn_repo);
    let client = authed_client();
    let response = client
        .post(format!("http://{addr}/api/repos/{id}/branches"))
        .json(&serde_json::json!({
            "svn_branch":"branches/feature",
            "git_branch":"feature",
            "skip_import":false,
            "dry_run":true
        }))
        .send()
        .await
        .unwrap();
    let plan: serde_json::Value = response.json().await.unwrap();
    assert_eq!(plan["admitted"], true, "{plan}");
    assert_eq!(plan["existing_svn_target"]["exists"], true);
    assert_eq!(plan["existing_svn_target"]["equivalent"], false);
    assert!(plan["proposed_svn_copy_source_revision"].is_null());
    assert_eq!(svn_youngest(&svn_repo), before);
    assert!(state.db.list_child_repositories(&id).unwrap().is_empty());
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R06_EXISTING_TARGET_NOT_EQUIVALENT",
            "exists":true,
            "equivalent":false,
            "svn_unchanged":true
        })
    );
    server.abort();
}

/// R06: explicit publish is refused; no scheduler-active child.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r06_no_active_on_partial() {
    let (addr, state, server, tmp, id, bare) = snapshot_imported_parent().await;
    let _ = push_feature_commits(tmp.path(), &bare, 1);
    let client = authed_client();
    let response = client
        .post(format!("http://{addr}/api/repos/{id}/branches"))
        .json(&serde_json::json!({
            "svn_branch":"branches/feature",
            "git_branch":"feature",
            "skip_import":false,
            "dry_run":false,
            "preview":false
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("publish_not_implemented"),
        "{body}"
    );
    assert!(state.db.list_child_repositories(&id).unwrap().is_empty());
    let parent = state.db.get_repository(&id).unwrap().unwrap();
    assert!(parent.enabled);
    assert_ne!(parent.sync_status, "reconciling");
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R06_NO_ACTIVE_ON_PARTIAL",
            "child_rows":0,
            "publish_refused":true
        })
    );
    server.abort();
}

fn bare_tip(bare: &std::path::Path, branch: &str) -> String {
    use std::process::Command;
    let out = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn bare_refs(bare: &std::path::Path) -> String {
    use std::process::Command;
    let out = Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "for-each-ref",
            "--format=%(refname) %(objectname)",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn svn_uuid(repo: &std::path::Path) -> String {
    use std::process::Command;
    let out = Command::new("svnlook")
        .args(["uuid", repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn svn_path_rev(url: &str) -> i64 {
    use std::process::Command;
    let out = Command::new("svn")
        .args([
            "info",
            "--show-item",
            "last-changed-revision",
            "--non-interactive",
            url,
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

fn durable_job_rows(state: &AppState) -> i64 {
    state
        .db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM kv_state WHERE key LIKE 'import_operation_v1:%' OR key LIKE 'git_to_svn_commit_v1:%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn push_commit_on(
    tmp: &std::path::Path,
    bare: &std::path::Path,
    branch: &str,
    file: &str,
    body: &str,
    message: &str,
) {
    let name = format!(
        "extra-{}-{}",
        branch.replace('/', "-"),
        file.replace('/', "-")
    );
    let work = clone_work(tmp, bare, &name);
    git_cmd(
        &work,
        &[
            "fetch",
            "--",
            "origin",
            &format!("refs/heads/{branch}:refs/remotes/origin/{branch}"),
        ],
    );
    git_cmd(
        &work,
        &["checkout", "-B", branch, &format!("origin/{branch}")],
    );
    std::fs::write(work.join(file), body).unwrap();
    git_cmd(&work, &["add", file]);
    git_cmd(&work, &["commit", "-m", message]);
    git_cmd(&work, &["push", "origin", branch]);
}

fn svn_commit_on(
    tmp: &std::path::Path,
    url: &str,
    dir_name: &str,
    file: &str,
    body: &str,
    message: &str,
) {
    use std::process::Command;
    let wc = tmp.join(dir_name);
    let checkout = Command::new("svn")
        .args([
            "checkout",
            "--non-interactive",
            "--username",
            "fixture",
            url,
            wc.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        checkout.status.success(),
        "{}",
        String::from_utf8_lossy(&checkout.stderr)
    );
    std::fs::write(wc.join(file), body).unwrap();
    let add = Command::new("svn")
        .args(["add", file])
        .current_dir(&wc)
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let commit = Command::new("svn")
        .args([
            "commit",
            "-m",
            message,
            "--username",
            "fixture",
            "--non-interactive",
        ])
        .current_dir(&wc)
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
}

async fn refresh_pair_fixture() -> (
    std::net::SocketAddr,
    std::sync::Arc<AppState>,
    tokio::task::JoinHandle<()>,
    tempfile::TempDir,
    String,
    String,
    std::path::PathBuf,
) {
    use std::process::Command;
    let (addr, state, server, tmp, id, bare) = snapshot_imported_parent().await;
    let _feature = push_feature_commits(tmp.path(), &bare, 1);
    let parent = state.db.get_repository(&id).unwrap().unwrap();
    let svn_url = parent.svn_url.clone();
    assert!(Command::new("svn")
        .args([
            "mkdir",
            &format!("{svn_url}/branches"),
            "-m",
            "branches",
            "--username",
            "fixture",
            "--non-interactive",
        ])
        .status()
        .unwrap()
        .success());
    assert!(Command::new("svn")
        .args([
            "copy",
            &format!("{svn_url}/trunk"),
            &format!("{svn_url}/branches/feature"),
            "-m",
            "pair path",
            "--username",
            "fixture",
            "--non-interactive",
        ])
        .status()
        .unwrap()
        .success());
    let mut child = parent.clone();
    let pair_id = format!("{id}-pair");
    child.id = pair_id.clone();
    child.name = format!("{}-pair", parent.name);
    child.parent_id = Some(id.clone());
    child.git_branch = "feature".into();
    child.svn_branch = "branches/feature".into();
    child.last_svn_rev = 0;
    child.last_git_sha.clear();
    state.db.insert_repository(&child).unwrap();
    (addr, state, server, tmp, id, pair_id, bare)
}

fn refresh_unchanged(
    state: &AppState,
    parent_id: &str,
    pair_id: &str,
    bare: &std::path::Path,
    svn_repo: &std::path::Path,
) -> (String, i64, i64, (i64, String), (i64, String), i64) {
    (
        bare_refs(bare),
        svn_youngest(svn_repo),
        durable_job_rows(state),
        state.db.get_repo_watermark(parent_id).unwrap(),
        state.db.get_repo_watermark(pair_id).unwrap(),
        state.db.count_sync_records().unwrap(),
    )
}

/// R13: preview pins both sides and does not mutate checkpoints or remotes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r13_preview_pins_inputs() {
    let (addr, state, server, tmp, parent_id, pair_id, bare) = refresh_pair_fixture().await;
    let svn_repo = tmp.path().join("svn-repo");
    let svn_url = state
        .db
        .get_repository(&parent_id)
        .unwrap()
        .unwrap()
        .svn_url;
    let before = refresh_unchanged(&state, &parent_id, &pair_id, &bare, &svn_repo);
    let workdir = state
        .config
        .daemon
        .data_dir
        .join("repos")
        .join(&parent_id)
        .join("git-repo");
    let head_before = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&workdir)
        .output()
        .unwrap();
    let head_before = String::from_utf8_lossy(&head_before.stdout)
        .trim()
        .to_string();
    let client = authed_client();
    let response = client
        .post(format!("http://{addr}/api/repos/{pair_id}/refresh"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let plan: serde_json::Value = response.json().await.unwrap();
    assert!(status.is_success(), "{plan}");
    assert_eq!(plan["mode"], "preview");
    assert_eq!(plan["operation"], "update_pair_from_parent");
    assert_eq!(plan["executed"], false);
    assert_eq!(plan["published"], false);
    assert_eq!(plan["durable_job_started"], false);
    assert_eq!(plan["policy_version"], "pair_refresh_preview_v1");
    assert_eq!(plan["pair_generation"], 1);
    assert_eq!(
        plan["generation_source"],
        "compatibility_single_registration"
    );
    assert_eq!(plan["git"]["pair_tip"], bare_tip(&bare, "feature"));
    assert_eq!(plan["git"]["parent_tip"], bare_tip(&bare, "main"));
    assert_eq!(plan["svn"]["uuid"], svn_uuid(&svn_repo));
    assert_eq!(plan["svn"]["pair_path"], "branches/feature");
    assert_eq!(plan["svn"]["parent_path"], "trunk");
    assert_eq!(
        plan["svn"]["pair_revision"],
        svn_path_rev(&format!("{svn_url}/branches/feature"))
    );
    assert_eq!(
        plan["svn"]["parent_revision"],
        svn_path_rev(&format!("{svn_url}/trunk"))
    );
    let digest = plan["plan_digest"].as_str().unwrap();
    assert_eq!(digest.len(), 64);
    assert_eq!(plan["plan_id"], digest);
    assert_eq!(plan["approval"]["eligible"], false);
    assert_eq!(plan["approval"]["binds_to"], "plan_digest");
    assert_eq!(plan["reanchor_status"], "NOT_IMPLEMENTED");
    assert_eq!(plan["execute_status"], "NOT_IMPLEMENTED");
    assert_eq!(plan["intended_result"]["discards_unsynced_work"], false);
    assert_eq!(plan["inspection"]["external_git_writes"], false);
    assert_eq!(plan["inspection"]["external_svn_writes"], false);
    assert_eq!(plan["inspection"]["checkpoint_mutation"], false);
    assert!(plan["pins_complete"].as_bool().unwrap(), "{plan}");
    let again = client
        .post(format!("http://{addr}/api/repos/{pair_id}/refresh"))
        .json(&serde_json::json!({"operation":"update_pair_from_parent","execute":false}))
        .send()
        .await
        .unwrap();
    let again: serde_json::Value = again.json().await.unwrap();
    assert_eq!(again["plan_digest"], digest);
    let root = client
        .post(format!("http://{addr}/api/repos/{parent_id}/refresh"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(root.status(), reqwest::StatusCode::BAD_REQUEST);
    let root_body: serde_json::Value = root.json().await.unwrap();
    assert!(
        root_body["error"]
            .as_str()
            .unwrap_or("")
            .contains("not_a_branch_pair"),
        "{root_body}"
    );
    assert_eq!(
        refresh_unchanged(&state, &parent_id, &pair_id, &bare, &svn_repo),
        before
    );
    let head_after = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&workdir)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&head_after.stdout).trim(),
        head_before
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R13_PREVIEW_PINS_INPUTS",
            "pins_complete":true,
            "digest_stable":true,
            "remotes_unchanged":true,
            "jobs_unchanged":true
        })
    );
    server.abort();
}

/// R13: unsynced work on both Git and SVN sides is reported and not discarded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r13_pending_both_sides() {
    let (addr, state, server, tmp, parent_id, pair_id, bare) = refresh_pair_fixture().await;
    let svn_url = state
        .db
        .get_repository(&parent_id)
        .unwrap()
        .unwrap()
        .svn_url;
    let client = authed_client();
    let first: serde_json::Value = client
        .post(format!("http://{addr}/api/repos/{pair_id}/refresh"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["executed"], false, "{first}");
    let pair_git = first["pending"]["pair_git"]["count"].as_u64().unwrap();
    let parent_git = first["pending"]["parent_git"]["count"].as_u64().unwrap();
    let pair_svn = first["pending"]["pair_svn"]["count"].as_u64().unwrap();
    let parent_svn = first["pending"]["parent_svn"]["count"].as_u64().unwrap();
    push_commit_on(
        tmp.path(),
        &bare,
        "main",
        "parent.txt",
        "parent\n",
        "parent git",
    );
    push_commit_on(
        tmp.path(),
        &bare,
        "feature",
        "pair.txt",
        "pair\n",
        "pair git",
    );
    svn_commit_on(
        tmp.path(),
        &format!("{svn_url}/trunk"),
        "wc-trunk",
        "parent-svn.txt",
        "parent svn\n",
        "parent svn",
    );
    svn_commit_on(
        tmp.path(),
        &format!("{svn_url}/branches/feature"),
        "wc-feature",
        "pair-svn.txt",
        "pair svn\n",
        "pair svn",
    );
    let svn_repo = tmp.path().join("svn-repo");
    let before_refs = bare_refs(&bare);
    let before_rev = svn_youngest(&svn_repo);
    let before_jobs = durable_job_rows(&state);
    let response = client
        .post(format!("http://{addr}/api/repos/{pair_id}/refresh"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let plan: serde_json::Value = response.json().await.unwrap();
    assert!(status.is_success(), "{plan}");
    assert_eq!(
        plan["pending"]["pair_git"]["count"].as_u64().unwrap(),
        pair_git + 1,
        "{plan}"
    );
    assert_eq!(
        plan["pending"]["parent_git"]["count"].as_u64().unwrap(),
        parent_git + 1,
        "{plan}"
    );
    assert!(
        plan["pending"]["pair_svn"]["head_revision"]
            .as_i64()
            .unwrap()
            > first["pending"]["pair_svn"]["head_revision"]
                .as_i64()
                .unwrap(),
        "{plan}"
    );
    assert!(
        plan["pending"]["parent_svn"]["head_revision"]
            .as_i64()
            .unwrap()
            > first["pending"]["parent_svn"]["head_revision"]
                .as_i64()
                .unwrap(),
        "{plan}"
    );
    assert!(plan["pending"]["pair_svn"]["count"].as_u64().unwrap() > pair_svn);
    assert!(plan["pending"]["parent_svn"]["count"].as_u64().unwrap() > parent_svn);
    assert!(
        plan["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "both_advanced"),
        "{plan}"
    );
    assert_eq!(plan["intended_result"]["discards_unsynced_work"], false);
    let summary = plan["intended_result"]["summary"].as_str().unwrap();
    assert!(summary.contains("preserved"), "{summary}");
    assert!(summary.contains("not discarded"), "{summary}");
    assert_eq!(plan["executed"], false);
    assert_eq!(bare_refs(&bare), before_refs);
    assert_eq!(svn_youngest(&svn_repo), before_rev);
    assert_eq!(durable_job_rows(&state), before_jobs);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R13_PENDING_BOTH_SIDES",
            "both_advanced":true,
            "discards_unsynced_work":false
        })
    );
    server.abort();
}

/// R13: execution is refused and does not start a durable job.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r13_execute_refused() {
    let (addr, state, server, tmp, parent_id, pair_id, bare) = refresh_pair_fixture().await;
    let svn_repo = tmp.path().join("svn-repo");
    let before = refresh_unchanged(&state, &parent_id, &pair_id, &bare, &svn_repo);
    let client = authed_client();
    for body in [
        serde_json::json!({"execute":true}),
        serde_json::json!({"dry_run":false}),
    ] {
        let response = client
            .post(format!("http://{addr}/api/repos/{pair_id}/refresh"))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let payload: serde_json::Value = response.json().await.unwrap();
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{payload}");
        let error = payload["error"].as_str().unwrap_or("");
        assert!(
            error.contains("refresh_execute_not_implemented"),
            "{payload}"
        );
        assert!(error.contains("No durable job"), "{payload}");
        assert!(error.contains("plan_digest="), "{payload}");
    }
    assert_eq!(
        refresh_unchanged(&state, &parent_id, &pair_id, &bare, &svn_repo),
        before
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R13_EXECUTE_REFUSED",
            "refused":true,
            "jobs_unchanged":true,
            "remotes_unchanged":true
        })
    );
    server.abort();
}

/// R13: re-anchor/recreate is an explicit NOT IMPLEMENTED refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r13_reanchor_not_implemented() {
    let (addr, state, server, tmp, parent_id, pair_id, bare) = refresh_pair_fixture().await;
    let svn_repo = tmp.path().join("svn-repo");
    let before = refresh_unchanged(&state, &parent_id, &pair_id, &bare, &svn_repo);
    let client = authed_client();
    for operation in ["reanchor", "recreate", "re-anchor"] {
        let response = client
            .post(format!("http://{addr}/api/repos/{pair_id}/refresh"))
            .json(&serde_json::json!({"operation": operation, "execute": true}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let payload: serde_json::Value = response.json().await.unwrap();
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{payload}");
        let error = payload["error"].as_str().unwrap_or("");
        assert!(error.contains("reanchor_not_implemented"), "{payload}");
        assert!(error.contains("NOT IMPLEMENTED"), "{payload}");
    }
    assert_eq!(
        refresh_unchanged(&state, &parent_id, &pair_id, &bare, &svn_repo),
        before
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R13_REANCHOR_NOT_IMPLEMENTED",
            "refused":true,
            "remotes_unchanged":true
        })
    );
    server.abort();
}

/// R13: a replaced mapped commit is not counted as new pair work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r13_rewritten_lineage_not_new_work() {
    let (addr, state, server, tmp, parent_id, pair_id, bare) = refresh_pair_fixture().await;
    let workdir = state
        .config
        .daemon
        .data_dir
        .join("repos")
        .join(&parent_id)
        .join("git-repo");
    git_cmd(
        &workdir,
        &[
            "fetch",
            "--no-tags",
            "--",
            "origin",
            "refs/heads/feature:refs/reposync/pair-refresh/keep",
        ],
    );
    let feature = bare_tip(&bare, "feature");
    state
        .db
        .conn()
        .execute(
            "INSERT INTO sync_records (id, repo_id, svn_rev, git_sha, direction, author, message, timestamp, synced_at, status)
             VALUES ('r13-mapped-feature', ?1, 2, ?2, 'svn_to_git', 'fixture', 'mapped', 't', 't', 'applied')",
            rusqlite::params![pair_id, feature],
        )
        .unwrap();
    let tree = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", &format!("{feature}^{{tree}}")])
            .current_dir(&workdir)
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let base = {
        let out = std::process::Command::new("git")
            .args(["rev-parse", &format!("{feature}^")])
            .current_dir(&workdir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let sibling = {
        let out = std::process::Command::new("git")
            .args([
                "commit-tree",
                &tree,
                "-p",
                &base,
                "-m",
                "rebased equivalent",
            ])
            .current_dir(&workdir)
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git_cmd(
        &workdir,
        &[
            "push",
            "--force",
            "origin",
            &format!("{sibling}:refs/heads/feature"),
        ],
    );
    let svn_repo = tmp.path().join("svn-repo");
    let before_refs = bare_refs(&bare);
    let before_rev = svn_youngest(&svn_repo);
    let before_jobs = durable_job_rows(&state);
    let client = authed_client();
    let response = client
        .post(format!("http://{addr}/api/repos/{pair_id}/refresh"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let plan: serde_json::Value = response.json().await.unwrap();
    assert!(status.is_success(), "{plan}");
    assert_eq!(plan["baseline"]["pair"]["git_sha"], feature, "{plan}");
    assert_eq!(plan["baseline"]["pair"]["evidence"], "applied_sync_record");
    assert_eq!(plan["git"]["pair_tip"], sibling);
    assert_eq!(plan["pending"]["pair_git"]["rewritten"], true, "{plan}");
    assert_eq!(plan["pending"]["pair_git"]["count"], 0);
    assert!(plan["pending"]["pair_git"]["shas"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        plan["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "rewritten_pair_lineage"),
        "{plan}"
    );
    assert_eq!(plan["intended_result"]["discards_unsynced_work"], false);
    assert!(plan["intended_result"]["summary"]
        .as_str()
        .unwrap()
        .contains("not discarded"));
    assert_eq!(bare_refs(&bare), before_refs);
    assert_eq!(svn_youngest(&svn_repo), before_rev);
    assert_eq!(durable_job_rows(&state), before_jobs);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R13_REWRITTEN_LINEAGE",
            "rewritten":true,
            "counted_as_new_work":false
        })
    );
    server.abort();
}

/// R13: the plan digest is stable for the same inputs and changes when a tip moves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r13_digest_binds_inputs() {
    let (addr, _state, server, tmp, _parent_id, pair_id, bare) = refresh_pair_fixture().await;
    let client = authed_client();
    let first: serde_json::Value = client
        .post(format!("http://{addr}/api/repos/{pair_id}/refresh"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let digest = first["plan_digest"].as_str().unwrap().to_string();
    let second: serde_json::Value = client
        .post(format!("http://{addr}/api/repos/{pair_id}/refresh"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second["plan_digest"], digest);
    push_commit_on(
        tmp.path(),
        &bare,
        "main",
        "moved.txt",
        "moved\n",
        "move parent tip",
    );
    let third: serde_json::Value = client
        .post(format!("http://{addr}/api/repos/{pair_id}/refresh"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(third["plan_digest"], digest, "{third}");
    assert_eq!(third["git"]["parent_tip"], bare_tip(&bare, "main"));
    assert_eq!(third["executed"], false);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"R13_DIGEST_BINDS_INPUTS",
            "stable":true,
            "changes_when_tip_moves":true
        })
    );
    server.abort();
}

/// #70: deleting the branch pair on screen replaces that history entry with the
/// parent (or the repository list) and does not keep polling the removed detail.
/// Failure variants are deterministic responses in the browser harness.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "browser fixture; run in e2e after npm ci and Chrome"]
async fn candidate_70_delete_viewed_pair_redirects_without_detail_polling() {
    use std::process::Command;

    let (addr, state, server, tmp) = build_test_server_full().await;
    let parent_id = "parent-repo";
    let viewed_id = "viewed-pair";
    let listed_id = "listed-pair";
    state
        .db
        .insert_repository(&fixture_branch_repo(
            parent_id,
            "Parent trunk",
            None,
            "main",
            "trunk",
        ))
        .unwrap();
    state
        .db
        .insert_repository(&fixture_branch_repo(
            viewed_id,
            "Viewed pair",
            Some(parent_id),
            "viewed-branch",
            "branches/viewed",
        ))
        .unwrap();
    state
        .db
        .insert_repository(&fixture_branch_repo(
            listed_id,
            "Listed pair",
            Some(parent_id),
            "listed-branch",
            "branches/listed",
        ))
        .unwrap();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("node")
        .arg(root.join("scripts/branch-pair-delete-browser.mjs"))
        .env("REPOSYNC_REAL_API", format!("http://{addr}"))
        .env("REPOSYNC_UI_TOKEN", TEST_TOKEN)
        .env("REPOSYNC_PARENT_ID", parent_id)
        .env("REPOSYNC_VIEWED_PAIR_ID", viewed_id)
        .env("REPOSYNC_LISTED_PAIR_ID", listed_id)
        .env("REPOSYNC_VIEWED_GIT_BRANCH", "viewed-branch")
        .env("REPOSYNC_LISTED_GIT_BRANCH", "listed-branch")
        .output()
        .expect("spawn branch-pair delete browser");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "branch-pair delete browser failed");
    assert!(state.db.get_repository(viewed_id).unwrap().is_none());
    assert!(state.db.get_repository(listed_id).unwrap().is_none());
    assert!(state.db.get_repository(parent_id).unwrap().is_some());
    server.abort();
    drop(tmp);
}

#[cfg(feature = "reliability-fixture")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_rs05_reconciliation_required_refuses_import_start() {
    let (addr, state, server, _tmp, id, _bare) = import_fixture().await;
    state
        .db
        .conn()
        .execute(
            "UPDATE repositories SET last_svn_rev=5, last_git_sha=?1, last_sync_at=datetime('now') WHERE id=?2",
            rusqlite::params![
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                id,
            ],
        )
        .unwrap();
    state
        .db
        .set_state(&format!("last_svn_rev_{id}"), "3")
        .unwrap();
    let client = authed_client();
    let status = client
        .get(format!("http://{addr}/api/repos/{id}/import/status"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(status["can_start"], false);
    let response = client
        .post(format!("http://{addr}/api/repos/{id}/import"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("not pending"),
        "expected import refusal for reconciliation_required repo: {body}"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case":"RS05_WEB_RECONCILIATION_REFUSES_IMPORT",
            "can_start":false,
            "status":400
        })
    );
    server.abort();
}

fn fixture_branch_repo(
    id: &str,
    name: &str,
    parent: Option<&str>,
    git_branch: &str,
    svn_branch: &str,
) -> reposync_core::models::Repository {
    let now = chrono::Utc::now().to_rfc3339();
    reposync_core::models::Repository {
        id: id.into(),
        name: name.into(),
        svn_url: "https://svn.test.invalid/repo".into(),
        svn_branch: svn_branch.into(),
        svn_username: "testuser".into(),
        git_provider: "github".into(),
        git_api_url: "https://git.test.invalid".into(),
        git_repo: "test/repo".into(),
        git_branch: git_branch.into(),
        sync_mode: "team".into(),
        poll_interval_secs: 60,
        lfs_threshold_mb: 1,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: parent.map(str::to_string),
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
    }
}

/// RS-16 / #65: import reconciliation_required blocks managed removal with HTTP 409.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_managed_remove_reconciliation_required_is_409() {
    use reposync_core::db::import_operations::ImportOperationState;

    let (addr, state, server, _tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let id = "r65-recon-root";
    state
        .db
        .insert_repository(&fixture_branch_repo(
            id,
            "Recon root",
            None,
            "main",
            "trunk",
        ))
        .unwrap();
    state
        .db
        .create_import_operation(id, "admin", "held-import", "fp")
        .unwrap();
    let active = state.db.active_import_operation(id).unwrap().unwrap();
    state
        .db
        .finish_import_operation(
            id,
            &active.id,
            ImportOperationState::ReconciliationRequired,
            "fixture held import for removal block",
        )
        .unwrap();

    let remove = client
        .post(format!("{base}/api/repos/{id}/remove"))
        .send()
        .await
        .unwrap();
    let status = remove.status();
    let body: serde_json::Value = remove.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{body}");
    assert_eq!(body["state"], "reconciliation_required");
    assert_eq!(body["action"], "managed_remove");
    assert_eq!(body["ok"], false);
    assert!(body["partial_cleanup"].is_object());
    assert_eq!(body["partial_cleanup"]["retry_is_local_cleanup_only"], true);
    let op_id = body["operation_id"].as_str().unwrap();

    let get = client
        .get(format!("{base}/api/repos/{id}/removal"))
        .send()
        .await
        .unwrap();
    let get_body: serde_json::Value = get.json().await.unwrap();
    assert_eq!(get_body["operation_id"], op_id);
    assert_eq!(get_body["state"], "reconciliation_required");
    assert!(state.db.get_repository(id).unwrap().is_some());
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "R65_REMOVE_RECONCILIATION",
            "http": 409,
            "operation_id_stable": true,
            "registration_kept": true
        })
    );
    server.abort();
}

/// RS-16: explicit branch-pair remote deletion opts default false; legacy omitted params stay destructive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_branch_pair_remote_deletion_explicit_false_defaults() {
    let (addr, state, server, _tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let parent_id = "bp-parent";
    let pair_id = "bp-pair";
    state
        .db
        .insert_repository(&fixture_branch_repo(
            parent_id, "Parent", None, "main", "trunk",
        ))
        .unwrap();
    state
        .db
        .insert_repository(&fixture_branch_repo(
            pair_id,
            "Pair",
            Some(parent_id),
            "feature",
            "branches/feature",
        ))
        .unwrap();

    let legacy = client
        .delete(format!("{base}/api/repos/{pair_id}/branch-pair"))
        .send()
        .await
        .unwrap();
    assert_eq!(legacy.status(), reqwest::StatusCode::OK);
    let legacy_body: serde_json::Value = legacy.json().await.unwrap();
    assert_eq!(legacy_body["remote_deletion"]["delete_git_requested"], true);
    assert_eq!(legacy_body["remote_deletion"]["delete_svn_requested"], true);
    assert_eq!(legacy_body["remote_deletion"]["explicit_opts"], false);
    assert!(state.db.get_repository(pair_id).unwrap().is_none());

    state
        .db
        .insert_repository(&fixture_branch_repo(
            pair_id,
            "Pair",
            Some(parent_id),
            "feature",
            "branches/feature",
        ))
        .unwrap();
    let explicit = client
        .delete(format!(
            "{base}/api/repos/{pair_id}/branch-pair?explicit_remote_deletion_opts=true&delete_git=false&delete_svn=false"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(explicit.status(), reqwest::StatusCode::OK);
    let explicit_body: serde_json::Value = explicit.json().await.unwrap();
    assert_eq!(explicit_body["remote_deletion"]["explicit_opts"], true);
    assert_eq!(
        explicit_body["remote_deletion"]["delete_git_requested"],
        false
    );
    assert_eq!(
        explicit_body["remote_deletion"]["delete_svn_requested"],
        false
    );
    assert!(
        state.db.get_repository(pair_id).unwrap().is_none(),
        "explicit safe UI query must remove the local branch-pair row"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "R65_BRANCH_REMOTE_OPTS",
            "legacy_defaults_destructive": true,
            "explicit_ui_defaults_safe": true
        })
    );
    server.abort();
}

/// RS-16: explicit POST disable matches legacy DELETE disable-only semantics.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_explicit_disable_matches_legacy_delete() {
    let (addr, state, server, _tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let explicit_id = "r65-disable-explicit";
    let legacy_id = "r65-disable-legacy";
    state
        .db
        .insert_repository(&fixture_branch_repo(
            explicit_id,
            "Disable explicit",
            None,
            "main",
            "trunk",
        ))
        .unwrap();
    state
        .db
        .insert_repository(&fixture_branch_repo(
            legacy_id,
            "Disable legacy",
            None,
            "main",
            "trunk",
        ))
        .unwrap();
    state
        .db
        .set_state(&format!("secret_svn_password_{explicit_id}"), "secret")
        .unwrap();
    state
        .db
        .set_state(&format!("secret_svn_password_{legacy_id}"), "secret")
        .unwrap();

    let disable = client
        .post(format!("{base}/api/repos/{explicit_id}/disable"))
        .send()
        .await
        .unwrap();
    assert_eq!(disable.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = disable.json().await.unwrap();
    assert_eq!(body["action"], "disable");
    assert_eq!(body["managed_removal"], false);
    assert_eq!(body["remote_git"], "untouched");
    assert_eq!(body["enabled"], false);
    assert!(
        !state
            .db
            .get_repository(explicit_id)
            .unwrap()
            .unwrap()
            .enabled
    );

    let legacy = client
        .delete(format!("{base}/api/repos/{legacy_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(legacy.status(), reqwest::StatusCode::OK);
    let legacy_body: serde_json::Value = legacy.json().await.unwrap();
    assert_eq!(legacy_body["action"], "disable");
    assert_eq!(legacy_body["enabled"], false);
    assert!(!state.db.get_repository(legacy_id).unwrap().unwrap().enabled);

    assert_eq!(
        state
            .db
            .get_repository(explicit_id)
            .unwrap()
            .unwrap()
            .enabled,
        state.db.get_repository(legacy_id).unwrap().unwrap().enabled
    );
    assert!(state.db.managed_removal(explicit_id).unwrap().is_none());
    assert!(state.db.managed_removal(legacy_id).unwrap().is_none());
    assert_eq!(
        state
            .db
            .get_state(&format!("secret_svn_password_{explicit_id}"))
            .unwrap()
            .as_deref(),
        Some("secret")
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "R65_EXPLICIT_DISABLE",
            "managed_journal": false,
            "enabled_matches_legacy_delete": true
        })
    );
    server.abort();
}

/// #65 dependency preview: parent removal refused with structured preview (HTTP 409).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_removal_preview_parent_blocked_with_children() {
    let (addr, state, server, _tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let parent_id = "r65-prev-parent";
    let child_id = "r65-prev-child";
    state
        .db
        .insert_repository(&fixture_branch_repo(
            parent_id,
            "Preview parent",
            None,
            "main",
            "trunk",
        ))
        .unwrap();
    state
        .db
        .insert_repository(&fixture_branch_repo(
            child_id,
            "Preview child",
            Some(parent_id),
            "feature",
            "branches/feature",
        ))
        .unwrap();

    let preview = client
        .get(format!("{base}/api/repos/{parent_id}/removal/preview"))
        .send()
        .await
        .unwrap();
    assert_eq!(preview.status(), reqwest::StatusCode::OK);
    let preview_body: serde_json::Value = preview.json().await.unwrap();
    assert_eq!(preview_body["action"], "removal_preview");
    assert_eq!(
        preview_body["dependency_preview"]["parent_removal_blocked"],
        true
    );
    assert_eq!(
        preview_body["dependency_preview"]["children"][0]["id"],
        child_id
    );

    let remove = client
        .post(format!("{base}/api/repos/{parent_id}/remove"))
        .send()
        .await
        .unwrap();
    assert_eq!(remove.status(), reqwest::StatusCode::CONFLICT);
    let remove_body: serde_json::Value = remove.json().await.unwrap();
    assert_eq!(remove_body["state"], "blocked");
    assert!(remove_body["dependency_preview"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == child_id));
    assert!(state.db.get_repository(parent_id).unwrap().unwrap().enabled);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case": "R65_REMOVAL_PREVIEW_PARENT", "http": 409, "preview": true})
    );
    server.abort();
}

/// #65: child removal must not delete a parent's per-repo credential.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_child_removal_preserves_parent_shared_credential() {
    let (addr, state, server, tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let parent_id = "r65-cred-parent";
    let child_id = "r65-cred-child";
    state
        .db
        .insert_repository(&fixture_branch_repo(
            parent_id,
            "Cred parent",
            None,
            "main",
            "trunk",
        ))
        .unwrap();
    state
        .db
        .insert_repository(&fixture_branch_repo(
            child_id,
            "Cred child",
            Some(parent_id),
            "feature",
            "branches/feature",
        ))
        .unwrap();
    state
        .db
        .set_state(&format!("secret_svn_password_{parent_id}"), "shared-secret")
        .unwrap();
    state
        .db
        .set_state(&format!("secret_svn_password_{child_id}"), "shared-secret")
        .unwrap();
    let data = state.config.daemon.data_dir.clone();
    std::fs::create_dir_all(data.join("repos").join(parent_id)).unwrap();
    std::fs::create_dir_all(data.join("repos").join(child_id)).unwrap();
    std::fs::write(data.join("repos").join(parent_id).join("keep.txt"), "p\n").unwrap();
    std::fs::write(data.join("repos").join(child_id).join("gone.txt"), "c\n").unwrap();

    let removed = client
        .post(format!("{base}/api/repos/{child_id}/remove"))
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), reqwest::StatusCode::OK);
    assert!(state.db.get_repository(child_id).unwrap().is_none());
    assert!(state.db.get_repository(parent_id).unwrap().is_some());
    assert_eq!(
        state
            .db
            .get_state(&format!("secret_svn_password_{parent_id}"))
            .unwrap()
            .as_deref(),
        Some("shared-secret")
    );
    assert!(state
        .db
        .get_state(&format!("secret_svn_password_{child_id}"))
        .unwrap()
        .is_none());
    assert!(!data.join("repos").join(child_id).exists());
    assert!(data.join("repos").join(parent_id).join("keep.txt").exists());

    let child2_id = "r65-cred-child2";
    state
        .db
        .insert_repository(&fixture_branch_repo(
            child2_id,
            "Cred child symlink",
            Some(parent_id),
            "feature2",
            "branches/feature2",
        ))
        .unwrap();
    let outside = tmp.path().join("outside-secret");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "safe\n").unwrap();
    let _ = std::os::unix::fs::symlink(&outside, data.join("repos").join(child2_id));
    let symlink_remove = client
        .post(format!("{base}/api/repos/{child2_id}/remove"))
        .send()
        .await
        .unwrap();
    assert_eq!(symlink_remove.status(), reqwest::StatusCode::CONFLICT);
    assert!(state.db.get_repository(child2_id).unwrap().is_some());
    assert_eq!(
        std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "safe\n"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case": "R65_CHILD_SHARED_CREDENTIAL", "parent_secret": "kept"})
    );
    server.abort();
}

/// #65: removing one child must not delete a sibling's owned local tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_child_removal_preserves_sibling_local_path() {
    let (addr, state, server, _tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let parent_id = "r65-sib-parent";
    let child_a = "r65-sib-a";
    let child_b = "r65-sib-b";
    state
        .db
        .insert_repository(&fixture_branch_repo(
            parent_id,
            "Sibling parent",
            None,
            "main",
            "trunk",
        ))
        .unwrap();
    state
        .db
        .insert_repository(&fixture_branch_repo(
            child_a,
            "Sibling A",
            Some(parent_id),
            "feature-a",
            "branches/a",
        ))
        .unwrap();
    state
        .db
        .insert_repository(&fixture_branch_repo(
            child_b,
            "Sibling B",
            Some(parent_id),
            "feature-b",
            "branches/b",
        ))
        .unwrap();
    let data = state.config.daemon.data_dir.clone();
    for id in [parent_id, child_a, child_b] {
        std::fs::create_dir_all(data.join("repos").join(id)).unwrap();
        std::fs::write(
            data.join("repos").join(id).join("data.txt"),
            format!("{id}\n"),
        )
        .unwrap();
    }

    let removed = client
        .post(format!("{base}/api/repos/{child_a}/remove"))
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), reqwest::StatusCode::OK);
    assert!(!data.join("repos").join(child_a).exists());
    assert_eq!(
        std::fs::read_to_string(data.join("repos").join(child_b).join("data.txt")).unwrap(),
        format!("{child_b}\n")
    );
    assert_eq!(
        std::fs::read_to_string(data.join("repos").join(parent_id).join("data.txt")).unwrap(),
        format!("{parent_id}\n")
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case": "R65_CHILD_SIBLING_PATH", "sibling_tree": "kept"})
    );
    server.abort();
}

/// #65: credential-chain inheritance appears in removal preview retained_for_repo_ids.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_credential_chain_inheritance_preview() {
    let (addr, state, server, _tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let parent_id = "r65-inherit-parent";
    let child_id = "r65-inherit-child";
    state
        .db
        .insert_repository(&fixture_branch_repo(
            parent_id,
            "Inherit parent",
            None,
            "main",
            "trunk",
        ))
        .unwrap();
    state
        .db
        .insert_repository(&fixture_branch_repo(
            child_id,
            "Inherit child",
            Some(parent_id),
            "feature",
            "branches/feature",
        ))
        .unwrap();
    state
        .db
        .set_state(&format!("secret_svn_password_{parent_id}"), "shared")
        .unwrap();
    let preview = client
        .get(format!("{base}/api/repos/{parent_id}/removal/preview"))
        .send()
        .await
        .unwrap();
    assert_eq!(preview.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = preview.json().await.unwrap();
    let creds = body["dependency_preview"]["credentials"]
        .as_array()
        .unwrap();
    let svn = creds
        .iter()
        .find(|c| c["key"] == format!("secret_svn_password_{parent_id}"))
        .expect("parent svn credential");
    let inheriting = svn["inheriting_repo_ids"].as_array().unwrap();
    assert!(
        inheriting.iter().any(|v| v == child_id),
        "child inheriting parent credential must be listed as inheriting: {svn}"
    );
    let retained = svn["retained_for_repo_ids"].as_array().unwrap();
    assert!(
        !retained.iter().any(|v| v == child_id),
        "inheriting child must not be listed as retaining its own key: {svn}"
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case": "R65_CREDENTIAL_CHAIN_INHERIT", "child_listed": true})
    );
    server.abort();
}

/// #65: blank Git remote must not produce shared registration entries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_blank_git_remote_not_shared_registration() {
    let (addr, state, server, _tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let blank_id = "r65-blank-remote";
    let mut blank = fixture_branch_repo(blank_id, "Blank remote", None, "main", "trunk");
    blank.git_api_url = String::new();
    blank.git_repo = String::new();
    state.db.insert_repository(&blank).unwrap();
    let mut blank2 = fixture_branch_repo("r65-blank-remote-2", "Blank2", None, "main", "trunk");
    blank2.git_api_url = String::new();
    blank2.git_repo = String::new();
    state.db.insert_repository(&blank2).unwrap();
    let preview = client
        .get(format!("{base}/api/repos/{blank_id}/removal/preview"))
        .send()
        .await
        .unwrap();
    assert_eq!(preview.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = preview.json().await.unwrap();
    assert!(body["dependency_preview"]["shared_git_registrations"]
        .as_array()
        .unwrap()
        .is_empty());
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case": "R65_BLANK_GIT_REMOTE", "shared": 0})
    );
    server.abort();
}

/// #65: restore completed managed removal while recovery metadata remains.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_restore_managed_registration() {
    let (addr, state, server, _tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let repo_id = "r65-restore-root";
    state
        .db
        .insert_repository(&fixture_branch_repo(
            repo_id,
            "Restore me",
            None,
            "main",
            "trunk",
        ))
        .unwrap();
    state
        .db
        .set_state(&format!("secret_git_token_{repo_id}"), "restore-token")
        .unwrap();
    state
        .db
        .set_state(&format!("last_git_sha_{repo_id}"), "inbound-checkpoint")
        .unwrap();
    state
        .db
        .conn()
        .execute(
            "UPDATE repositories SET last_git_sha=?1 WHERE id=?2",
            rusqlite::params!["emitted-tip", repo_id],
        )
        .unwrap();
    state
        .db
        .conn()
        .execute(
            "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
             VALUES (1, 'abc', 'svn_to_git', 't', 'a', 'b', ?1)",
            [repo_id],
        )
        .unwrap();
    let data = state.config.daemon.data_dir.clone();
    std::fs::create_dir_all(data.join("repos").join(repo_id)).unwrap();

    let removed = client
        .post(format!("{base}/api/repos/{repo_id}/remove"))
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), reqwest::StatusCode::OK);

    let restore = client
        .post(format!("{base}/api/repos/{repo_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(restore.status(), reqwest::StatusCode::OK);
    let restore_body: serde_json::Value = restore.json().await.unwrap();
    assert_eq!(restore_body["state"], "restored");
    let repo = state.db.get_repository(repo_id).unwrap().unwrap();
    assert!(!repo.enabled);
    assert!(repo.last_git_sha.is_empty());
    assert_eq!(
        state
            .db
            .resolve_credential_chain(repo_id, "secret_git_token")
            .as_deref(),
        Some("restore-token")
    );
    assert_eq!(
        state
            .db
            .get_state(&format!("last_git_sha_{repo_id}"))
            .unwrap(),
        None
    );
    assert!(state.db.removal_tombstone(repo_id).unwrap().is_none());

    let again = client
        .post(format!("{base}/api/repos/{repo_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), reqwest::StatusCode::OK);
    assert_eq!(
        again.json::<serde_json::Value>().await.unwrap()["state"],
        "already_listed"
    );

    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case": "R65_MANAGED_RESTORE", "enabled": false})
    );
    server.abort();
}

/// #65: removal preview surfaces active managed removal as HTTP 202 or 409.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_removal_preview_reports_202_or_409_for_active_removal() {
    use reposync_core::db::import_operations::ImportOperationState;

    let (addr, state, server, _tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let reconcile_id = "r65-preview-recon";
    let cancelling_id = "r65-preview-cancel";
    for (id, label) in [(reconcile_id, "Recon"), (cancelling_id, "Cancel")] {
        state
            .db
            .insert_repository(&fixture_branch_repo(id, label, None, "main", "trunk"))
            .unwrap();
    }
    state
        .db
        .create_import_operation(reconcile_id, "admin", "held-import", "fp")
        .unwrap();
    let active = state
        .db
        .active_import_operation(reconcile_id)
        .unwrap()
        .unwrap();
    state
        .db
        .finish_import_operation(
            reconcile_id,
            &active.id,
            ImportOperationState::ReconciliationRequired,
            "fixture held import",
        )
        .unwrap();
    state
        .db
        .create_import_operation(cancelling_id, "admin", "running-import", "fp2")
        .unwrap();

    client
        .post(format!("{base}/api/repos/{reconcile_id}/remove"))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/api/repos/{cancelling_id}/remove"))
        .send()
        .await
        .unwrap();

    let recon_preview = client
        .get(format!("{base}/api/repos/{reconcile_id}/removal/preview"))
        .send()
        .await
        .unwrap();
    assert_eq!(recon_preview.status(), reqwest::StatusCode::CONFLICT);
    let recon_body: serde_json::Value = recon_preview.json().await.unwrap();
    assert_eq!(
        recon_body["active_removal"]["state"],
        "reconciliation_required"
    );
    assert!(recon_body["dependency_preview"].is_object());

    let cancel_preview = client
        .get(format!("{base}/api/repos/{cancelling_id}/removal/preview"))
        .send()
        .await
        .unwrap();
    assert_eq!(cancel_preview.status(), reqwest::StatusCode::ACCEPTED);
    let cancel_body: serde_json::Value = cancel_preview.json().await.unwrap();
    assert_eq!(cancel_body["active_removal"]["state"], "cancelling");
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({
            "case": "R65_REMOVAL_PREVIEW_STATUS",
            "reconciliation_http": 409,
            "cancelling_http": 202
        })
    );
    server.abort();
}

/// #65: managed child removal records remote deletion outcomes and fails closed on error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_managed_remove_remote_deletion_outcomes() {
    let (addr, state, server, tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let parent_id = "r65-remote-parent";
    let child_id = "r65-remote-child";
    state
        .db
        .insert_repository(&fixture_branch_repo(
            parent_id,
            "Remote parent",
            None,
            "main",
            "trunk",
        ))
        .unwrap();
    state
        .db
        .insert_repository(&fixture_branch_repo(
            child_id,
            "Remote child",
            Some(parent_id),
            "feature",
            "branches/feature",
        ))
        .unwrap();
    state
        .db
        .set_state(&format!("secret_git_token_{child_id}"), "")
        .unwrap();
    state
        .db
        .set_state(&format!("secret_git_token_{parent_id}"), "parent-token")
        .unwrap();
    std::fs::create_dir_all(tmp.path().join("repos").join(child_id)).unwrap();

    let remove = client
        .post(format!(
            "{base}/api/repos/{child_id}/remove?explicit_remote_deletion_opts=true&delete_git=true&delete_svn=false"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(remove.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = remove.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert_eq!(body["state"], "failed");
    assert_eq!(body["remote_git"], "failed");
    assert_eq!(body["remote_svn"], "untouched");
    assert!(state.db.get_repository(child_id).unwrap().is_some());

    server.abort();
}

/// #65: missing parent during managed remote deletion must fail the operation (not hang Running).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_r65_managed_remove_missing_parent_fails_operation() {
    let (addr, state, server, _tmp) = build_test_server_full().await;
    let client = authed_client();
    let base = format!("http://{addr}");
    let child_id = "r65-orphan-child";
    let child = fixture_branch_repo(
        child_id,
        "Orphan",
        Some("r65-missing-parent"),
        "feature",
        "branches/feature",
    );
    state.db.insert_repository(&child).unwrap();
    std::fs::create_dir_all(state.config.daemon.data_dir.join("repos").join(child_id)).unwrap();

    let remove = client
        .post(format!(
            "{base}/api/repos/{child_id}/remove?explicit_remote_deletion_opts=true&delete_git=true&delete_svn=false"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(remove.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = remove.json().await.unwrap();
    assert_eq!(body["ok"], false);
    assert_eq!(body["state"], "failed");
    use reposync_core::db::managed_remove::ManagedRemoveState;
    let op = state.db.managed_removal(child_id).unwrap().unwrap();
    assert_eq!(op.state, ManagedRemoveState::Failed);
    assert!(state.db.get_repository(child_id).unwrap().is_some());

    server.abort();
}
