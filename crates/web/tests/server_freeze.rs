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
    assert!(
        pair.status().is_success(),
        "late pair: {}",
        pair.text().await.unwrap()
    );
    let pair: serde_json::Value = pair.json().await.unwrap();
    assert_eq!(pair["last_git_sha"], feature_tip);
    assert!(pair["last_svn_rev"].as_i64().unwrap() > 0);
    let pair_id = pair["id"].as_str().unwrap();
    assert_eq!(state.db.get_repo_watermark(pair_id).unwrap().1, feature_tip);
    assert!(state.db.list_commit_map(100).unwrap().is_empty());
    let svn_tree = Command::new("svn")
        .args(["list", &target_url, "--non-interactive"])
        .output()
        .unwrap();
    assert!(svn_tree.status.success());
    assert!(!String::from_utf8_lossy(&svn_tree.stdout).contains("feature.txt"));
    eprintln!("EXPECTED BASELINE FAILURE R06 API: skip_import recorded provider ref {feature_tip} while disposable SVN target had no feature.txt and no mapping");

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

    let delete = client
        .delete(format!("{base}/api/repos/{id}"))
        .send()
        .await
        .unwrap();
    assert!(delete.status().is_success());
    let delete_body: serde_json::Value = delete.json().await.unwrap();
    assert_eq!(delete_body["message"], "repository disabled");
    assert!(!state.db.get_repository(id).unwrap().unwrap().enabled);
    assert!(state.db.get_repository(id).unwrap().is_some());
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
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
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
async fn candidate_64a_stalled_svn_info_child_and_descendant_are_stopped() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let descendant_stopped = |pid: &str| {
        if !Command::new("kill")
            .args(["-0", pid])
            .status()
            .unwrap()
            .success()
        {
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
    let descendant = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_string();
    assert!(Command::new("kill")
        .args(["-0", &descendant])
        .status()
        .unwrap()
        .success());
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
        while !descendant_stopped(&descendant) {
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
