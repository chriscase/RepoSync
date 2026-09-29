//! Repository management API endpoints (multi-repo support).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use reposync_core::db::import_operations::ImportOperationState;
use reposync_core::db::queries::AuditLogInput;
use reposync_core::db::Database;
use reposync_core::errors::DatabaseError;
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::{self, ImportConfig, ImportPhase, ImportProgress, ImportRunState};
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
            status: "unknown".to_string(),
        }
    }
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
        .route("/api/repos/:id/sync", post(trigger_sync))
        .route("/api/repos/:id/import", post(start_repo_import))
        .route("/api/repos/:id/import/status", get(repo_import_status))
        .route(
            "/api/repos/:id/import/:operation_id/cancel",
            post(cancel_repo_import),
        )
        .route(
            "/api/repos/:id/import/cancel",
            post(cancel_repo_import_without_id),
        )
        .route("/api/repos/:id/credentials", get(get_credentials))
        .route("/api/repos/:id/credentials", post(save_credentials))
        .route("/api/repos/:id/branches", post(create_branch_pair))
        .route("/api/repos/:id/branches", get(list_branch_pairs))
        .route("/api/repos/:id/branch-pair", delete(delete_branch_pair))
        .route("/api/repos/:id/test-svn", post(test_repo_svn))
        .route("/api/repos/:id/test-git", post(test_repo_git))
        .route("/api/repos/:id/skip-commit", post(skip_commit))
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

    Ok(Json(RepoDetail::from(repo)))
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

    // Soft delete: disable the repository rather than removing it.
    let existing = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    let disabled = reposync_core::models::Repository {
        enabled: false,
        updated_at: Utc::now().to_rfc3339(),
        ..existing
    };

    db.update_repository(&disabled)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "repository disabled",
    })))
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
}

impl Drop for ImportPreparationGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let outcome = self
            .db
            .get_import_operation(&self.repo_id, &self.operation_id);
        let (state, detail) = match outcome {
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
}

