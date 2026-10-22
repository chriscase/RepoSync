//! RepoSync web server and REST API.
//!
//! Provides an Axum-based HTTP server with:
//! - Status and health endpoints
//! - Conflict management API
//! - Configuration and identity mapping API
//! - Audit log API
//! - GitHub / SVN webhook receivers
//! - WebSocket endpoint for live updates
//! - Simple session-based authentication

pub mod api;
pub mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::Router;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::info;

use reposync_core::config::AppConfig;
use reposync_core::db::Database;
use reposync_core::import::ImportProgress;
use reposync_core::sync_engine::SyncEngine;

use std::collections::HashMap;

/// Shared application state accessible from all handlers.
pub struct AppState {
    pub db: Database,
    pub sync_engine: Arc<SyncEngine>,
    pub config: AppConfig,
    /// Channel for triggering immediate sync cycles.
    pub sync_trigger: tokio::sync::mpsc::Sender<()>,
    /// Broadcast channel for live WebSocket updates.
    pub ws_broadcast: broadcast::Sender<String>,
    /// Active sessions (token -> expiry timestamp).
    pub sessions:
        tokio::sync::RwLock<std::collections::HashMap<String, chrono::DateTime<chrono::Utc>>>,
    /// Import progress tracking (shared with background import task).
    pub import_progress: Arc<tokio::sync::RwLock<ImportProgress>>,
    /// Path to the TOML config file on disk.
    pub config_path: std::path::PathBuf,
    /// Previous network byte counters for rate calculation.
    pub prev_net_snapshot: std::sync::Mutex<Option<(u64, u64, std::time::Instant)>>,
    /// Per-repo import progress tracking (repo_id -> progress).
    pub repo_import_progress:
        tokio::sync::RwLock<HashMap<String, Arc<tokio::sync::RwLock<ImportProgress>>>>,
    /// Login attempt tracker for rate limiting (IP -> (count, window_start)).
    pub login_attempts: std::sync::Mutex<HashMap<String, (u32, std::time::Instant)>>,
    /// Handles for in-flight import tasks, for graceful shutdown.
    pub import_handles: tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Scheduler supervision owned by this state. `Arc` clones share the mutex.
    /// Health copies the value out and drops the guard before any await.
    pub scheduler_service: std::sync::Mutex<SchedulerServiceState>,
}

/// How the scheduler task ended, as seen from its `JoinHandle`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerTaskEnd {
    /// `run` returned. A clean shutdown only when a shutdown signal was requested.
    Completed,
    /// The task panicked.
    Panicked { message: String },
    /// The task failed without a panic payload (for example, it was cancelled).
    Failed { message: String },
}

impl SchedulerTaskEnd {
    pub fn from_join(result: Result<(), tokio::task::JoinError>) -> Self {
        match result {
            Ok(()) => Self::Completed,
            Err(err) if err.is_panic() => Self::Panicked {
                message: err.to_string(),
            },
            Err(err) => Self::Failed {
                message: err.to_string(),
            },
        }
    }
}

/// Service level exposed to `/api/status/health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerServiceLevel {
    Running,
    Degraded,
    Fatal,
}

/// Shared scheduler supervision for one `AppState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerServiceState {
    pub level: SchedulerServiceLevel,
    pub reason: Option<String>,
}

impl SchedulerServiceState {
    pub fn running() -> Self {
        Self {
            level: SchedulerServiceLevel::Running,
            reason: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.level == SchedulerServiceLevel::Running
    }
}

/// Failure to record when the scheduler `JoinHandle` resolves.
///
/// A requested shutdown that returns `Completed` is not a failure. Any other
/// end (exit before shutdown, panic, or join error), including a panic while
/// shutting down, is fatal: the scheduler is no longer running.
pub fn scheduler_failure_from_task_end(
    shutdown_requested: bool,
    end: &SchedulerTaskEnd,
) -> Option<(SchedulerServiceLevel, String)> {
    match (shutdown_requested, end) {
        (true, SchedulerTaskEnd::Completed) => None,
        (true, SchedulerTaskEnd::Panicked { message }) => Some((
            SchedulerServiceLevel::Fatal,
            format!("scheduler task panicked during shutdown: {message}"),
        )),
        (true, SchedulerTaskEnd::Failed { message }) => Some((
            SchedulerServiceLevel::Fatal,
            format!("scheduler task failed during shutdown: {message}"),
        )),
        (false, SchedulerTaskEnd::Completed) => Some((
            SchedulerServiceLevel::Fatal,
            "scheduler task exited before shutdown".into(),
        )),
        (false, SchedulerTaskEnd::Panicked { message }) => Some((
            SchedulerServiceLevel::Fatal,
            format!("scheduler task panicked: {message}"),
        )),
        (false, SchedulerTaskEnd::Failed { message }) => Some((
            SchedulerServiceLevel::Fatal,
            format!("scheduler task failed: {message}"),
        )),
    }
}

/// How long shutdown waits for the scheduler task to finish its current cycle.
///
/// Expiry is fatal (`scheduler_failure_from_stop_timeout`). This budget is
/// not a liveness limit on poll intervals: a 601s or 3600s poll that is
/// still inside `run` before shutdown is requested stays running.
pub const SCHEDULER_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The scheduler `JoinHandle` was still pending when [`SCHEDULER_STOP_TIMEOUT`]
/// elapsed after shutdown was requested.
pub fn scheduler_failure_from_stop_timeout() -> (SchedulerServiceLevel, String) {
    (
        SchedulerServiceLevel::Fatal,
        format!(
            "scheduler did not stop within {}s of shutdown",
            SCHEDULER_STOP_TIMEOUT.as_secs()
        ),
    )
}

impl AppState {
    /// Get or create the import progress tracker for a specific repository.
    pub async fn get_repo_import_progress(
        &self,
        repo_id: &str,
    ) -> Arc<tokio::sync::RwLock<ImportProgress>> {
        {
            let map = self.repo_import_progress.read().await;
            if let Some(progress) = map.get(repo_id) {
                return progress.clone();
            }
        }
        let mut map = self.repo_import_progress.write().await;
        // Double-check after acquiring write lock.
        map.entry(repo_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(ImportProgress::default())))
            .clone()
    }

