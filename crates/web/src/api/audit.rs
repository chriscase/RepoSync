//! Audit log API endpoints.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::auth::validate_session;
use crate::api::status::AppError;
use crate::AppState;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AuditQuery {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub page: Option<u32>,
    pub success: Option<bool>,
    pub action: Option<String>,
    pub repo_id: Option<String>,
}

#[derive(Serialize)]
struct AuditEntryView {
    id: i64,
    created_at: String,
    action: String,
    details: Option<String>,
    author: Option<String>,
    direction: Option<String>,
    svn_rev: Option<i64>,
    git_sha: Option<String>,
    success: bool,
    repo_id: Option<String>,
}

#[derive(Serialize)]
struct AuditListResponse {
    entries: Vec<AuditEntryView>,
    total: u64,
    page: Option<u64>,
    per_page: u32,
    total_pages: u64,
    offset: u64,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/audit", get(list_audit))
}

async fn list_audit(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Result<Json<AuditListResponse>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    if query.page == Some(0) || query.limit == Some(0) || query.repo_id.as_deref() == Some("") {
        return Err(AppError::BadRequest(
            "page/limit must be positive and repo_id must not be empty".into(),
        ));
    }
    let limit = query.limit.unwrap_or(50).min(500);
    // An explicit page takes precedence; legacy offset requests keep their actual offset in the envelope.
    let offset = query
        .page
        .map(|page| u64::from(page - 1) * u64::from(limit))
        .unwrap_or(u64::from(query.offset.unwrap_or(0)));
    let (entries, total) = state
        .db
        .list_audit_log_filtered(
            query.repo_id.as_deref(),
            query.success,
            query.action.as_deref().filter(|s| !s.is_empty()),
            limit,
            offset,
        )
        .map_err(|e| AppError::Internal(format!("database error: {e}")))?;
    let entries = entries
        .into_iter()
        .map(|e| AuditEntryView {
            id: e.id,
            created_at: e.created_at,
            action: e.action,
            details: e.details,
            author: e.author,
            direction: e.direction,
            svn_rev: e.svn_rev,
            git_sha: e.git_sha,
            success: e.success,
            repo_id: e.repo_id,
        })
        .collect();
    Ok(Json(AuditListResponse {
        entries,
        total,
        page: offset
            .is_multiple_of(u64::from(limit))
            .then_some(offset / u64::from(limit) + 1),
        per_page: limit,
        total_pages: total.div_ceil(u64::from(limit)),
        offset,
    }))
}