async fn start_repo_import(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    axum::extract::Query(import_query): axum::extract::Query<ImportQuery>,
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

    // This cancellation increment cannot account for the legacy reset's
    // destructive local cleanup and force-push. Refuse before enrollment or
    // any workdir, credential, checkpoint, mapping, or target mutation.
    if import_query.reset {
        return Err(AppError::BadRequest(
            "Reset & Reimport is unavailable while safe import cancellation is in effect; request a separately reviewed recovery plan".into(),
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

    // 2b. Acquire the process-wide busy slot so that no scheduler cycle
    // can touch this repo's working tree while the import runs. If the
    // scheduler is currently in the middle of a cycle we wait briefly
    // for it to finish before starting the import. The guard is moved
    // into the background task and released when the import completes.
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
    let fingerprint_source = serde_json::json!({
        "svn_url":repo.svn_url, "svn_branch":repo.svn_branch,
        "git_api_url":repo.git_api_url, "git_repo":repo.git_repo, "git_branch":repo.git_branch,
        "workdir":workdir.display().to_string(), "allowed_paths":repo.allowed_paths,
        "blocked_patterns":repo.blocked_patterns, "lfs_threshold_mb":repo.lfs_threshold_mb,
        "sync_mode":repo.sync_mode, "auto_merge":repo.auto_merge,
    })
    .to_string();
    let fingerprint = hex::encode(Sha256::digest(fingerprint_source.as_bytes()));
    let operation = db
        .create_import_operation(&id, &user_id, &request_id, &fingerprint)
        .map_err(import_write_error)?;
    let operation_id = operation.id.clone();
    let mut preparation_guard = ImportPreparationGuard {
        db,
        repo_id: id.clone(),
        operation_id: operation_id.clone(),
        armed: true,
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
                .map_err(|e| AppError::Internal(format!("Git clone stopped or timed out: {e}")))?;
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
                    Ok(_) => {
                        import::run_full_import(
                            &svn_client,
                            &git_client,
                            &identity_mapper,
                            &import_db,
                            &file_policy,
                            &import_config,
                            ImportRunState {
                                progress: progress.clone(),
                                ws_broadcast: ws_broadcast.clone(),
                                repo_id: Some(repo_id_clone.clone()),
                                operation_id: Some(worker_operation_id.clone()),
                                cancel_signal: Some(cancel_signal),
                            },
                        )
                        .await
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
                let detail = format!("import stopped with error; inspect local work: {e}");
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

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "Import started",
        "operation_id": operation_id,
        "lifecycle": "queued",
    })))
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
    /// Auto-create the SVN branch via `svn copy` from the parent's branch.
    /// Defaults to true when not specified.
    auto_create_svn_branch: Option<bool>,
    /// Auto-create the Git branch via the provider API from the parent's branch.
    /// Defaults to true when not specified.
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

    // Auto-create SVN branch if requested (default: true)
    let auto_svn = body.auto_create_svn_branch.unwrap_or(true);
    let auto_git = body.auto_create_git_branch.unwrap_or(true);

    if auto_svn {
        let svn_password = db
            .resolve_credential_chain(&parent.id, "secret_svn_password")
            .unwrap_or_default();
        let svn_client = SvnClient::new(&parent.svn_url, &parent.svn_username, &svn_password);
        // Get current HEAD rev for the copy
        let parent_svn_url = if parent.svn_branch.is_empty() {
            parent.svn_url.clone()
        } else {
            format!(
                "{}/{}",
                parent.svn_url.trim_end_matches('/'),
                parent.svn_branch.trim_start_matches('/')
            )
        };
        let info_client = SvnClient::new(&parent_svn_url, &parent.svn_username, &svn_password);
        match info_client.info().await {
            Ok(info) => {
                // Determine branches_path and branch name from svn_branch
                // e.g., "branches/fix-123" → branches_path="branches", name="fix-123"
                let (branches_path, branch_name) = if let Some(pos) = svn_branch.rfind('/') {
                    (&svn_branch[..pos], &svn_branch[pos + 1..])
                } else {
                    ("branches", svn_branch.as_str())
                };
                match svn_client
                    .create_branch(
                        branch_name,
                        &parent.svn_branch,
                        branches_path,
                        info.latest_rev,
                    )
                    .await
                {
                    Ok(()) => {
                        info!(
                            branch = %svn_branch,
                            from = %parent.svn_branch,
                            rev = info.latest_rev,
                            "auto-created SVN branch"
                        );
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        // Treat "already exists" as success
                        if err_str.contains("already exists") || err_str.contains("E160020") {
                            info!(branch = %svn_branch, "SVN branch already exists, continuing");
                        } else {
                            return Err(AppError::Internal(format!(
                                "failed to create SVN branch: {}",
                                e
                            )));
                        }
                    }
                }
            }
            Err(e) => {
                return Err(AppError::Internal(format!(
                    "failed to query SVN info for branch creation: {}",
                    e
                )));
            }
        }
    }

    // Auto-create Git branch if requested (default: true)
    if auto_git {
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
            .create_branch(&parent.git_repo, &git_branch, &parent.git_branch)
            .await
        {
            Ok(()) => {
                info!(
                    branch = %git_branch,
                    from = %parent.git_branch,
                    "auto-created Git branch"
                );
            }
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("already exists")
                    || err_str.contains("Reference already exists")
                {
                    info!(branch = %git_branch, "Git branch already exists, continuing");
                } else {
                    return Err(AppError::Internal(format!(
                        "failed to create Git branch: {}",
                        e
                    )));
                }
            }
        }
    }

    let new_id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    // Build name: use root repo name + this branch name
    let root_name = ancestors
        .last()
        .map(|r| r.name.as_str())
        .unwrap_or(&parent.name);
    let name = format!("{} / {}", root_name, git_branch);

    let child = reposync_core::models::Repository {
        id: new_id.clone(),
        name,
        svn_url: parent.svn_url.clone(),
        svn_branch: svn_branch.clone(),
        svn_username: parent.svn_username.clone(),
        git_provider: parent.git_provider.clone(),
        git_api_url: parent.git_api_url.clone(),
        git_repo: parent.git_repo.clone(),
        git_branch: git_branch.clone(),
        sync_mode: parent.sync_mode.clone(),
        poll_interval_secs: parent.poll_interval_secs,
        lfs_threshold_mb: parent.lfs_threshold_mb,
        auto_merge: parent.auto_merge,
        enabled: true,
        created_by: parent.created_by.clone(),
        parent_id: Some(parent.id.clone()),
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".to_string(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: parent.allowed_paths.clone(),
        blocked_patterns: parent.blocked_patterns.clone(),
        consecutive_errors: 0,
        teams_webhook_url: parent.teams_webhook_url.clone(),
    };

    db.insert_repository(&child)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

    // skip_import: set watermarks to current state so we start from now
    if body.skip_import {
        let svn_url = format!(
            "{}/{}",
            parent.svn_url.trim_end_matches('/'),
            svn_branch.trim_start_matches('/')
        );

        // Read credentials via ancestor chain
        let svn_password = db.resolve_credential_chain(&parent.id, "secret_svn_password");

        let svn_client = SvnClient::new(
            &svn_url,
            &parent.svn_username,
            svn_password.as_deref().unwrap_or(""),
        );

        // Get the current HEAD SHA of the new Git branch so the sync engine
        // doesn't try to replay every commit from the start of history.
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
        let git_head_sha = match github_client
            .get_branch_sha(&parent.git_repo, &git_branch)
            .await
        {
            Ok(sha) => sha,
            Err(e) => {
                warn!(
                    repo_id = %new_id,
                    error = %e,
                    "could not query Git HEAD SHA for skip_import; git watermark not set"
                );
                String::new()
            }
        };

        match svn_client.info().await {
            Ok(svn_info) => {
                let latest_rev = svn_info.latest_rev;
                if let Err(e) = db.update_repo_watermark(&new_id, latest_rev, &git_head_sha) {
                    warn!(repo_id = %new_id, error = %e, "failed to set watermark for branch pair");
                } else {
                    info!(
                        repo_id = %new_id,
                        latest_rev,
                        git_head_sha = %git_head_sha,
                        "Branch pair created in 'start from now' mode, watermark set to r{} / {}",
                        latest_rev,
                        &git_head_sha[..8.min(git_head_sha.len())]
                    );
                }
            }
            Err(e) => {
                warn!(
                    repo_id = %new_id,
                    error = %e,
                    "could not query SVN info for skip_import; watermark not set"
                );
            }
        }
    }

    // Return the newly created repo
    let created = db
        .get_repository(&new_id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::Internal("failed to read back created branch pair".into()))?;

    // Broadcast branch creation event (Teams + WebSocket)
    let branch_event = serde_json::json!({
        "type": "branch_pair_created",
        "repo_name": created.name,
        "git_branch": created.git_branch,
        "svn_branch": created.svn_branch,
    });
    let _ = state.ws_broadcast.send(branch_event.to_string());

    // Send Teams notification directly (for events not in ws_broadcast listener)
    let teams_url = db
        .get_state("teams_webhook_url")
        .ok()
        .flatten()
        .filter(|v| !v.is_empty());
    if let Some(url) = teams_url {
        let card = reposync_core::notify::teams::format_branch_created(
            &created.name,
            &created.git_branch,
            &created.svn_branch,
        );
        let notifier = reposync_core::notify::teams::TeamsNotifier::new(url);
        let _ = notifier.send_card(card).await;
    }

    Ok(Json(serde_json::to_value(created).map_err(|e| {
        AppError::Internal(format!("serialization error: {}", e))
    })?))
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
// Skip Commit — advance watermark past a stuck commit
// ---------------------------------------------------------------------------

async fn skip_commit(
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
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    // Get HEAD SHA of the git branch to skip to
    let git_token = db
        .resolve_credential_chain(&id, "secret_git_token")
        .unwrap_or_default();
    let provider = match repo.git_provider.as_str() {
        "gitea" => reposync_core::config::GitProvider::Gitea,
        _ => reposync_core::config::GitProvider::GitHub,
    };
    let github_client =
        reposync_core::git::github::GitHubClient::new(&repo.git_api_url, &git_token, provider);
    let head_sha = github_client
        .get_branch_sha(&repo.git_repo, &repo.git_branch)
        .await
        .map_err(|e| AppError::Internal(format!("failed to get branch HEAD: {}", e)))?;

    let old_sha = repo.last_git_sha.clone();

    // Advance all watermarks atomically
    db.advance_all_watermarks(&id, &head_sha)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

    // Reset circuit breaker state
    let _ = db.reset_consecutive_errors(&id);
    let _ = db.conn().execute(
        "UPDATE repositories SET sync_status = 'idle' WHERE id = ?1",
        rusqlite::params![&id],
    );

    let _ = db.insert_audit_log_with_repo(AuditLogInput {
        action: "skip_commit",
        direction: None,
        svn_rev: None,
        git_sha: Some(&head_sha),
        author: None,
        details: Some(&format!(
            "Skipped from {} to HEAD {}",
            &old_sha[..8.min(old_sha.len())],
            &head_sha[..8.min(head_sha.len())]
        )),
        success: true,
        repo_id: Some(&id),
    });

    info!(
        repo_id = %id,
        old_sha = %&old_sha[..8.min(old_sha.len())],
        new_sha = %&head_sha[..8.min(head_sha.len())],
        "skipped commit: watermark advanced to HEAD"
    );

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "Watermark advanced to HEAD",
        "old_sha": old_sha,
        "new_sha": head_sha,
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
