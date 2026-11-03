//! Authentication endpoints (multi-user with backward-compatible single-password fallback).

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use tracing::info;

use crate::api::status::AppError;
use crate::AppState;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginRequest {
    /// Username for multi-user login. Optional for backward compat with
    /// single-password mode.
    pub username: Option<String>,
    pub password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    token: String,
    expires_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<UserInfo>,
}

#[derive(Serialize, Clone)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub email: String,
    pub role: String,
}

#[derive(Deserialize)]
pub struct LogoutRequest {
    pub token: String,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/verify", post(verify))
        .route("/api/auth/me", get(me))
        .route("/api/auth/info", get(auth_info))
}

/// Public (no auth) endpoint returning login page context: whether LDAP is
/// enabled and the domain so users know which credentials to enter.
async fn auth_info(
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let db = &state.db;
    let ldap_enabled = db.is_ldap_enabled().unwrap_or(false);
    let ldap_domain = if ldap_enabled {
        db.load_ldap_config().ok().flatten().map(|cfg| {
            // Extract domain from base_dn: "dc=mgc,dc=mentorg,dc=com" → "mgc.mentorg.com"
            cfg.base_dn
                .split(',')
                .filter_map(|part| {
                    let part = part.trim();
                    if part.to_lowercase().starts_with("dc=") {
                        Some(part[3..].to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(".")
        })
    } else {
        None
    };
    Json(serde_json::json!({
        "ldap_enabled": ldap_enabled,
        "ldap_domain": ldap_domain,
    }))
}

async fn login(
    State(state): State<Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AppError> {
    // Rate limiting: max 10 login attempts per IP per 60 seconds
    {
        let ip = addr.ip().to_string();
        let mut attempts = state.login_attempts.lock().unwrap_or_else(|p| p.into_inner());
        let (count, window_start) = attempts
            .entry(ip)
            .or_insert((0, std::time::Instant::now()));
        if window_start.elapsed() > std::time::Duration::from_secs(60) {
            *count = 0;
            *window_start = std::time::Instant::now();
        }
        *count += 1;
        if *count > 10 {
            return Err(AppError::BadRequest(
                "too many login attempts, please try again later".into(),
            ));
        }
    }

    // Check if any users exist in the database (multi-user mode).
    let has_users = {
        let db = &state.db;
        db.count_users()
            .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
            > 0
    };

    if has_users {
        // Multi-user mode: require username
        let username = body.username.as_deref().unwrap_or("");
        if username.is_empty() {
            return Err(AppError::BadRequest("username is required".into()));
        }

        // -------------------------------------------------------------------
        // Try LDAP authentication first (if enabled)
        // -------------------------------------------------------------------
        let ldap_result = {
            let db = &state.db;
            let ldap_enabled = db.is_ldap_enabled().unwrap_or(false);
            if ldap_enabled {
                db.load_ldap_config()
                    .map_err(|e| AppError::Internal(format!("ldap config error: {}", e)))?
            } else {
                None
            }
        };

        if let Some(ref ldap_config) = ldap_result {
            tracing::debug!("login: before LDAP authenticate for '{}'", username);
            tracing::info!("Attempting LDAP auth for '{}' against {}", username, ldap_config.url);
            // LDAP auth is async (uses tokio-native-tls) — run directly
            // LDAP auth with a 5-second timeout to prevent blocking the
            // tokio runtime when the LDAP server is slow or unreachable.
            let ldap_auth_result = match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                ldap_config.authenticate(username, &body.password),
            ).await {
                Ok(result) => result,
                Err(_) => {
                    tracing::warn!("LDAP auth timed out after 5s for '{}'", username);
                    Err(reposync_core::ldap_auth::LdapAuthError::ConnectionFailed(
                        "LDAP connection timed out after 5 seconds".into()
                    ))
                }
            };
            match ldap_auth_result {
                Ok(ldap_user) => {
                    tracing::debug!("login: LDAP auth succeeded for '{}', provisioning user", username);
                    // LDAP auth succeeded — provision or update local user
                    let (token, expires_at, user_info) = {
                        let db = &state.db;

                        let local_user = db
                            .get_user_by_username(username)
                            .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

                        let user = if let Some(existing) = local_user {
                            // Update display_name and email from LDAP
                            let _ = db.update_user(
                                &existing.id,
                                &ldap_user.display_name,
                                &ldap_user.email,
                                &existing.role,
                                existing.enabled,
                            );
                            db.get_user(&existing.id)
                                .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
                                .unwrap_or(existing)
                        } else {
                            // Auto-provision new user from LDAP attributes
                            let random_hash = reposync_core::crypto::hash_password(
                                &Uuid::new_v4().to_string(),
                            )
                            .map_err(|e| {
                                AppError::Internal(format!("password hashing error: {}", e))
                            })?;

                            let now = Utc::now().to_rfc3339();
                            let new_user = reposync_core::models::User {
                                id: Uuid::new_v4().to_string(),
                                username: ldap_user.username.clone(),
                                display_name: ldap_user.display_name.clone(),
                                email: ldap_user.email.clone(),
                                password_hash: random_hash,
                                role: "user".to_string(),
                                enabled: true,
                                created_at: now.clone(),
                                updated_at: now,
                            };

                            db.insert_user(&new_user)
                                .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;
                            new_user
                        };

                        if !user.enabled {
                            return Err(AppError::Unauthorized("account is disabled".into()));
                        }

                        // Create DB session
                        let token = Uuid::new_v4().to_string();
                        let now = Utc::now();
                        let expires_at = now + Duration::hours(24);

                        let session = reposync_core::models::Session {
                            token: token.clone(),
                            user_id: user.id.clone(),
                            expires_at: expires_at.to_rfc3339(),
                            created_at: now.to_rfc3339(),
                        };

                        db.insert_session(&session)
                            .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

                        let info = UserInfo {
                            id: user.id,
                            username: user.username,
                            display_name: user.display_name,
                            email: user.email,
                            role: user.role,
                        };

                        (token, expires_at, info)
                    };

                    {
                        let mut sessions = state.sessions.write().await;
                        sessions.insert(token.clone(), expires_at);
                    }

                    return Ok(Json(LoginResponse {
                        token,
                        expires_at: expires_at.to_rfc3339(),
                        user: Some(user_info),
                    }));
                }
                Err(e) => {
                    // LDAP failed — fall through to local auth
                    tracing::debug!("login: LDAP auth failed, falling through to local auth");
                    tracing::warn!("LDAP auth failed for '{}': {}", username, e);
                }
            }
        }

        // -------------------------------------------------------------------
        // Local bcrypt authentication
        // -------------------------------------------------------------------
        let (token, expires_at, user_info) = {
            let db = &state.db;

            let user = db
                .get_user_by_username(username)
                .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
                .ok_or_else(|| AppError::Unauthorized("invalid username or password".into()))?;

            if !user.enabled {
                return Err(AppError::Unauthorized("account is disabled".into()));
            }

            // bcrypt is intentionally CPU-heavy — run on blocking thread
            let pw = body.password.clone();
            let hash = user.password_hash.clone();
            let password_valid = tokio::task::spawn_blocking(move || {
                reposync_core::crypto::verify_password(&pw, &hash)
            })
            .await
            .map_err(|e| AppError::Internal(format!("spawn_blocking: {}", e)))?
            .map_err(|e| AppError::Internal(format!("password verification error: {}", e)))?;

            if !password_valid {
                return Err(AppError::Unauthorized("invalid username or password".into()));
            }

            // Create DB session
            let token = Uuid::new_v4().to_string();
            let now = Utc::now();
            let expires_at = now + Duration::hours(24);

            let session = reposync_core::models::Session {
                token: token.clone(),
                user_id: user.id.clone(),
                expires_at: expires_at.to_rfc3339(),
                created_at: now.to_rfc3339(),
            };

            db.insert_session(&session)
                .map_err(|e| AppError::Internal(format!("database error: {}", e)))?;

            let info = UserInfo {
                id: user.id,
                username: user.username,
                display_name: user.display_name,
                email: user.email,
                role: user.role,
            };

            (token, expires_at, info)
        };

        {
            let mut sessions = state.sessions.write().await;
            sessions.insert(token.clone(), expires_at);
        }

        Ok(Json(LoginResponse {
            token,
            expires_at: expires_at.to_rfc3339(),
            user: Some(user_info),
        }))
    } else {
        // Backward-compatible single-password mode
        let configured_password = state.config.web.admin_password.as_deref().unwrap_or("");

        if configured_password.is_empty() {
            return Err(AppError::BadRequest(
                "authentication is not configured (no admin password set and no users created)".into(),
            ));
        }

        // Check if the stored value is a bcrypt hash (starts with "$2")
        let password_matches = if configured_password.starts_with("$2") {
            // bcrypt hash comparison (constant-time internally)
            reposync_core::crypto::verify_password(&body.password, configured_password)
                .unwrap_or(false)
        } else {
            // Legacy plaintext — constant-time comparison
            use subtle::ConstantTimeEq;
            let matches: bool = if body.password.len() == configured_password.len() {
                body.password.as_bytes().ct_eq(configured_password.as_bytes()).into()
            } else {
                false
            };
            // Auto-migrate to bcrypt if match succeeds
            if matches {
                if let Ok(hash) = reposync_core::crypto::hash_password(&body.password) {
                    let _ = state.db.set_state("secret_admin_password_hash", &hash);
                    let _ = state.db.set_state("secret_admin_password", "");
                    info!("auto-migrated admin password to bcrypt hash");
                }
            }
            matches
        };

        if !password_matches {
            return Err(AppError::Unauthorized("invalid password".into()));
        }

        let token = Uuid::new_v4().to_string();
        let expires_at = Utc::now() + Duration::hours(24);

        {
            let mut sessions = state.sessions.write().await;
            sessions.insert(token.clone(), expires_at);
        }

        Ok(Json(LoginResponse {
            token,
            expires_at: expires_at.to_rfc3339(),
            user: None,
        }))
    }
}

async fn logout(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LogoutRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Remove from DB sessions first
    let _ = state.db.delete_session(&body.token);

    // Remove from in-memory sessions
    {
        let mut sessions = state.sessions.write().await;
        sessions.remove(&body.token);
    }

    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "logged out",
    })))
}

