//! Setup wizard API endpoints.
//!
//! - Test SVN / Git connections
//! - Apply configuration from wizard data (generates TOML server-side)
//! - Trigger full SVN→Git history import with progress tracking
//! - Poll import status

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};
use uuid::Uuid;

use reposync_core::busy::BusyGuard;
use reposync_core::config::AppConfig;
use reposync_core::db::import_operations::{
    import_target_fingerprint, resolve_repo_import_baseline, ImportOperation, ImportOperationState,
};
use reposync_core::db::Database;
use reposync_core::errors::DatabaseError;
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::{self, ImportConfig, ImportPhase, ImportProgress};
use reposync_core::models::Repository;
use reposync_core::svn::SvnClient;

use crate::api::auth::validate_session_with_role;
use crate::api::status::AppError;
use crate::AppState;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct TestSvnRequest {
    pub url: String,
    pub username: String,
    pub password: Option<String>,
}

#[derive(Deserialize)]
pub struct TestGitRequest {
    pub api_url: String,
    pub repo: String,
    pub provider: String,
    pub token: Option<String>,
}

#[derive(Serialize)]
pub struct TestConnectionResponse {
    pub ok: bool,
    pub message: String,
}

#[derive(Deserialize)]
pub struct ApplyConfigRequest {
    // SVN
    pub svn_url: String,
    pub svn_username: String,
    pub svn_password_env: Option<String>,
    /// Actual SVN password (stored securely in DB, not in TOML).
    pub svn_password: Option<String>,
    pub svn_layout: Option<String>,
    pub svn_trunk_path: Option<String>,
    pub svn_branches_path: Option<String>,
    pub svn_tags_path: Option<String>,

    // Git
    pub git_provider: Option<String>,
    pub git_api_url: String,
    pub git_repo: String,
    pub git_token_env: Option<String>,
    /// Actual Git token (stored securely in DB, not in TOML).
    pub git_token: Option<String>,
    pub git_default_branch: Option<String>,

    // Sync
    pub sync_mode: Option<String>,
    pub sync_auto_merge: Option<bool>,
    pub sync_tags: Option<bool>,

    // File policy
    pub max_file_size: Option<u64>,
    pub lfs_threshold: Option<u64>,
    pub lfs_patterns: Option<Vec<String>>,
    pub ignore_patterns: Option<Vec<String>>,

    // Identity
    pub identity_email_domain: Option<String>,
    pub identity_mapping_file: Option<String>,
    pub identity_mappings: Option<Vec<IdentityMappingEntry>>,

    // Daemon
    pub daemon_poll_interval: Option<u64>,
    pub daemon_log_level: Option<String>,
    pub daemon_data_dir: Option<String>,

