//! Unactivated, in-process HTTP adapter for sealed migrated fixture copies.
//! The normal WebServer router never merges this route family.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use reposync_core::db::{
    candidate_dto::Response,
    candidate_migration::CopySession,
    candidate_readers::{Direction, Source},
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::AppState;

#[derive(Default)]
struct Grants {
    repositories: BTreeSet<String>,
    diagnostics: bool,
}

/// Trusted fixture enrollment: copy and grants share one context. A new copy
/// gets a new enrollment and no grants, even with the same authentication DB.
pub struct InspectionContext {
    copy: Arc<CopySession>,
    enrollment: Uuid,
    grants: RwLock<BTreeMap<(Uuid, String), Grants>>,
    grants_available: AtomicBool,
    reader_opens: AtomicUsize,
}

impl InspectionContext {
    pub fn new(copy: Arc<CopySession>) -> Self {
        Self {
            copy,
            enrollment: Uuid::new_v4(),
            grants: RwLock::new(BTreeMap::new()),
            grants_available: AtomicBool::new(true),
            reader_opens: AtomicUsize::new(0),
        }
    }
    pub async fn grant_read(&self, principal: &str, repository: &str) {
        self.grants
            .write()
            .await
            .entry((self.enrollment, principal.into()))
            .or_default()
            .repositories
            .insert(repository.into());
    }
    pub async fn revoke_read(&self, principal: &str, repository: &str) {
        if let Some(grants) = self
            .grants
            .write()
            .await
            .get_mut(&(self.enrollment, principal.into()))
        {
            grants.repositories.remove(repository);
        }
    }
    pub async fn set_diagnostics(&self, principal: &str, enabled: bool) {
        self.grants
            .write()
            .await
            .entry((self.enrollment, principal.into()))
            .or_default()
            .diagnostics = enabled;
    }
    /// Fixture failure injection for the external policy store.
    pub fn set_grants_available(&self, available: bool) {
        self.grants_available.store(available, Ordering::SeqCst);
    }
    pub fn reader_open_attempts(&self) -> usize {
        self.reader_opens.load(Ordering::SeqCst)
    }
}

struct InspectionState {
    // Authentication state is separate from the immutable candidate copy.
    auth: Arc<AppState>,
    context: Arc<InspectionContext>,
}

type Reply = Result<Json<Response>, (StatusCode, Json<Value>)>;

fn refusal(status: StatusCode, code: &'static str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": code})))
}

#[derive(Clone, Copy)]
enum Visibility {
    Scoped,
    Diagnostic,
}

