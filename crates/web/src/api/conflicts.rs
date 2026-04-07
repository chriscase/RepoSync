//! Conflict management API endpoints.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::auth::validate_session;
use crate::api::status::AppError;
use crate::AppState;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListConflictsQuery {
    pub per_page: Option<u32>,
    pub status: Option<String>,
    pub repo_id: Option<String>,
}

#[derive(Serialize)]
struct ConflictListItem {
    id: String,
    file_path: String,
    conflict_type: String,
    status: String,
    svn_revision: Option<i64>,
    git_hash: Option<String>,
    created_at: String,
    resolved_at: Option<String>,
    repo_id: Option<String>,
}

#[derive(Serialize)]
struct ConflictDetail {
    id: String,
    file_path: String,
    conflict_type: String,
    svn_content: Option<String>,
    git_content: Option<String>,
    base_content: Option<String>,
    svn_revision: Option<i64>,
    git_hash: Option<String>,
    status: String,
    resolution: Option<String>,
    resolved_by: Option<String>,
    created_at: String,
    resolved_at: Option<String>,
    repo_id: Option<String>,
}

#[derive(Deserialize)]
pub struct ResolveConflictRequest {
    pub resolution: String,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/conflicts", get(list_conflicts))
        .route("/api/conflicts/:id", get(get_conflict))
        .route("/api/conflicts/:id/resolve", post(resolve_conflict))
        .route("/api/conflicts/:id/defer", post(defer_conflict))
}

async fn list_conflicts(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<ListConflictsQuery>,
) -> Result<Json<Vec<ConflictListItem>>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let limit = query.per_page.unwrap_or(20).min(100);
    let status_filter = query.status.as_deref();
    let repo_filter = query.repo_id.as_deref();

    let db = &state.db;
    let entries = db
        .list_conflicts(status_filter, limit)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

    let items = entries
        .into_iter()
        .filter(|c| match repo_filter {
            Some(rid) => c.repo_id.as_deref() == Some(rid),
            None => true,
        })
        .map(|c| ConflictListItem {
            id: c.id,
            file_path: c.file_path,
            conflict_type: c.conflict_type,
            status: c.status,
            svn_revision: c.svn_rev,
            git_hash: c.git_sha,
            created_at: c.created_at,
            resolved_at: c.resolved_at,
            repo_id: c.repo_id,
        })
        .collect();

    Ok(Json(items))
}

async fn get_conflict(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ConflictDetail>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;
    let conflict = db
        .get_conflict(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound(format!("conflict '{}' not found", id)))?;

    Ok(Json(ConflictDetail {
        id: conflict.id,
        file_path: conflict.file_path,
        conflict_type: conflict.conflict_type,
        svn_content: conflict.svn_content,
        git_content: conflict.git_content,
        base_content: conflict.base_content,
        svn_revision: conflict.svn_rev,
        git_hash: conflict.git_sha,
        status: conflict.status,
        resolution: conflict.resolution,
        resolved_by: conflict.resolved_by,
        created_at: conflict.created_at,
        resolved_at: conflict.resolved_at,
        repo_id: conflict.repo_id,
    }))
}

async fn resolve_conflict(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ResolveConflictRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let resolution = match body.resolution.as_str() {
        "accept_svn" | "accept_git" | "custom" => body.resolution.as_str(),
        other => {
            return Err(AppError::BadRequest(format!(
                "invalid resolution '{}': must be accept_svn, accept_git, or custom",
                other
            )));
        }
    };

    let db = &state.db;
    db.resolve_conflict(&id, "resolved", resolution, "api")
        .map_err(|e| AppError::Internal(format!("failed to resolve conflict: {}", e)))?;

    // Broadcast update via WebSocket
    let update = serde_json::json!({
        "type": "conflict_resolved",
        "conflict_id": id,
        "resolution": body.resolution,
    });
    let _ = state.ws_broadcast.send(update.to_string());

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": format!("conflict {} resolved", id),
    })))
}

async fn defer_conflict(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;
    db.resolve_conflict(&id, "deferred", "deferred", "api")
        .map_err(|e| AppError::Internal(format!("failed to defer conflict: {}", e)))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": format!("conflict {} deferred", id),
    })))
}