    // Web
    pub web_listen: Option<String>,
    pub web_admin_password_env: Option<String>,
    /// Actual admin password (stored securely in DB, not in TOML).
    pub web_admin_password: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct IdentityMappingEntry {
    pub svn_username: String,
    pub name: String,
    pub email: String,
}

#[derive(Serialize)]
pub struct ApplyConfigResponse {
    pub ok: bool,
    pub message: String,
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct ImportActionResponse {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<ImportOperationState>,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// Response for `GET /api/setup/config` — returns saved config for wizard pre-population.
#[derive(Serialize)]
pub struct SetupConfigResponse {
    // SVN
    pub svn_url: String,
    pub svn_username: String,
    pub svn_layout: String,
    pub svn_trunk_path: String,
    pub svn_password_set: bool,

    // Git
    pub git_provider: String,
    pub git_api_url: String,
    pub git_repo: String,
    pub git_branch: String,
    pub git_token_set: bool,

    // Sync
    pub sync_mode: String,
    pub auto_merge: bool,
    pub sync_tags: bool,
    pub lfs_threshold: u64,

    // Identity
    pub email_domain: String,

    // Server
    pub listen: String,
    pub auth_mode: String,
    pub poll_interval: u64,
    pub log_level: String,
    pub data_dir: String,
    pub admin_password_set: bool,

    /// Whether a config file exists on disk.
    pub config_exists: bool,
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/setup/config", get(get_setup_config))
        .route("/api/setup/test-svn", post(test_svn_connection))
        .route("/api/setup/test-git", post(test_git_connection))
        .route("/api/setup/apply", post(apply_config))
        .route("/api/setup/import", post(start_import))
        .route("/api/setup/import/status", get(import_status))
        .route("/api/setup/import/cancel", post(cancel_import))
        .route("/api/setup/reset-reimport", post(reset_and_reimport))
}

// ---------------------------------------------------------------------------
// Get saved config for wizard pre-population
// ---------------------------------------------------------------------------

async fn get_setup_config(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<SetupConfigResponse>, AppError> {
    crate::api::auth::validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let cfg = &state.config;

    let db = &state.db;

    let svn_password_set = db
        .get_state("secret_svn_password")
        .ok()
        .flatten()
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        || cfg.svn.password.is_some();

    let git_token_set = db
        .get_state("secret_git_token")
        .ok()
        .flatten()
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        || cfg.github.token.is_some();

    let admin_password_set = db
        .get_state("secret_admin_password")
        .ok()
        .flatten()
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        || cfg.web.admin_password.is_some();

    Ok(Json(SetupConfigResponse {
        svn_url: cfg.svn.url.clone(),
        svn_username: cfg.svn.username.clone(),
        svn_layout: if cfg.svn.trunk_path.is_empty() {
            "single".into()
        } else {
            "standard".into()
        },
        svn_trunk_path: cfg.svn.trunk_path.clone(),
        svn_password_set,

        git_provider: "github".into(),
        git_api_url: cfg.github.api_url.clone(),
        git_repo: cfg.github.repo.clone(),
        git_branch: cfg.github.default_branch.clone(),
        git_token_set,

        sync_mode: format!("{:?}", cfg.sync.mode).to_lowercase(),
        auto_merge: cfg.sync.auto_merge,
        sync_tags: cfg.sync.sync_tags,
        lfs_threshold: cfg.sync.lfs_threshold,

        email_domain: cfg.identity.email_domain.clone().unwrap_or_default(),

        listen: cfg.web.listen.clone(),
        auth_mode: "simple".into(),
        poll_interval: cfg.daemon.poll_interval_secs,
        log_level: cfg.daemon.log_level.clone(),
        data_dir: cfg.daemon.data_dir.display().to_string(),
        admin_password_set,

        config_exists: true,
    }))
}

// ---------------------------------------------------------------------------
// Test connections
// ---------------------------------------------------------------------------

async fn test_svn_connection(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<TestSvnRequest>,
) -> Result<Json<TestConnectionResponse>, AppError> {
    // Require auth unless this is a fresh instance with no users/password configured
    let has_users = state.db.count_users().unwrap_or(0) > 0;
    if state.config.web.admin_password.is_some() || has_users {
        crate::api::auth::validate_session(
            &state,
            headers.get("authorization").and_then(|v| v.to_str().ok()),
        )
        .await?;
    }
    let url = body.url.trim().to_string();
    let username = body.username.trim().to_string();

    if url.is_empty() {
        return Ok(Json(TestConnectionResponse {
            ok: false,
            message: "URL is empty".into(),
        }));
    }

    let mut args = vec!["info", "--non-interactive", "--username", &username];
    let password = body.password.as_deref().unwrap_or("");
    if !password.is_empty() {
        args.push("--password");
        args.push(password);
    }
    args.push(&url);
    let result = tokio::process::Command::new("svn")
        .args(&args)
        .output()
        .await;

    match result {
        Ok(output) => {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let info = stdout
                    .lines()
                    .find(|l| l.starts_with("Repository Root:") || l.starts_with("URL:"))
                    .map(|l| l.trim().to_string())
                    .unwrap_or_else(|| "SVN server responded successfully".into());
                Ok(Json(TestConnectionResponse {
                    ok: true,
                    message: info,
                }))
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let msg = stderr
                    .lines()
                    .find(|l| l.contains("E1") || l.contains("Unable"))
                    .unwrap_or("SVN command failed")
                    .trim()
                    .to_string();
                Ok(Json(TestConnectionResponse {
                    ok: false,
                    message: msg,
                }))
            }
        }
        Err(e) => Ok(Json(TestConnectionResponse {
            ok: false,
            message: format!("Failed to run svn command: {}", e),
        })),
    }
}

async fn test_git_connection(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<TestGitRequest>,
) -> Result<Json<TestConnectionResponse>, AppError> {
    let has_users = state.db.count_users().unwrap_or(0) > 0;
    if state.config.web.admin_password.is_some() || has_users {
        crate::api::auth::validate_session(
            &state,
            headers.get("authorization").and_then(|v| v.to_str().ok()),
        )
        .await?;
    }
    let api_url = body.api_url.trim().trim_end_matches('/').to_string();
    let repo = body.repo.trim().to_string();

    if api_url.is_empty() || repo.is_empty() {
        return Ok(Json(TestConnectionResponse {
            ok: false,
            message: "API URL and repository are required".into(),
        }));
    }

    let check_url = format!("{}/repos/{}", api_url, repo);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AppError::Internal(format!("http client error: {}", e)))?;

    let mut req = client.get(&check_url);
    if let Some(ref token) = body.token {
        if !token.is_empty() {
            req = req.header("Authorization", format!("token {}", token));
        }
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let name = json
                        .get("full_name")
                        .or_else(|| json.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&repo);
                    Ok(Json(TestConnectionResponse {
                        ok: true,
                        message: format!("Repository found: {}", name),
                    }))
                } else {
                    Ok(Json(TestConnectionResponse {
                        ok: true,
                        message: "Repository is accessible".into(),
                    }))
                }
            } else if status.as_u16() == 404 {
                Ok(Json(TestConnectionResponse {
                    ok: false,
                    message: "Repository not found (404). Check the repo name and API URL.".into(),
                }))
            } else if status.as_u16() == 401 || status.as_u16() == 403 {
                Ok(Json(TestConnectionResponse {
                    ok: false,
                    message: format!(
                        "Authentication required ({}). The API URL is reachable but the repo may be private.",
                        status
                    ),
                }))
            } else {
                Ok(Json(TestConnectionResponse {
                    ok: false,
                    message: format!("Server returned status {}", status),
                }))
            }
        }
        Err(e) => {
            let msg = if e.is_connect() {
                format!(
                    "Cannot connect to {}: connection refused or host not reachable",
                    api_url
                )
            } else if e.is_timeout() {
                "Connection timed out after 10 seconds".into()
            } else {
                format!("Request failed: {}", e)
            };
            Ok(Json(TestConnectionResponse {
                ok: false,
                message: msg,
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// Apply configuration
// ---------------------------------------------------------------------------

async fn apply_config(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ApplyConfigRequest>,
) -> Result<Json<ApplyConfigResponse>, AppError> {
    let has_users = state.db.count_users().unwrap_or(0) > 0;
    if state.config.web.admin_password.is_some() || has_users {
        crate::api::auth::validate_session(
            &state,
            headers.get("authorization").and_then(|v| v.to_str().ok()),
        )
        .await?;
    }
    let mut warnings = Vec::new();

    // Write identity mapping file if mappings are provided
    if let Some(ref mappings) = body.identity_mappings {
        if !mappings.is_empty() {
            let mapping_path = body
                .identity_mapping_file
                .as_deref()
                .unwrap_or("identity-mappings.toml");
            let mut mapping_lines = Vec::new();
            for m in mappings {
                mapping_lines.push(format!("[mappings.\"{}\"]", m.svn_username));
                mapping_lines.push(format!("name = \"{}\"", m.name));
                mapping_lines.push(format!("email = \"{}\"", m.email));
                mapping_lines.push(String::new());
            }
            let mapping_content = mapping_lines.join("\n");

            // Resolve path relative to config file directory
            let config_dir = state
                .config_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            let full_mapping_path = if std::path::Path::new(mapping_path).is_absolute() {
                std::path::PathBuf::from(mapping_path)
            } else {
                config_dir.join(mapping_path)
            };

            if let Err(e) = std::fs::write(&full_mapping_path, &mapping_content) {
                warnings.push(format!("Failed to write identity mapping file: {}", e));
            } else {
                info!(
                    path = %full_mapping_path.display(),
                    count = mappings.len(),
                    "Wrote identity mappings file"
                );
            }

            // Also store in DB for the dashboard to display
            let db = &state.db;
            let json_val = serde_json::to_string(mappings).unwrap_or_default();
            let now = chrono::Utc::now().to_rfc3339();
            let _ = db.conn().execute(
                "INSERT OR REPLACE INTO kv_state (key, value, updated_at) VALUES ('identity_mappings', ?1, ?2)",
                rusqlite::params![json_val, now],
            );
        }
    }

    // Store secrets in DB encrypted (never written to TOML file)
    {
        let db = &state.db;

        if let Some(ref password) = body.svn_password {
            if !password.is_empty() {
                match reposync_core::crypto::get_or_create_encryption_key(db) {
                    Ok(key) => match reposync_core::crypto::encrypt_credential(password, &key) {
                        Ok((ct, nonce)) => {
                            let _ = db.store_encrypted_secret("svn_password", &ct, &nonce);
                            // Remove legacy plaintext if present
                            let _ = db.set_state("secret_svn_password", "");
                            info!("SVN password encrypted and stored in database");
                        }
                        Err(e) => warn!("Failed to encrypt SVN password: {}", e),
                    },
                    Err(e) => warn!("Failed to get encryption key: {}", e),
                }
            }
        }

        if let Some(ref token) = body.git_token {
            if !token.is_empty() {
                match reposync_core::crypto::get_or_create_encryption_key(db) {
                    Ok(key) => match reposync_core::crypto::encrypt_credential(token, &key) {
                        Ok((ct, nonce)) => {
                            let _ = db.store_encrypted_secret("git_token", &ct, &nonce);
                            let _ = db.set_state("secret_git_token", "");
                            info!("Git token encrypted and stored in database");
                        }
                        Err(e) => warn!("Failed to encrypt Git token: {}", e),
                    },
                    Err(e) => warn!("Failed to get encryption key: {}", e),
                }
            }
        }

        if let Some(ref password) = body.web_admin_password {
            if !password.is_empty() {
                match reposync_core::crypto::hash_password(password) {
                    Ok(hash) => {
                        let _ = db.set_state("secret_admin_password_hash", &hash);
                        // Remove legacy plaintext if present
                        let _ = db.set_state("secret_admin_password", "");
                        info!("Admin password hashed and stored in database");
                    }
                    Err(e) => warn!("Failed to hash admin password: {}", e),
                }
            }
        }
    }

    // Upsert repository row in the DB (TOML file is never written by the API).
    {
        let db = &state.db;
        let now = chrono::Utc::now().to_rfc3339();
        let provider = body.git_provider.as_deref().unwrap_or("github");
        let sync_mode = body.sync_mode.as_deref().unwrap_or("direct");
        let git_branch = body.git_default_branch.as_deref().unwrap_or("main");
        let poll_secs = body.daemon_poll_interval.unwrap_or(60) as i64;
        let lfs_mb = body.lfs_threshold.unwrap_or(0) as i64;
        let auto_merge = body.sync_auto_merge.unwrap_or(true);

        // Check if a default repository already exists (update it), otherwise create one.
        let existing_repos = db.list_repositories().unwrap_or_default();
        if let Some(existing) = existing_repos.into_iter().next() {
            let updated = reposync_core::models::Repository {
                svn_url: body.svn_url.clone(),
                svn_branch: body.svn_trunk_path.clone().unwrap_or_default(),
                svn_username: body.svn_username.clone(),
                git_provider: provider.to_string(),
                git_api_url: body.git_api_url.clone(),
                git_repo: body.git_repo.clone(),
                git_branch: git_branch.to_string(),
                sync_mode: sync_mode.to_string(),
                poll_interval_secs: poll_secs,
                lfs_threshold_mb: lfs_mb,
                auto_merge,
                updated_at: now,
                ..existing
            };
            match db.update_repository(&updated) {
                Ok(()) => info!(id = %updated.id, "Updated repository from setup wizard"),
                Err(e) => warnings.push(format!("Failed to update repository: {}", e)),
            }
        } else {
            let new_repo = reposync_core::models::Repository {
                id: uuid::Uuid::new_v4().to_string(),
                name: body.git_repo.clone(),
                svn_url: body.svn_url.clone(),
                svn_branch: body.svn_trunk_path.clone().unwrap_or_default(),
                svn_username: body.svn_username.clone(),
                git_provider: provider.to_string(),
                git_api_url: body.git_api_url.clone(),
                git_repo: body.git_repo.clone(),
                git_branch: git_branch.to_string(),
                sync_mode: sync_mode.to_string(),
                poll_interval_secs: poll_secs,
                lfs_threshold_mb: lfs_mb,
                auto_merge,
                enabled: true,
                created_by: None,
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
            match db.insert_repository(&new_repo) {
                Ok(()) => {
                    info!(id = %new_repo.id, name = %new_repo.name, "Created repository from setup wizard")
                }
                Err(e) => warnings.push(format!("Failed to create repository: {}", e)),
            }
        }
    }

    // LFS check
    if body.lfs_threshold.unwrap_or(0) > 0 {
        match reposync_core::lfs::preflight_check() {
            Ok(version) => {
                info!("LFS preflight passed: {}", version);
            }
            Err(e) => {
                warnings.push(format!(
                    "Git LFS is configured but not available: {}. Large files will be committed directly.",
                    e
                ));
            }
        }
    }

    info!("Configuration applied (credentials saved to DB)");

    Ok(Json(ApplyConfigResponse {
        ok: true,
        message: "Configuration saved successfully".into(),
        warnings,
    }))
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

fn setup_import_phase_busy(phase: &ImportPhase) -> bool {
    matches!(
        phase,
        ImportPhase::Connecting
            | ImportPhase::Importing
            | ImportPhase::Verifying
            | ImportPhase::FinalPush
    )
}

fn setup_repository(db: &Database) -> Result<Repository, AppError> {
    db.list_repositories()
        .map_err(|e| AppError::Internal(e.to_string()))?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::BadRequest("apply configuration before starting import".into()))
}

fn import_write_error(error: DatabaseError) -> AppError {
    match error {
        DatabaseError::Other(message) => AppError::BadRequest(message),
        other => AppError::Internal(format!("import operation persistence failed: {other}")),
    }
}

fn action_response(
    ok: bool,
    message: impl Into<String>,
    op: Option<&ImportOperation>,
) -> Json<ImportActionResponse> {
    Json(ImportActionResponse {
        ok,
        message: message.into(),
        operation_id: op.map(|o| o.id.clone()),
        lifecycle: op.map(|o| o.state.clone()),
    })
}

fn overlay_setup_import_status(
    progress: &ImportProgress,
    op: Option<&ImportOperation>,
    repo_id: Option<&str>,
) -> serde_json::Value {
    let mut value = serde_json::to_value(progress).unwrap_or_else(|_| serde_json::json!({}));
    let Some(object) = value.as_object_mut() else {
        return value;
    };
    let busy = repo_id.is_some_and(reposync_core::busy::is_busy)
        || setup_import_phase_busy(&progress.phase)
        || op.is_some_and(|o| !o.state.is_terminal());
    object.insert("busy".into(), serde_json::json!(busy));
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
        if let Some(phase) = terminal_phase {
            object.insert("phase".into(), serde_json::json!(phase));
        } else if op.state == ImportOperationState::CancelRequested
            || op.state == ImportOperationState::Cancelling
        {
            object.insert("cancelling".into(), serde_json::json!(true));
        }
        object.insert("operation_id".into(), serde_json::json!(op.id));
        object.insert("lifecycle".into(), serde_json::to_value(&op.state).unwrap());
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
    value
}

async fn setup_auth(
    state: &Arc<AppState>,
    headers: &HeaderMap,
) -> Result<(String, String), AppError> {
    let has_users = state.db.count_users().unwrap_or(0) > 0;
    if state.config.web.admin_password.is_some() || has_users {
        validate_session_with_role(
            state,
            headers.get("authorization").and_then(|v| v.to_str().ok()),
        )
        .await
    } else {
        Ok(("setup-wizard".into(), "admin".into()))
    }
}

enum SetupAdmission {
    Ready {
        repo: Box<Repository>,
        operation: Box<ImportOperation>,
        busy_guard: BusyGuard,
    },
    AlreadyRecorded(Box<ImportOperation>),
    Busy {
        message: String,
        operation: Option<Box<ImportOperation>>,
    },
}

/// RS-C07 (#64): acquire the per-repository busy slot and enroll a durable
/// `import_operation_v1` row *before* clone/init/credential rewrite.
async fn admit_setup_import(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    user_id: &str,
) -> Result<SetupAdmission, AppError> {
    let db = &state.db;
    let repo = setup_repository(db)?;
    let request_id = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    if let Some(previous) = db
        .latest_import_operation(&repo.id)
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        if previous.request_id == request_id && previous.initiator_id == user_id {
            return Ok(SetupAdmission::AlreadyRecorded(Box::new(previous)));
        }
    }
    if let Some(active) = db
        .active_import_operation(&repo.id)
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        return Ok(SetupAdmission::Busy {
            message: "An import is already running".into(),
            operation: Some(Box::new(active)),
        });
    }
    if resolve_repo_import_baseline(db, &repo.id)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .is_verified()
    {
        return Ok(SetupAdmission::Busy {
            message: "repository already has a completed baseline; refusing implicit full replay"
                .into(),
            operation: db
                .latest_import_operation(&repo.id)
                .ok()
                .flatten()
                .map(Box::new),
        });
    }
    {
        let p = state.import_progress.read().await;
        if setup_import_phase_busy(&p.phase) {
            return Ok(SetupAdmission::Busy {
                message: "An import is already running".into(),
                operation: None,
            });
        }
    }

    let busy_guard = {
        let mut guard = None;
        for attempt in 0..30 {
            if let Some(active) = db
                .active_import_operation(&repo.id)
                .map_err(|e| AppError::Internal(e.to_string()))?
            {
                if active.request_id == request_id && active.initiator_id == user_id {
                    return Ok(SetupAdmission::AlreadyRecorded(Box::new(active)));
                }
                return Ok(SetupAdmission::Busy {
                    message: "An import is already running".into(),
                    operation: Some(Box::new(active)),
                });
            }
            if let Some(g) = reposync_core::busy::try_acquire(&repo.id) {
                guard = Some(g);
                break;
            }
            if attempt == 0 {
                info!(repo_id = %repo.id, "waiting for in-flight writer before setup import");
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        match guard {
            Some(g) => g,
            None => {
                return Ok(SetupAdmission::Busy {
                    message: "A sync cycle is currently running for this repository. Please retry in a moment.".into(),
                    operation: None,
                });
            }
        }
    };

    let workdir = state.config.daemon.data_dir.join("git-repo");
    let fingerprint = import_target_fingerprint(&repo, &workdir);
    let operation = db
        .create_import_operation(&repo.id, user_id, &request_id, &fingerprint)
        .map_err(import_write_error)?;
    Ok(SetupAdmission::Ready {
        repo: Box::new(repo),
        operation: Box::new(operation),
        busy_guard,
    })
}

const SETUP_PREPARATION_HOLD_DETAIL: &str =
    "preparation stopped before worker start; inspect local work and target";

struct SetupPreparationGuard<'a> {
    db: &'a Database,
    repo_id: String,
    operation_id: String,
    armed: bool,
}

impl SetupPreparationGuard<'_> {
    fn try_finalize_preparation_hold(&mut self) -> Result<(), DatabaseError> {
        if !self.armed {
            return Ok(());
        }
        self.db.finish_import_operation(
            &self.repo_id,
            &self.operation_id,
            ImportOperationState::ReconciliationRequired,
            SETUP_PREPARATION_HOLD_DETAIL,
        )?;
        self.armed = false;
        Ok(())
    }

    fn finalize_preparation_hold(&mut self) -> Result<(), AppError> {
        self.try_finalize_preparation_hold()
            .map_err(import_write_error)
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SetupPreparationGuard<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.try_finalize_preparation_hold() {
            error!(repo_id = %self.repo_id, error = %e, "failed to persist setup import preparation outcome");
        }
    }
}

async fn start_import(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<ImportActionResponse>, AppError> {
    let (user_id, _role) = setup_auth(&state, &headers).await?;
    match admit_setup_import(&state, &headers, &user_id).await? {
        SetupAdmission::AlreadyRecorded(operation) => Ok(Json(ImportActionResponse {
            ok: true,
            message: "Import request already recorded".into(),
            operation_id: Some(operation.id),
            lifecycle: Some(operation.state),
        })),
        SetupAdmission::Busy { message, operation } => {
            Ok(action_response(false, message, operation.as_deref()))
        }
        SetupAdmission::Ready {
            repo,
            operation,
            busy_guard,
        } => {
            let operation_id = operation.id.clone();
            let mut preparation_guard = SetupPreparationGuard {
                db: &state.db,
                repo_id: repo.id.clone(),
                operation_id: operation_id.clone(),
                armed: true,
            };

            {
                let mut p = state.import_progress.write().await;
                *p = ImportProgress::default();
                p.phase = ImportPhase::Importing;
                p.started_at = Some(chrono::Utc::now().to_rfc3339());
            }

            if let Err(e) =
                spawn_import_task(&state, repo.id.clone(), operation_id.clone(), busy_guard).await
            {
                {
                    let mut p = state.import_progress.write().await;
                    p.phase = ImportPhase::Failed;
                    p.completed_at = Some(chrono::Utc::now().to_rfc3339());
                    p.push_log("[error] setup import preparation failed".into());
                }
                preparation_guard.finalize_preparation_hold()?;
                return Err(e);
            }
            preparation_guard.disarm();

            Ok(Json(ImportActionResponse {
                ok: true,
                message: "Import started".into(),
                operation_id: Some(operation_id),
                lifecycle: Some(ImportOperationState::Queued),
            }))
        }
    }
}

async fn import_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    setup_auth(&state, &headers).await?;
    let mut progress = state.import_progress.read().await.clone();
    if progress.phase == ImportPhase::Idle {
        if let Ok(Some(db_progress)) = state.db.load_import_progress() {
            if db_progress.phase != ImportPhase::Idle {
                progress = db_progress;
            }
        }
    }
    let repo = state
        .db
        .list_repositories()
        .ok()
        .and_then(|repos| repos.into_iter().next());
    let op = repo.as_ref().and_then(|repo| {
        state
            .db
            .active_import_operation(&repo.id)
            .ok()
            .flatten()
            .or_else(|| state.db.latest_import_operation(&repo.id).ok().flatten())
    });
    Ok(Json(overlay_setup_import_status(
        &progress,
        op.as_ref(),
        repo.as_ref().map(|r| r.id.as_str()),
    )))
}

async fn cancel_import(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<ImportActionResponse>, AppError> {
    setup_auth(&state, &headers).await?;
    if let Ok(repo) = setup_repository(&state.db) {
        if let Some(active) = state
            .db
            .active_import_operation(&repo.id)
            .map_err(|e| AppError::Internal(e.to_string()))?
        {
            let op = state
                .db
                .request_import_cancel(&repo.id, &active.id)
                .map_err(import_write_error)?;
            if !op.state.is_terminal() {
                let mut p = state.import_progress.write().await;
                p.cancel_requested = true;
                p.cancel_signal.store(true, Ordering::Release);
            }
            return Ok(Json(ImportActionResponse {
                ok: true,
                message: if op.state.is_terminal() {
                    "terminal outcome retained; published history is not undone".into()
                } else {
                    "cancellation durably requested; worker still stopping; published history is not undone".into()
                },
                operation_id: Some(op.id.clone()),
                lifecycle: Some(op.state),
            }));
        }
    }
    let mut p = state.import_progress.write().await;
    if setup_import_phase_busy(&p.phase) {
        p.cancel_requested = true;
        p.cancel_signal.store(true, Ordering::Release);
        Ok(action_response(
            true,
            "Cancellation requested; published history is not undone",
            None,
        ))
    } else {
        Ok(action_response(
            false,
            "No import is currently running",
            None,
        ))
    }
}

// ---------------------------------------------------------------------------
// Shared import helper
// ---------------------------------------------------------------------------

/// Resolve config, build clients, and spawn the background import task.
/// Used by both `start_import` and `reset_and_reimport`. Caller must already
/// hold `busy_guard` and a durable `import_operation_v1` row, and must keep
/// `SetupPreparationGuard` armed until this returns `Ok` so a failed clone
/// (or any other error before the worker starts) holds the enrolled row.
async fn spawn_import_task(
    state: &Arc<AppState>,
    repo_id: String,
    operation_id: String,
    busy_guard: BusyGuard,
) -> Result<(), AppError> {
    // Load config from file
    let config_content = std::fs::read_to_string(&state.config_path)
        .map_err(|e| AppError::Internal(format!("failed to read config: {}", e)))?;
    let mut config: AppConfig = toml::from_str(&config_content)
        .map_err(|e| AppError::Internal(format!("failed to parse config: {}", e)))?;
    config
        .resolve_env_vars()
        .map_err(|e| AppError::Internal(format!("failed to resolve env vars: {}", e)))?;

    // Load secrets from DB
    let (db_svn_password, db_git_token) = {
        let db = &state.db;
        let conn = db.conn();
        let svn_pw: Option<String> = conn
            .query_row(
                "SELECT value FROM kv_state WHERE key = 'secret_svn_password'",
                [],
                |row| row.get(0),
            )
            .ok();
        let git_tok: Option<String> = conn
            .query_row(
                "SELECT value FROM kv_state WHERE key = 'secret_git_token'",
                [],
                |row| row.get(0),
            )
            .ok();
        (svn_pw, git_tok)
    };

    // Build clients
    let svn_password = config
        .svn
        .password
        .clone()
        .or(db_svn_password)
        .unwrap_or_default();
    let svn_import_url = {
        let base = config.svn.url.trim_end_matches('/');
        let trunk = if config.svn.trunk_path.is_empty() {
            "trunk"
        } else {
            &config.svn.trunk_path
        };
        if trunk.is_empty() || trunk == "/" {
            base.to_string()
        } else {
            format!("{}/{}", base, trunk.trim_start_matches('/'))
        }
    };
    info!(svn_import_url = %svn_import_url, "SVN import URL");
    let cancel_signal = state.import_progress.read().await.cancel_signal.clone();
    let svn_client = SvnClient::new(&svn_import_url, &config.svn.username, &svn_password)
        .with_cancel_signal(cancel_signal.clone());

    let git_token = config.github.token.clone().or(db_git_token);
    let git_repo_path = config.daemon.data_dir.join("git-repo");

    std::fs::create_dir_all(&config.daemon.data_dir)
        .map_err(|e| AppError::Internal(format!("failed to create data dir: {}", e)))?;

    let git_client = if git_repo_path.join(".git").exists() {
        GitClient::new(&git_repo_path)
            .map_err(|e| AppError::Internal(format!("failed to open git repo: {}", e)))?
    } else {
        // A missing/unreachable target must never be replaced with a freshly
        // inited local repository. Leave the enrolled import_operation_v1 row
        // held; do not start the importer.
        let clone_url = config.github.clone_url();
        GitClient::clone_repo(&clone_url, &git_repo_path, git_token.as_deref()).map_err(|e| {
            AppError::BadRequest(format!(
                "Git target could not be cloned; import held for inspection: {e}"
            ))
        })?
    };

    git_client
        .ensure_remote_credentials("origin", git_token.as_deref())
        .map_err(|e| AppError::Internal(format!("failed to set git credentials: {}", e)))?;

    let git_client = Arc::new(std::sync::Mutex::new(git_client));
    let identity_mapper = IdentityMapper::new(&config.identity)
        .map_err(|e| AppError::Internal(format!("failed to init identity mapper: {}", e)))?;
    let file_policy = FilePolicy::from(&config.sync);
    let db_path = config.daemon.data_dir.join("reposync.db");
    let import_db = Database::new(&db_path)
        .map_err(|e| AppError::Internal(format!("failed to open db: {}", e)))?;

    let import_config = ImportConfig {
        committer_name: "RepoSync".into(),
        committer_email: "reposync@localhost".into(),
        remote_name: "origin".into(),
        branch: config.github.default_branch.clone(),
        push_token: git_token,
        message_prefix: None,
        trunk_path: config.svn.trunk_path.clone(),
    };

    let progress = state.import_progress.clone();
    let ws_broadcast = Some(state.ws_broadcast.clone());
    let worker_repo_id = repo_id;
    let worker_operation_id = operation_id;
    let worker_cancel = cancel_signal;

    tokio::spawn(async move {
        let _busy_guard = busy_guard;
        let current = import_db.get_import_operation(&worker_repo_id, &worker_operation_id);
        let result = match current {
            Ok(Some(op)) if op.cancel_requested => {
                Ok(import::ImportOutcome::Cancelled { commits: 0 })
            }
            Ok(Some(_)) => {
                match import_db.start_import_operation(&worker_repo_id, &worker_operation_id) {
                    Ok(_) => {
                        import::run_full_import(
                            &svn_client,
                            &git_client,
                            &identity_mapper,
                            &import_db,
                            &file_policy,
                            &import_config,
                            import::ImportRunState {
                                progress: progress.clone(),
                                ws_broadcast: ws_broadcast.clone(),
                                repo_id: Some(worker_repo_id.clone()),
                                operation_id: Some(worker_operation_id.clone()),
                                cancel_signal: Some(worker_cancel),
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
                    &worker_repo_id,
                    &worker_operation_id,
                    svn_rev,
                    &git_sha,
                ) {
                    Ok(_) => {
                        info!(repo_id = %worker_repo_id, commits, "setup import completed and confirmed");
                        ImportPhase::Completed
                    }
                    Err(e) => {
                        error!(repo_id = %worker_repo_id, error = %e, "setup import finalization failed");
                        let _ = import_db.finish_import_operation(
                            &worker_repo_id,
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
                    .get_import_operation(&worker_repo_id, &worker_operation_id)
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
                    &worker_repo_id,
                    &worker_operation_id,
                    state,
                    &detail,
                ) {
                    error!(repo_id = %worker_repo_id, error = %e, "setup cancel finalization failed");
                    ImportPhase::Failed
                } else if uncertain {
                    ImportPhase::Failed
                } else {
                    ImportPhase::Cancelled
                }
            }
            Ok(import::ImportOutcome::ReconciliationRequired { reason, .. }) => {
                if let Err(e) = import_db.finish_import_operation(
                    &worker_repo_id,
                    &worker_operation_id,
                    ImportOperationState::ReconciliationRequired,
                    &reason,
                ) {
                    error!(repo_id = %worker_repo_id, error = %e, "uncertain setup outcome persistence failed");
                }
                ImportPhase::Failed
            }
            Err(e) => {
                let detail = format!("import stopped with error; inspect local work: {e:#}");
                if let Err(write_error) = import_db.finish_import_operation(
                    &worker_repo_id,
                    &worker_operation_id,
                    ImportOperationState::Failed,
                    &detail,
                ) {
                    error!(repo_id = %worker_repo_id, error = %write_error, "setup failure persistence failed");
                }
                ImportPhase::Failed
            }
        };

        let mut p = progress.write().await;
        p.phase = terminal;
        p.completed_at = Some(chrono::Utc::now().to_rfc3339());

        if let Err(e) = import_db.persist_import_progress(&p) {
            tracing::warn!("failed to persist final import progress: {}", e);
        }

        if let Some(ref sender) = ws_broadcast {
            let json = serde_json::json!({
                "type": "import_progress",
                "phase": format!("{:?}", p.phase).to_lowercase(),
                "current_rev": p.current_rev,
                "total_revs": p.total_revs,
                "commits_created": p.commits_created,
            });
            let _ = sender.send(json.to_string());
        }
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// Reset & Reimport
// ---------------------------------------------------------------------------

async fn reset_and_reimport(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<ImportActionResponse>, AppError> {
    // Admin only
    let (user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    let repo = setup_repository(&state.db)?;
    if let Some(active) = state
        .db
        .active_import_operation(&repo.id)
        .map_err(|e| AppError::Internal(e.to_string()))?
    {
        return Ok(action_response(
            false,
            "import is active or held; inspect or cancel before reset",
            Some(&active),
        ));
    }
    {
        let p = state.import_progress.read().await;
        if setup_import_phase_busy(&p.phase) || reposync_core::busy::is_busy(&repo.id) {
            return Ok(action_response(false, "An import is already running", None));
        }
    }

    let busy_guard = match reposync_core::busy::try_acquire(&repo.id) {
        Some(guard) => guard,
        None => {
            return Ok(action_response(
                false,
                "A sync cycle is currently running for this repository. Please retry in a moment.",
                None,
            ));
        }
    };

    // Set phase to Connecting immediately — this pauses the scheduler
    {
        let mut p = state.import_progress.write().await;
        *p = ImportProgress::default();
        p.phase = ImportPhase::Connecting;
        p.started_at = Some(chrono::Utc::now().to_rfc3339());
        p.push_log("[info] Reset & Reimport: starting...".into());
    }

    // Load config to find paths and git remote info
    let config_content = std::fs::read_to_string(&state.config_path)
        .map_err(|e| AppError::Internal(format!("failed to read config: {}", e)))?;
    let mut config: AppConfig = toml::from_str(&config_content)
        .map_err(|e| AppError::Internal(format!("failed to parse config: {}", e)))?;
    config
        .resolve_env_vars()
        .map_err(|e| AppError::Internal(format!("failed to resolve env vars: {}", e)))?;

    let git_repo_path = config.daemon.data_dir.join("git-repo");
    let git_token = {
        let db = &state.db;
        config
            .github
            .token
            .clone()
            .or_else(|| {
                db.conn()
                    .query_row(
                        "SELECT value FROM kv_state WHERE key = 'secret_git_token'",
                        [],
                        |row| row.get(0),
                    )
                    .ok()
            })
            .unwrap_or_default()
    };

    // 1. Wipe local git repo
    {
        let mut p = state.import_progress.write().await;
        p.push_log("[info] Deleting local git repository...".into());
    }
    if git_repo_path.exists() {
        std::fs::remove_dir_all(&git_repo_path)
            .map_err(|e| AppError::Internal(format!("failed to delete git repo: {}", e)))?;
    }
    info!("deleted local git repo at {}", git_repo_path.display());

    // 2. Create fresh empty repo and force-push to remote
    {
        let mut p = state.import_progress.write().await;
        p.push_log("[info] Creating empty git repository and resetting remote...".into());
    }
    std::fs::create_dir_all(&git_repo_path)
        .map_err(|e| AppError::Internal(format!("mkdir failed: {}", e)))?;
    let branch = &config.github.default_branch;
    let clone_url = format!(
        "https://x-access-token:{}@{}",
        git_token,
        config.github.clone_url().trim_start_matches("https://")
    );

    // git init + empty commit + force push
    let init_cmds = [
        vec!["init", "--initial-branch", branch],
        vec![
            "commit",
            "--allow-empty",
            "-m",
            "Reset for full SVN reimport",
        ],
        vec!["remote", "add", "origin", &clone_url],
        vec!["push", "--force", "origin", branch],
    ];
    for args in &init_cmds {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&git_repo_path)
            .output()
            .map_err(|e| AppError::Internal(format!("git {} failed: {}", args[0], e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // git init may fail with --initial-branch on older git, retry without
            if args[0] == "init" {
                let _ = std::process::Command::new("git")
                    .args(["init"])
                    .current_dir(&git_repo_path)
                    .output();
            } else {
                let msg = format!("git {} failed: {}", args[0], stderr);
                let mut p = state.import_progress.write().await;
                p.phase = ImportPhase::Failed;
                p.push_log(format!("[error] {}", msg));
                return Ok(action_response(false, msg, None));
            }
        }
    }
    info!("force-pushed empty commit to remote");

    // 3. Clear DB sync data
    {
        let mut p = state.import_progress.write().await;
        p.push_log("[info] Clearing sync data from database...".into());
    }
    state
        .db
        .clear_sync_data()
        .map_err(|e| AppError::Internal(format!("failed to clear sync data: {}", e)))?;

    // 4. Delete the git repo again so spawn_import_task can create it fresh
    //    (it expects either .git to exist or not — we need a clean state)
    std::fs::remove_dir_all(&git_repo_path).ok();

    // 5. Enroll a durable operation after destructive prep, then spawn.
    let request_id = format!("setup-reset-{}", Uuid::new_v4());
    let fingerprint = import_target_fingerprint(&repo, &git_repo_path);
    let operation = state
        .db
        .create_import_operation(&repo.id, &user_id, &request_id, &fingerprint)
        .map_err(import_write_error)?;
    let operation_id = operation.id.clone();
    let mut preparation_guard = SetupPreparationGuard {
        db: &state.db,
        repo_id: repo.id.clone(),
        operation_id: operation_id.clone(),
        armed: true,
    };
    {
        let mut p = state.import_progress.write().await;
        p.phase = ImportPhase::Importing;
        p.push_log("[info] Starting full SVN import from revision 0...".into());
    }

    if let Err(e) =
        spawn_import_task(&state, repo.id.clone(), operation_id.clone(), busy_guard).await
    {
        {
            let mut p = state.import_progress.write().await;
            p.phase = ImportPhase::Failed;
            p.completed_at = Some(chrono::Utc::now().to_rfc3339());
            p.push_log("[error] setup import preparation failed".into());
        }
        preparation_guard.finalize_preparation_hold()?;
        return Err(e);
    }
    preparation_guard.disarm();

    Ok(Json(ImportActionResponse {
        ok: true,
        message: "Reset and reimport started".into(),
        operation_id: Some(operation_id),
        lifecycle: Some(ImportOperationState::Queued),
    }))
}
