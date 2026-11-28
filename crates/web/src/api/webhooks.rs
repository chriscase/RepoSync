//! Webhook receiver endpoints for GitHub and SVN push notifications.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::error::TrySendError;
use tracing::{info, warn};

use crate::api::status::AppError;
use crate::AppState;

use reposync_core::config::GitProvider;

// ---------------------------------------------------------------------------
// GitHub webhook types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GitHubPushPayload {
    #[serde(rename = "ref")]
    git_ref: String,
    /// GitHub/Gitea hint that the push was not a fast-forward. Never authority
    /// to reset; polling inspection remains the safety gate.
    #[serde(default)]
    forced: bool,
    commits: Option<Vec<GitHubCommitPayload>>,
    repository: Option<GitHubRepoPayload>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GitHubCommitPayload {
    id: String,
    message: String,
    author: GitHubAuthorPayload,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GitHubAuthorPayload {
    name: String,
    email: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRepoPayload {
    full_name: String,
}

// ---------------------------------------------------------------------------
// SVN webhook types
// ---------------------------------------------------------------------------

/// SVN post-commit notification. Only `revision` is required: the hook that
/// `reposync svn-hook install` writes sends only `revision`. Every field is a
/// hint; the inspection reads SVN itself.
#[derive(Debug, Deserialize)]
struct SvnPostCommitPayload {
    revision: i64,
    #[serde(default)]
    author: String,
}

#[derive(Serialize)]
struct WebhookResponse {
    ok: bool,
    message: String,
    /// `true` only when this delivery queued a new inspection. A duplicate,
    /// out-of-scope or ignored delivery, or one that arrives while an
    /// inspection is already pending, reports `false`.
    triggered: bool,
    /// Present on GitHub push events. `true` only means "inspect"; never reset.
    #[serde(skip_serializing_if = "Option::is_none")]
    forced_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inspection: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/webhook/github", post(github_webhook))
        .route("/webhook/svn", post(svn_webhook))
}

// ---------------------------------------------------------------------------
// Delivery deduplication, scope and inspection trigger
// ---------------------------------------------------------------------------

/// How many delivery IDs are remembered. Older IDs are forgotten first. A
/// forgotten ID that is redelivered costs one extra inspection, nothing more.
const DELIVERY_LOG_CAPACITY: usize = 4096;

/// Delivery IDs, from authenticated senders, that already queued an
/// inspection, in arrival order.
///
/// The log is process-wide and in memory only: a restart forgets it, which at
/// worst lets one redelivery prompt one more inspection. It is never used to
/// decide what to replay.
#[derive(Default)]
struct DeliveryLog {
    order: VecDeque<String>,
    seen: HashSet<String>,
}

impl DeliveryLog {
    /// Record `key`. Returns `false` when it was already recorded.
    fn record(&mut self, key: &str) -> bool {
        if self.seen.contains(key) {
            return false;
        }
        while self.order.len() >= DELIVERY_LOG_CAPACITY {
            match self.order.pop_front() {
                Some(old) => {
                    self.seen.remove(&old);
                }
                None => break,
            }
        }
        self.seen.insert(key.to_string());
        self.order.push_back(key.to_string());
        true
    }

    fn forget(&mut self, key: &str) {
        if self.seen.remove(key) {
            self.order.retain(|k| k != key);
        }
    }
}

fn delivery_log() -> &'static Mutex<DeliveryLog> {
    static LOG: OnceLock<Mutex<DeliveryLog>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(DeliveryLog::default()))
}

/// Record a delivery key. Returns `false` when it was already handled.
fn record_delivery(key: &str) -> bool {
    delivery_log()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .record(key)
}

fn forget_delivery(key: &str) {
    delivery_log()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .forget(key);
}

/// The provider's delivery ID, when it sent a usable one.
fn delivery_id(headers: &HeaderMap, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|v| !v.is_empty() && v.len() <= 128)
            .map(str::to_string)
    })
}

