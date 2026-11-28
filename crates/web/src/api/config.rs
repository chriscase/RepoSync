//! Configuration API endpoints.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::auth::validate_session_with_role;
use crate::api::status::AppError;
use crate::AppState;
use reposync_core::identity::mapping_file::{AuthorEntry, MappingFile};

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
        .route(
            "/api/config/notifications/test",
            post(test_teams_notification),
        )
}

async fn require_admin(state: &Arc<AppState>, authorization: Option<&str>) -> Result<(), AppError> {
    let (_, role) = validate_session_with_role(state, authorization).await?;
    if role != "admin" {
        return Err(AppError::Forbidden("admin access required".into()));
    }
    Ok(())
}

async fn get_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<ConfigResponse>, AppError> {
    require_admin(
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

/// The mappings the identity mapper reads: the entries of the mapping file
/// the server writes. The GitHub login, which that file does not hold, comes
/// from the copy kept in the database. Before that file exists, the list
/// saved in the database by earlier versions is returned, so saving it again
/// writes it to the file.
async fn get_identity_mappings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<AuthorMapping>>, AppError> {
    require_admin(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let saved = saved_mappings(&state)?;
    let target = crate::api::setup::setup_mapping_target(&state);
    if !target.exists() {
        return Ok(Json(saved));
    }
    let entries = MappingFile::load(&target).map_err(|e| {
        AppError::Internal(format!(
            "could not read identity mapping file {}: {}",
            target.display(),
            e
        ))
    })?;
    let github: HashMap<String, String> = saved
        .into_iter()
        .filter_map(|m| m.github.map(|g| (m.svn_username, g)))
        .collect();
    let mut mappings: Vec<AuthorMapping> = entries
        .into_iter()
        .map(|(svn_username, entry)| AuthorMapping {
            github: github.get(&svn_username).cloned(),
            svn_username,
            name: entry.name,
            email: entry.email,
        })
        .collect();
    mappings.sort_by(|a, b| a.svn_username.cmp(&b.svn_username));
    Ok(Json(mappings))
}

fn saved_mappings(state: &AppState) -> Result<Vec<AuthorMapping>, AppError> {
    match state
        .db
        .get_state("identity_mappings")
        .map_err(|e| AppError::Internal(format!("db error: {}", e)))?
    {
        Some(json_str) => serde_json::from_str(&json_str)
            .map_err(|e| AppError::Internal(format!("parse identity mappings: {}", e))),
        None => Ok(vec![]),
    }
}

/// Replace the identity mappings. The list is written to the mapping file the
/// identity mapper reads, which picks up the change on its next lookup.
async fn update_identity_mappings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<UpdateMappingsRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let mut authors: HashMap<String, AuthorEntry> = HashMap::new();
    for m in &body.mappings {
        if m.svn_username.trim().is_empty() || m.name.trim().is_empty() || m.email.trim().is_empty()
        {
            return Err(AppError::BadRequest(
                "each mapping needs an SVN username, a name and an email".into(),
            ));
        }
        let entry = AuthorEntry {
            name: m.name.clone(),
            email: m.email.clone(),
        };
        if authors.insert(m.svn_username.clone(), entry).is_some() {
            return Err(AppError::BadRequest(format!(
                "SVN username '{}' is mapped more than once",
                m.svn_username
            )));
        }
    }

    let target = crate::api::setup::setup_mapping_target(&state);
    {
        let _guard = crate::api::setup::MAPPING_WRITE
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // An empty list on a server with no mapping file has nothing to write.
        if !(authors.is_empty() && !target.exists()) {
            MappingFile::save(&target, &authors).map_err(|e| {
                AppError::Internal(format!(
                    "could not write identity mapping file {}: {}",
                    target.display(),
                    e
                ))
            })?;
        }
    }

    let json_str = serde_json::to_string(&body.mappings)
        .map_err(|e| AppError::Internal(format!("serialize mappings: {}", e)))?;
    state
        .db
        .set_state("identity_mappings", &json_str)
        .map_err(|e| AppError::Internal(format!("db error: {}", e)))?;

    let mut warnings = Vec::new();
    if state.config.identity.mapping_file.is_none() && !authors.is_empty() {
        warnings.push(format!(
            "Identity mappings were saved to {}, but they take effect only once \
             identity.mapping_file in the server configuration points to that file.",
            target.display()
        ));
    }
    Ok(Json(serde_json::json!({
        "ok": true,
        "count": body.mappings.len(),
        "warnings": warnings,
    })))
}

// ---------------------------------------------------------------------------
// Notification config (Teams webhook)
// ---------------------------------------------------------------------------

async fn get_notification_config(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    require_admin(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;
    let teams_url = db
        .get_state("teams_webhook_url")
        .unwrap_or(None)
        .unwrap_or_default();

    // Notifications go to the server configuration's URL when it sets one, so
    // say so; the URL itself is not returned.
    let server_config_url_set = state
        .config
        .notifications
        .teams_webhook_url
        .as_deref()
        .is_some_and(|u| !u.trim().is_empty());

    Ok(Json(serde_json::json!({
        "teams_webhook_url": teams_url,
        "server_config_url_set": server_config_url_set,
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
        return Err(AppError::Forbidden("admin access required".into()));
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
        return Err(AppError::Forbidden("admin access required".into()));
    }

    // The same URL notifications go to: the server configuration's, if set,
    // otherwise the one saved here.
    let url = state
        .config
        .notifications
        .teams_webhook_url
        .clone()
        .filter(|u| !u.trim().is_empty())
        .or_else(|| state.db.get_state("teams_webhook_url").unwrap_or(None))
        .unwrap_or_default();

    if url.trim().is_empty() {
        return Err(AppError::BadRequest(
            "No Teams webhook URL configured".into(),
        ));
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
