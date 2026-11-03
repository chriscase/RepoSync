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
    pub login_attempts:
        std::sync::Mutex<HashMap<String, (u32, std::time::Instant)>>,
    /// Handles for in-flight import tasks, for graceful shutdown.
    pub import_handles:
        tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
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
}

/// The web server.
pub struct WebServer {
    state: Arc<AppState>,
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
        });
        Self { state }
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

        // CORS: use configured origins, or derive from the listen address.
        let cors = {
            let origins = &self.state.config.web.cors_origins;
            let allow_origin = if origins.is_empty() {
                // Derive from listen address for dev convenience
                let origin = format!("http://{}", addr);
                tower_http::cors::AllowOrigin::exact(origin.parse().unwrap_or_else(|_| {
                    "http://localhost:3000".parse().unwrap()
                }))
            } else {
                let parsed: Vec<axum::http::HeaderValue> = origins
                    .iter()
                    .filter_map(|o| o.parse().ok())
                    .collect();
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
                        ).into_response(),
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
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .with_graceful_shutdown(async {
                // Wait until the process receives a shutdown signal.
                // The daemon's main.rs drops the web_handle or signals shutdown.
                tokio::signal::ctrl_c().await.ok();
            })
            .await?;

        Ok(())
    }
}
