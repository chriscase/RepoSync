//! Repository management API endpoints (multi-repo support).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use reposync_core::db::git_push_operations::GitPushOperationState;
use reposync_core::db::import_operations::{
    import_target_fingerprint, ImportOperation, ImportOperationState, SnapshotPin,
};
use reposync_core::db::queries::AuditLogInput;
use reposync_core::db::svn_commit_operations::SvnCommitOperationState;
use reposync_core::db::Database;
use reposync_core::errors::DatabaseError;
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::{self, ImportConfig, ImportPhase, ImportProgress, ImportRunState};
use reposync_core::late_pair::{
    collect_verified_mappings, evaluate_admission, probe_svn_target, LatePairRequest,
};
use reposync_core::pair_refresh::{
    analyze_git_preview, branch_svn_url, build_preview, execute_refusal, execution_requested,
    format_refusal, parse_operation, reanchor_refusal, svn_path_missing, GitLayout,
    RefreshObservations, RefreshOperation,
};
use reposync_core::skip_commit::{
    build_skip_context, execute_exact_skip, reason as skip_reason, SkipCommitRequest,
};
use reposync_core::svn::SvnClient;

use crate::api::auth::{validate_session, validate_session_with_role};
use crate::api::status::AppError;
use crate::AppState;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateRepoRequest {
    name: String,
    svn_url: String,
    #[serde(default)]
    svn_branch: String,
    #[serde(default)]
    svn_username: String,
    #[serde(default = "default_github")]
    git_provider: String,
    #[serde(default)]
    git_api_url: String,
    #[serde(default)]
    git_repo: String,
    #[serde(default = "default_main")]
    git_branch: String,
    #[serde(default = "default_direct")]
    sync_mode: String,
    #[serde(default = "default_60")]
    poll_interval_secs: i64,
    #[serde(default)]
    lfs_threshold_mb: i64,
    #[serde(default = "default_true")]
    auto_merge: bool,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_github() -> String {
    "github".to_string()
}

fn default_main() -> String {
    "main".to_string()
}

fn default_direct() -> String {
    "direct".to_string()
}

fn default_60() -> i64 {
    60
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct UpdateRepoRequest {
    name: Option<String>,
    svn_url: Option<String>,
    svn_branch: Option<String>,
    svn_username: Option<String>,
    git_provider: Option<String>,
    git_api_url: Option<String>,
    git_repo: Option<String>,
    git_branch: Option<String>,
    sync_mode: Option<String>,
    poll_interval_secs: Option<i64>,
    lfs_threshold_mb: Option<i64>,
    auto_merge: Option<bool>,
    enabled: Option<bool>,
    allowed_paths: Option<String>,
    blocked_patterns: Option<String>,
}

#[derive(Serialize)]
struct RepoSummary {
    id: String,
    name: String,
    parent_id: Option<String>,
    svn_url: String,
    svn_branch: String,
    git_provider: String,
    git_repo: String,
    git_branch: String,
    sync_mode: String,
    enabled: bool,
    created_at: String,
    updated_at: String,
    /// Current sync status label, if available.
    status: String,
}

#[derive(Serialize)]
struct RepoDetail {
    id: String,
    name: String,
    parent_id: Option<String>,
    svn_url: String,
    svn_branch: String,
    svn_username: String,
    git_provider: String,
    git_api_url: String,
    git_repo: String,
    git_branch: String,
    sync_mode: String,
    poll_interval_secs: i64,
    lfs_threshold_mb: i64,
    auto_merge: bool,
    enabled: bool,
    created_by: Option<String>,
    created_at: String,
    updated_at: String,
    /// Current sync status label, if available.
    status: String,
    /// True until a verified baseline mapping is recorded.
    initializing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    import_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    starting_revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    history_boundary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_pin: Option<SnapshotPin>,
}

impl From<reposync_core::models::Repository> for RepoDetail {
    fn from(r: reposync_core::models::Repository) -> Self {
        Self {
            id: r.id,
            name: r.name,
            parent_id: r.parent_id,
            svn_url: r.svn_url,
            svn_branch: r.svn_branch,
            svn_username: r.svn_username,
            git_provider: r.git_provider,
            git_api_url: r.git_api_url,
            git_repo: r.git_repo,
            git_branch: r.git_branch,
            sync_mode: r.sync_mode,
            poll_interval_secs: r.poll_interval_secs,
            lfs_threshold_mb: r.lfs_threshold_mb,
            auto_merge: r.auto_merge,
            enabled: r.enabled,
            created_by: r.created_by,
            created_at: r.created_at,
            updated_at: r.updated_at,
            status: if r.last_sync_at.is_none() && r.last_svn_rev == 0 {
                "initializing".to_string()
            } else {
                r.sync_status
            },
            initializing: r.last_sync_at.is_none() && r.last_svn_rev == 0,
            import_mode: None,
            starting_revision: None,
            history_boundary: None,
            snapshot_pin: None,
        }
    }
}

fn enrich_repo_detail(db: &Database, mut detail: RepoDetail) -> Result<RepoDetail, AppError> {
    if let Some(op) = db
        .latest_import_operation(&detail.id)
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        let mode = if op.snapshot_pin.is_some() || op.operation_type == "snapshot_import" {
            "snapshot"
        } else {
            "full"
        };
        detail.import_mode = Some(mode.into());
        if let Some(pin) = op.snapshot_pin.clone() {
            detail.starting_revision = Some(pin.operative_rev);
            detail.history_boundary = Some(pin.history_boundary());
            detail.snapshot_pin = Some(pin);
        }
        if !op.state.is_terminal() || op.state != ImportOperationState::Completed {
            detail.initializing = true;
            if detail.status == "unknown" || detail.status == "idle" {
                detail.status = "initializing".into();
            }
        }
    }
    Ok(detail)
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Credential request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SaveCredentialsRequest {
    svn_password: Option<String>,
    git_token: Option<String>,
}

#[derive(Serialize)]
struct CredentialStatus {
    svn_password_set: bool,
    git_token_set: bool,
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/repos", get(list_repos))
        .route("/api/repos", post(create_repo))
        .route("/api/repos/:id", get(get_repo))
        .route("/api/repos/:id", put(update_repo))
        .route("/api/repos/:id", delete(delete_repo))
        .route("/api/repos/:id/remove", post(remove_repo))
        .route("/api/repos/:id/removal", get(get_removal))
        .route("/api/repos/:id/sync", post(trigger_sync))
        .route("/api/repos/:id/import", post(start_repo_import))
        .route("/api/repos/:id/import/status", get(repo_import_status))
        .route(
            "/api/repos/:id/import/:operation_id/cancel",
            post(cancel_repo_import),
        )
        .route(
            "/api/repos/:id/import/:operation_id/reconcile",
            post(reconcile_repo_import),
        )
        .route(
            "/api/repos/:id/import/:operation_id/resume",
            post(resume_repo_import),
        )
        .route(
            "/api/repos/:id/svn-commit/:operation_id",
            get(svn_commit_status),
        )
        .route(
            "/api/repos/:id/svn-commit/:operation_id/reconcile",
            post(reconcile_svn_commit),
        )
        .route(
            "/api/repos/:id/git-push/:operation_id",
            get(git_push_status),
        )
        .route(
            "/api/repos/:id/git-push/:operation_id/reconcile",
            post(reconcile_git_push),
        )
        .route(
            "/api/repos/:id/import/cancel",
            post(cancel_repo_import_without_id),
        )
        .route("/api/repos/:id/credentials", get(get_credentials))
        .route("/api/repos/:id/credentials", post(save_credentials))
        .route("/api/repos/:id/branches", post(create_branch_pair))
        .route("/api/repos/:id/branches", get(list_branch_pairs))
        .route("/api/repos/:id/refresh", post(preview_pair_refresh))
        .route("/api/repos/:id/branch-pair", delete(delete_branch_pair))
        .route("/api/repos/:id/test-svn", post(test_repo_svn))
        .route("/api/repos/:id/test-git", post(test_repo_git))
        .route("/api/repos/:id/skip-commit", post(skip_commit))
        .route(
            "/api/repos/:id/skip-commit/context",
            get(skip_commit_context),
        )
        .route("/api/repos/:id/retry", post(retry_repo))
        .route("/api/repos/:id/hooks/pre-commit", get(get_pre_commit_hook))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_repos(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<RepoSummary>>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;

    let repos = db
        .list_repositories()
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

    let summaries: Vec<RepoSummary> = repos
        .into_iter()
        .map(|r| RepoSummary {
            id: r.id,
            name: r.name,
            parent_id: r.parent_id,
            svn_url: r.svn_url,
            svn_branch: r.svn_branch,
            git_provider: r.git_provider,
            git_repo: r.git_repo,
            git_branch: r.git_branch,
            sync_mode: r.sync_mode,
            enabled: r.enabled,
            created_at: r.created_at,
            updated_at: r.updated_at,
            status: "unknown".to_string(),
        })
        .collect();

    Ok(Json(summaries))
}

async fn create_repo(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateRepoRequest>,
) -> Result<Json<RepoDetail>, AppError> {
    let (user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    if body.name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    if body.svn_url.is_empty() {
        return Err(AppError::BadRequest("svn_url is required".into()));
    }

    let now = Utc::now().to_rfc3339();
    let repo = reposync_core::models::Repository {
        id: Uuid::new_v4().to_string(),
        name: body.name,
        svn_url: body.svn_url,
        svn_branch: body.svn_branch,
        svn_username: body.svn_username,
        git_provider: body.git_provider,
        git_api_url: body.git_api_url,
        git_repo: body.git_repo,
        git_branch: body.git_branch,
        sync_mode: body.sync_mode,
        poll_interval_secs: body.poll_interval_secs,
        lfs_threshold_mb: body.lfs_threshold_mb,
        auto_merge: body.auto_merge,
        enabled: body.enabled,
        created_by: Some(user_id),
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".to_string(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    };

    let db = &state.db;

    db.insert_repository(&repo)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

    Ok(Json(RepoDetail::from(repo)))
}

async fn get_repo(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<RepoDetail>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;

    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    Ok(Json(enrich_repo_detail(db, RepoDetail::from(repo))?))
}

async fn update_repo(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateRepoRequest>,
) -> Result<Json<RepoDetail>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;
    reject_held_import(db, &id)?;
    if db
        .managed_remove_blocks_new_work(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        return Err(AppError::BadRequest(
            "repository removal is in progress or completed; the registration cannot be reactivated"
                .into(),
        ));
    }

    let existing = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    let now = Utc::now().to_rfc3339();
    let updated = reposync_core::models::Repository {
        id: id.clone(),
        name: body.name.unwrap_or(existing.name),
        svn_url: body.svn_url.unwrap_or(existing.svn_url),
        svn_branch: body.svn_branch.unwrap_or(existing.svn_branch),
        svn_username: body.svn_username.unwrap_or(existing.svn_username),
        git_provider: body.git_provider.unwrap_or(existing.git_provider),
        git_api_url: body.git_api_url.unwrap_or(existing.git_api_url),
        git_repo: body.git_repo.unwrap_or(existing.git_repo),
        git_branch: body.git_branch.unwrap_or(existing.git_branch),
        sync_mode: body.sync_mode.unwrap_or(existing.sync_mode),
        poll_interval_secs: body
            .poll_interval_secs
            .unwrap_or(existing.poll_interval_secs),
        lfs_threshold_mb: body.lfs_threshold_mb.unwrap_or(existing.lfs_threshold_mb),
        auto_merge: body.auto_merge.unwrap_or(existing.auto_merge),
        enabled: body.enabled.unwrap_or(existing.enabled),
        created_by: existing.created_by,
        parent_id: existing.parent_id,
        created_at: existing.created_at,
        updated_at: now,
        last_svn_rev: existing.last_svn_rev,
        last_git_sha: existing.last_git_sha,
        last_sync_at: existing.last_sync_at,
        sync_status: existing.sync_status,
        total_syncs: existing.total_syncs,
        total_errors: existing.total_errors,
        allowed_paths: body.allowed_paths.or(existing.allowed_paths),
        blocked_patterns: body.blocked_patterns.or(existing.blocked_patterns),
        consecutive_errors: existing.consecutive_errors,
        teams_webhook_url: existing.teams_webhook_url,
    };

    db.update_repository(&updated)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

    Ok(Json(RepoDetail::from(updated)))
}

async fn delete_repo(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;
    reject_held_import(db, &id)?;

    // Legacy DELETE stays non-destructive: disable only. Managed removal is
    // POST /api/repos/:id/remove and never runs from this route.
    let disabled = db
        .legacy_disable_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;
    if !disabled {
        return Err(AppError::NotFound("repository not found".into()));
    }

    Ok(Json(serde_json::json!({
        "ok": true,
        "action": "disable",
        "message": "repository disabled",
        "enabled": false,
    })))
}

fn removal_response(
    operation: &reposync_core::db::managed_remove::ManagedRemoveOperation,
    registration_listed: bool,
) -> (StatusCode, Json<serde_json::Value>) {
    use reposync_core::db::managed_remove::ManagedRemoveState;
    let status = match operation.state {
        ManagedRemoveState::Completed => StatusCode::OK,
        ManagedRemoveState::Failed | ManagedRemoveState::ReconciliationRequired => {
            StatusCode::CONFLICT
        }
        ManagedRemoveState::Queued
        | ManagedRemoveState::Cancelling
        | ManagedRemoveState::Running => StatusCode::ACCEPTED,
    };
    let retryable = !operation.state.is_terminal_success();
    let message = operation.outcome_detail.clone().unwrap_or_else(|| {
        match operation.state {
            ManagedRemoveState::Completed => {
                "removed from RepoSync; remote Git and SVN history were not modified; restore is not supported"
            }
            ManagedRemoveState::Cancelling | ManagedRemoveState::Queued | ManagedRemoveState::Running => {
                "removal is waiting for in-flight work to stop; local data was not deleted"
            }
            ManagedRemoveState::Failed => "local cleanup failed; removal was not completed",
            ManagedRemoveState::ReconciliationRequired => {
                "removal is blocked by an unresolved external effect; registration and local data were kept"
            }
        }
        .to_string()
    });
    (
        status,
        Json(serde_json::json!({
            "ok": operation.state.is_terminal_success(),
            "action": "managed_remove",
            "state": operation.state,
            "operation_id": operation.id,
            "message": message,
            "remote_git": "untouched",
            "remote_svn": "untouched",
            "restore_supported": false,
            "retryable": retryable,
            "registration_listed": registration_listed,
        })),
    )
}

fn registration_listed(db: &Database, repo_id: &str) -> Result<bool, AppError> {
    Ok(db
        .get_repository(repo_id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .is_some())
}

async fn signal_import_stop(state: &AppState, repo_id: &str) {
    if let Ok(Some(active)) = state.db.active_import_operation(repo_id) {
        if !active.state.is_terminal() {
            let _ = state.db.request_import_cancel(repo_id, &active.id);
        }
    }
    let progress = state.get_repo_import_progress(repo_id).await;
    let mut progress = progress.write().await;
    progress.cancel_requested = true;
    progress
        .cancel_signal
        .store(true, std::sync::atomic::Ordering::Release);
}

async fn remove_repo(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<axum::response::Response, AppError> {
    let (user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }
    if reposync_core::managed_remove::validate_repo_id(&id).is_err() {
        return Err(AppError::BadRequest(
            "repository id is not a single safe path component".into(),
        ));
    }
    let request_id = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or("managed-remove")
        .to_string();

    let advance = state
        .db
        .prepare_managed_remove(&id, &user_id, &request_id)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    use reposync_core::db::managed_remove::{RemovalAdvance, RemovalBlocker};
    let operation = match advance {
        RemovalAdvance::NotFound => {
            return Err(AppError::NotFound("repository not found".into()));
        }
        RemovalAdvance::ParentBlocked { child_count } => {
            return Err(AppError::BadRequest(format!(
                "parent removal is blocked while {child_count} child registration(s) exist; dependency preview is a later #65 slice and children are not removed"
            )));
        }
        RemovalAdvance::Completed { operation } => {
            let listed = registration_listed(&state.db, &id)?;
            let (status, body) = removal_response(&operation, listed);
            return Ok((status, body).into_response());
        }
        RemovalAdvance::Waiting { operation, blocker } => {
            if matches!(blocker, RemovalBlocker::ImportRunning) {
                signal_import_stop(&state, &id).await;
            }
            let listed = registration_listed(&state.db, &id)?;
            let (status, body) = removal_response(&operation, listed);
            return Ok((status, body).into_response());
        }
        RemovalAdvance::Cleanup { operation } => operation,
    };

    let progress = state.get_repo_import_progress(&id).await;
    let phase = progress.read().await.phase.clone();
    let memory_active = !matches!(
        phase,
        ImportPhase::Idle | ImportPhase::Completed | ImportPhase::Failed | ImportPhase::Cancelled
    );
    if memory_active || reposync_core::busy::is_busy(&id) {
        signal_import_stop(&state, &id).await;
        let operation = state
            .db
            .note_removal_waiting(
                &id,
                &operation.id,
                "in-process worker still holds the repository; local data was not deleted",
            )
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let listed = registration_listed(&state.db, &id)?;
        let (status, body) = removal_response(&operation, listed);
        return Ok((status, body).into_response());
    }
    let Some(_busy) = reposync_core::busy::try_acquire(&id) else {
        let operation = state
            .db
            .note_removal_waiting(
                &id,
                &operation.id,
                "in-process worker still holds the repository; local data was not deleted",
            )
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let listed = registration_listed(&state.db, &id)?;
        let (status, body) = removal_response(&operation, listed);
        return Ok((status, body).into_response());
    };
    let advance = state
        .db
        .prepare_managed_remove(&id, &user_id, &request_id)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let operation = match advance {
        RemovalAdvance::Cleanup { operation } => operation,
        RemovalAdvance::Completed { operation } => {
            let listed = registration_listed(&state.db, &id)?;
            let (status, body) = removal_response(&operation, listed);
            return Ok((status, body).into_response());
        }
        RemovalAdvance::Waiting { operation, blocker } => {
            if matches!(blocker, RemovalBlocker::ImportRunning) {
                signal_import_stop(&state, &id).await;
            }
            let listed = registration_listed(&state.db, &id)?;
            let (status, body) = removal_response(&operation, listed);
            return Ok((status, body).into_response());
        }
        RemovalAdvance::ParentBlocked { child_count } => {
            return Err(AppError::BadRequest(format!(
                "parent removal is blocked while {child_count} child registration(s) exist; dependency preview is a later #65 slice and children are not removed"
            )));
        }
        RemovalAdvance::NotFound => {
            return Err(AppError::NotFound("repository not found".into()));
        }
    };

    let data_dir = state.config.daemon.data_dir.clone();
    let cleanup = reposync_core::managed_remove::remove_owned_repo_tree(&data_dir, &id);
    if let Err(error) = cleanup {
        let operation = state
            .db
            .fail_managed_remove(&id, &operation.id, &error.to_string())
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let listed = registration_listed(&state.db, &id)?;
        let (status, body) = removal_response(&operation, listed);
        return Ok((status, body).into_response());
    }
    match state.db.complete_managed_remove(&id, &operation.id) {
        Ok(operation) => {
            let listed = registration_listed(&state.db, &id)?;
            let (status, body) = removal_response(&operation, listed);
            Ok((status, body).into_response())
        }
        Err(error) => {
            let operation = state
                .db
                .fail_managed_remove(&id, &operation.id, &error.to_string())
                .map_err(|e| AppError::Internal(e.to_string()))?;
            let listed = registration_listed(&state.db, &id)?;
            let (status, body) = removal_response(&operation, listed);
            Ok((status, body).into_response())
        }
    }
}

async fn get_removal(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<axum::response::Response, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    let Some(operation) = state
        .db
        .managed_removal(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
    else {
        return Err(AppError::NotFound("managed removal not found".into()));
    };
    let listed = registration_listed(&state.db, &id)?;
    let (status, body) = removal_response(&operation, listed);
    Ok((status, body).into_response())
}

async fn trigger_sync(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    reject_held_import(db, &id)?;
    if db
        .managed_remove_blocks_new_work(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        return Err(AppError::BadRequest(
            "repository removal is in progress or completed; sync was not started".into(),
        ));
    }

    // Check import progress for this repo to give useful status.
    let progress = state.get_repo_import_progress(&id).await;
    let p = progress.read().await;
    let import_phase = format!("{:?}", p.phase).to_lowercase();

    info!(repo_id = %id, "manual sync triggered for repository");

    // Record an audit entry so the scheduler can pick it up.
    let _ = db.insert_audit_log(
        "sync_trigger",
        Some("api"),
        None,
        None,
        None,
        Some(&format!(
            "Manual sync triggered for repo '{}' ({})",
            repo.name, id
        )),
        true,
    );

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "Sync triggered",
        "repo_name": repo.name,
        "enabled": repo.enabled,
        "import_phase": import_phase,
    })))
}

// ---------------------------------------------------------------------------
// Per-repo import
// ---------------------------------------------------------------------------

fn reject_held_import(db: &Database, repo_id: &str) -> Result<(), AppError> {
    if db
        .active_import_operation(repo_id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .is_some()
    {
        Err(AppError::BadRequest(
            "repository has active or unresolved import work".into(),
        ))
    } else {
        Ok(())
    }
}

fn import_write_error(error: DatabaseError) -> AppError {
    match error {
        DatabaseError::Other(message) => AppError::BadRequest(message),
        other => AppError::Internal(format!("import operation persistence failed: {other}")),
    }
}

struct ImportPreparationGuard<'a> {
    db: &'a Database,
    repo_id: String,
    operation_id: String,
    armed: bool,
    cleanup_unconfirmed: bool,
}

impl Drop for ImportPreparationGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let outcome = self
            .db
            .get_import_operation(&self.repo_id, &self.operation_id);
        let (state, detail) = if self.cleanup_unconfirmed {
            (
                ImportOperationState::ReconciliationRequired,
                "preparation command cleanup unconfirmed; inspect local work and target",
            )
        } else {
            match outcome {
                Ok(Some(op))
                    if op.cancel_requested
                        && op.last_local_svn_rev.is_none()
                        && op.intended_git_sha.is_none() =>
                {
                    (
                        ImportOperationState::Cancelled,
                        "cancelled during quiesced preparation",
                    )
                }
                _ => (
                    ImportOperationState::ReconciliationRequired,
                    "preparation stopped before worker start; inspect local work and target",
                ),
            }
        };
        if let Err(e) =
            self.db
                .finish_import_operation(&self.repo_id, &self.operation_id, state, detail)
        {
            error!(repo_id = %self.repo_id, error = %e, "failed to persist import preparation outcome");
        }
    }
}

#[derive(serde::Deserialize, Default)]
struct ImportQuery {
    #[serde(default)]
    reset: bool,
    #[serde(default)]
    import_mode: Option<String>,
    #[serde(default)]
    svn_revision: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct StartImportBody {
    #[serde(default)]
    import_mode: Option<String>,
    #[serde(default)]
    svn_revision: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TeamImportMode {
    Full,
    Snapshot,
}

fn parse_team_import_mode(raw: Option<&str>) -> Result<TeamImportMode, AppError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("full") | Some("full-history") | Some("full_history") => {
            Ok(TeamImportMode::Full)
        }
        Some("snapshot") => Ok(TeamImportMode::Snapshot),
        Some(other) => Err(AppError::BadRequest(format!(
            "import_mode must be full or snapshot, got {other}"
        ))),
    }
}

async fn start_repo_import(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    axum::extract::Query(import_query): axum::extract::Query<ImportQuery>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    let (user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;
    let request_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // 1. Load repo config from DB
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;
    if db
        .managed_remove_blocks_new_work(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        return Err(AppError::BadRequest(
            "repository removal is in progress or completed; import was not started".into(),
        ));
    }

    // This cancellation increment cannot account for the legacy reset's
    // destructive local cleanup and force-push. Refuse before enrollment or
    // any workdir, credential, checkpoint, mapping, or target mutation.
    if import_query.reset {
        return Err(AppError::BadRequest(
            "Reset & Reimport is unavailable while safe import cancellation is in effect; request a separately reviewed recovery plan".into(),
        ));
    }

    let body: StartImportBody = if body.is_empty() {
        StartImportBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| AppError::BadRequest(format!("invalid import request body: {e}")))?
    };
    let import_mode = parse_team_import_mode(
        body.import_mode
            .as_deref()
            .or(import_query.import_mode.as_deref()),
    )?;
    let requested_revision = body
        .svn_revision
        .as_deref()
        .or(import_query.svn_revision.as_deref());
    if import_mode == TeamImportMode::Full && requested_revision.is_some() {
        return Err(AppError::BadRequest(
            "svn_revision is only valid with import_mode=snapshot".into(),
        ));
    }

    if let Some(previous) = db
        .latest_import_operation(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        if previous.request_id == request_id && previous.initiator_id == user_id {
            return Ok(Json(
                serde_json::json!({"ok":true,"message":"Import request already recorded",
                "operation_id":previous.id,"lifecycle":previous.state}),
            ));
        }
    }
    if db
        .active_import_operation(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .is_some()
    {
        return Err(AppError::BadRequest(
            "repository import is active or held for reconciliation".into(),
        ));
    }
    if repo.last_svn_rev > 0 {
        return Err(AppError::BadRequest(
            "repository already has a completed baseline; refusing implicit full replay".into(),
        ));
    }
    let registrations = db
        .list_repositories()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if registrations.iter().any(|other| {
        other.id != id
            && other.git_api_url == repo.git_api_url
            && other.git_repo == repo.git_repo
            && other.git_branch == repo.git_branch
    }) {
        return Err(AppError::BadRequest(
            "another repository registration shares this Git target".into(),
        ));
    }

    // 2. Check if an import is already running for this repo
    let progress = state.get_repo_import_progress(&id).await;
    {
        let p = progress.read().await;
        if p.phase == ImportPhase::Importing {
            return Ok(Json(serde_json::json!({
                "ok": false,
                "message": "An import is already running for this repository",
            })));
        }
    }

    // 2b. RS-C07 (#64): acquire exclusive writer ownership before any mutable
    // prep that touches this repo's Git/SVN working tree. If the scheduler is
    // currently in the middle of a cycle we wait briefly for it to finish
    // before starting the import. The guard is moved into the background task
    // and released when the import completes.
    let busy_guard = {
        let mut guard = None;
        for attempt in 0..30 {
            if let Some(active) = db
                .active_import_operation(&id)
                .map_err(|e| AppError::Internal(e.to_string()))?
            {
                if active.request_id == request_id && active.initiator_id == user_id {
                    return Ok(Json(
                        serde_json::json!({"ok":true,"message":"Import request already recorded",
                        "operation_id":active.id,"lifecycle":active.state}),
                    ));
                }
                return Err(AppError::BadRequest(
                    "repository import is active or held".into(),
                ));
            }
            if let Some(g) = reposync_core::busy::try_acquire(&id) {
                guard = Some(g);
                break;
            }
            if attempt == 0 {
                info!(repo_id = %id, "waiting for in-flight sync cycle to finish before import");
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        match guard {
            Some(g) => g,
            None => {
                return Ok(Json(serde_json::json!({
                    "ok": false,
                    "message": "A sync cycle is currently running for this repository. Please retry in a moment.",
                })));
            }
        }
    };

    let workdir = state
        .config
        .daemon
        .data_dir
        .join("repos")
        .join(&id)
        .join("git-repo");
    let fingerprint = import_target_fingerprint(&repo, &workdir);
    let operation = db
        .create_import_operation(&id, &user_id, &request_id, &fingerprint)
        .map_err(import_write_error)?;
    let operation_id = operation.id.clone();
    let mut preparation_guard = ImportPreparationGuard {
        db,
        repo_id: id.clone(),
        operation_id: operation_id.clone(),
        armed: true,
        cleanup_unconfirmed: false,
    };

    // 3. Reset progress
    {
        let mut p = progress.write().await;
        *p = ImportProgress::default();
        p.phase = ImportPhase::Importing;
        p.started_at = Some(chrono::Utc::now().to_rfc3339());
    }

    // 4. Read credentials from kv_state
    let svn_password_repo = db
        .get_state(&format!("secret_svn_password_{}", id))
        .unwrap_or(None);
    let svn_password_global = db.get_state("secret_svn_password").unwrap_or(None);
    let svn_password = svn_password_repo
        .clone()
        .or(svn_password_global.clone())
        .unwrap_or_default();
    debug!(
        repo_id = %id,
        source = if svn_password_repo.is_some() { "repo-specific" } else if svn_password_global.is_some() { "global" } else { "none" },
        "resolved SVN password for import"
    );

    let git_token_repo = db
        .get_state(&format!("secret_git_token_{}", id))
        .unwrap_or(None);
    let git_token_global = db.get_state("secret_git_token").unwrap_or(None);
    let git_token: Option<String> = git_token_repo.clone().or(git_token_global.clone());
    debug!(
        repo_id = %id,
        source = if git_token_repo.is_some() { "repo-specific" } else if git_token_global.is_some() { "global" } else { "none" },
        "resolved Git token for import"
    );

    // 5. Build SVN import URL
    let svn_import_url = {
        let base = repo.svn_url.trim_end_matches('/');
        let branch = if repo.svn_branch.is_empty() {
            "trunk"
        } else {
            &repo.svn_branch
        };
        if branch.is_empty() || branch == "/" {
            base.to_string()
        } else {
            format!("{}/{}", base, branch.trim_start_matches('/'))
        }
    };

    info!(repo_id = %id, svn_import_url = %svn_import_url, "starting per-repo import");

    let svn_client = SvnClient::new(&svn_import_url, &repo.svn_username, &svn_password)
        .with_cancel_signal(progress.read().await.cancel_signal.clone());

    // 6. Build the git repo path: {data_dir}/repos/{repo_id}/git-repo
    let data_dir = state.config.daemon.data_dir.clone();
    let git_repo_path = data_dir.join("repos").join(&id).join("git-repo");

    std::fs::create_dir_all(&git_repo_path)
        .map_err(|e| AppError::Internal(format!("failed to create repo dir: {}", e)))?;

    // 7. Build clone URL from repo config
    let clone_url = reposync_core::git::remote_url::derive_git_remote_url(
        &repo.git_api_url,
        None,
        &repo.git_repo,
    );

    let git_client = if git_repo_path.join(".git").exists() {
        GitClient::new(&git_repo_path)
            .map_err(|e| AppError::Internal(format!("failed to open git repo: {}", e)))?
    } else {
        // A missing/unreachable target must never be replaced with a fresh
        // local repository. Supervise the clone and its descendants.
        let authenticated_url = match (
            git_token.as_deref(),
            clone_url.strip_prefix("https://"),
            clone_url.strip_prefix("http://"),
        ) {
            (Some(token), Some(rest), _) => format!("https://x-access-token:{token}@{rest}"),
            (Some(token), _, Some(rest)) => format!("http://x-access-token:{token}@{rest}"),
            _ => clone_url.clone(),
        };
        let mut clone = tokio::process::Command::new("git");
        clone
            .arg("clone")
            .arg("--")
            .arg(&authenticated_url)
            .arg(&git_repo_path)
            .env("GIT_TERMINAL_PROMPT", "0");
        let signal = progress.read().await.cancel_signal.clone();
        let output =
            reposync_core::process::run(clone, std::time::Duration::from_secs(300), Some(&signal))
                .await
                .map_err(|e| {
                    preparation_guard.cleanup_unconfirmed =
                        reposync_core::process::cleanup_unconfirmed(&e);
                    AppError::Internal(format!("Git clone stopped or timed out: {e}"))
                })?;
        if !output.status.success() {
            return Err(AppError::BadRequest(format!(
                "Git target could not be cloned (exit {:?}); import held for inspection",
                output.status.code()
            )));
        }
        GitClient::new(&git_repo_path)
            .map_err(|e| AppError::Internal(format!("failed to open cloned git repo: {e}")))?
    };

    // An empty bare target often advertises master even when this managed
    // repository is configured for main. Align unborn HEAD before replay.
    git_client
        .ensure_head_on_branch(&repo.git_branch)
        .map_err(|e| AppError::Internal(format!("failed to select import branch: {e}")))?;

    // 8. Configure git remote credentials
    git_client
        .ensure_remote_credentials("origin", git_token.as_deref())
        .map_err(|e| AppError::Internal(format!("failed to set git credentials: {}", e)))?;

    {
        let mut inspect = tokio::process::Command::new("git");
        inspect
            .args([
                "ls-remote",
                "--exit-code",
                "origin",
                &format!("refs/heads/{}", repo.git_branch),
            ])
            .current_dir(&git_repo_path)
            .env("GIT_TERMINAL_PROMPT", "0");
        let signal = progress.read().await.cancel_signal.clone();
        let remote =
            reposync_core::process::run(inspect, std::time::Duration::from_secs(60), Some(&signal))
                .await
                .map_err(|e| {
                    preparation_guard.cleanup_unconfirmed =
                        reposync_core::process::cleanup_unconfirmed(&e);
                    AppError::Internal(format!("Git target inspection stopped or timed out: {e}"))
                })?;
        match remote.status.code() {
            Some(0) => {
                return Err(AppError::BadRequest(
                    "Git target branch already exists; import baseline requires review".into(),
                ))
            }
            Some(2) => {}
            _ => {
                return Err(AppError::BadRequest(
                    "Git target could not be inspected; import held for review".into(),
                ))
            }
        }
    }

    if reposync_core::snapshot::snapshot_workdir_is_born(&git_repo_path).unwrap_or(false) {
        return Err(AppError::BadRequest(
            "existing non-empty Git workdir refuses import overwrite; choose a new target".into(),
        ));
    }

    let snapshot_pin = if import_mode == TeamImportMode::Snapshot {
        db.mark_snapshot_import_request(&id, &operation_id)
            .map_err(import_write_error)?;
        let requested = match reposync_core::snapshot::SnapshotRevision::parse(requested_revision) {
            Ok(requested) => requested,
            Err(e) => {
                let detail = format!("invalid snapshot revision: {e:#}");
                let _ = db.finish_import_operation(
                    &id,
                    &operation_id,
                    ImportOperationState::Failed,
                    &detail,
                );
                preparation_guard.armed = false;
                return Err(AppError::BadRequest(detail));
            }
        };
        match reposync_core::snapshot::resolve_snapshot_pin(&svn_client, requested).await {
            Ok(pin) => {
                db.pin_snapshot_import(&id, &operation_id, pin.clone())
                    .map_err(import_write_error)?;
                Some(pin)
            }
            Err(e) => {
                let detail = format!("invalid or inaccessible snapshot revision: {e:#}");
                let _ = db.finish_import_operation(
                    &id,
                    &operation_id,
                    ImportOperationState::Failed,
                    &detail,
                );
                preparation_guard.armed = false;
                return Err(AppError::BadRequest(detail));
            }
        }
    } else {
        None
    };

    let git_client = Arc::new(std::sync::Mutex::new(git_client));

    // 9. Create IdentityMapper and FilePolicy (use defaults for per-repo)
    let identity_config = reposync_core::config::IdentityConfig::default();
    let identity_mapper = IdentityMapper::new(&identity_config)
        .map_err(|e| AppError::Internal(format!("failed to init identity mapper: {}", e)))?;

    let lfs_threshold_bytes = if repo.lfs_threshold_mb > 0 {
        (repo.lfs_threshold_mb as u64) * 1024 * 1024
    } else {
        0
    };
    let file_policy = FilePolicy::with_lfs(0, vec![], lfs_threshold_bytes, &[]);

    // 10. Open a separate DB connection for the import task
    let db_path = data_dir.join("reposync.db");
    let import_db = Database::new(&db_path)
        .map_err(|e| AppError::Internal(format!("failed to open db: {}", e)))?;

    // 11. Build ImportConfig from repo settings
    let import_config = ImportConfig {
        committer_name: "RepoSync".into(),
        committer_email: "reposync@localhost".into(),
        remote_name: "origin".into(),
        branch: repo.git_branch.clone(),
        push_token: git_token,
        message_prefix: None,
        trunk_path: repo.svn_branch.clone(),
    };

    let ws_broadcast = Some(state.ws_broadcast.clone());
    let repo_id_clone = id.clone();
    let worker_operation_id = operation_id.clone();
    let cancel_signal = progress.read().await.cancel_signal.clone();
    let worker_pin = snapshot_pin.clone();

    // 12. Spawn the import task (tracked for graceful shutdown)
    let state_for_handle = state.clone();
    let handle = tokio::spawn(async move {
        // Hold the busy guard for the entire lifetime of the import so
        // the scheduler skips this repo until we're done.
        let _busy_guard = busy_guard;
        let current = import_db.get_import_operation(&repo_id_clone, &worker_operation_id);
        let result = match current {
            Ok(Some(op)) if op.cancel_requested => {
                Ok(import::ImportOutcome::Cancelled { commits: 0 })
            }
            Ok(Some(_)) => {
                match import_db.start_import_operation(&repo_id_clone, &worker_operation_id) {
                    Ok(started) => {
                        let run_state = ImportRunState {
                            progress: progress.clone(),
                            ws_broadcast: ws_broadcast.clone(),
                            repo_id: Some(repo_id_clone.clone()),
                            operation_id: Some(worker_operation_id.clone()),
                            cancel_signal: Some(cancel_signal),
                        };
                        if let Some(pin) = started.snapshot_pin.or(worker_pin) {
                            import::run_snapshot_import(
                                &svn_client,
                                &git_client,
                                &import_db,
                                &file_policy,
                                &import_config,
                                &pin,
                                run_state,
                            )
                            .await
                        } else {
                            import::run_full_import(
                                &svn_client,
                                &git_client,
                                &identity_mapper,
                                &import_db,
                                &file_policy,
                                &import_config,
                                run_state,
                            )
                            .await
                        }
                    }
                    Err(e) => Err(e.into()),
                }
            }
            Ok(None) => Err(anyhow::anyhow!("operation disappeared before start")),
            Err(e) => Err(e.into()),
        };
        let terminal = match result {
            Ok(import::ImportOutcome::Completed {
                commits,
                svn_rev,
                git_sha,
            }) => {
                match import_db.complete_import_operation(
                    &repo_id_clone,
                    &worker_operation_id,
                    svn_rev,
                    &git_sha,
                ) {
                    Ok(_) => {
                        info!(repo_id = %repo_id_clone, commits, "per-repo import completed and confirmed");
                        ImportPhase::Completed
                    }
                    Err(e) => {
                        error!(repo_id = %repo_id_clone, error = %e, "import finalization failed");
                        let _ = import_db.finish_import_operation(
                            &repo_id_clone,
                            &worker_operation_id,
                            ImportOperationState::ReconciliationRequired,
                            &format!("finalization failed: {e}"),
                        );
                        ImportPhase::Failed
                    }
                }
            }
            Ok(import::ImportOutcome::Cancelled { commits }) => {
                let op = import_db
                    .get_import_operation(&repo_id_clone, &worker_operation_id)
                    .ok()
                    .flatten();
                let uncertain = op.as_ref().is_some_and(|o| o.intended_git_sha.is_some());
                let state = if uncertain {
                    ImportOperationState::ReconciliationRequired
                } else {
                    ImportOperationState::Cancelled
                };
                let detail = format!(
                    "stopped after {commits} local commits; published history is not undone"
                );
                if let Err(e) = import_db.finish_import_operation(
                    &repo_id_clone,
                    &worker_operation_id,
                    state,
                    &detail,
                ) {
                    error!(repo_id = %repo_id_clone, error = %e, "cancel finalization failed");
                    ImportPhase::Failed
                } else if uncertain {
                    ImportPhase::Failed
                } else {
                    ImportPhase::Cancelled
                }
            }
            Ok(import::ImportOutcome::ReconciliationRequired { reason, .. }) => {
                if let Err(e) = import_db.finish_import_operation(
                    &repo_id_clone,
                    &worker_operation_id,
                    ImportOperationState::ReconciliationRequired,
                    &reason,
                ) {
                    error!(repo_id = %repo_id_clone, error = %e, "uncertain outcome persistence failed");
                }
                ImportPhase::Failed
            }
            Err(e) => {
                let detail = format!("import stopped with error; inspect local work: {e:#}");
                if let Err(write_error) = import_db.finish_import_operation(
                    &repo_id_clone,
                    &worker_operation_id,
                    ImportOperationState::Failed,
                    &detail,
                ) {
                    error!(repo_id = %repo_id_clone, error = %write_error, "failure persistence failed");
                }
                ImportPhase::Failed
            }
        };
        let mut p = progress.write().await;
        p.phase = terminal;
        p.completed_at = Some(chrono::Utc::now().to_rfc3339());

        if let Err(e) = import_db.persist_import_progress(&p) {
            tracing::warn!(
                "failed to persist import progress for repo {}: {}",
                repo_id_clone,
                e
            );
        }

        if let Some(ref sender) = ws_broadcast {
            let json = serde_json::json!({
                "type": "repo_import_progress",
                "repo_id": repo_id_clone,
                "phase": format!("{:?}", p.phase).to_lowercase(),
                "current_rev": p.current_rev,
                "total_revs": p.total_revs,
                "commits_created": p.commits_created,
            });
            let _ = sender.send(json.to_string());
        }
    });

    // Track the import handle so graceful shutdown waits for it
    {
        let mut handles = state_for_handle.import_handles.lock().await;
        // Clean up finished handles
        handles.retain(|h| !h.is_finished());
        handles.push(handle);
    }
    preparation_guard.armed = false;

    let mut started = serde_json::json!({
        "ok": true,
        "message": "Import started",
        "operation_id": operation_id,
        "lifecycle": "queued",
        "import_mode": match import_mode {
            TeamImportMode::Full => "full",
            TeamImportMode::Snapshot => "snapshot",
        },
    });
    if let Some(pin) = snapshot_pin {
        started["starting_revision"] = serde_json::json!(pin.operative_rev);
        started["history_boundary"] = serde_json::json!(pin.history_boundary());
        started["snapshot_pin"] = serde_json::to_value(pin).unwrap_or(serde_json::Value::Null);
    }
    Ok(Json(started))
}

async fn repo_import_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    // Verify the repository exists
    let db = &state.db;
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    let op = db
        .latest_import_operation(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if role != "admin" && op.as_ref().is_none_or(|o| o.initiator_id != user_id) {
        return Err(AppError::Unauthorized(
            "import status requires initiating context or admin".into(),
        ));
    }
    let progress = state.get_repo_import_progress(&id).await;
    let p = progress.read().await;
    let mut value = serde_json::to_value(&*p).map_err(|e| AppError::Internal(e.to_string()))?;
    value.as_object_mut().unwrap().insert(
        "can_start".into(),
        serde_json::json!(op.is_none() && repo.last_svn_rev == 0 && repo.last_sync_at.is_none()),
    );
    if let Some(op) = op {
        let terminal_phase = match op.state {
            ImportOperationState::Queued
            | ImportOperationState::Running
            | ImportOperationState::CancelRequested
            | ImportOperationState::Cancelling => None,
            ImportOperationState::Completed => Some("completed"),
            ImportOperationState::Cancelled => Some("cancelled"),
            ImportOperationState::Failed | ImportOperationState::ReconciliationRequired => {
                Some("failed")
            }
        };
        let object = value.as_object_mut().unwrap();
        if let Some(phase) = terminal_phase {
            object.insert("phase".into(), serde_json::json!(phase));
        }
        object.insert("operation_id".into(), serde_json::json!(op.id));
        object.insert("lifecycle".into(), serde_json::to_value(&op.state).unwrap());
        object.insert(
            "current_rev".into(),
            serde_json::json!(op.processed_revisions),
        );
        object.insert(
            "total_revs".into(),
            serde_json::json!(op.total_revisions.unwrap_or(0)),
        );
        object.insert(
            "commits_created".into(),
            serde_json::json!(op.local_commits),
        );
        object.insert(
            "batches_pushed".into(),
            serde_json::json!(op.confirmed_batches),
        );
        object.insert(
            "last_local_svn_rev".into(),
            serde_json::json!(op.last_local_svn_rev),
        );
        object.insert(
            "last_local_git_sha".into(),
            serde_json::json!(op.last_local_git_sha),
        );
        object.insert(
            "last_confirmed_svn_rev".into(),
            serde_json::json!(op.last_confirmed_svn_rev),
        );
        object.insert(
            "last_confirmed_git_sha".into(),
            serde_json::json!(op.last_confirmed_git_sha),
        );
        object.insert("intended_ref".into(), serde_json::json!(op.intended_ref));
        object.insert(
            "intended_git_sha".into(),
            serde_json::json!(op.intended_git_sha),
        );
        object.insert(
            "outcome_detail".into(),
            serde_json::json!(op.outcome_detail),
        );
        object.insert(
            "resume_authorized".into(),
            serde_json::json!(op.resume_authorized),
        );
        object.insert(
            "can_resume".into(),
            serde_json::json!(
                op.resume_authorized
                    && reposync_core::db::import_operations::import_resume_checkpoint(&op)
                        .is_some()
            ),
        );
        let import_mode = if op.snapshot_pin.is_some() || op.operation_type == "snapshot_import" {
            "snapshot"
        } else {
            "full"
        };
        object.insert("import_mode".into(), serde_json::json!(import_mode));
        if let Some(pin) = &op.snapshot_pin {
            object.insert(
                "starting_revision".into(),
                serde_json::json!(pin.operative_rev),
            );
            object.insert(
                "history_boundary".into(),
                serde_json::json!(pin.history_boundary()),
            );
            object.insert(
                "snapshot_pin".into(),
                serde_json::to_value(pin).unwrap_or(serde_json::Value::Null),
            );
            object.insert("earlier_history_imported".into(), serde_json::json!(false));
        } else {
            object.insert("earlier_history_imported".into(), serde_json::json!(true));
        }
        object.insert("started_at".into(), serde_json::json!(op.created_at));
        if op.state.is_terminal() {
            object.insert("completed_at".into(), serde_json::json!(op.updated_at));
        }
    }
    Ok(Json(value))
}

async fn cancel_repo_import(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path((id, operation_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    state
        .db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;
    let op = state
        .db
        .get_import_operation(&id, &operation_id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("import operation not found for repository".into()))?;
    if role != "admin" && op.initiator_id != user_id {
        return Err(AppError::Unauthorized(
            "import cancellation requires initiating context or admin".into(),
        ));
    }
    let op = state
        .db
        .request_import_cancel(&id, &operation_id)
        .map_err(import_write_error)?;
    if !op.state.is_terminal() {
        let progress = state.get_repo_import_progress(&id).await;
        let mut p = progress.write().await;
        p.cancel_requested = true;
        p.cancel_signal
            .store(true, std::sync::atomic::Ordering::Release);
    }
    Ok(Json(serde_json::json!({
        "ok": true, "operation_id": operation_id, "lifecycle": op.state,
        "message": if op.state.is_terminal() { "terminal outcome retained" } else { "cancellation durably requested; worker still stopping" },
    })))
}

fn reconciliation_result(
    before: &ImportOperation,
    after: &ImportOperation,
    observed_ref: Option<&str>,
    observed_sha: Option<&str>,
    publication_proved: bool,
    publication_receipt_recorded: bool,
    checkpoint_completed: bool,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "operation_id": before.id,
        "previous_lifecycle": before.state,
        "lifecycle": after.state,
        "recorded_intended_ref": before.intended_ref,
        "recorded_intended_git_sha": before.intended_git_sha,
        "observed_remote_ref": observed_ref,
        "observed_remote_git_sha": observed_sha,
        "last_local_svn_rev": after.last_local_svn_rev,
        "last_local_git_sha": after.last_local_git_sha,
        "last_confirmed_svn_rev": after.last_confirmed_svn_rev,
        "last_confirmed_git_sha": after.last_confirmed_git_sha,
        "publication_proved": publication_proved,
        "publication_receipt_recorded": publication_receipt_recorded,
        "checkpoint_completed": checkpoint_completed,
        "may_resume": after.resume_authorized,
        "resume_authorized": after.resume_authorized,
        "remaining_reason": after.outcome_detail,
    }))
}

async fn reconcile_repo_import(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path((id, operation_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }
    let db = &state.db;
    // The same slot excludes in-flight import, scheduler, sync and deletion.
    // Waiting lets concurrent exact reconciliation requests converge without
    // turning the second request into a new publication attempt.
    let mut busy_guard = None;
    for _ in 0..100 {
        busy_guard = reposync_core::busy::try_acquire(&id);
        if busy_guard.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let _busy_guard = busy_guard.ok_or_else(|| {
        AppError::BadRequest("repository is busy; retry remote verification later".into())
    })?;

    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;
    let requested = db
        .get_import_operation(&id, &operation_id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("import operation not found for repository".into()))?;
    let active = db
        .active_import_operation(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if active.is_none() && requested.state == ImportOperationState::Completed {
        return Ok(reconciliation_result(
            &requested, &requested, None, None, false, false, true,
        ));
    }
    if active.as_ref().is_none_or(|op| op.id != operation_id)
        || requested.state != ImportOperationState::ReconciliationRequired
        || !matches!(
            requested.operation_type.as_str(),
            "full_import" | "snapshot_import"
        )
    {
        return Err(AppError::BadRequest(
            "operation is not this repository's active reconciliation hold".into(),
        ));
    }
    let workdir = state
        .config
        .daemon
        .data_dir
        .join("repos")
        .join(&id)
        .join("git-repo");
    let reconciled =
        match import::apply_import_reconciliation(db, &repo, &operation_id, &workdir).await {
            Ok(reconciled) => reconciled,
            Err(error) => return Err(import_write_error(error)),
        };
    if reconciled.finalized {
        let progress = state.get_repo_import_progress(&id).await;
        let mut progress = progress.write().await;
        progress.phase = ImportPhase::Completed;
        progress.completed_at = Some(chrono::Utc::now().to_rfc3339());
    }
    let publication_proved = matches!(
        reconciled.inspect,
        import::ImportInspect::UniqueMatch { .. }
    );
    Ok(reconciliation_result(
        &requested,
        &reconciled.operation,
        reconciled.observed_ref.as_deref(),
        reconciled.observed_sha.as_deref(),
        publication_proved,
        reconciled.publication_recorded,
        reconciled.finalized,
    ))
}

async fn resume_repo_import(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path((id, operation_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }
    let db = &state.db;
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;
    let requested = db
        .get_import_operation(&id, &operation_id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("import operation not found for repository".into()))?;
    let active = db
        .active_import_operation(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if active.as_ref().is_none_or(|op| op.id != operation_id) {
        return Err(AppError::BadRequest(
            "operation is not this repository's active import hold".into(),
        ));
    }
    if requested.operation_type != "full_import" {
        return Err(AppError::BadRequest(
            "only full-history imports may resume from a checkpoint".into(),
        ));
    }

    let progress = state.get_repo_import_progress(&id).await;
    {
        let p = progress.read().await;
        if p.phase == ImportPhase::Importing {
            return Ok(Json(serde_json::json!({
                "ok": false,
                "message": "An import is already running for this repository",
            })));
        }
    }

    let busy_guard = {
        let mut guard = None;
        for attempt in 0..30 {
            if let Some(g) = reposync_core::busy::try_acquire(&id) {
                guard = Some(g);
                break;
            }
            if attempt == 0 {
                info!(repo_id = %id, "waiting for in-flight sync cycle to finish before import resume");
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        guard.ok_or_else(|| {
            AppError::BadRequest(
                "A sync cycle is currently running for this repository. Please retry in a moment."
                    .into(),
            )
        })?
    };

    let resumed = db
        .resume_import_operation(&id, &operation_id)
        .map_err(import_write_error)?;

    let data_dir = state.config.daemon.data_dir.clone();
    let git_repo_path = data_dir.join("repos").join(&id).join("git-repo");
    if !git_repo_path.join(".git").exists() {
        return Err(AppError::BadRequest(
            "managed Git workdir is unavailable for import resume".into(),
        ));
    }

    {
        let mut p = progress.write().await;
        p.phase = ImportPhase::Importing;
        p.started_at = Some(chrono::Utc::now().to_rfc3339());
        p.completed_at = None;
        p.cancel_requested = false;
        p.cancel_signal
            .store(false, std::sync::atomic::Ordering::Release);
        p.total_revs = resumed.total_revisions.unwrap_or(0) as i64;
        p.commits_created = resumed.local_commits;
        p.batches_pushed = resumed.confirmed_batches;
        p.current_rev = resumed.processed_revisions as i64;
    }

    let svn_password_repo = db
        .get_state(&format!("secret_svn_password_{}", id))
        .unwrap_or(None);
    let svn_password_global = db.get_state("secret_svn_password").unwrap_or(None);
    let svn_password = svn_password_repo
        .clone()
        .or(svn_password_global.clone())
        .unwrap_or_default();
    let git_token_repo = db
        .get_state(&format!("secret_git_token_{}", id))
        .unwrap_or(None);
    let git_token_global = db.get_state("secret_git_token").unwrap_or(None);
    let git_token: Option<String> = git_token_repo.clone().or(git_token_global.clone());

    let svn_import_url = {
        let base = repo.svn_url.trim_end_matches('/');
        let branch = if repo.svn_branch.is_empty() {
            "trunk"
        } else {
            &repo.svn_branch
        };
        if branch.is_empty() || branch == "/" {
            base.to_string()
        } else {
            format!("{}/{}", base, branch.trim_start_matches('/'))
        }
    };
    let svn_client = SvnClient::new(&svn_import_url, &repo.svn_username, &svn_password)
        .with_cancel_signal(progress.read().await.cancel_signal.clone());
    let git_client = Arc::new(std::sync::Mutex::new(
        GitClient::new(&git_repo_path)
            .map_err(|e| AppError::Internal(format!("failed to open git repo: {e}")))?,
    ));
    git_client
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .ensure_remote_credentials("origin", git_token.as_deref())
        .map_err(|e| AppError::Internal(format!("failed to set git credentials: {e}")))?;

    let identity_config = reposync_core::config::IdentityConfig::default();
    let identity_mapper = IdentityMapper::new(&identity_config)
        .map_err(|e| AppError::Internal(format!("failed to init identity mapper: {e}")))?;
    let lfs_threshold_bytes = if repo.lfs_threshold_mb > 0 {
        (repo.lfs_threshold_mb as u64) * 1024 * 1024
    } else {
        0
    };
    let file_policy = FilePolicy::with_lfs(0, vec![], lfs_threshold_bytes, &[]);
    let import_config = ImportConfig {
        committer_name: "RepoSync".into(),
        committer_email: "reposync@localhost".into(),
        remote_name: "origin".into(),
        branch: repo.git_branch.clone(),
        push_token: git_token,
        message_prefix: None,
        trunk_path: repo.svn_branch.clone(),
    };

    let db_path = data_dir.join("reposync.db");
    let import_db = Database::new(&db_path)
        .map_err(|e| AppError::Internal(format!("failed to open db: {e}")))?;
    let ws_broadcast = Some(state.ws_broadcast.clone());
    let repo_id_clone = id.clone();
    let worker_operation_id = operation_id.clone();
    let cancel_signal = progress.read().await.cancel_signal.clone();
    let state_for_handle = state.clone();

    let handle = tokio::spawn(async move {
        let _busy_guard = busy_guard;
        let result = {
            let run_state = ImportRunState {
                progress: progress.clone(),
                ws_broadcast: ws_broadcast.clone(),
                repo_id: Some(repo_id_clone.clone()),
                operation_id: Some(worker_operation_id.clone()),
                cancel_signal: Some(cancel_signal),
            };
            import::run_full_import(
                &svn_client,
                &git_client,
                &identity_mapper,
                &import_db,
                &file_policy,
                &import_config,
                run_state,
            )
            .await
        };
        let terminal = match result {
            Ok(import::ImportOutcome::Completed {
                commits,
                svn_rev,
                git_sha,
            }) => {
                match import_db.complete_import_operation(
                    &repo_id_clone,
                    &worker_operation_id,
                    svn_rev,
                    &git_sha,
                ) {
                    Ok(_) => {
                        info!(repo_id = %repo_id_clone, commits, "per-repo import resumed and completed");
                        ImportPhase::Completed
                    }
                    Err(e) => {
                        error!(repo_id = %repo_id_clone, error = %e, "import resume finalization failed");
                        let _ = import_db.finish_import_operation(
                            &repo_id_clone,
                            &worker_operation_id,
                            ImportOperationState::ReconciliationRequired,
                            &format!("finalization failed: {e}"),
                        );
                        ImportPhase::Failed
                    }
                }
            }
            Ok(import::ImportOutcome::Cancelled { commits }) => {
                let op = import_db
                    .get_import_operation(&repo_id_clone, &worker_operation_id)
                    .ok()
                    .flatten();
                let uncertain = op.as_ref().is_some_and(|o| o.intended_git_sha.is_some());
                let state = if uncertain {
                    ImportOperationState::ReconciliationRequired
                } else {
                    ImportOperationState::Cancelled
                };
                let detail = format!(
                    "stopped after {commits} local commits; published history is not undone"
                );
                if import_db
                    .finish_import_operation(&repo_id_clone, &worker_operation_id, state, &detail)
                    .is_err()
                    || uncertain
                {
                    ImportPhase::Failed
                } else {
                    ImportPhase::Cancelled
                }
            }
            Ok(import::ImportOutcome::ReconciliationRequired { reason, .. }) => {
                let _ = import_db.finish_import_operation(
                    &repo_id_clone,
                    &worker_operation_id,
                    ImportOperationState::ReconciliationRequired,
                    &reason,
                );
                ImportPhase::Failed
            }
            Err(e) => {
                let detail = format!("import resume stopped with error: {e:#}");
                let _ = import_db.finish_import_operation(
                    &repo_id_clone,
                    &worker_operation_id,
                    ImportOperationState::Failed,
                    &detail,
                );
                ImportPhase::Failed
            }
        };
        let mut p = progress.write().await;
        p.phase = terminal;
        p.completed_at = Some(chrono::Utc::now().to_rfc3339());
        let _ = import_db.persist_import_progress(&p);
        if let Some(ref sender) = ws_broadcast {
            let json = serde_json::json!({
                "type": "repo_import_progress",
                "repo_id": repo_id_clone,
                "phase": format!("{:?}", p.phase).to_lowercase(),
                "current_rev": p.current_rev,
                "total_revs": p.total_revs,
                "commits_created": p.commits_created,
            });
            let _ = sender.send(json.to_string());
        }
    });

    {
        let mut handles = state_for_handle.import_handles.lock().await;
        handles.retain(|h| !h.is_finished());
        handles.push(handle);
    }

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "Import resume started",
        "operation_id": operation_id,
        "lifecycle": resumed.state,
        "resume_from_svn_rev": resumed.last_confirmed_svn_rev,
        "processed_revisions": resumed.processed_revisions,
        "total_revisions": resumed.total_revisions,
    })))
}

fn svn_commit_status_json(
    op: &reposync_core::db::svn_commit_operations::SvnCommitOperation,
) -> serde_json::Value {
    serde_json::json!({
        "operation_id": op.id,
        "lifecycle": op.state,
        "operation_type": op.operation_type,
        "source_git_sha": op.source_git_sha,
        "source_git_parent": op.source_git_parent,
        "source_git_tree": op.source_git_tree,
        "target_svn_uuid": op.target_svn_uuid,
        "target_svn_path": op.target_svn_path,
        "pre_write_svn_rev": op.pre_write_svn_rev,
        "pre_write_svn_tree": op.pre_write_svn_tree,
        "intended_svn_tree": op.intended_svn_tree,
        "last_confirmed_svn_rev": op.last_confirmed_svn_rev,
        "last_confirmed_svn_tree": op.last_confirmed_svn_tree,
        "resume_authorized": op.resume_authorized,
        "outcome_detail": op.outcome_detail,
    })
}

async fn svn_commit_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path((id, operation_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }
    state
        .db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;
    let op = state
        .db
        .get_svn_commit_operation(&id, &operation_id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("git-to-svn commit operation not found".into()))?;
    Ok(Json(svn_commit_status_json(&op)))
}

fn git_push_status_json(
    op: &reposync_core::db::git_push_operations::GitPushOperation,
) -> serde_json::Value {
    serde_json::json!({
        "operation_id": op.id,
        "lifecycle": op.state,
        "operation_type": op.operation_type,
        "source_svn_rev": op.source_svn_rev,
        "pre_push_git_remote": op.pre_push_git_remote,
        "pre_push_git_branch": op.pre_push_git_branch,
        "pre_push_git_sha": op.pre_push_git_sha,
        "pre_push_git_tree": op.pre_push_git_tree,
        "intended_local_git_sha": op.intended_local_git_sha,
        "intended_local_git_parent": op.intended_local_git_parent,
        "intended_local_git_tree": op.intended_local_git_tree,
        "last_confirmed_git_sha": op.last_confirmed_git_sha,
        "last_confirmed_git_tree": op.last_confirmed_git_tree,
        "resume_authorized": op.resume_authorized,
        "outcome_detail": op.outcome_detail,
    })
}

async fn git_push_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path((id, operation_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }
    state
        .db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;
    let op = state
        .db
        .get_git_push_operation(&id, &operation_id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("svn-to-git push operation not found".into()))?;
    Ok(Json(git_push_status_json(&op)))
}

async fn reconcile_git_push(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path((id, operation_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }
    let mut busy_guard = None;
    for _ in 0..100 {
        busy_guard = reposync_core::busy::try_acquire(&id);
        if busy_guard.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let _busy_guard = busy_guard.ok_or_else(|| {
        AppError::BadRequest("repository is busy; retry remote verification later".into())
    })?;

    let db = &state.db;
    let requested = db
        .get_git_push_operation(&id, &operation_id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("svn-to-git push operation not found".into()))?;
    if requested.state == GitPushOperationState::Completed
        && db
            .active_git_push_operation(&id)
            .map_err(|e| AppError::Internal(e.to_string()))?
            .is_none()
    {
        return Ok(Json(serde_json::json!({
            "operation_id": requested.id,
            "previous_lifecycle": requested.state,
            "lifecycle": requested.state,
            "publication_proved": true,
            "checkpoint_completed": true,
            "may_resume": false,
            "remaining_reason": requested.outcome_detail,
        })));
    }

    let workdir = state
        .config
        .daemon
        .data_dir
        .join("repos")
        .join(&id)
        .join("git-repo");
    let git = GitClient::new(&workdir)
        .map_err(|e| AppError::Internal(format!("git workdir unavailable: {e}")))?;
    let reconciled =
        reposync_core::git_push::apply_git_push_reconciliation(db, &id, &operation_id, &git)
            .map_err(import_write_error)?;
    Ok(Json(serde_json::json!({
        "operation_id": requested.id,
        "previous_lifecycle": requested.state,
        "lifecycle": reconciled.operation.state,
        "recorded_intended_git_sha": requested.intended_local_git_sha,
        "recorded_pre_push_git_sha": requested.pre_push_git_sha,
        "observed": match &reconciled.inspect {
            reposync_core::git_push::GitPushInspect::UniqueMatch { git_sha, git_tree } => {
                serde_json::json!({"kind":"unique_match","git_sha":git_sha,"git_tree":git_tree})
            }
            reposync_core::git_push::GitPushInspect::AbsentUnchanged => {
                serde_json::json!({"kind":"absent_unchanged"})
            }
            reposync_core::git_push::GitPushInspect::Conflict { reason } => {
                serde_json::json!({"kind":"conflict","reason":reason})
            }
            reposync_core::git_push::GitPushInspect::Unavailable { reason } => {
                serde_json::json!({"kind":"unavailable","reason":reason})
            }
        },
        "publication_proved": reconciled.finalized,
        "checkpoint_completed": reconciled.finalized,
        "may_resume": reconciled.resume_authorized,
        "remaining_reason": reconciled.operation.outcome_detail,
        "resume_authorized": reconciled.operation.resume_authorized,
    })))
}

async fn reconcile_svn_commit(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path((id, operation_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }
    let mut busy_guard = None;
    for _ in 0..100 {
        busy_guard = reposync_core::busy::try_acquire(&id);
        if busy_guard.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let _busy_guard = busy_guard.ok_or_else(|| {
        AppError::BadRequest("repository is busy; retry remote verification later".into())
    })?;

    let db = &state.db;
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;
    let requested = db
        .get_svn_commit_operation(&id, &operation_id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .ok_or_else(|| AppError::NotFound("git-to-svn commit operation not found".into()))?;
    if requested.state == SvnCommitOperationState::Completed
        && db
            .active_svn_commit_operation(&id)
            .map_err(|e| AppError::Internal(e.to_string()))?
            .is_none()
    {
        return Ok(Json(serde_json::json!({
            "operation_id": requested.id,
            "previous_lifecycle": requested.state,
            "lifecycle": requested.state,
            "publication_proved": true,
            "checkpoint_completed": true,
            "may_resume": false,
            "remaining_reason": requested.outcome_detail,
        })));
    }

    let svn_password = db
        .resolve_credential_chain(&id, "secret_svn_password")
        .unwrap_or_default();
    let svn = SvnClient::new(&repo.svn_url, &repo.svn_username, &svn_password);
    let allowed_paths: Vec<String> = repo
        .allowed_paths
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let blocked_patterns: Vec<String> = repo
        .blocked_patterns
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let projection = serde_json::json!({
        "allowed_paths": allowed_paths,
        "blocked_patterns": blocked_patterns,
    })
    .to_string();
    let reconciled = reposync_core::svn_commit::apply_svn_commit_reconciliation(
        db,
        &id,
        &operation_id,
        &svn,
        &projection,
    )
    .await
    .map_err(import_write_error)?;
    Ok(Json(serde_json::json!({
        "operation_id": requested.id,
        "previous_lifecycle": requested.state,
        "lifecycle": reconciled.operation.state,
        "recorded_source_git_sha": requested.source_git_sha,
        "recorded_pre_write_svn_rev": requested.pre_write_svn_rev,
        "recorded_target_svn_uuid": requested.target_svn_uuid,
        "observed": match &reconciled.inspect {
            reposync_core::svn_commit::SvnCommitInspect::UniqueMatch { svn_rev, svn_tree, .. } => {
                serde_json::json!({"kind":"unique_match","svn_rev":svn_rev,"svn_tree":svn_tree})
            }
            reposync_core::svn_commit::SvnCommitInspect::AbsentUnchanged => {
                serde_json::json!({"kind":"absent_unchanged"})
            }
            reposync_core::svn_commit::SvnCommitInspect::Conflict { reason } => {
                serde_json::json!({"kind":"conflict","reason":reason})
            }
            reposync_core::svn_commit::SvnCommitInspect::Unavailable { reason } => {
                serde_json::json!({"kind":"unavailable","reason":reason})
            }
        },
        "publication_proved": reconciled.finalized,
        "checkpoint_completed": reconciled.finalized,
        "may_resume": reconciled.resume_authorized,
        "remaining_reason": reconciled.operation.outcome_detail,
        "resume_authorized": reconciled.operation.resume_authorized,
    })))
}

async fn cancel_repo_import_without_id(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    Err(AppError::BadRequest(
        "exact operation ID required; use /api/repos/{id}/import/{operation_id}/cancel".into(),
    ))
}

async fn get_credentials(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<CredentialStatus>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;

    // Verify repo exists
    let _repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    let svn_key = format!("secret_svn_password_{}", id);
    let git_key = format!("secret_git_token_{}", id);

    let svn_set = db
        .get_state(&svn_key)
        .unwrap_or(None)
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let git_set = db
        .get_state(&git_key)
        .unwrap_or(None)
        .map(|v| !v.is_empty())
        .unwrap_or(false);

    // Fall back to global keys for repos that were migrated from single-repo config
    let svn_set = svn_set
        || db
            .get_state("secret_svn_password")
            .unwrap_or(None)
            .map(|v| !v.is_empty())
            .unwrap_or(false);
    let git_set = git_set
        || db
            .get_state("secret_git_token")
            .unwrap_or(None)
            .map(|v| !v.is_empty())
            .unwrap_or(false);

    Ok(Json(CredentialStatus {
        svn_password_set: svn_set,
        git_token_set: git_set,
    }))
}

async fn save_credentials(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SaveCredentialsRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;

    // Verify repo exists
    let _repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    let now = Utc::now().to_rfc3339();

    if let Some(ref password) = body.svn_password {
        if !password.is_empty() {
            let key = format!("secret_svn_password_{}", id);
            let _ = db.conn().execute(
                "INSERT OR REPLACE INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![key, password, now],
            );
            // Also update global key for backward compat with current sync engine
            let _ = db.conn().execute(
                "INSERT OR REPLACE INTO kv_state (key, value, updated_at) VALUES ('secret_svn_password', ?1, ?2)",
                rusqlite::params![password, now],
            );
            tracing::info!(repo_id = %id, "SVN password stored for repository");
        }
    }

    if let Some(ref token) = body.git_token {
        if !token.is_empty() {
            let key = format!("secret_git_token_{}", id);
            let _ = db.conn().execute(
                "INSERT OR REPLACE INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![key, token, now],
            );
            // Also update global key for backward compat
            let _ = db.conn().execute(
                "INSERT OR REPLACE INTO kv_state (key, value, updated_at) VALUES ('secret_git_token', ?1, ?2)",
                rusqlite::params![token, now],
            );

            // Propagate to all descendant branch pairs so a single token
            // rotation at the parent doesn't require manual updates on
            // every child. Uses BFS to walk the full descendant tree.
            let mut queue: Vec<String> = vec![id.clone()];
            let mut propagated = 0;
            while let Some(current) = queue.pop() {
                if let Ok(children) = db.list_child_repositories(&current) {
                    for child in children {
                        let child_key = format!("secret_git_token_{}", child.id);
                        let _ = db.conn().execute(
                            "INSERT OR REPLACE INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)",
                            rusqlite::params![child_key, token, now],
                        );
                        queue.push(child.id);
                        propagated += 1;
                    }
                }
            }
            tracing::info!(
                repo_id = %id,
                propagated_to_children = propagated,
                "Git token stored and propagated to descendants"
            );
        }
    }

    if let Some(ref password) = body.svn_password {
        if !password.is_empty() {
            // Also propagate SVN password to descendants for the same reason.
            let mut queue: Vec<String> = vec![id.clone()];
            let mut propagated = 0;
            while let Some(current) = queue.pop() {
                if let Ok(children) = db.list_child_repositories(&current) {
                    for child in children {
                        let child_key = format!("secret_svn_password_{}", child.id);
                        let _ = db.conn().execute(
                            "INSERT OR REPLACE INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)",
                            rusqlite::params![child_key, password, now],
                        );
                        queue.push(child.id);
                        propagated += 1;
                    }
                }
            }
            if propagated > 0 {
                tracing::info!(
                    repo_id = %id,
                    propagated_to_children = propagated,
                    "SVN password propagated to descendants"
                );
            }
        }
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Branch pair endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateBranchPairRequest {
    svn_branch: String,
    git_branch: String,
    #[serde(default)]
    skip_import: bool,
    /// Explicit compatibility flag for the historical skip_import / start-from-now
    /// request. Even when set, this slice never applies watermarks or publishes.
    #[serde(default)]
    compatibility_skip_import: bool,
    /// Preview/plan only. Defaults to true so this slice cannot publish.
    #[serde(default = "default_true")]
    dry_run: bool,
    /// Alias for dry_run. When true, forces preview mode.
    #[serde(default)]
    preview: Option<bool>,
    /// Auto-create the SVN branch via `svn copy` from the parent's branch.
    /// Ignored in preview; this slice does not copy.
    auto_create_svn_branch: Option<bool>,
    /// Auto-create the Git branch via the provider API from the parent's branch.
    /// Ignored in preview; this slice does not create Git refs.
    auto_create_git_branch: Option<bool>,
}

async fn create_branch_pair(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CreateBranchPairRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;
    reject_held_import(db, &id)?;

    // Load parent repo
    let parent = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound(format!("repository {} not found", id)))?;
    if db
        .managed_remove_blocks_new_work(&id)
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        return Err(AppError::BadRequest(
            "parent removal blocks a new child registration".into(),
        ));
    }

    // Check nesting depth (max 4 levels: root + 3 children)
    let ancestors = db
        .resolve_ancestor_chain(&parent.id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;
    if ancestors.len() >= 3 {
        return Err(AppError::BadRequest(
            "maximum nesting depth of 4 exceeded".into(),
        ));
    }

    // --- Input validation ---
    let git_branch = body.git_branch.trim().to_string();
    let svn_branch = body.svn_branch.trim().to_string();

    if git_branch.is_empty() {
        return Err(AppError::BadRequest("git_branch is required".into()));
    }
    if svn_branch.is_empty() {
        return Err(AppError::BadRequest("svn_branch is required".into()));
    }

    // Length limits
    if git_branch.len() > 200 {
        return Err(AppError::BadRequest(
            "git_branch exceeds 200 character limit".into(),
        ));
    }
    if svn_branch.len() > 200 {
        return Err(AppError::BadRequest(
            "svn_branch exceeds 200 character limit".into(),
        ));
    }

    // Character validation: alphanumeric, dots, underscores, hyphens, slashes
    let valid_branch_chars = |s: &str| -> bool {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    };
    if !valid_branch_chars(&git_branch) {
        return Err(AppError::BadRequest(
            "git_branch contains invalid characters (only alphanumeric, '.', '_', '-', '/' allowed)".into(),
        ));
    }
    if !valid_branch_chars(&svn_branch) {
        return Err(AppError::BadRequest(
            "svn_branch contains invalid characters (only alphanumeric, '.', '_', '-', '/' allowed)".into(),
        ));
    }

    // Path traversal prevention
    if git_branch.contains("..") || svn_branch.contains("..") {
        return Err(AppError::BadRequest(
            "branch names must not contain '..'".into(),
        ));
    }

    // Structural validation
    if git_branch.contains("//") || svn_branch.contains("//") {
        return Err(AppError::BadRequest(
            "branch names must not contain '//'".into(),
        ));
    }
    if git_branch.starts_with('/')
        || git_branch.ends_with('/')
        || svn_branch.starts_with('/')
        || svn_branch.ends_with('/')
    {
        return Err(AppError::BadRequest(
            "branch names must not start or end with '/'".into(),
        ));
    }
    if git_branch.starts_with('-') || svn_branch.starts_with('-') {
        return Err(AppError::BadRequest(
            "branch names must not start with '-'".into(),
        ));
    }

    // Git-specific reserved names
    if git_branch.ends_with(".lock") || git_branch == "HEAD" || git_branch.starts_with("refs/") {
        return Err(AppError::BadRequest(
            "git_branch uses a reserved name".into(),
        ));
    }

    // Parent must be enabled
    if !parent.enabled {
        return Err(AppError::BadRequest(
            "cannot create branch pair from a disabled repository".into(),
        ));
    }

    // Duplicate check: ensure no existing child has the same git_branch
    let existing_children = db
        .list_child_repositories(&parent.id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;
    if existing_children.iter().any(|c| c.git_branch == git_branch) {
        return Err(AppError::BadRequest(format!(
            "a branch pair with git_branch '{}' already exists",
            git_branch
        )));
    }

    // #67 admission/preview: prove SVN-origin lineage before any copy,
    // checkpoint, remote mutation, or scheduler-active child row.
    let dry_run = body.preview.unwrap_or(body.dry_run);
    let request = LatePairRequest {
        parent_id: parent.id.clone(),
        git_branch: git_branch.clone(),
        svn_branch: svn_branch.clone(),
        skip_import: body.skip_import,
        compatibility_skip_import: body.compatibility_skip_import,
        dry_run,
    };
    let mappings = collect_verified_mappings(db, &parent.id)
        .map_err(|e| AppError::Internal(format!("failed to read verified mappings: {e}")))?;
    let git_workdir = state
        .config
        .daemon
        .data_dir
        .join("repos")
        .join(&parent.id)
        .join("git-repo");
    let workdir = git_workdir
        .join(".git")
        .exists()
        .then_some(git_workdir.as_path())
        .or_else(|| {
            git_workdir
                .join("HEAD")
                .exists()
                .then_some(git_workdir.as_path())
        });

    let mut provider_tip = None;
    if workdir.is_none() && !mappings.is_empty() {
        let git_token = db
            .resolve_credential_chain(&parent.id, "secret_git_token")
            .unwrap_or_default();
        let provider = match parent.git_provider.as_str() {
            "gitea" => reposync_core::config::GitProvider::Gitea,
            _ => reposync_core::config::GitProvider::GitHub,
        };
        let github_client = reposync_core::git::github::GitHubClient::new(
            &parent.git_api_url,
            &git_token,
            provider,
        );
        if let Ok(sha) = github_client
            .get_branch_sha(&parent.git_repo, &git_branch)
            .await
        {
            provider_tip = Some(sha);
        }
    }

    let admission = evaluate_admission(&mappings, workdir, provider_tip.as_deref(), &request);
    let mut plan = match admission {
        Ok(plan) => plan,
        Err(refuse) => {
            info!(
                parent_id = %parent.id,
                reason = %refuse.reason,
                "late-pair admission refused before remote mutation"
            );
            return Err(AppError::BadRequest(refuse.error_message()));
        }
    };

    attach_svn_probe(&parent, db, &mut plan).await;
    if body.auto_create_svn_branch.unwrap_or(true) || body.auto_create_git_branch.unwrap_or(true) {
        plan.unknowns.push(
            "auto_create_svn_branch / auto_create_git_branch are ignored in this preview slice; no remote refs are created".into(),
        );
    }
    info!(
        parent_id = %parent.id,
        git_branch = %git_branch,
        svn_branch = %svn_branch,
        git_tip = ?plan.git_tip,
        baseline = ?plan.svn_source_revision,
        "late-pair preview admitted; no child row or remote mutation"
    );

    Ok(Json(serde_json::to_value(&plan).map_err(|e| {
        AppError::Internal(format!("serialization error: {}", e))
    })?))
}

async fn attach_svn_probe(
    parent: &reposync_core::models::Repository,
    db: &Database,
    plan: &mut reposync_core::late_pair::LatePairPlan,
) {
    let svn_password = db
        .resolve_credential_chain(&parent.id, "secret_svn_password")
        .unwrap_or_default();
    let parent_svn_url = if parent.svn_branch.is_empty() {
        parent.svn_url.clone()
    } else {
        format!(
            "{}/{}",
            parent.svn_url.trim_end_matches('/'),
            parent.svn_branch.trim_start_matches('/')
        )
    };
    let target_url = format!(
        "{}/{}",
        parent.svn_url.trim_end_matches('/'),
        plan.svn_branch.trim_start_matches('/')
    );
    let parent_client = SvnClient::new(&parent_svn_url, &parent.svn_username, &svn_password);
    let target_client = SvnClient::new(&target_url, &parent.svn_username, &svn_password);
    let (probe, pending) = probe_svn_target(
        &parent_client,
        &target_client,
        &parent_svn_url,
        &target_url,
        plan.svn_source_revision,
    )
    .await;
    plan.existing_svn_target = probe;
    plan.pending_svn = pending;
    plan.svn_target_revision = plan.existing_svn_target.revision;
    if plan.existing_svn_target.exists {
        plan.unknowns.push(
            "existing SVN target is not treated as equivalent; reconcile/replay is later".into(),
        );
        plan.proposed_svn_copy_source_revision = None;
        plan.conflicts.push(
            "existing SVN target requires lineage/tree verification before any copy or checkpoint"
                .into(),
        );
    }
}

#[derive(Deserialize)]
struct RefreshPreviewRequest {
    #[serde(default)]
    operation: String,
    #[serde(default)]
    execute: bool,
    /// `false` is an execute attempt and is refused. Omitted means preview.
    #[serde(default)]
    dry_run: Option<bool>,
}

async fn preview_pair_refresh(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RefreshPreviewRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let operation = parse_operation(&body.operation).map_err(|other| {
        AppError::BadRequest(format!(
            "unknown_refresh_operation: '{other}' is not a refresh mode. Supported preview is update_pair_from_parent. reanchor is NOT IMPLEMENTED."
        ))
    })?;
    if operation == RefreshOperation::Reanchor {
        let (reason, detail) = reanchor_refusal();
        info!(pair_id = %id, "pair refresh re-anchor refused; not implemented");
        return Err(AppError::BadRequest(format_refusal(reason, detail)));
    }

    let db = &state.db;
    let pair = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("repository {id} not found")))?;
    let parent_id = pair.parent_id.clone().ok_or_else(|| {
        AppError::BadRequest(
            "not_a_branch_pair: coordinated refresh applies to an existing child pair; this repository has no parent"
                .into(),
        )
    })?;
    let parent = db
        .get_repository(&parent_id)
        .map_err(|e| AppError::Internal(format!("database error: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("parent repository {parent_id} not found")))?;

    let pair_mappings = collect_verified_mappings(db, &pair.id)
        .map_err(|e| AppError::Internal(format!("failed to read pair mappings: {e}")))?;
    let parent_mappings = collect_verified_mappings(db, &parent.id)
        .map_err(|e| AppError::Internal(format!("failed to read parent mappings: {e}")))?;
    let parent_dir = pair_refresh_git_dir(&state, &parent.id);
    let pair_dir = pair_refresh_git_dir(&state, &pair.id);
    let same_remote = pair.git_repo == parent.git_repo && pair.git_api_url == parent.git_api_url;
    let facts = analyze_git_preview(
        GitLayout {
            parent_dir: parent_dir.as_deref(),
            pair_dir: pair_dir.as_deref(),
            same_remote,
        },
        &pair_mappings,
        &parent_mappings,
        &pair.git_branch,
        &parent.git_branch,
    );

    let parent_password = db
        .resolve_credential_chain(&parent.id, "secret_svn_password")
        .unwrap_or_default();
    let pair_password = db
        .resolve_credential_chain(&pair.id, "secret_svn_password")
        .unwrap_or_default();
    let parent_url = branch_svn_url(&parent.svn_url, &parent.svn_branch);
    let pair_url = branch_svn_url(&pair.svn_url, &pair.svn_branch);
    let parent_client = SvnClient::new(&parent_url, &parent.svn_username, &parent_password);
    let pair_client = SvnClient::new(&pair_url, &pair.svn_username, &pair_password);
    let parent_pin = read_svn_pin(
        parent_client.info().await,
        parent_client.last_changed_revision().await,
    );
    let pair_pin = read_svn_pin(
        pair_client.info().await,
        pair_client.last_changed_revision().await,
    );
    let (parent_revision, parent_uuid, parent_missing, parent_svn_note) = parent_pin;
    let (pair_revision, pair_uuid, pair_missing, pair_svn_note) = pair_pin;
    let mut notes = facts.notes;
    if let Some(note) = parent_svn_note {
        notes.push(format!("parent SVN info: {note}"));
    }
    if let Some(note) = pair_svn_note {
        notes.push(format!("pair SVN info: {note}"));
    }

    let observations = RefreshObservations {
        pair_id: pair.id.clone(),
        parent_id: parent.id.clone(),
        pair_git_branch: pair.git_branch.clone(),
        parent_git_branch: parent.git_branch.clone(),
        pair_git_tip: facts.pair.remote_tip.clone(),
        parent_git_tip: facts.parent.remote_tip.clone(),
        pair_local_tip: facts.pair.local_tip.clone(),
        parent_local_tip: facts.parent.local_tip.clone(),
        pair_baseline: facts.pair_baseline.clone(),
        parent_baseline: facts.parent_baseline.clone(),
        pair_pending_git: facts.pair_pending.clone(),
        parent_pending_git: facts.parent_pending.clone(),
        svn_uuid: parent_uuid.clone(),
        pair_svn_uuid: pair_uuid,
        pair_svn_path: pair.svn_branch.clone(),
        parent_svn_path: parent.svn_branch.clone(),
        pair_svn_url: pair_url,
        parent_svn_url: parent_url,
        pair_svn_revision: pair_revision,
        parent_svn_revision: parent_revision,
        pair_svn_missing: pair_missing,
        parent_svn_missing: parent_missing,
        pair_local_ahead: facts.pair.local_ahead.clone(),
        parent_local_ahead: facts.parent.local_ahead.clone(),
        pair_local_ahead_complete: facts.pair.local_ahead_complete,
        parent_local_ahead_complete: facts.parent.local_ahead_complete,
        pair_local_diverged: facts.pair.local_diverged,
        parent_local_diverged: facts.parent.local_diverged,
        notes,
    };
    let plan = build_preview(&observations);
    info!(
        pair_id = %pair.id,
        parent_id = %parent.id,
        plan_digest = %plan.plan_digest,
        pins_complete = plan.pins_complete,
        "pair refresh preview; no durable job and no external write"
    );
    if execution_requested(body.execute, body.dry_run) {
        let (reason, detail) = execute_refusal(Some(&plan.plan_digest));
        return Err(AppError::BadRequest(format_refusal(&reason, &detail)));
    }
    Ok(Json(serde_json::to_value(&plan).map_err(|e| {
        AppError::Internal(format!("serialization error: {e}"))
    })?))
}

fn pair_refresh_git_dir(state: &AppState, repo_id: &str) -> Option<std::path::PathBuf> {
    let path = state
        .config
        .daemon
        .data_dir
        .join("repos")
        .join(repo_id)
        .join("git-repo");
    (path.join(".git").exists() || path.join("HEAD").exists()).then_some(path)
}

fn read_svn_pin(
    info: Result<reposync_core::svn::SvnInfo, reposync_core::errors::SvnError>,
    last_changed: Result<i64, reposync_core::errors::SvnError>,
) -> (Option<i64>, Option<String>, bool, Option<String>) {
    match info {
        Ok(info) => {
            let (revision, note) = match last_changed {
                Ok(revision) => (Some(revision), None),
                Err(err) => (
                    Some(info.latest_rev),
                    Some(format!(
                        "last-changed revision unreadable ({err}); pinned repository HEAD instead"
                    )),
                ),
            };
            (revision, Some(info.uuid), false, note)
        }
        Err(err) => {
            let text = err.to_string();
            let missing = svn_path_missing(&text);
            (None, None, missing, Some(text))
        }
    }
}

async fn list_branch_pairs(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;

    let children = db
        .list_child_repositories(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

    let result: Vec<serde_json::Value> = children
        .into_iter()
        .map(|r| serde_json::to_value(r).unwrap_or(serde_json::Value::Null))
        .collect();

    Ok(Json(result))
}

#[derive(Deserialize)]
struct DeleteBranchPairQuery {
    #[serde(default = "default_true")]
    delete_git: bool,
    #[serde(default = "default_true")]
    delete_svn: bool,
}
async fn delete_branch_pair(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Query(opts): Query<DeleteBranchPairQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;

    // Load the branch pair
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound(format!("repository {} not found", id)))?;

    // Only branch pairs (with a parent) can be hard-deleted
    let parent_id = repo.parent_id.as_ref().ok_or_else(|| {
        AppError::BadRequest("only branch pairs can be deleted (this is a root repository)".into())
    })?;

    // Block if it has children
    let children = db
        .list_child_repositories(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;
    if !children.is_empty() {
        return Err(AppError::BadRequest(format!(
            "cannot delete: branch pair has {} child pair(s) — delete them first",
            children.len()
        )));
    }

    // Acquire busy guard to prevent sync/import racing
    reject_held_import(&state.db, &id)?;
    let _busy_guard = reposync_core::busy::try_acquire(&id).ok_or_else(|| {
        AppError::BadRequest("branch pair is currently busy (sync or import in progress)".into())
    })?;

    // Load parent for credential resolution
    let parent = db
        .get_repository(parent_id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::Internal("parent repository not found".into()))?;

    let mut warnings: Vec<String> = Vec::new();

    // Optionally delete Git branch on remote
    if opts.delete_git && !repo.git_branch.is_empty() {
        let git_token = db
            .resolve_credential_chain(&parent.id, "secret_git_token")
            .unwrap_or_default();
        let provider = match parent.git_provider.as_str() {
            "gitea" => reposync_core::config::GitProvider::Gitea,
            _ => reposync_core::config::GitProvider::GitHub,
        };
        let github_client = reposync_core::git::github::GitHubClient::new(
            &parent.git_api_url,
            &git_token,
            provider,
        );
        match github_client
            .delete_branch(&parent.git_repo, &repo.git_branch)
            .await
        {
            Ok(()) => info!(branch = %repo.git_branch, "deleted Git branch"),
            Err(e) => {
                let msg = format!("failed to delete Git branch '{}': {}", repo.git_branch, e);
                warn!(%msg);
                warnings.push(msg);
            }
        }
    }

    // Optionally delete SVN branch on remote
    if opts.delete_svn && !repo.svn_branch.is_empty() {
        let svn_password = db
            .resolve_credential_chain(&parent.id, "secret_svn_password")
            .unwrap_or_default();
        let svn_client = reposync_core::svn::SvnClient::new(
            &parent.svn_url,
            &parent.svn_username,
            &svn_password,
        );
        match svn_client.delete_branch(&repo.svn_branch).await {
            Ok(()) => info!(branch = %repo.svn_branch, "deleted SVN branch"),
            Err(e) => {
                let msg = format!("failed to delete SVN branch '{}': {}", repo.svn_branch, e);
                warn!(%msg);
                warnings.push(msg);
            }
        }
    }

    // Delete local filesystem (git clone)
    let repo_dir = state.config.daemon.data_dir.join("repos").join(&id);
    if repo_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&repo_dir) {
            let msg = format!("failed to remove local repo dir: {}", e);
            warn!(%msg);
            warnings.push(msg);
        } else {
            debug!(path = %repo_dir.display(), "removed local repo directory");
        }
    }

    // Hard-delete all DB records
    let repo_name = repo.name.clone();
    db.hard_delete_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error during deletion: {}", e)))?;

    // Audit log (written to parent's audit trail since the child repo no longer exists)
    let _ = db.insert_audit_log_with_repo(AuditLogInput {
        action: "deleted_branch_pair",
        direction: None,
        svn_rev: None,
        git_sha: None,
        author: None,
        details: Some(&format!(
            "Deleted branch pair '{}' (git: {}, svn: {})",
            repo_name, repo.git_branch, repo.svn_branch
        )),
        success: true,
        repo_id: Some(parent_id),
    });

    info!(
        repo_id = %id,
        repo_name = %repo_name,
        git_branch = %repo.git_branch,
        svn_branch = %repo.svn_branch,
        warnings = ?warnings,
        "branch pair deleted"
    );

    // Send Teams notification for deletion
    let teams_url = db
        .get_state("teams_webhook_url")
        .ok()
        .flatten()
        .filter(|v| !v.is_empty());
    if let Some(url) = teams_url {
        let card =
            reposync_core::notify::teams::format_branch_deleted(&repo_name, &repo.git_branch);
        let notifier = reposync_core::notify::teams::TeamsNotifier::new(url);
        let _ = notifier.send_card(card).await;
    }

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": format!("Branch pair '{}' deleted", repo_name),
        "warnings": warnings,
    })))
}

/// Optional form-value overrides for the SVN test endpoint.
/// When supplied, these take precedence over whatever is in the database so
/// that the user can test unsaved edits.
#[derive(Deserialize, Default)]
#[serde(default)]
struct TestRepoSvnBody {
    svn_url: Option<String>,
    svn_branch: Option<String>,
    svn_username: Option<String>,
    svn_password: Option<String>,
}

/// Optional form-value overrides for the Git test endpoint.
#[derive(Deserialize, Default)]
#[serde(default)]
struct TestRepoGitBody {
    git_api_url: Option<String>,
    git_repo: Option<String>,
    git_token: Option<String>,
}

/// Test SVN connection using form overrides (if provided) or stored credentials.
async fn test_repo_svn(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<TestRepoSvnBody>>,
) -> Result<Json<serde_json::Value>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("db error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repo not found".into()))?;

    let overrides = body.map(|Json(b)| b).unwrap_or_default();

    // Prefer form-supplied values; fall back to saved values.
    let svn_url_base = overrides
        .svn_url
        .filter(|s| !s.is_empty())
        .unwrap_or(repo.svn_url);
    let svn_branch = overrides
        .svn_branch
        .filter(|s| !s.is_empty())
        .unwrap_or(repo.svn_branch);
    let svn_username = overrides
        .svn_username
        .filter(|s| !s.is_empty())
        .unwrap_or(repo.svn_username);

    // Password: prefer form value, then stored per-repo → parent → global.
    let password = overrides
        .svn_password
        .filter(|v| !v.is_empty())
        .or_else(|| db.resolve_credential_chain(&id, "secret_svn_password"))
        .unwrap_or_default();

    let svn_url = if svn_branch.is_empty() {
        svn_url_base
    } else {
        format!(
            "{}/{}",
            svn_url_base.trim_end_matches('/'),
            svn_branch.trim_start_matches('/')
        )
    };

    let result = tokio::process::Command::new("svn")
        .args([
            "info",
            "--non-interactive",
            "--username",
            &svn_username,
            "--password",
            &password,
            &svn_url,
        ])
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let info = stdout
                .lines()
                .find(|l| l.starts_with("Repository Root:") || l.starts_with("URL:"))
                .unwrap_or("SVN server responded successfully");
            Ok(Json(
                serde_json::json!({"ok": true, "message": info.trim()}),
            ))
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = stderr
                .lines()
                .find(|l| l.contains("E1") || l.contains("Unable") || l.contains("Authentication"))
                .unwrap_or("SVN command failed");
            Ok(Json(
                serde_json::json!({"ok": false, "message": msg.trim()}),
            ))
        }
        Err(e) => Ok(Json(
            serde_json::json!({"ok": false, "message": format!("Failed to run svn: {}", e)}),
        )),
    }
}

/// Test Git connection using form overrides (if provided) or stored credentials.
async fn test_repo_git(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<TestRepoGitBody>>,
) -> Result<Json<serde_json::Value>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("db error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repo not found".into()))?;

    let overrides = body.map(|Json(b)| b).unwrap_or_default();

    let git_api_url = overrides
        .git_api_url
        .filter(|s| !s.is_empty())
        .unwrap_or(repo.git_api_url);
    let git_repo = overrides
        .git_repo
        .filter(|s| !s.is_empty())
        .unwrap_or(repo.git_repo);

    // Token: prefer form value, then stored per-repo → parent → global.
    let token = overrides
        .git_token
        .filter(|v| !v.is_empty())
        .or_else(|| db.resolve_credential_chain(&id, "secret_git_token"));

    let check_url = format!("{}/repos/{}", git_api_url.trim_end_matches('/'), git_repo);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(format!("http client error: {}", e)))?;

    let mut req = client.get(&check_url);
    if let Some(ref tok) = token {
        req = req.header("Authorization", format!("token {}", tok));
    }

    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                let name = json
                    .get("full_name")
                    .or_else(|| json.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&git_repo);
                Ok(Json(
                    serde_json::json!({"ok": true, "message": format!("Repository found: {}", name)}),
                ))
            } else {
                Ok(Json(
                    serde_json::json!({"ok": true, "message": "Repository is accessible"}),
                ))
            }
        }
        Ok(resp) => {
            let status = resp.status();
            Ok(Json(
                serde_json::json!({"ok": false, "message": format!("HTTP {} — check credentials and URL", status)}),
            ))
        }
        Err(e) => Ok(Json(
            serde_json::json!({"ok": false, "message": format!("Connection failed: {}", e)}),
        )),
    }
}

