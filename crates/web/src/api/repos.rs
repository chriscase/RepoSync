//! Repository management API endpoints (multi-repo support).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use uuid::Uuid;

use reposync_core::db::Database;
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::{self, ImportConfig, ImportPhase, ImportProgress};
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
        .route("/api/repos/:id/credentials", get(get_credentials))
        .route("/api/repos/:id/credentials", post(save_credentials))
        .route("/api/repos/:id/branches", post(create_branch_pair))
        .route("/api/repos/:id/branches", get(list_branch_pairs))
        .route("/api/repos/:id/test-svn", post(test_repo_svn))
        .route("/api/repos/:id/test-git", post(test_repo_git))
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
        poll_interval_secs: body.poll_interval_secs.unwrap_or(existing.poll_interval_secs),
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
        Some(&format!("Manual sync triggered for repo '{}' ({})", repo.name, id)),
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
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let db = &state.db;

    // 1. Load repo config from DB
    let repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

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

    // 3. Reset progress
    {
        let mut p = progress.write().await;
        *p = ImportProgress::default();
        p.phase = ImportPhase::Importing;
        p.started_at = Some(chrono::Utc::now().to_rfc3339());
    }

    // 4. Read credentials from kv_state
    let svn_password = db
        .get_state(&format!("secret_svn_password_{}", id))
        .unwrap_or(None)
        .or_else(|| db.get_state("secret_svn_password").unwrap_or(None))
        .unwrap_or_default();

    let git_token: Option<String> = db
        .get_state(&format!("secret_git_token_{}", id))
        .unwrap_or(None)
        .or_else(|| db.get_state("secret_git_token").unwrap_or(None));

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

    info!(repo_id = %id, svn_import_url = %svn_import_url, reset = import_query.reset, "starting per-repo import");

    let svn_client = SvnClient::new(&svn_import_url, &repo.svn_username, &svn_password);

    // 6. Build the git repo path: {data_dir}/repos/{repo_id}/git-repo
    let data_dir = state.config.daemon.data_dir.clone();
    let git_repo_path = data_dir.join("repos").join(&id).join("git-repo");

    // If reset=true, wipe the local git repo and force-push empty to remote
    if import_query.reset {
        info!(repo_id = %id, "resetting: wiping local git repo and remote");
        {
            let mut p = progress.write().await;
            p.push_log("[info] Reset: deleting local git repository...".into());
        }
        if git_repo_path.exists() {
            std::fs::remove_dir_all(&git_repo_path)
                .map_err(|e| AppError::Internal(format!("failed to delete git repo: {}", e)))?;
        }

        // Create fresh repo, force-push empty commit to wipe remote
        std::fs::create_dir_all(&git_repo_path)
            .map_err(|e| AppError::Internal(format!("mkdir failed: {}", e)))?;

        let base_clone_url = reposync_core::git::remote_url::derive_git_remote_url(
            &repo.git_api_url,
            None,
            &repo.git_repo,
        );
        // Embed token in URL for the force-push
        let clone_url = if let Some(ref tok) = git_token {
            if let Some(rest) = base_clone_url.strip_prefix("https://") {
                format!("https://x-access-token:{}@{}", tok, rest)
            } else {
                base_clone_url.clone()
            }
        } else {
            base_clone_url.clone()
        };
        let branch = if repo.git_branch.is_empty() { "main" } else { &repo.git_branch };

        let init_cmds: Vec<Vec<&str>> = vec![
            vec!["init", "--initial-branch", branch],
            vec!["commit", "--allow-empty", "-m", "Reset for full SVN reimport"],
            vec!["remote", "add", "origin", &clone_url],
            vec!["push", "--force", "origin", branch],
        ];
        for args in &init_cmds {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&git_repo_path)
                .output()
                .map_err(|e| AppError::Internal(format!("git {:?} failed: {}", args[0], e)))?;
            if !output.status.success() && args[0] != "push" {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(cmd = ?args[0], %stderr, "git command failed during reset");
            }
        }

        // Reset watermark to 0
        let _ = db.update_repo_watermark(&id, 0, "");
        {
            let mut p = progress.write().await;
            p.push_log("[info] Reset: remote wiped, starting fresh import...".into());
        }
        info!(repo_id = %id, "reset complete, git repo and remote wiped");
    }

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
        match GitClient::clone_repo(&clone_url, &git_repo_path, git_token.as_deref()) {
            Ok(client) => client,
            Err(_) => {
                info!("Clone failed, initializing empty repo with remote");
                let output = std::process::Command::new("git")
                    .args(["init", "--initial-branch", &repo.git_branch])
                    .current_dir(&git_repo_path)
                    .output()
                    .map_err(|e| AppError::Internal(format!("git init failed: {}", e)))?;
                if !output.status.success() {
                    let _ = std::process::Command::new("git")
                        .args(["init"])
                        .current_dir(&git_repo_path)
                        .output();
                }
                let _ = std::process::Command::new("git")
                    .args(["remote", "add", "origin", &clone_url])
                    .current_dir(&git_repo_path)
                    .output();
                GitClient::new(&git_repo_path)
                    .map_err(|e| AppError::Internal(format!("git open failed: {}", e)))?
            }
        }
    };

    // 8. Configure git remote credentials
    git_client
        .ensure_remote_credentials("origin", git_token.as_deref())
        .map_err(|e| AppError::Internal(format!("failed to set git credentials: {}", e)))?;

    // Install git-lfs hooks if available
    let _ = std::process::Command::new("git")
        .args(["lfs", "install"])
        .current_dir(&git_repo_path)
        .output();

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
    };

    let ws_broadcast = Some(state.ws_broadcast.clone());
    let repo_id_clone = id.clone();

    // 12. Spawn the import task
    tokio::spawn(async move {
        // Hold the busy guard for the entire lifetime of the import so
        // the scheduler skips this repo until we're done.
        let _busy_guard = busy_guard;
        let result = import::run_full_import(
            &svn_client,
            &git_client,
            &identity_mapper,
            &import_db,
            &file_policy,
            &import_config,
            progress.clone(),
            ws_broadcast.clone(),
        )
        .await;

        let mut p = progress.write().await;
        match result {
            Ok(count) => {
                if p.phase != ImportPhase::Cancelled {
                    p.phase = ImportPhase::Completed;
                }
                p.completed_at = Some(chrono::Utc::now().to_rfc3339());
                p.push_log(format!(
                    "[info] Import complete: {} commits created",
                    count
                ));
                info!(repo_id = %repo_id_clone, count, "per-repo import completed successfully");

                // Update repo watermark so scheduler knows where import ended.
                // Read from the watermarks table (where run_full_import writes)
                // or fall back to the import progress total_revs.
                let last_rev = import_db.get_watermark("svn_rev")
                    .ok().flatten()
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or_else(|| p.total_revs); // fallback to total revisions
                let head_sha = {
                    let g = git_client.lock().unwrap_or_else(|p| p.into_inner());
                    g.get_head_sha().unwrap_or_default()
                };
                if last_rev > 0 {
                    let _ = import_db.update_repo_watermark(&repo_id_clone, last_rev, &head_sha);
                    // Also write per-repo kv_state keys for backward compat
                    let _ = import_db.set_state(
                        &format!("last_svn_rev_{}", repo_id_clone),
                        &last_rev.to_string(),
                    );
                    let _ = import_db.set_state(
                        &format!("last_git_sha_{}", repo_id_clone),
                        &head_sha,
                    );
                    info!(repo_id = %repo_id_clone, last_rev, %head_sha, "updated repo watermark after import");
                }
            }
            Err(e) => {
                p.phase = ImportPhase::Failed;
                p.completed_at = Some(chrono::Utc::now().to_rfc3339());
                let msg = format!("[error] Import failed: {}", e);
                p.push_log(msg.clone());
                p.errors.push(msg);
                error!(repo_id = %repo_id_clone, "per-repo import failed: {}", e);
            }
        }

        if let Err(e) = import_db.persist_import_progress(&p) {
            tracing::warn!("failed to persist import progress for repo {}: {}", repo_id_clone, e);
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

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "Import started",
    })))
}

