//! Unactivated, in-process HTTP adapter for sealed migrated fixture copies.
//! The normal WebServer router never merges this route family.
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use reposync_core::db::{
    candidate_dto::Response,
    candidate_migration::CopySession,
    candidate_readers::{Direction, Source},
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{api::auth::validate_session, AppState};

struct InspectionState {
    // Authentication state is separate from the immutable candidate copy.
    auth: Arc<AppState>,
    copy: Arc<CopySession>,
}

type Reply = Result<Json<Response>, (StatusCode, Json<Value>)>;

fn refusal(status: StatusCode, code: &'static str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": code})))
}

async fn authenticate(state: &InspectionState, headers: &HeaderMap) -> Result<(), (StatusCode, Json<Value>)> {
    validate_session(
        &state.auth,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await
    .map_err(|_| refusal(StatusCode::UNAUTHORIZED, "unauthorized"))
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

async fn lookup(State(state): State<Arc<InspectionState>>, headers: HeaderMap, Query(q): Query<LookupQuery>) -> Reply {
    authenticate(&state, &headers).await?;
    let repo=repository(&q.repository)?;
    let d=direction(&q.direction)?;
    let source=match (d,q.source_svn_rev,q.source_git_sha) {
        (Direction::SvnToGit,Some(rev),None) if rev>0 => Source::Svn(rev),
        (Direction::GitToSvn,None,Some(sha)) if [40,64].contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) => Source::Git(sha),
        _ => return Err(refusal(StatusCode::BAD_REQUEST,"invalid_request")),
    };
    let readers=state.copy.readers().map_err(|_|refusal(StatusCode::INTERNAL_SERVER_ERROR,"copy_unavailable"))?;
    readers.lookup_dto(repo,q.generation,d,&source).map(Json).map_err(|_|refusal(StatusCode::INTERNAL_SERVER_ERROR,"copy_read_failed"))
}

async fn list(State(state): State<Arc<InspectionState>>, headers: HeaderMap, Query(q): Query<ListQuery>) -> Reply {
    authenticate(&state, &headers).await?;
    let repo=repository(&q.repository)?;
    let d=direction(&q.direction)?;
    let limit=q.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {return Err(refusal(StatusCode::BAD_REQUEST,"invalid_request"));}
    let readers=state.copy.readers().map_err(|_|refusal(StatusCode::INTERNAL_SERVER_ERROR,"copy_unavailable"))?;
    readers.list_dto(repo,q.generation,d,q.after_id,limit).map(Json).map_err(|_|refusal(StatusCode::INTERNAL_SERVER_ERROR,"copy_read_failed"))
}

async fn status(State(state): State<Arc<InspectionState>>, headers: HeaderMap, Query(q): Query<ScopeQuery>) -> Reply {
    authenticate(&state, &headers).await?;
    let repo=repository(&q.repository)?;
    let readers=state.copy.readers().map_err(|_|refusal(StatusCode::INTERNAL_SERVER_ERROR,"copy_unavailable"))?;
    readers.status_dto(repo,q.generation).map(Json).map_err(|_|refusal(StatusCode::INTERNAL_SERVER_ERROR,"copy_read_failed"))
}

async fn last_emitted(State(state): State<Arc<InspectionState>>, headers: HeaderMap, Query(q): Query<EmittedQuery>) -> Reply {
    authenticate(&state, &headers).await?;
    let repo=repository(&q.repository)?;
    let d=direction(&q.direction)?;
    let readers=state.copy.readers().map_err(|_|refusal(StatusCode::INTERNAL_SERVER_ERROR,"copy_unavailable"))?;
    readers.last_emitted_dto(repo,q.generation,d).map(Json).map_err(|_|refusal(StatusCode::INTERNAL_SERVER_ERROR,"copy_read_failed"))
}

/// Construct only in explicit fixture tests. There is no listener, path input,
/// migration, legacy global page, or operational router registration here.
pub fn routes(auth: Arc<AppState>, copy: Arc<CopySession>) -> Router {
    let state=Arc::new(InspectionState{auth,copy});
    Router::new()
        .route("/__reliability/copy-read/lookup",get(lookup))
        .route("/__reliability/copy-read/list",get(list))
        .route("/__reliability/copy-read/status",get(status))
        .route("/__reliability/copy-read/last-emitted",get(last_emitted))
        .with_state(state)
}