// ---------------------------------------------------------------------------
// Skip Commit — exact per-commit disposition (#66 / RS-C04)
// ---------------------------------------------------------------------------

fn repo_git_workdir(state: &AppState, repo_id: &str) -> std::path::PathBuf {
    state
        .config
        .daemon
        .data_dir
        .join("repos")
        .join(repo_id)
        .join("git-repo")
}

fn workdir_ready(path: &std::path::Path) -> bool {
    path.join(".git").exists() || path.join("HEAD").exists()
}

async fn observed_remote_tip(
    repo: &reposync_core::models::Repository,
    db: &Database,
    workdir: Option<&std::path::Path>,
) -> Result<Option<String>, AppError> {
    let git_token = db
        .resolve_credential_chain(&repo.id, "secret_git_token")
        .unwrap_or_default();
    let provider = match repo.git_provider.as_str() {
        "gitea" => reposync_core::config::GitProvider::Gitea,
        _ => reposync_core::config::GitProvider::GitHub,
    };
    let github_client =
        reposync_core::git::github::GitHubClient::new(&repo.git_api_url, &git_token, provider);
    match github_client
        .get_branch_sha(&repo.git_repo, &repo.git_branch)
        .await
    {
        Ok(sha) => Ok(Some(sha)),
        Err(e) => {
            warn!(
                repo_id = %repo.id,
                error = %e,
                "could not fetch remote tip for skip-commit; falling back to local workdir"
            );
            if let Some(workdir) = workdir {
                if workdir_ready(workdir) {
                    return reposync_core::skip_commit::git_success(
                        workdir,
                        &["rev-parse", "HEAD"],
                    )
                    .map(|output| {
                        Some(
                            String::from_utf8_lossy(&output.stdout)
                                .trim()
                                .to_ascii_lowercase(),
                        )
                    })
                    .map_err(|refuse| AppError::Conflict(refuse.message()));
                }
            }
            Ok(None)
        }
    }
}