#[derive(Deserialize)]
pub struct VerifyRequest {
    pub token: String,
}

async fn verify(
    State(state): State<Arc<AppState>>,
    Json(body): Json<VerifyRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Check DB sessions first
    let db_result = {
        let db = &state.db;
        if let Ok(Some(session)) = db.get_session(&body.token) {
            if let Ok(Some(user)) = db.get_user(&session.user_id) {
                Some(serde_json::json!({
                    "valid": true,
                    "expires_at": session.expires_at,
                    "user": {
                        "id": user.id,
                        "username": user.username,
                        "display_name": user.display_name,
                        "email": user.email,
                        "role": user.role,
                    }
                }))
            } else {
                Some(serde_json::json!({
                    "valid": true,
                    "expires_at": session.expires_at,
                }))
            }
        } else {
            None
        }
    };

    if let Some(result) = db_result {
        return Ok(Json(result));
    }

    // Fallback to in-memory sessions (backward compat)
    let sessions = state.sessions.read().await;
    if let Some(expires_at) = sessions.get(&body.token) {
        if *expires_at > Utc::now() {
            return Ok(Json(serde_json::json!({
                "valid": true,
                "expires_at": expires_at.to_rfc3339(),
            })));
        }
    }

    Ok(Json(serde_json::json!({
        "valid": false,
    })))
}