/// Reduce a configured repository to a lowercase `owner/name`. Accepts
/// `owner/name`, `owner/name.git` and clone URLs.
fn normalize_repo_name(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let path = if let Some((_, rest)) = trimmed.split_once("://") {
        rest.split_once('/').map(|(_, p)| p).unwrap_or("")
    } else if let Some((_, rest)) = trimmed.split_once(':').filter(|(h, _)| h.contains('@')) {
        rest
    } else {
        trimmed
    };
    let mut parts = path.rsplitn(3, '/');
    let name = parts.next().filter(|p| !p.is_empty())?;
    let owner = parts.next().filter(|p| !p.is_empty())?;
    Some(format!("{}/{}", owner, name).to_ascii_lowercase())
}

/// Whether a push concerns something RepoSync syncs.
#[derive(Debug, PartialEq, Eq)]
enum PushScope {
    /// The configured `github.repo` at `github.default_branch`, or an enabled
    /// pair's Git repository at that pair's branch.
    Synced,
    /// No configured repository or enabled pair uses this repository.
    RepositoryNotSynced,
    /// The repository is synced, but not at this branch.
    BranchNotSynced,
}

/// A configured branch as the bare name a push's `refs/heads/<name>` carries.
fn configured_branch(value: &str) -> &str {
    let trimmed = value.trim();
    trimmed.strip_prefix("refs/heads/").unwrap_or(trimmed)
}

/// Scope a push of `branch` (without `refs/heads/`) to `full_name`. Branch
/// names are compared exactly, as Git does; repository names are not
/// case-sensitive, as on GitHub and Gitea.
fn push_scope(state: &AppState, full_name: &str, branch: &str) -> PushScope {
    let Some(wanted) = normalize_repo_name(full_name) else {
        return PushScope::RepositoryNotSynced;
    };
    let mut repository_synced = false;
    if normalize_repo_name(&state.config.github.repo).as_deref() == Some(wanted.as_str()) {
        if configured_branch(&state.config.github.default_branch) == branch {
            return PushScope::Synced;
        }
        repository_synced = true;
    }
    match state.db.list_repositories() {
        Ok(repos) => {
            for r in repos.iter().filter(|r| {
                r.enabled && normalize_repo_name(&r.git_repo).as_deref() == Some(wanted.as_str())
            }) {
                if configured_branch(&r.git_branch) == branch {
                    return PushScope::Synced;
                }
                repository_synced = true;
            }
        }
        Err(e) => {
            // Without the pair list the push cannot be scoped. Polling still
            // inspects every pair, so not triggering loses nothing.
            warn!("webhook scope check could not read repositories: {}", e);
        }
    }
    if repository_synced {
        PushScope::BranchNotSynced
    } else {
        PushScope::RepositoryNotSynced
    }
}

enum Inspection {
    /// A new inspection was queued.
    Queued,
    /// One is already queued; this delivery is folded into it.
    AlreadyPending,
}

/// Ask the scheduler for the same inspection a poll performs. Never waits:
/// a burst of deliveries collapses into the inspections already queued.
fn request_inspection(state: &AppState) -> Result<Inspection, AppError> {
    match state.sync_trigger.try_send(()) {
        Ok(()) => Ok(Inspection::Queued),
        Err(TrySendError::Full(())) => Ok(Inspection::AlreadyPending),
        Err(TrySendError::Closed(())) => Err(AppError::Internal(
            "webhook could not reach the scheduler; sync trigger is closed".into(),
        )),
    }
}

/// Dedup, then trigger. A delivery whose trigger fails is forgotten, so a
/// redelivery can try again.
fn dedup_and_inspect(state: &AppState, key: Option<&str>) -> Result<Option<Inspection>, AppError> {
    if let Some(key) = key {
        if !record_delivery(key) {
            return Ok(None);
        }
    }
    match request_inspection(state) {
        Ok(outcome) => Ok(Some(outcome)),
        Err(e) => {
            if let Some(key) = key {
                forget_delivery(key);
            }
            Err(e)
        }
    }
}