    /// Record a degraded or fatal scheduler outcome for this state.
    ///
    /// `Arc` clones share `scheduler_service`. `Running` restores the initial
    /// value, including a cleared reason.
    pub fn record_scheduler_failure(
        &self,
        level: SchedulerServiceLevel,
        reason: impl Into<String>,
    ) {
        let mut service = self
            .scheduler_service
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if level == SchedulerServiceLevel::Running {
            *service = SchedulerServiceState::running();
            return;
        }
        *service = SchedulerServiceState {
            level,
            reason: Some(reason.into()),
        };
    }

    /// Current scheduler supervision. The initial value is running.
    pub fn scheduler_service_state(&self) -> SchedulerServiceState {
        self.scheduler_service
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// The web server.
pub struct WebServer {
    state: Arc<AppState>,
    recovery_service: Option<Arc<reposync_core::pair_refresh::service::RefreshService>>,
}

impl WebServer {
    /// Create a new web server with the given dependencies.
    pub fn new(
        config: AppConfig,
        db: Database,
        sync_engine: Arc<SyncEngine>,
        sync_trigger: tokio::sync::mpsc::Sender<()>,
        config_path: std::path::PathBuf,
        import_progress: Arc<tokio::sync::RwLock<ImportProgress>>,
    ) -> Self {
        db.hold_interrupted_imports()
            .expect("cannot safely start while import operation recovery state is unwritable");
        db.hold_interrupted_svn_commits()
            .expect("cannot safely start while git-to-svn commit recovery state is unwritable");
        db.hold_interrupted_git_pushes()
            .expect("cannot safely start while svn-to-git push recovery state is unwritable");
        db.hold_interrupted_team_cycle_mappings()
            .expect("cannot safely start while team cycle mapping recovery state is unwritable");
        Self::from_dependencies(
            config,
            db,
            sync_engine,
            sync_trigger,
            config_path,
            import_progress,
        )
    }

    fn from_dependencies(
        config: AppConfig,
        db: Database,
        sync_engine: Arc<SyncEngine>,
        sync_trigger: tokio::sync::mpsc::Sender<()>,
        config_path: std::path::PathBuf,
        import_progress: Arc<tokio::sync::RwLock<ImportProgress>>,
    ) -> Self {
        let (ws_tx, _) = broadcast::channel(256);
        let state = Arc::new(AppState {
            db,
            sync_engine,
            config,
            sync_trigger,
            ws_broadcast: ws_tx,
            sessions: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            import_progress,
            config_path,
            prev_net_snapshot: std::sync::Mutex::new(None),
            repo_import_progress: tokio::sync::RwLock::new(HashMap::new()),
            login_attempts: std::sync::Mutex::new(HashMap::new()),
            import_handles: tokio::sync::Mutex::new(Vec::new()),
            scheduler_service: std::sync::Mutex::new(SchedulerServiceState::running()),
        });
        Self {
            state,
            recovery_service: None,
        }
    }

    /// Dedicated recovery serving does not run ordinary journal startup or
    /// expose configuration, import, synchronization or repository mutations.
    pub fn for_pair_refresh_recovery(
        config: AppConfig,
        db: Database,
        sync_engine: Arc<SyncEngine>,
        config_path: std::path::PathBuf,
        lock: Arc<reposync_core::data_dir::LockGuard>,
    ) -> anyhow::Result<Self> {
        let service = reposync_core::pair_refresh::service::RefreshService::recovery_only(
            sync_engine.clone(),
            lock,
        )?;
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        let mut server = Self::from_dependencies(
            config,
            db,
            sync_engine,
            sender,
            config_path,
            Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        );
        server.recovery_service = Some(service);
        Ok(server)
    }

    /// Get a clone of the shared application state.
    pub fn app_state(&self) -> Arc<AppState> {
        self.state.clone()
    }

    /// Get a clone of the broadcast sender for pushing events.
    pub fn broadcast_sender(&self) -> broadcast::Sender<String> {
        self.state.ws_broadcast.clone()
    }

    /// Start the web server, listening on the given address.
    pub async fn start(self, listen_addr: &str) -> anyhow::Result<()> {
        let addr: SocketAddr = listen_addr.parse()?;
        if let Some(service) = self.recovery_service {
            let app = Router::new()
                .merge(api::auth::routes())
                .merge(api::pair_refresh::recovery_routes(service.clone()))
                .layer(DefaultBodyLimit::max(16 * 1024))
                .layer(TraceLayer::new_for_http())
                .with_state(self.state);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            info!(addr = %addr, "serving authenticated pair refresh recovery");
            let result = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                tokio::signal::ctrl_c().await.ok();
            })
            .await;
            service.shutdown().await;
            result?;
            return Ok(());
        }

        // CORS: use configured origins, or derive from the listen address.
        let cors = {
            let origins = &self.state.config.web.cors_origins;
            let allow_origin = if origins.is_empty() {
                // Derive from listen address for dev convenience
                let origin = format!("http://{}", addr);
                tower_http::cors::AllowOrigin::exact(
                    origin
                        .parse()
                        .unwrap_or_else(|_| "http://localhost:3000".parse().unwrap()),
                )
            } else {
                let parsed: Vec<axum::http::HeaderValue> =
                    origins.iter().filter_map(|o| o.parse().ok()).collect();
                tower_http::cors::AllowOrigin::list(parsed)
            };
            CorsLayer::new()
                .allow_origin(allow_origin)
                .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
                .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
        };

        // Serve the React SPA from the "static" directory next to the binary.
        // All unknown routes fall back to index.html for client-side routing.
        let static_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.join("static")))
            .unwrap_or_else(|| std::path::PathBuf::from("static"));
        let index_html_path = static_dir.join("index.html");
        // ServeDir serves real files (JS, CSS, images). For any path that
        // doesn't match a file, the fallback handler returns index.html with
        // status 200 so client-side routing works correctly.
        let serve_static = ServeDir::new(&static_dir);
        let spa_fallback = {
            let index_path = index_html_path.clone();
            axum::routing::get(move |_uri: Uri| {
                let path = index_path.clone();
                async move {
                    match tokio::fs::read(&path).await {
                        Ok(contents) => (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            contents,
                        )
                            .into_response(),
                        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                }
            })
        };
        let serve_spa = serve_static.fallback(spa_fallback);

        let app = Router::new()
            // API routes
            .merge(api::status::routes())
            .merge(api::conflicts::routes())
            .merge(api::config::routes())
            .merge(api::auth::routes())
            .merge(api::audit::routes())
            .merge(api::sync_history::routes())
            .merge(api::seed::routes())
            .merge(api::webhooks::routes())
            .merge(api::setup::routes())
            .merge(api::users::routes())
            .merge(api::repos::routes())
            .merge(api::pair_refresh::routes(self.state.sync_engine.clone()))
            // WebSocket
            .merge(ws::routes())
            // React SPA (fallback for everything else)
            .fallback_service(serve_spa)
            // Middleware
            .layer(DefaultBodyLimit::max(2 * 1024 * 1024)) // 2 MB max request body
            .layer(TraceLayer::new_for_http())
            .layer(cors)
            .with_state(self.state);

        info!(addr = %addr, "starting web server");

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            // Wait until the process receives a shutdown signal.
            // The daemon's main.rs drops the web_handle or signals shutdown.
            tokio::signal::ctrl_c().await.ok();
        })
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod scheduler_service_tests {
    use super::*;

    #[test]
    fn task_end_before_shutdown_is_fatal() {
        let (level, reason) = scheduler_failure_from_task_end(false, &SchedulerTaskEnd::Completed)
            .expect("early exit");
        assert_eq!(level, SchedulerServiceLevel::Fatal);
        assert_eq!(reason, "scheduler task exited before shutdown");

        let (level, reason) = scheduler_failure_from_task_end(
            false,
            &SchedulerTaskEnd::Panicked {
                message: "boom".into(),
            },
        )
        .expect("panic");
        assert_eq!(level, SchedulerServiceLevel::Fatal);
        assert!(reason.contains("panicked"));
        assert!(reason.contains("boom"));

        let (level, reason) = scheduler_failure_from_task_end(
            false,
            &SchedulerTaskEnd::Failed {
                message: "cancelled".into(),
            },
        )
        .expect("join error");
        assert_eq!(level, SchedulerServiceLevel::Fatal);
        assert!(reason.contains("cancelled"));
    }

    #[test]
    fn stop_timeout_expiry_is_fatal() {
        assert_eq!(SCHEDULER_STOP_TIMEOUT, std::time::Duration::from_secs(10));
        let (level, reason) = scheduler_failure_from_stop_timeout();
        assert_eq!(level, SchedulerServiceLevel::Fatal);
        assert_eq!(reason, "scheduler did not stop within 10s of shutdown");
    }

    #[test]
    fn completed_shutdown_is_not_a_failure() {
        assert!(scheduler_failure_from_task_end(true, &SchedulerTaskEnd::Completed).is_none());
        let (level, reason) = scheduler_failure_from_task_end(
            true,
            &SchedulerTaskEnd::Panicked {
                message: "boom".into(),
            },
        )
        .expect("panic during shutdown");
        assert_eq!(level, SchedulerServiceLevel::Fatal);
        assert!(reason.contains("during shutdown"));
    }

    fn supervision_fixture(dir: &std::path::Path) -> AppState {
        std::fs::create_dir_all(dir).unwrap();
        let git_path = dir.join("git");
        git2::Repository::init(&git_path).unwrap();
        let toml = format!(
            "[daemon]\ndata_dir = {:?}\n[svn]\nurl = 'https://svn.test.invalid/repo'\nusername = 'fixture'\npassword_env = ''\n[github]\nrepo = 'test/repo'\ntoken_env = ''\n",
            dir.display().to_string()
        );
        let config: AppConfig = toml::from_str(&toml).unwrap();
        let engine = SyncEngine::new(
            config.clone(),
            Database::in_memory().unwrap(),
            reposync_core::svn::SvnClient::new("https://svn.test.invalid/repo", "fixture", ""),
            reposync_core::git::GitClient::new(&git_path).unwrap(),
            Arc::new(
                reposync_core::identity::IdentityMapper::new(
                    &reposync_core::config::IdentityConfig::default(),
                )
                .unwrap(),
            ),
        );
        let (sync_trigger, _rx) = tokio::sync::mpsc::channel(1);
        let (ws_broadcast, _) = broadcast::channel(8);
        AppState {
            db: Database::in_memory().unwrap(),
            sync_engine: Arc::new(engine),
            config,
            sync_trigger,
            ws_broadcast,
            sessions: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
            config_path: dir.join("config.toml"),
            prev_net_snapshot: std::sync::Mutex::new(None),
            repo_import_progress: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            login_attempts: std::sync::Mutex::new(std::collections::HashMap::new()),
            import_handles: tokio::sync::Mutex::new(Vec::new()),
            scheduler_service: std::sync::Mutex::new(SchedulerServiceState::running()),
        }
    }

    #[test]
    fn service_records_are_isolated_per_state() {
        let dir = tempfile::tempdir().unwrap();
        let fatal = Arc::new(supervision_fixture(&dir.path().join("fatal")));
        let degraded = supervision_fixture(&dir.path().join("degraded"));
        assert!(fatal.scheduler_service_state().is_running());
        assert!(degraded.scheduler_service_state().is_running());

        Arc::clone(&fatal)
            .record_scheduler_failure(SchedulerServiceLevel::Fatal, "scheduler task panicked");
        degraded.record_scheduler_failure(SchedulerServiceLevel::Degraded, "sync lagging");

        let fatal_state = fatal.scheduler_service_state();
        assert_eq!(fatal_state.level, SchedulerServiceLevel::Fatal);
        assert_eq!(
            fatal_state.reason.as_deref(),
            Some("scheduler task panicked")
        );
        let degraded_state = degraded.scheduler_service_state();
        assert_eq!(degraded_state.level, SchedulerServiceLevel::Degraded);
        assert_eq!(degraded_state.reason.as_deref(), Some("sync lagging"));

        fatal.record_scheduler_failure(SchedulerServiceLevel::Running, "");
        assert!(fatal.scheduler_service_state().is_running());
        assert!(fatal.scheduler_service_state().reason.is_none());
        assert_eq!(
            degraded.scheduler_service_state().level,
            SchedulerServiceLevel::Degraded
        );
        drop(degraded);
        assert!(fatal.scheduler_service_state().is_running());
    }

    #[tokio::test]
    async fn join_panic_is_classified_as_panicked() {
        let handle = tokio::spawn(async { panic!("scheduler-boom") });
        match SchedulerTaskEnd::from_join(handle.await) {
            SchedulerTaskEnd::Panicked { message } => {
                assert!(message.contains("panic"), "{message}");
            }
            other => panic!("expected a panicked join, got {other:?}"),
        }
    }
}