async fn me(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(|| AppError::Unauthorized("missing or invalid Authorization header".into()))?;

    // Check DB session
    let db_result = {
        let db = &state.db;
        if let Ok(Some(session)) = db.get_session(token) {
            if let Ok(Some(user)) = db.get_user(&session.user_id) {
                Some(serde_json::json!({
                    "id": user.id,
                    "username": user.username,
                    "display_name": user.display_name,
                    "email": user.email,
                    "role": user.role,
                }))
            } else {
                None
            }
        } else {
            None
        }
    };

    if let Some(user_json) = db_result {
        return Ok(Json(user_json));
    }

    // Fallback: if in-memory session exists (legacy mode), return minimal info
    let sessions = state.sessions.read().await;
    if let Some(expires_at) = sessions.get(token) {
        if *expires_at > Utc::now() {
            return Ok(Json(serde_json::json!({
                "id": "legacy",
                "username": "admin",
                "display_name": "Admin",
                "email": "",
                "role": "admin",
            })));
        }
    }

    Err(AppError::Unauthorized("session expired or invalid".into()))
}

/// Middleware helper to validate a session token from the Authorization header.
///
/// Call this from handlers that require authentication. Returns `Ok(())` if
/// the token is valid, or `Err(AppError::Unauthorized)` otherwise.
///
/// Also opportunistically prunes expired sessions to prevent unbounded growth.
pub async fn validate_session(
    state: &Arc<AppState>,
    auth_header: Option<&str>,
) -> Result<(), AppError> {
    // If no admin password is configured AND no users exist, require setup first.
    // Setup endpoints handle their own auth bypass for fresh instances.
    if state.config.web.admin_password.is_none() {
        let has_users = state.db.count_users()
            .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
            > 0;
        if !has_users {
            return Err(AppError::Forbidden(
                "initial setup required — please complete the setup wizard".into(),
            ));
        }
    }

    let token = auth_header
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(|| AppError::Unauthorized("missing or invalid Authorization header".into()))?;

    // Check DB sessions first
    {
        let db = &state.db;
        if let Ok(Some(_session)) = db.get_session(token) {
            // Valid DB session — also prune expired sessions opportunistically
            let _ = db.prune_expired_sessions();
            return Ok(());
        }
    }

    // Fallback to in-memory sessions (backward compat)
    let now = Utc::now();
    let sessions = state.sessions.read().await;
    if let Some(expires_at) = sessions.get(token) {
        if *expires_at > now {
            return Ok(());
        }
    }
    // Don't prune here — let login/logout handle pruning

    Err(AppError::Unauthorized("session expired or invalid".into()))
}

/// Validate a session and return the user's role. Returns `None` for legacy
/// single-password sessions (treated as admin).
pub async fn validate_session_with_role(
    state: &Arc<AppState>,
    auth_header: Option<&str>,
) -> Result<(String, String), AppError> {
    // If no admin password is configured AND no users exist, require setup first.
    if state.config.web.admin_password.is_none() {
        let has_users = state.db.count_users()
            .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
            > 0;
        if !has_users {
            return Err(AppError::Forbidden(
                "initial setup required — please complete the setup wizard".into(),
            ));
        }
    }

    let token = auth_header
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(|| AppError::Unauthorized("missing or invalid Authorization header".into()))?;

    // Check DB sessions first
    {
        let db = &state.db;
        if let Ok(Some(session)) = db.get_session(token) {
            if let Ok(Some(user)) = db.get_user(&session.user_id) {
                return Ok((user.id, user.role));
            }
        }
    }

    // Fallback to in-memory sessions (backward compat — treat as admin)
    let sessions = state.sessions.read().await;
    if let Some(expires_at) = sessions.get(token) {
        if *expires_at > Utc::now() {
            return Ok(("legacy".into(), "admin".into()));
        }
    }

    Err(AppError::Unauthorized("session expired or invalid".into()))
}