fn ignored(message: String) -> Json<WebhookResponse> {
    Json(WebhookResponse {
        ok: true,
        message,
        triggered: false,
        forced_hint: None,
        inspection: None,
    })
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn github_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<WebhookResponse>, AppError> {
    // Determine provider from headers
    let is_gitea =
        headers.contains_key("x-gitea-event") || headers.contains_key("x-gitea-signature");

    // Authenticity comes first: nothing below runs for an unverified body
    // when a secret is configured.
    if state.config.github.webhook_secret.is_none() {
        tracing::warn!(
            "GitHub webhook secret not configured - webhook payloads are not authenticated"
        );
    }
    if let Some(secret) = state.config.github.webhook_secret.as_deref() {
        let (signature, provider) = if is_gitea {
            let sig = headers
                .get("x-gitea-signature")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| AppError::Unauthorized("missing X-Gitea-Signature header".into()))?;
            (sig.to_string(), GitProvider::Gitea)
        } else {
            let sig = headers
                .get("x-hub-signature-256")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    AppError::Unauthorized("missing X-Hub-Signature-256 header".into())
                })?;
            (sig.to_string(), GitProvider::GitHub)
        };

        if !reposync_core::git::github::GitHubClient::verify_webhook_signature(
            &body, &signature, secret, &provider,
        ) {
            return Err(AppError::Unauthorized(
                "webhook signature verification failed".into(),
            ));
        }
    }

    // Parse the event type
    let event_type = headers
        .get("x-gitea-event")
        .or_else(|| headers.get("x-github-event"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    if event_type != "push" {
        info!(event_type, "ignoring non-push GitHub webhook event");
        return Ok(ignored(format!("event type '{}' ignored", event_type)));
    }

    // Parse the payload
    let payload: GitHubPushPayload = serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequest(format!("invalid JSON payload: {}", e)))?;

    // Scope: only branch pushes to a repository and branch RepoSync syncs.
    // Ignoring a push only gives up the early inspection; polling still
    // inspects every pair.
    let Some(repo_name) = payload.repository.as_ref().map(|r| r.full_name.as_str()) else {
        info!("ignoring push webhook without a repository");
        return Ok(ignored("push ignored: payload names no repository".into()));
    };
    let Some(branch) = payload.git_ref.strip_prefix("refs/heads/") else {
        info!(git_ref = %payload.git_ref, "ignoring push webhook for a non-branch ref");
        return Ok(ignored(format!(
            "push ignored: '{}' is not a branch",
            payload.git_ref
        )));
    };
    match push_scope(&state, repo_name, branch) {
        PushScope::Synced => {}
        PushScope::RepositoryNotSynced => {
            info!(
                repo = repo_name,
                "ignoring push webhook for a repository RepoSync does not sync"
            );
            return Ok(ignored(format!(
                "push ignored: repository '{}' is not synced by this RepoSync",
                repo_name
            )));
        }
        PushScope::BranchNotSynced => {
            info!(
                repo = repo_name,
                branch, "ignoring push webhook for a branch no enabled pair syncs"
            );
            return Ok(ignored(format!(
                "push ignored: branch '{}' of '{}' is not synced by this RepoSync",
                branch, repo_name
            )));
        }
    }

    let commit_count = payload.commits.as_ref().map(|c| c.len()).unwrap_or(0);
    // Delivery IDs are only trusted from authenticated senders; otherwise
    // anyone could pre-record a predictable ID and suppress the real delivery.
    let delivery = state
        .config
        .github
        .webhook_secret
        .as_ref()
        .and_then(|_| delivery_id(&headers, &["x-github-delivery", "x-gitea-delivery"]))
        .map(|id| format!("git:{}", id));

    // Same ordinary inspection as an unforced push. Do not reset from `forced`,
    // and do not trust `before`/`after`: a stale or out-of-order delivery only
    // prompts an inspection of the current remote state.
    let Some(outcome) = dedup_and_inspect(&state, delivery.as_deref())? else {
        info!(
            repo = repo_name,
            delivery = delivery.as_deref().unwrap_or(""),
            "ignoring duplicate GitHub webhook delivery"
        );
        return Ok(ignored("duplicate delivery ignored".into()));
    };
    let triggered = matches!(outcome, Inspection::Queued);

    info!(
        repo = repo_name,
        git_ref = %payload.git_ref,
        commits = commit_count,
        forced_hint = payload.forced,
        triggered,
        "received GitHub push webhook; forced is a hint, polling inspection is the safety gate"
    );

    // Broadcast notification
    let update = serde_json::json!({
        "type": "webhook_received",
        "source": "github",
        "ref": payload.git_ref,
        "commits": commit_count,
        "forced_hint": payload.forced,
        "inspection": "polling_safety_gate",
    });
    let _ = state.ws_broadcast.send(update.to_string());

    let action = if triggered {
        "sync triggered"
    } else {
        "sync already pending"
    };
    let message = if payload.forced {
        format!(
            "push event received (forced=true is a hint only; polling inspects), {} commits, {}",
            commit_count, action
        )
    } else {
        format!("push event received, {} commits, {}", commit_count, action)
    };
    Ok(Json(WebhookResponse {
        ok: true,
        message,
        triggered,
        forced_hint: Some(payload.forced),
        inspection: Some("polling_safety_gate"),
    }))
}

