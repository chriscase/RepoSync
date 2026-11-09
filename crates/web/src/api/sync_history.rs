//! Sync history and commit-map API endpoints.
//!
//! # Mutex safety
//!
//! `Database::conn()` returns a `MutexGuard<Connection>` (std::sync::Mutex).
//! **std::sync::Mutex is NOT re-entrant.** If you hold a `MutexGuard` and
//! then call any `db.*()` query method (which internally calls `self.conn()`),
//! the thread will deadlock permanently. Because all authenticated handlers
//! go through `validate_session` → `db.get_session()` → `self.conn()`, a
//! single deadlocked thread holding the mutex freezes the entire web server.
//!
//! Safe patterns:
//!   - Use `db.*()` helper methods (they acquire and release internally).
//!   - OR call `db.conn()` and use the returned `Connection` directly.
//!   - NEVER mix: do not call `db.*()` while a `db.conn()` guard is alive.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::api::auth::validate_session;
use crate::api::status::AppError;
use crate::AppState;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct HistoryQuery {
    pub limit: Option<u32>,
    pub repo_id: Option<String>,
}

#[derive(Serialize)]
pub struct CommitMapEntryView {
    pub id: i64,
    pub svn_rev: i64,
    /// `null` for a no-target row (#54); see `target_state`.
    pub git_sha: Option<String>,
    /// `git_commit`, `filtered_no_target`, `no_target_unverified` or
    /// `legacy_empty_value`. Only `filtered_no_target` is a proven no-target
    /// row; the canonical outcome tables remain the authority.
    pub target_state: &'static str,
    pub direction: String,
    pub synced_at: String,
    pub svn_author: String,
    pub git_author: String,
    pub repo_id: Option<String>,
}

impl From<reposync_core::db::nullable_commit_map::CommitMapRow> for CommitMapEntryView {
    fn from(row: reposync_core::db::nullable_commit_map::CommitMapRow) -> Self {
        Self {
            id: row.id,
            svn_rev: row.svn_rev,
            git_sha: row.git_sha,
            target_state: row.target.as_str(),
            direction: row.direction,
            synced_at: row.synced_at,
            svn_author: row.svn_author,
            git_author: row.git_author,
            repo_id: row.repo_id,
        }
    }
}

#[derive(Serialize)]
pub struct CommitMapResponse {
    pub entries: Vec<CommitMapEntryView>,
    pub total: usize,
}

#[derive(Serialize)]
pub struct SyncRecordView {
    pub id: String,
    pub svn_rev: Option<i64>,
    pub git_sha: Option<String>,
    pub direction: String,
    pub author: String,
    pub message: String,
    pub timestamp: String,
    pub synced_at: String,
    pub status: String,
    pub repo_id: Option<String>,
}

#[derive(Serialize)]
pub struct SyncRecordResponse {
    pub entries: Vec<SyncRecordView>,
    pub total: usize,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/commit-map", get(list_commit_map))
        .route("/api/sync-records", get(list_sync_records))
}

async fn list_commit_map(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<CommitMapResponse>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let limit = query.limit.unwrap_or(100).min(500);

    let db = &state.db;

    // `list_commit_map_rows` acquires and releases the connection itself; do
    // not hold a `db.conn()` guard around it (std::sync::Mutex is not
    // re-entrant; see the module doc).
    let views: Vec<CommitMapEntryView> = db
        .list_commit_map_rows(query.repo_id.as_deref(), limit)
        .map_err(|e| AppError::Internal(format!("database error: {}", e)))?
        .into_iter()
        .map(CommitMapEntryView::from)
        .collect();

    let total = views.len();
    Ok(Json(CommitMapResponse {
        entries: views,
        total,
    }))
}

async fn list_sync_records(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<SyncRecordResponse>, AppError> {
    validate_session(
        &state,
        headers.get("authorization").and_then(|v| v.to_str().ok()),
    )
    .await?;

    let limit = query.limit.unwrap_or(100).min(500);

    let db = &state.db;

    let conn = db.conn();
    let (sql, params_list): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(ref rid) =
        query.repo_id
    {
        (
            "SELECT id, svn_rev, git_sha, direction, author, message, timestamp, synced_at, status, repo_id
             FROM sync_records WHERE repo_id = ?1 ORDER BY synced_at DESC LIMIT ?2".to_string(),
            vec![Box::new(rid.clone()), Box::new(limit)],
        )
    } else {
        (
            "SELECT id, svn_rev, git_sha, direction, author, message, timestamp, synced_at, status, repo_id
             FROM sync_records ORDER BY synced_at DESC LIMIT ?1".to_string(),
            vec![Box::new(limit)],
        )
    };
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| AppError::Internal(format!("prepare error: {}", e)))?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        params_list.iter().map(|b| b.as_ref()).collect();

    let entries: Vec<SyncRecordView> = stmt
        .query_map(params_refs.as_slice(), |row| {
            Ok(SyncRecordView {
                id: row.get(0)?,
                svn_rev: row.get(1)?,
                git_sha: row.get(2)?,
                direction: row.get(3)?,
                author: row.get(4)?,
                message: row.get(5)?,
                timestamp: row.get(6)?,
                synced_at: row.get(7)?,
                status: row.get(8)?,
                repo_id: row.get(9)?,
            })
        })
        .map_err(|e| AppError::Internal(format!("query error: {}", e)))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Internal(format!("row error: {}", e)))?;

    let total = entries.len();
    Ok(Json(SyncRecordResponse { entries, total }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reposync_core::db::nullable_commit_map::{CommitMapRow, CommitMapTarget};

    fn row(git_sha: Option<&str>, target: CommitMapTarget) -> CommitMapRow {
        CommitMapRow {
            id: 9,
            svn_rev: 7,
            git_sha: git_sha.map(str::to_string),
            target,
            direction: "svn_to_git".into(),
            synced_at: "2026-11-09T00:00:00Z".into(),
            svn_author: "dev".into(),
            git_author: String::new(),
            repo_id: Some("pair".into()),
        }
    }

    #[test]
    fn no_target_rows_serialize_null_sha_with_explicit_state() {
        for (sha, target, state) in [
            (
                None,
                CommitMapTarget::FilteredNoTarget,
                "filtered_no_target",
            ),
            (
                None,
                CommitMapTarget::NoTargetUnverified,
                "no_target_unverified",
            ),
            (
                Some(""),
                CommitMapTarget::LegacyEmptyValue,
                "legacy_empty_value",
            ),
        ] {
            let json = serde_json::to_value(CommitMapEntryView::from(row(sha, target))).unwrap();
            assert_eq!(json["target_state"], state);
            match sha {
                None => assert!(json["git_sha"].is_null(), "{json}"),
                Some(value) => assert_eq!(json["git_sha"], value),
            }
        }
        let sha = "a".repeat(40);
        let json = serde_json::to_value(CommitMapEntryView::from(row(
            Some(&sha),
            CommitMapTarget::GitCommit,
        )))
        .unwrap();
        assert_eq!(json["git_sha"], sha.as_str());
        assert_eq!(json["target_state"], "git_commit");
        assert_eq!(json["svn_rev"], 7);
        assert_eq!(json["repo_id"], "pair");
    }
}
