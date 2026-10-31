//! Explicit v2 execution approval and durable request lookup. The legacy v1
//! preview continues to refuse execution.
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use reposync_core::pair_refresh::service::{
    RefreshAction, RefreshReceiptView, RefreshResponse, RefreshService,
};
use reposync_core::sync_engine::SyncEngine;

use crate::api::status::AppError;
use crate::AppState;

pub fn routes(engine: Arc<SyncEngine>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v2/repos/:id/refresh/review", post(review))
        .route(
            "/api/v2/repos/:id/refresh/review/candidate",
            post(review_candidate),
        )
        .route("/api/v2/repos/:id/refresh/prepare", post(prepare))
        .route(
            "/api/v2/repos/:id/refresh/recovery/prepare",
            post(prepare_recovery),
        )
        .route("/api/v2/repos/:id/refresh/action", post(act))
        .route("/api/v2/repos/:id/refresh/requests/:request", get(receipt))
        .layer(Extension(RefreshService::new(engine)))
}

/// Recovery-only surface: no new preparations or ordinary writer routes.
pub fn recovery_routes(service: Arc<RefreshService>) -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v2/repos/:id/refresh/recovery/prepare",
            post(prepare_recovery),
        )
        .route("/api/v2/repos/:id/refresh/action", post(act))
        .route("/api/v2/repos/:id/refresh/requests/:request", get(receipt))
        .layer(Extension(service))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareRequest {
    request_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Execute,
    Reconcile,
    Cancel,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionRequest {
    request_id: String,
    preparation_id: Uuid,
    approval_digest: String,
    action: Action,
}

fn authenticated_bearer(
    state: &AppState,
    service: &RefreshService,
    headers: &HeaderMap,
) -> Result<String, AppError> {
    service
        .require_engine(&state.sync_engine)
        .map_err(refused)?;
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty() && value.len() <= 4096)
        .ok_or_else(|| AppError::Unauthorized("current administrator session required".into()))?;
    state
        .sync_engine
        .capture_pair_refresh_actor_policy(bearer)
        .map_err(|_| AppError::Unauthorized("current administrator session required".into()))?;
    Ok(bearer.to_owned())
}

fn refused(error: anyhow::Error) -> AppError {
    AppError::Conflict(reposync_core::errors::redact_vcs_error_detail(
        &error.to_string(),
    ))
}

async fn prepare(
    State(state): State<Arc<AppState>>,
    Extension(service): Extension<Arc<RefreshService>>,
    headers: HeaderMap,
    Path(child): Path<String>,
    Json(body): Json<PrepareRequest>,
) -> Result<Json<RefreshResponse>, AppError> {
    let bearer = authenticated_bearer(&state, &service, &headers)?;
    service
        .prepare(bearer, child, body.request_id)
        .await
        .map(Json)
        .map_err(refused)
}

async fn prepare_recovery(
    State(state): State<Arc<AppState>>,
    Extension(service): Extension<Arc<RefreshService>>,
    headers: HeaderMap,
    Path(child): Path<String>,
    Json(body): Json<PrepareRequest>,
) -> Result<Json<RefreshResponse>, AppError> {
    let bearer = authenticated_bearer(&state, &service, &headers)?;
    service
        .prepare_recovery(bearer, child, body.request_id)
        .await
        .map(Json)
        .map_err(refused)
}

async fn act(
    State(state): State<Arc<AppState>>,
    Extension(service): Extension<Arc<RefreshService>>,
    headers: HeaderMap,
    Path(child): Path<String>,
    Json(body): Json<ActionRequest>,
) -> Result<Json<RefreshReceiptView>, AppError> {
    let bearer = authenticated_bearer(&state, &service, &headers)?;
    let action = match body.action {
        Action::Execute => RefreshAction::Execute,
        Action::Reconcile => RefreshAction::Reconcile,
        Action::Cancel => RefreshAction::Cancel,
    };
    service
        .act(
            bearer,
            &child,
            &body.request_id,
            body.preparation_id,
            body.approval_digest,
            action,
        )
        .await
        .map(Json)
        .map_err(refused)
}

async fn receipt(
    State(state): State<Arc<AppState>>,
    Extension(service): Extension<Arc<RefreshService>>,
    headers: HeaderMap,
    Path((child, request)): Path<(String, String)>,
) -> Result<Json<RefreshReceiptView>, AppError> {
    let bearer = authenticated_bearer(&state, &service, &headers)?;
    service
        .receipt(&bearer, &child, &request)
        .map_err(refused)?
        .map(Json)
        .ok_or_else(|| AppError::NotFound("refresh request not found".into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewRequest {}

async fn review(
    State(state): State<Arc<AppState>>,
    Extension(service): Extension<Arc<RefreshService>>,
    headers: HeaderMap,
    Path(child): Path<String>,
    Json(_body): Json<ReviewRequest>,
) -> Result<Json<reposync_core::pair_refresh::service::RefreshReviewResponse>, AppError> {
    let bearer = authenticated_bearer(&state, &service, &headers)?;
    service
        .review(bearer, child)
        .await
        .map(Json)
        .map_err(review_refused)
}

async fn review_candidate(
    State(state): State<Arc<AppState>>,
    Extension(service): Extension<Arc<RefreshService>>,
    headers: HeaderMap,
    Path(child): Path<String>,
    Json(body): Json<reposync_core::pair_refresh::service::RefreshReviewCandidateRequest>,
) -> Result<Json<reposync_core::pair_refresh::service::RefreshReviewResponse>, AppError> {
    let bearer = authenticated_bearer(&state, &service, &headers)?;
    service
        .review_candidate(bearer, child, body)
        .await
        .map(Json)
        .map_err(review_refused)
}

fn review_refused(error: anyhow::Error) -> AppError {
    use reposync_core::pair_refresh::service::RefreshReviewRefusal;
    let reason = error
        .downcast_ref::<RefreshReviewRefusal>()
        .copied()
        .unwrap_or(RefreshReviewRefusal::Unavailable);
    if matches!(reason, RefreshReviewRefusal::AuthorizationChanged) {
        AppError::Unauthorized(reason.code().into())
    } else {
        AppError::Conflict(reason.code().into())
    }
}

/// Test-only transport seam under the existing explicit fixture feature. The
/// supplied service still authenticates against the actual AppState engine and
/// captures registered sources; no observer/capability/result is injectable.
#[cfg(feature = "reliability-fixture")]
pub fn review_fixture_routes(service: Arc<RefreshService>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v2/repos/:id/refresh/review", post(review))
        .route(
            "/api/v2/repos/:id/refresh/review/candidate",
            post(review_candidate),
        )
        .route("/api/v2/repos/:id/refresh/prepare", post(prepare))
        .route(
            "/api/v2/repos/:id/refresh/recovery/prepare",
            post(prepare_recovery),
        )
        .route("/api/v2/repos/:id/refresh/action", post(act))
        .route("/api/v2/repos/:id/refresh/requests/:request", get(receipt))
        .layer(Extension(service))
}