async fn svn_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<WebhookResponse>, AppError> {
    // Verify the shared secret before reading the body.
    if state.config.svn.webhook_secret.is_none() {
        tracing::warn!(
            "SVN webhook secret not configured - webhook payloads are not authenticated"
        );
    }
    if let Some(ref secret) = state.config.svn.webhook_secret {
        let provided = headers
            .get("x-webhook-secret")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::Unauthorized("missing X-Webhook-Secret header".into()))?;

        // Constant-time comparison using subtle crate
        use subtle::ConstantTimeEq;
        let matches: bool = if provided.len() == secret.len() {
            provided.as_bytes().ct_eq(secret.as_bytes()).into()
        } else {
            false
        };

        if !matches {
            return Err(AppError::Unauthorized("invalid webhook secret".into()));
        }
    }

    let payload: SvnPostCommitPayload = serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequest(format!("invalid JSON payload: {}", e)))?;

    // The revision is only a hint; the inspection reads SVN itself.
    // As for Git, only an authenticated sender's delivery ID is recorded.
    let delivery = state
        .config
        .svn
        .webhook_secret
        .as_ref()
        .and_then(|_| delivery_id(&headers, &["x-webhook-delivery"]))
        .map(|id| format!("svn:{}", id));
    let Some(outcome) = dedup_and_inspect(&state, delivery.as_deref())? else {
        info!(
            revision = payload.revision,
            "ignoring duplicate SVN webhook delivery"
        );
        return Ok(ignored("duplicate delivery ignored".into()));
    };
    let triggered = matches!(outcome, Inspection::Queued);

    info!(
        revision = payload.revision,
        author = %payload.author,
        triggered,
        "received SVN post-commit webhook"
    );

    // Broadcast notification
    let update = serde_json::json!({
        "type": "webhook_received",
        "source": "svn",
        "revision": payload.revision,
        "author": payload.author,
    });
    let _ = state.ws_broadcast.send(update.to_string());

    Ok(Json(WebhookResponse {
        ok: true,
        message: format!(
            "SVN revision {} received, {}",
            payload.revision,
            if triggered {
                "sync triggered"
            } else {
                "sync already pending"
            }
        ),
        triggered,
        forced_hint: None,
        inspection: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_repo_name_accepts_names_and_urls() {
        for value in [
            "Owner/Repo",
            "owner/repo.git",
            "https://github.example/owner/repo.git",
            "https://github.example/owner/repo/",
            "git@github.example:owner/repo.git",
        ] {
            assert_eq!(
                normalize_repo_name(value).as_deref(),
                Some("owner/repo"),
                "{value}"
            );
        }
        for value in ["", "repo", "/", "https://github.example/"] {
            assert_eq!(normalize_repo_name(value), None, "{value}");
        }
    }

    #[test]
    fn delivery_log_dedups_and_stays_bounded() {
        let mut log = DeliveryLog::default();
        assert!(log.record("a"));
        assert!(!log.record("a"));
        log.forget("a");
        assert!(log.record("a"));
        for i in 0..DELIVERY_LOG_CAPACITY {
            assert!(log.record(&format!("k{i}")));
        }
        assert_eq!(log.order.len(), DELIVERY_LOG_CAPACITY);
        assert_eq!(log.seen.len(), DELIVERY_LOG_CAPACITY);
        // The oldest entry was evicted, so it counts as new again.
        assert!(log.record("a"));
    }
}