async fn repo_import_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<ImportProgress>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    // Verify the repository exists
    let db = &state.db;
    let _repo = db
        .get_repository(&id)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repository not found".into()))?;

    let progress = state.get_repo_import_progress(&id).await;
    let p = progress.read().await;
    Ok(Json(p.clone()))
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
            tracing::info!(repo_id = %id, "Git token stored for repository");
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

    if body.svn_branch.is_empty() {
        return Err(AppError::BadRequest("svn_branch is required".into()));
    }
    if body.git_branch.is_empty() {
        return Err(AppError::BadRequest("git_branch is required".into()));
    }

    // Auto-create SVN branch if requested (default: true)
    let auto_svn = body.auto_create_svn_branch.unwrap_or(true);
    let auto_git = body.auto_create_git_branch.unwrap_or(true);

    if auto_svn {
        let svn_password = db
            .resolve_credential_chain(&parent.id, "secret_svn_password")
            .unwrap_or_default();
        let svn_client = SvnClient::new(
            &parent.svn_url,
            &parent.svn_username,
            &svn_password,
        );
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
                // Determine branches_path and branch name from body.svn_branch
                // e.g., "branches/fix-123" → branches_path="branches", name="fix-123"
                let (branches_path, branch_name) = if let Some(pos) = body.svn_branch.rfind('/') {
                    (&body.svn_branch[..pos], &body.svn_branch[pos + 1..])
                } else {
                    ("branches", body.svn_branch.as_str())
                };
                match svn_client
                    .create_branch(branch_name, &parent.svn_branch, branches_path, info.latest_rev)
                    .await
                {
                    Ok(()) => {
                        info!(
                            branch = %body.svn_branch,
                            from = %parent.svn_branch,
                            rev = info.latest_rev,
                            "auto-created SVN branch"
                        );
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        // Treat "already exists" as success
                        if err_str.contains("already exists") || err_str.contains("E160020") {
                            info!(branch = %body.svn_branch, "SVN branch already exists, continuing");
                        } else {
                            return Err(AppError::Internal(format!(
                                "failed to create SVN branch: {}", e
                            )));
                        }
                    }
                }
            }
            Err(e) => {
                return Err(AppError::Internal(format!(
                    "failed to query SVN info for branch creation: {}", e
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
            .create_branch(&parent.git_repo, &body.git_branch, &parent.git_branch)
            .await
        {
            Ok(()) => {
                info!(
                    branch = %body.git_branch,
                    from = %parent.git_branch,
                    "auto-created Git branch"
                );
            }
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("already exists") || err_str.contains("Reference already exists") {
                    info!(branch = %body.git_branch, "Git branch already exists, continuing");
                } else {
                    return Err(AppError::Internal(format!(
                        "failed to create Git branch: {}", e
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
    let name = format!("{} / {}", root_name, body.git_branch);

    let child = reposync_core::models::Repository {
        id: new_id.clone(),
        name,
        svn_url: parent.svn_url.clone(),
        svn_branch: body.svn_branch.clone(),
        svn_username: parent.svn_username.clone(),
        git_provider: parent.git_provider.clone(),
        git_api_url: parent.git_api_url.clone(),
        git_repo: parent.git_repo.clone(),
        git_branch: body.git_branch.clone(),
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
    };

    db.insert_repository(&child)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

    // skip_import: set watermark to latest SVN rev so we start from now
    if body.skip_import {
        let svn_url = format!(
            "{}/{}",
            parent.svn_url.trim_end_matches('/'),
            body.svn_branch.trim_start_matches('/')
        );

        // Read credentials via ancestor chain
        let svn_password = db.resolve_credential_chain(&parent.id, "secret_svn_password");

        let svn_client = SvnClient::new(
            &svn_url,
            &parent.svn_username,
            svn_password.as_deref().unwrap_or(""),
        );

        match svn_client.info().await {
            Ok(svn_info) => {
                let latest_rev = svn_info.latest_rev;
                if let Err(e) = db.update_repo_watermark(&new_id, latest_rev, "") {
                    warn!(repo_id = %new_id, error = %e, "failed to set watermark for branch pair");
                } else {
                    info!(
                        repo_id = %new_id,
                        latest_rev,
                        "Branch pair created in 'start from now' mode, watermark set to r{}",
                        latest_rev
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

    Ok(Json(serde_json::to_value(created).map_err(|e| AppError::Internal(format!("serialization error: {}", e)))?))
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
    ).await?;

    let db = &state.db;
    let repo = db.get_repository(&id)
        .map_err(|e| AppError::Internal(format!("db error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repo not found".into()))?;

    let overrides = body.map(|Json(b)| b).unwrap_or_default();

    // Prefer form-supplied values; fall back to saved values.
    let svn_url_base = overrides.svn_url.filter(|s| !s.is_empty()).unwrap_or(repo.svn_url);
    let svn_branch = overrides.svn_branch.filter(|s| !s.is_empty()).unwrap_or(repo.svn_branch);
    let svn_username = overrides.svn_username.filter(|s| !s.is_empty()).unwrap_or(repo.svn_username);

    // Password: prefer form value, then stored per-repo → parent → global.
    let password = overrides.svn_password
        .filter(|v| !v.is_empty())
        .or_else(|| db.resolve_credential_chain(&id, "secret_svn_password"))
        .unwrap_or_default();

    let svn_url = if svn_branch.is_empty() {
        svn_url_base
    } else {
        format!("{}/{}", svn_url_base.trim_end_matches('/'), svn_branch.trim_start_matches('/'))
    };

    let result = tokio::process::Command::new("svn")
        .args(["info", "--non-interactive", "--username", &svn_username, "--password", &password, &svn_url])
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let info = stdout.lines()
                .find(|l| l.starts_with("Repository Root:") || l.starts_with("URL:"))
                .unwrap_or("SVN server responded successfully");
            Ok(Json(serde_json::json!({"ok": true, "message": info.trim()})))
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = stderr.lines().find(|l| l.contains("E1") || l.contains("Unable") || l.contains("Authentication"))
                .unwrap_or("SVN command failed");
            Ok(Json(serde_json::json!({"ok": false, "message": msg.trim()})))
        }
        Err(e) => Ok(Json(serde_json::json!({"ok": false, "message": format!("Failed to run svn: {}", e)}))),
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
    ).await?;

    let db = &state.db;
    let repo = db.get_repository(&id)
        .map_err(|e| AppError::Internal(format!("db error: {}", e)))?
        .ok_or_else(|| AppError::NotFound("repo not found".into()))?;

    let overrides = body.map(|Json(b)| b).unwrap_or_default();

    let git_api_url = overrides.git_api_url.filter(|s| !s.is_empty()).unwrap_or(repo.git_api_url);
    let git_repo = overrides.git_repo.filter(|s| !s.is_empty()).unwrap_or(repo.git_repo);

    // Token: prefer form value, then stored per-repo → parent → global.
    let token = overrides.git_token
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
                let name = json.get("full_name").or_else(|| json.get("name"))
                    .and_then(|v| v.as_str()).unwrap_or(&git_repo);
                Ok(Json(serde_json::json!({"ok": true, "message": format!("Repository found: {}", name)})))
            } else {
                Ok(Json(serde_json::json!({"ok": true, "message": "Repository is accessible"})))
            }
        }
        Ok(resp) => {
            let status = resp.status();
            Ok(Json(serde_json::json!({"ok": false, "message": format!("HTTP {} — check credentials and URL", status)})))
        }
        Err(e) => Ok(Json(serde_json::json!({"ok": false, "message": format!("Connection failed: {}", e)}))),
    }
}
