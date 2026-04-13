//! Setup wizard API endpoints.
//!
//! - Test SVN / Git connections
//! - Apply configuration from wizard data (generates TOML server-side)
//! - Trigger full SVN→Git history import with progress tracking
//! - Poll import status

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use reposync_core::config::AppConfig;
use reposync_core::db::Database;
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::{self, ImportConfig, ImportProgress, ImportPhase};
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
    ).await?;

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
        svn_layout: if cfg.svn.trunk_path.is_empty() { "single".into() } else { "standard".into() },
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
        ).await?;
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
        ).await?;
    }
    let api_url = body.api_url.trim().trim_end_matches('/').to_string();
    let repo = body.repo.trim().to_string();

    if api_url.is_empty() || repo.is_empty() {
        return Ok(Json(TestConnectionResponse {
            ok: false,
            message: "API URL and repository are required".into(),
        }));
    }

    let check_url = if body.provider == "gitea" {
        format!("{}/repos/{}", api_url, repo)
    } else {
        format!("{}/repos/{}", api_url, repo)
    };

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
                    message: "Repository not found (404). Check the repo name and API URL."
                        .into(),
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
        ).await?;
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
            let config_dir = state.config_path.parent().unwrap_or_else(|| std::path::Path::new("."));
            let full_mapping_path = if std::path::Path::new(mapping_path).is_absolute() {
                std::path::PathBuf::from(mapping_path)
            } else {
                config_dir.join(mapping_path)
            };

            if let Err(e) = std::fs::write(&full_mapping_path, &mapping_content) {
                warnings.push(format!(
                    "Failed to write identity mapping file: {}",
                    e
                ));
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
                Ok(()) => info!(id = %new_repo.id, name = %new_repo.name, "Created repository from setup wizard"),
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

async fn start_import(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<ImportActionResponse>, AppError> {
    let has_users = state.db.count_users().unwrap_or(0) > 0;
    if state.config.web.admin_password.is_some() || has_users {
        crate::api::auth::validate_session(
            &state,
            headers.get("authorization").and_then(|v| v.to_str().ok()),
        ).await?;
    }
    // Check if already running
    {
        let p = state.import_progress.read().await;
        if p.phase == ImportPhase::Importing {
            return Ok(Json(ImportActionResponse {
                ok: false,
                message: "An import is already running".into(),
            }));
        }
    }

    // Reset progress
    {
        let mut p = state.import_progress.write().await;
        *p = ImportProgress::default();
        p.phase = ImportPhase::Importing;
        p.started_at = Some(chrono::Utc::now().to_rfc3339());
    }

    spawn_import_task(&state).await?;

    Ok(Json(ImportActionResponse {
        ok: true,
        message: "Import started".into(),
    }))
}

async fn import_status(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<ImportProgress>, AppError> {
    let has_users = state.db.count_users().unwrap_or(0) > 0;
    if state.config.web.admin_password.is_some() || has_users {
        crate::api::auth::validate_session(
            &state,
            headers.get("authorization").and_then(|v| v.to_str().ok()),
        ).await?;
    }
    let p = state.import_progress.read().await;

    // If in-memory progress shows Idle, check the DB for persisted state
    // (e.g. after a daemon restart mid-import).
    if p.phase == ImportPhase::Idle {
        let db = &state.db;
        if let Ok(Some(db_progress)) = db.load_import_progress() {
            if db_progress.phase != ImportPhase::Idle {
                return Ok(Json(db_progress));
            }
        }
    }

    Ok(Json(p.clone()))
}

async fn cancel_import(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<ImportActionResponse>, AppError> {
    let has_users = state.db.count_users().unwrap_or(0) > 0;
    if state.config.web.admin_password.is_some() || has_users {
        crate::api::auth::validate_session(
            &state,
            headers.get("authorization").and_then(|v| v.to_str().ok()),
        ).await?;
    }
    let mut p = state.import_progress.write().await;
    if p.phase == ImportPhase::Importing {
        p.cancel_requested = true;
        Ok(Json(ImportActionResponse {
            ok: true,
            message: "Cancellation requested".into(),
        }))
    } else {
        Ok(Json(ImportActionResponse {
            ok: false,
            message: "No import is currently running".into(),
        }))
    }
}

// ---------------------------------------------------------------------------
// Shared import helper
// ---------------------------------------------------------------------------

/// Resolve config, build clients, and spawn the background import task.
/// Used by both `start_import` and `reset_and_reimport`.
async fn spawn_import_task(state: &Arc<AppState>) -> Result<(), AppError> {
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
    let svn_client = SvnClient::new(&svn_import_url, &config.svn.username, &svn_password);

    let git_token = config.github.token.clone().or(db_git_token);
    let git_repo_path = config.daemon.data_dir.join("git-repo");

    std::fs::create_dir_all(&config.daemon.data_dir)
        .map_err(|e| AppError::Internal(format!("failed to create data dir: {}", e)))?;

    let git_client = if git_repo_path.join(".git").exists() {
        GitClient::new(&git_repo_path)
            .map_err(|e| AppError::Internal(format!("failed to open git repo: {}", e)))?
    } else {
        let clone_url = config.github.clone_url();
        match GitClient::clone_repo(&clone_url, &git_repo_path, git_token.as_deref()) {
            Ok(client) => client,
            Err(_) => {
                info!("Clone failed, initializing empty repo with remote");
                std::fs::create_dir_all(&git_repo_path)
                    .map_err(|e| AppError::Internal(format!("mkdir failed: {}", e)))?;
                let output = std::process::Command::new("git")
                    .args(["init", "--initial-branch", &config.github.default_branch])
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

    tokio::spawn(async move {
        let result = import::run_full_import(
            &svn_client,
            &git_client,
            &identity_mapper,
            &import_db,
            &file_policy,
            &import_config,
            progress.clone(),
            ws_broadcast.clone(),
            None, // setup wizard doesn't have a repo_id yet
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
                info!(count, "import completed successfully");
            }
            Err(e) => {
                p.phase = ImportPhase::Failed;
                p.completed_at = Some(chrono::Utc::now().to_rfc3339());
                let msg = format!("[error] Import failed: {}", e);
                p.push_log(msg.clone());
                p.errors.push(msg);
                error!("import failed: {}", e);
            }
        }

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
    let (_user_id, role) = validate_session_with_role(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;
    if role != "admin" {
        return Err(AppError::Unauthorized("admin access required".into()));
    }

    // If an import is already running, cancel it and wait for it to stop.
    {
        let is_active = {
            let p = state.import_progress.read().await;
            matches!(
                p.phase,
                ImportPhase::Importing
                    | ImportPhase::Connecting
                    | ImportPhase::Verifying
                    | ImportPhase::FinalPush
            )
        };

        if is_active {
            info!("cancelling running import before reset");
            {
                let mut p = state.import_progress.write().await;
                p.cancel_requested = true;
            }

            // Poll until the import stops (max 30 seconds)
            let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                let phase = state.import_progress.read().await.phase.clone();
                if matches!(
                    phase,
                    ImportPhase::Idle
                        | ImportPhase::Completed
                        | ImportPhase::Failed
                        | ImportPhase::Cancelled
                ) {
                    info!(?phase, "previous import stopped");
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Ok(Json(ImportActionResponse {
                        ok: false,
                        message: "Timed out waiting for running import to cancel".into(),
                    }));
                }
            }
        }
    }

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
        config
            .github
            .clone_url()
            .trim_start_matches("https://")
    );

    // git init + empty commit + force push
    let init_cmds = [
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
                return Ok(Json(ImportActionResponse {
                    ok: false,
                    message: msg,
                }));
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

    // 5. Reset progress for import phase and spawn import
    {
        let mut p = state.import_progress.write().await;
        p.phase = ImportPhase::Importing;
        p.push_log("[info] Starting full SVN import from revision 0...".into());
    }

    spawn_import_task(&state).await?;

    Ok(Json(ImportActionResponse {
        ok: true,
        message: "Reset and reimport started".into(),
    }))
}