async fn skip_commit_context(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    let workdir = repo_git_workdir(&state, &id);
    if !workdir_ready(&workdir) {
        return Err(AppError::Conflict(format!(
            "{}: local Git workdir is unavailable for exact skip planning",
            skip_reason::WORKDIR_UNAVAILABLE
        )));
    }

    let remote_tip = observed_remote_tip(&repo, db, Some(&workdir)).await?;
    let context = build_skip_context(db, &id, &workdir, remote_tip.as_deref())
        .map_err(|refuse| AppError::Conflict(refuse.message()))?;
    Ok(Json(serde_json::json!({ "ok": true, "context": context })))
}

async fn skip_commit(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SkipCommitRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;
    reject_held_import(db, &id)?;
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    let workdir = repo_git_workdir(&state, &id);
    if !workdir_ready(&workdir) {
        return Err(AppError::Conflict(format!(
            "{}: local Git workdir is unavailable; exact skip requires proven ancestry",
            skip_reason::WORKDIR_UNAVAILABLE
        )));
    }

    let remote_tip = observed_remote_tip(&repo, db, Some(&workdir))
        .await?
        .ok_or_else(|| {
            AppError::Conflict(format!(
                "{}: remote tip could not be observed for exact skip",
                skip_reason::TIP_MISMATCH
            ))
        })?;

    let bridge_tip = reposync_core::skip_commit::git_success(&workdir, &["rev-parse", "HEAD"])
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .to_ascii_lowercase()
        })
        .ok();

    let outcome = execute_exact_skip(db, &id, &workdir, &remote_tip, bridge_tip.as_deref(), &body)
        .map_err(|refuse| AppError::Conflict(refuse.message()))?;

    let _ = db.reset_consecutive_errors(&id);
    let _ = db.conn().execute(
        "UPDATE repositories SET sync_status = 'idle' WHERE id = ?1",
        rusqlite::params![&id],
    );

    let excluded_summary = outcome
        .excluded_commits
        .iter()
        .map(|sha| sha.chars().take(8).collect::<String>())
        .collect::<Vec<_>>()
        .join(", ");
    let _ = db.insert_audit_log_with_repo(AuditLogInput {
        action: "skip_commit",
        direction: Some("git_to_svn"),
        svn_rev: None,
        git_sha: Some(&outcome.new_cursor),
        author: None,
        details: Some(&format!(
            "Exact skip from {} to {} excluded [{}]; {} pending commit(s) remain",
            &outcome.old_cursor[..8.min(outcome.old_cursor.len())],
            &outcome.new_cursor[..8.min(outcome.new_cursor.len())],
            excluded_summary,
            outcome.remaining_pending.len()
        )),
        success: true,
        repo_id: Some(&id),
    });

    info!(
        repo_id = %id,
        old_cursor = %outcome.old_cursor,
        new_cursor = %outcome.new_cursor,
        excluded = outcome.excluded_commits.len(),
        remaining_pending = outcome.remaining_pending.len(),
        "exact skip-commit accepted"
    );

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "Selected commits excluded; frontier advanced without adopting live HEAD",
        "old_sha": outcome.old_cursor,
        "new_sha": outcome.new_cursor,
        "excluded_commits": outcome.excluded_commits,
        "remaining_pending": outcome.remaining_pending,
    })))
}