async fn authorize(
    state: &InspectionState,
    headers: &HeaderMap,
    repo: &str,
) -> Result<Visibility, (StatusCode, Json<Value>)> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| !v.is_empty())
        .ok_or_else(|| refusal(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    // Candidate identity resolution deliberately has no legacy-session fallback.
    // Expired/deleted named sessions, disabled/missing users and DB errors fail
    // closed even when an unrelated in-memory token has the same spelling.
    let session = state
        .auth
        .db
        .get_session(token)
        .ok()
        .flatten()
        .ok_or_else(|| refusal(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let expires = DateTime::parse_from_rfc3339(&session.expires_at)
        .map_err(|_| refusal(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    if expires <= Utc::now() {
        return Err(refusal(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    let user = state
        .auth
        .db
        .get_user(&session.user_id)
        .ok()
        .flatten()
        .filter(|user| user.enabled)
        .ok_or_else(|| refusal(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    if !state.context.grants_available.load(Ordering::SeqCst) {
        return Err(refusal(StatusCode::FORBIDDEN, "forbidden"));
    }
    let grants = state.context.grants.read().await;
    let Some(grant) = grants
        .get(&(state.context.enrollment, user.id))
        .filter(|grant| grant.repositories.contains(repo))
    else {
        // Ungranted, unknown and neighboring IDs have the same reply.
        return Err(refusal(StatusCode::FORBIDDEN, "forbidden"));
    };
    Ok(if grant.diagnostics && user.role == "admin" {
        Visibility::Diagnostic
    } else {
        Visibility::Scoped
    })
}

fn direction(value: &str) -> Result<Direction, (StatusCode, Json<Value>)> {
    match value {
        "svn_to_git" => Ok(Direction::SvnToGit),
        "git_to_svn" => Ok(Direction::GitToSvn),
        _ => Err(refusal(StatusCode::BAD_REQUEST, "invalid_request")),
    }
}

fn repository(value: &str) -> Result<&str, (StatusCode, Json<Value>)> {
    if value.is_empty() || value.len() > 128 {
        Err(refusal(StatusCode::BAD_REQUEST, "invalid_request"))
    } else {
        Ok(value)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeQuery {
    repository: String,
    generation: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LookupQuery {
    repository: String,
    generation: Option<i64>,
    direction: String,
    source_svn_rev: Option<i64>,
    source_git_sha: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    repository: String,
    generation: Option<i64>,
    direction: String,
    after_id: Option<i64>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmittedQuery {
    repository: String,
    generation: Option<i64>,
    direction: String,
}

async fn lookup(
    State(state): State<Arc<InspectionState>>,
    headers: HeaderMap,
    Query(q): Query<LookupQuery>,
) -> Reply {
    let repo = repository(&q.repository)?;
    let visibility = authorize(&state, &headers, repo).await?;
    let d = direction(&q.direction)?;
    let source = match (d, q.source_svn_rev, q.source_git_sha) {
        (Direction::SvnToGit, Some(rev), None) if rev > 0 => Source::Svn(rev),
        (Direction::GitToSvn, None, Some(sha))
            if [40, 64].contains(&sha.len())
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) =>
        {
            Source::Git(sha)
        }
        _ => return Err(refusal(StatusCode::BAD_REQUEST, "invalid_request")),
    };
    state.context.reader_opens.fetch_add(1, Ordering::SeqCst);
    let readers = state
        .context
        .copy
        .readers()
        .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "copy_unavailable"))?;
    match visibility {
        Visibility::Scoped => readers.lookup_scoped_dto(repo, q.generation, d, &source),
        Visibility::Diagnostic => readers.lookup_dto(repo, q.generation, d, &source),
    }
    .map(Json)
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "copy_read_failed"))
}

async fn list(
    State(state): State<Arc<InspectionState>>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Reply {
    let repo = repository(&q.repository)?;
    let visibility = authorize(&state, &headers, repo).await?;
    let d = direction(&q.direction)?;
    let limit = q.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return Err(refusal(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    state.context.reader_opens.fetch_add(1, Ordering::SeqCst);
    let readers = state
        .context
        .copy
        .readers()
        .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "copy_unavailable"))?;
    match visibility {
        Visibility::Scoped => readers.list_scoped_dto(repo, q.generation, d, q.after_id, limit),
        Visibility::Diagnostic => readers.list_dto(repo, q.generation, d, q.after_id, limit),
    }
    .map(Json)
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "copy_read_failed"))
}

async fn status(
    State(state): State<Arc<InspectionState>>,
    headers: HeaderMap,
    Query(q): Query<ScopeQuery>,
) -> Reply {
    let repo = repository(&q.repository)?;
    let visibility = authorize(&state, &headers, repo).await?;
    state.context.reader_opens.fetch_add(1, Ordering::SeqCst);
    let readers = state
        .context
        .copy
        .readers()
        .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "copy_unavailable"))?;
    match visibility {
        Visibility::Scoped => readers.status_scoped_dto(repo, q.generation),
        Visibility::Diagnostic => readers.status_dto(repo, q.generation),
    }
    .map(Json)
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "copy_read_failed"))
}

async fn last_emitted(
    State(state): State<Arc<InspectionState>>,
    headers: HeaderMap,
    Query(q): Query<EmittedQuery>,
) -> Reply {
    let repo = repository(&q.repository)?;
    let visibility = authorize(&state, &headers, repo).await?;
    let d = direction(&q.direction)?;
    state.context.reader_opens.fetch_add(1, Ordering::SeqCst);
    let readers = state
        .context
        .copy
        .readers()
        .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "copy_unavailable"))?;
    match visibility {
        Visibility::Scoped => readers.last_emitted_scoped_dto(repo, q.generation, d),
        Visibility::Diagnostic => readers.last_emitted_dto(repo, q.generation, d),
    }
    .map(Json)
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "copy_read_failed"))
}

/// Construct only in explicit fixture tests. There is no listener, path input,
/// migration, legacy global page, or operational router registration here.
pub fn routes(auth: Arc<AppState>, context: Arc<InspectionContext>) -> Router {
    let state = Arc::new(InspectionState { auth, context });
    Router::new()
        .route("/__reliability/copy-read/lookup", get(lookup))
        .route("/__reliability/copy-read/list", get(list))
        .route("/__reliability/copy-read/status", get(status))
        .route("/__reliability/copy-read/last-emitted", get(last_emitted))
        .with_state(state)
}
