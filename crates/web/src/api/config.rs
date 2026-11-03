//! Configuration API endpoints.

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::auth::validate_session;
use crate::api::status::AppError;
use crate::AppState;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ConfigResponse {
    daemon: DaemonConfigView,
    svn: SvnConfigView,
    github: GitHubConfigView,
    web: WebConfigView,
    sync: SyncConfigView,
}

#[derive(Serialize)]
struct DaemonConfigView {
    poll_interval_secs: u64,
    log_level: String,
    data_dir: String,
}

#[derive(Serialize)]
struct SvnConfigView {
    url: String,
    username: String,
    password: String, // redacted
    trunk_path: String,
}

#[derive(Serialize)]
struct GitHubConfigView {
    api_url: String,
    repo: String,
    token: String, // redacted
    default_branch: String,
}

#[derive(Serialize)]
struct WebConfigView {
    listen: String,
    auth_mode: String,
}

#[derive(Serialize)]
struct SyncConfigView {
    mode: String,
    auto_merge: bool,
    sync_tags: bool,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Identity mapping types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
struct AuthorMapping {
    svn_username: String,
    name: String,
    email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    github: Option<String>,
}

#[derive(Deserialize)]
struct UpdateMappingsRequest {
    mappings: Vec<AuthorMapping>,
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/config", get(get_config))
        .route(
            "/api/config/identity",
            get(get_identity_mappings).put(update_identity_mappings),
        )
        .route(
            "/api/config/notifications",
            get(get_notification_config).post(save_notification_config),
        )
        .route("/api/config/notifications/test", post(test_teams_notification))
}

async fn get_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<ConfigResponse>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let cfg = &state.config;

    Ok(Json(ConfigResponse {
        daemon: DaemonConfigView {
            poll_interval_secs: cfg.daemon.poll_interval_secs,
            log_level: cfg.daemon.log_level.clone(),
            data_dir: cfg.daemon.data_dir.display().to_string(),
        },
        svn: SvnConfigView {
            url: cfg.svn.url.clone(),
            username: cfg.svn.username.clone(),
            password: "***REDACTED***".into(),
            trunk_path: cfg.svn.trunk_path.clone(),
        },
        github: GitHubConfigView {
            api_url: cfg.github.api_url.clone(),
            repo: cfg.github.repo.clone(),
            token: "***REDACTED***".into(),
            default_branch: cfg.github.default_branch.clone(),
        },
        web: WebConfigView {
            listen: cfg.web.listen.clone(),
            auth_mode: format!("{:?}", cfg.web.auth_mode),
        },
        sync: SyncConfigView {
            mode: format!("{:?}", cfg.sync.mode),
            auto_merge: cfg.sync.auto_merge,
            sync_tags: cfg.sync.sync_tags,
        },
    }))
}

async fn get_identity_mappings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<AuthorMapping>>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;

    let value: Option<String> = db
        .get_state("identity_mappings")
        .map_err(|e| AppError::Internal(format!("db error: {}", e)))?;

    match value {
        Some(json_str) => {
            let mappings: Vec<AuthorMapping> = serde_json::from_str(&json_str)
                .map_err(|e| AppError::Internal(format!("parse identity mappings: {}", e)))?;
            Ok(Json(mappings))
        }
        None => Ok(Json(vec![])),
    }
}

async fn update_identity_mappings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<UpdateMappingsRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let json_str = serde_json::to_string(&body.mappings)
        .map_err(|e| AppError::Internal(format!("serialize mappings: {}", e)))?;

    let db = &state.db;

    db.set_state("identity_mappings", &json_str)
        .map_err(|e| AppError::Internal(format!("db error: {}", e)))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "count": body.mappings.len(),
    })))
}

// ---------------------------------------------------------------------------
// Notification config (Teams webhook)
// ---------------------------------------------------------------------------

async fn get_notification_config(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::api::auth::validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;
    let teams_url = db.get_state("teams_webhook_url").unwrap_or(None).unwrap_or_default();

    Ok(Json(serde_json::json!({
        "teams_webhook_url": teams_url,
    })))
}

#[derive(serde::Deserialize)]
struct SaveNotificationConfig {
    teams_webhook_url: Option<String>,
}

async fn save_notification_config(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<SaveNotificationConfig>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = crate::api::auth::validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;
    let url = body.teams_webhook_url.unwrap_or_default();
    db.set_state("teams_webhook_url", &url)
        .map_err(|e| AppError::Internal(format!("db error: {}", e)))?;

    tracing::info!(
        url_set = !url.is_empty(),
        "Teams notification webhook URL updated"
    );

    Ok(Json(serde_json::json!({
        "ok": true,
    })))
}

async fn test_teams_notification(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = crate::api::auth::validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;
    let url = db
        .get_state("teams_webhook_url")
        .unwrap_or(None)
        .unwrap_or_default();

    if url.is_empty() {
        return Err(AppError::BadRequest("No Teams webhook URL configured".into()));
    }

    let notifier = reposync_core::notify::teams::TeamsNotifier::new(url);
    notifier
        .send_test()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to send test notification: {}", e)))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "Test notification sent to Teams",
    })))
}