// ---------------------------------------------------------------------------
// Retry — resume a circuit-broken repo
// ---------------------------------------------------------------------------

async fn retry_repo(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;
    reject_held_import(db, &id)?;
    let _ = db.reset_consecutive_errors(&id);
    let _ = db.conn().execute(
        "UPDATE repositories SET sync_status = 'idle' WHERE id = ?1",
        rusqlite::params![&id],
    );

    info!(repo_id = %id, "retry: circuit breaker reset, sync resumed");

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "Sync resumed",
    })))
}

// ---------------------------------------------------------------------------
// Git Hook Download — serve a pre-commit hook script
// ---------------------------------------------------------------------------

async fn get_pre_commit_hook(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<axum::response::Response, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let db = &state.db;
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    let allowed: Vec<String> = repo
        .allowed_paths
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let blocked: Vec<String> = repo
        .blocked_patterns
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let mut script = String::from("#!/bin/bash\n");
    script.push_str("# RepoSync pre-commit hook — validates file paths against SVN rules\n");
    script.push_str(
        "# Install: cp this file .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit\n",
    );
    script.push_str("# Or: mkdir -p .githooks && cp this file .githooks/pre-commit && git config core.hooksPath .githooks\n\n");

    if allowed.is_empty() && blocked.is_empty() {
        script.push_str("# No path rules configured for this repository.\nexit 0\n");
    } else {
        script.push_str("ERRORS=0\n\n");

        if !allowed.is_empty() {
            script.push_str("# Allowed path prefixes\n");
            script.push_str("ALLOWED_PATHS=(");
            for (i, p) in allowed.iter().enumerate() {
                if i > 0 {
                    script.push(' ');
                }
                script.push_str(&format!("\"{}\"", p));
            }
            script.push_str(")\n\n");

            script.push_str("for file in $(git diff --cached --name-only --diff-filter=ACM); do\n");
            script.push_str("  ALLOWED=0\n");
            script.push_str("  for prefix in \"${ALLOWED_PATHS[@]}\"; do\n");
            script.push_str("    if [[ \"$file\" == \"$prefix\"* ]]; then\n");
            script.push_str("      ALLOWED=1\n");
            script.push_str("      break\n");
            script.push_str("    fi\n");
            script.push_str("  done\n");
            script.push_str("  if [ $ALLOWED -eq 0 ]; then\n");
            script.push_str(
                "    echo \"ERROR: '$file' is not under an allowed path: ${ALLOWED_PATHS[*]}\"\n",
            );
            script.push_str("    ERRORS=$((ERRORS + 1))\n");
            script.push_str("  fi\n");
            script.push_str("done\n\n");
        }

        if !blocked.is_empty() {
            script.push_str("# Blocked patterns\n");
            for pattern in &blocked {
                script.push_str(&format!(
                    "for file in $(git diff --cached --name-only --diff-filter=ACM); do\n  case \"$file\" in\n    {}) echo \"ERROR: '$file' matches blocked pattern '{}'\"; ERRORS=$((ERRORS + 1));;\n  esac\ndone\n\n",
                    pattern, pattern
                ));
            }
        }

        script.push_str("if [ $ERRORS -gt 0 ]; then\n");
        script.push_str("  echo \"\"\n");
        script.push_str("  echo \"Commit blocked: $ERRORS file(s) violate SVN path rules.\"\n");
        script.push_str("  echo \"These files would be rejected by the SVN server.\"\n");
        script.push_str("  exit 1\n");
        script.push_str("fi\n");
    }

    Ok(axum::response::Response::builder()
        .status(200)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Content-Disposition", "attachment; filename=\"pre-commit\"")
        .body(axum::body::Body::from(script))
        .unwrap())
}
