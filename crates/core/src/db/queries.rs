//! Typed query helpers for every table in the RepoSync database.

use chrono::{DateTime, Utc};
use rusqlite::params;
use tracing::{debug, info};
use uuid::Uuid;

use super::Database;
use crate::errors::DatabaseError;
use crate::models;

// ---------------------------------------------------------------------------
// Domain structs returned by queries
// ---------------------------------------------------------------------------

/// A row from the `commit_map` table.
#[derive(Debug, Clone)]
pub struct CommitMapEntry {
    pub id: i64,
    pub svn_rev: i64,
    pub git_sha: String,
    pub direction: String,
    pub synced_at: String,
    pub svn_author: String,
    pub git_author: String,
    pub repo_id: Option<String>,
}

/// A row from the `sync_state` table.
#[derive(Debug, Clone)]
pub struct SyncStateEntry {
    pub id: i64,
    pub state: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub details: Option<String>,
}

/// A row from the `conflicts` table.
#[derive(Debug, Clone)]
pub struct ConflictEntry {
    pub id: String,
    pub file_path: String,
    pub conflict_type: String,
    pub svn_content: Option<String>,
    pub git_content: Option<String>,
    pub base_content: Option<String>,
    pub svn_rev: Option<i64>,
    pub git_sha: Option<String>,
    pub status: String,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub repo_id: Option<String>,
}

/// A row from the `watermarks` table.
#[derive(Debug, Clone)]
pub struct WatermarkEntry {
    pub source: String,
    pub value: String,
    pub updated_at: String,
}

/// A row from the `audit_log` table.
#[derive(Debug, Clone)]
pub struct AuditLogEntry {
    pub id: i64,
    pub action: String,
    pub direction: Option<String>,
    pub svn_rev: Option<i64>,
    pub git_sha: Option<String>,
    pub author: Option<String>,
    pub details: Option<String>,
    pub created_at: String,
    pub success: bool,
    pub repo_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Query implementations
// ---------------------------------------------------------------------------

impl Database {
    // -- commit_map ---------------------------------------------------------

    /// Insert a new commit-map entry linking an SVN revision to a Git SHA.
    pub fn insert_commit_map(
        &self,
        svn_rev: i64,
        git_sha: &str,
        direction: &str,
        svn_author: &str,
        git_author: &str,
    ) -> Result<i64, DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![svn_rev, git_sha, direction, now, svn_author, git_author],
        )?;
        let id = conn.last_insert_rowid();
        debug!(id, svn_rev, git_sha, direction, "inserted commit_map entry");
        Ok(id)
    }

    /// Look up a Git SHA by SVN revision.
    pub fn get_git_sha_for_svn_rev(&self, svn_rev: i64) -> Result<Option<String>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT git_sha FROM commit_map WHERE svn_rev = ?1 LIMIT 1")?;
        let mut rows = stmt.query_map(params![svn_rev], |row| row.get(0))?;
        match rows.next() {
            Some(Ok(sha)) => Ok(Some(sha)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Look up an SVN revision by Git SHA.
    pub fn get_svn_rev_for_git_sha(&self, git_sha: &str) -> Result<Option<i64>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT svn_rev FROM commit_map WHERE git_sha = ?1 LIMIT 1")?;
        let mut rows = stmt.query_map(params![git_sha], |row| row.get(0))?;
        match rows.next() {
            Some(Ok(rev)) => Ok(Some(rev)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Return the most recent N commit-map entries ordered by synced_at desc.
    pub fn list_commit_map(&self, limit: u32) -> Result<Vec<CommitMapEntry>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id
             FROM commit_map ORDER BY id DESC LIMIT ?1",
        )?;
        let entries = stmt
            .query_map(params![limit], |row| {
                Ok(CommitMapEntry {
                    id: row.get(0)?,
                    svn_rev: row.get(1)?,
                    git_sha: row.get(2)?,
                    direction: row.get(3)?,
                    synced_at: row.get(4)?,
                    svn_author: row.get(5)?,
                    git_author: row.get(6)?,
                    repo_id: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Check whether a given SVN revision has already been synced.
    pub fn is_svn_rev_synced(&self, svn_rev: i64) -> Result<bool, DatabaseError> {
        let conn = self.conn();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM commit_map WHERE svn_rev = ?1)",
            params![svn_rev],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    /// Check whether a given Git SHA has already been synced.
    pub fn is_git_sha_synced(&self, git_sha: &str) -> Result<bool, DatabaseError> {
        let conn = self.conn();
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM commit_map WHERE git_sha = ?1)",
            params![git_sha],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    // -- sync_state ---------------------------------------------------------

    /// Record the start of a new sync cycle.
    pub fn start_sync_state(
        &self,
        state: &str,
        details: Option<&str>,
    ) -> Result<i64, DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO sync_state (state, started_at, details) VALUES (?1, ?2, ?3)",
            params![state, now, details],
        )?;
        let id = conn.last_insert_rowid();
        debug!(id, state, "started sync_state");
        Ok(id)
    }

    /// Mark a sync-state entry as completed.
    pub fn complete_sync_state(
        &self,
        id: i64,
        state: &str,
        details: Option<&str>,
    ) -> Result<(), DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE sync_state SET state = ?1, completed_at = ?2, details = ?3 WHERE id = ?4",
            params![state, now, details, id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "sync_state".into(),
                id: id.to_string(),
            });
        }
        debug!(id, state, "completed sync_state");
        Ok(())
    }

    /// Get the latest sync-state entry.
    pub fn get_latest_sync_state(&self) -> Result<Option<SyncStateEntry>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, state, started_at, completed_at, details
             FROM sync_state ORDER BY id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([], |row| {
            Ok(SyncStateEntry {
                id: row.get(0)?,
                state: row.get(1)?,
                started_at: row.get(2)?,
                completed_at: row.get(3)?,
                details: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(Ok(entry)) => Ok(Some(entry)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    // -- conflicts ----------------------------------------------------------

    /// Insert a new conflict record.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_conflict_entry(
        &self,
        file_path: &str,
        conflict_type: &str,
        svn_content: Option<&str>,
        git_content: Option<&str>,
        base_content: Option<&str>,
        svn_rev: Option<i64>,
        git_sha: Option<&str>,
    ) -> Result<String, DatabaseError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO conflicts (id, file_path, conflict_type, svn_content, git_content,
             base_content, svn_rev, git_sha, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'detected', ?9)",
            params![
                id,
                file_path,
                conflict_type,
                svn_content,
                git_content,
                base_content,
                svn_rev,
                git_sha,
                now
            ],
        )?;
        debug!(id = %id, file_path, conflict_type, "inserted conflict");
        Ok(id)
    }

    /// Insert a conflict from a model struct.
    pub fn insert_conflict(&self, conflict: &models::Conflict) -> Result<String, DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO conflicts (id, file_path, conflict_type, svn_content, git_content,
             base_content, svn_rev, git_sha, status, created_at, repo_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                conflict.id,
                conflict.file_path,
                conflict.conflict_type,
                conflict.svn_content,
                conflict.git_content,
                conflict.base_content,
                conflict.svn_revision,
                conflict.git_hash,
                conflict.status,
                now,
                conflict.repo_id
            ],
        )?;
        debug!(id = %conflict.id, file_path = %conflict.file_path, "inserted conflict");
        Ok(conflict.id.clone())
    }

    /// Get a conflict by ID (returns an error if not found).
    pub fn get_conflict_entry(&self, id: &str) -> Result<ConflictEntry, DatabaseError> {
        let conn = self.conn();
        conn.query_row(
            "SELECT id, file_path, conflict_type, svn_content, git_content, base_content,
             svn_rev, git_sha, status, resolution, resolved_by, created_at, resolved_at, repo_id
             FROM conflicts WHERE id = ?1",
            params![id],
            |row| {
                Ok(ConflictEntry {
                    id: row.get(0)?,
                    file_path: row.get(1)?,
                    conflict_type: row.get(2)?,
                    svn_content: row.get(3)?,
                    git_content: row.get(4)?,
                    base_content: row.get(5)?,
                    svn_rev: row.get(6)?,
                    git_sha: row.get(7)?,
                    status: row.get(8)?,
                    resolution: row.get(9)?,
                    resolved_by: row.get(10)?,
                    created_at: row.get(11)?,
                    resolved_at: row.get(12)?,
                    repo_id: row.get(13)?,
                })
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => DatabaseError::NotFound {
                entity: "conflict".into(),
                id: id.to_string(),
            },
            other => other.into(),
        })
    }

    /// Get a conflict by ID, returning `Option` instead of an error on not-found.
    pub fn get_conflict(&self, id: &str) -> Result<Option<ConflictEntry>, DatabaseError> {
        match self.get_conflict_entry(id) {
            Ok(entry) => Ok(Some(entry)),
            Err(DatabaseError::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// List conflicts filtered by status, ordered by creation date descending.
    pub fn list_conflicts(
        &self,
        status: Option<&str>,
        limit: u32,
    ) -> Result<Vec<ConflictEntry>, DatabaseError> {
        let conn = self.conn();
        let (sql, bound_params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match status {
            Some(s) => (
                "SELECT id, file_path, conflict_type, svn_content, git_content, base_content,
                 svn_rev, git_sha, status, resolution, resolved_by, created_at, resolved_at, repo_id
                 FROM conflicts WHERE status = ?1 ORDER BY created_at DESC LIMIT ?2"
                    .to_string(),
                vec![Box::new(s.to_string()), Box::new(limit)],
            ),
            None => (
                "SELECT id, file_path, conflict_type, svn_content, git_content, base_content,
                 svn_rev, git_sha, status, resolution, resolved_by, created_at, resolved_at, repo_id
                 FROM conflicts ORDER BY created_at DESC LIMIT ?1"
                    .to_string(),
                vec![Box::new(limit)],
            ),
        };

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            bound_params.iter().map(|p| p.as_ref()).collect();
        let entries = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(ConflictEntry {
                    id: row.get(0)?,
                    file_path: row.get(1)?,
                    conflict_type: row.get(2)?,
                    svn_content: row.get(3)?,
                    git_content: row.get(4)?,
                    base_content: row.get(5)?,
                    svn_rev: row.get(6)?,
                    git_sha: row.get(7)?,
                    status: row.get(8)?,
                    resolution: row.get(9)?,
                    resolved_by: row.get(10)?,
                    created_at: row.get(11)?,
                    resolved_at: row.get(12)?,
                    repo_id: row.get(13)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// List conflicts with pagination support for the web layer.
    pub fn list_conflicts_paginated(
        &self,
        status: Option<&str>,
        pagination: &models::Pagination,
    ) -> Result<models::PaginatedResult<models::WebConflict>, DatabaseError> {
        self.list_conflicts_paginated_filtered(status, None, pagination)
    }

    /// List conflicts with optional status and repo_id filters.
    pub fn list_conflicts_paginated_filtered(
        &self,
        status: Option<&str>,
        repo_id: Option<&str>,
        pagination: &models::Pagination,
    ) -> Result<models::PaginatedResult<models::WebConflict>, DatabaseError> {
        let conn = self.conn();

        // Build WHERE clause dynamically
        let mut where_clauses: Vec<String> = Vec::new();
        let mut count_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(s) = status {
            where_clauses.push(format!("status = ?{}", count_params.len() + 1));
            count_params.push(Box::new(s.to_string()));
        }
        if let Some(r) = repo_id {
            where_clauses.push(format!("repo_id = ?{}", count_params.len() + 1));
            count_params.push(Box::new(r.to_string()));
        }
        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", where_clauses.join(" AND "))
        };

        // Count total
        let count_param_refs: Vec<&dyn rusqlite::types::ToSql> =
            count_params.iter().map(|p| p.as_ref()).collect();
        let total: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM conflicts{}", where_sql),
            count_param_refs.as_slice(),
            |row| row.get(0),
        )?;

        let per_page = pagination.per_page.max(1);
        let total_pages = ((total as u64).saturating_add(per_page as u64 - 1)) / per_page as u64;
        let offset = ((pagination.page.max(1) - 1) as i64) * per_page as i64;

        // Build the SELECT with the same WHERE plus LIMIT/OFFSET
        let mut bound_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(s) = status {
            bound_params.push(Box::new(s.to_string()));
        }
        if let Some(r) = repo_id {
            bound_params.push(Box::new(r.to_string()));
        }
        bound_params.push(Box::new(per_page as i64));
        bound_params.push(Box::new(offset));

        let sql = format!(
            "SELECT id, file_path, conflict_type, svn_content, git_content, base_content,
             svn_rev, git_sha, status, resolution, resolved_by, created_at, resolved_at, repo_id
             FROM conflicts{} ORDER BY created_at DESC LIMIT ?{} OFFSET ?{}",
            where_sql,
            bound_params.len() - 1,
            bound_params.len()
        );

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            bound_params.iter().map(|p| p.as_ref()).collect();
        let items = stmt
            .query_map(param_refs.as_slice(), |row| {
                let created_at_str: String = row.get(11)?;
                let resolved_at_str: Option<String> = row.get(12)?;
                Ok(models::WebConflict {
                    id: row.get(0)?,
                    file_path: row.get(1)?,
                    conflict_type: row.get(2)?,
                    svn_content: row.get(3)?,
                    git_content: row.get(4)?,
                    base_content: row.get(5)?,
                    diff: None,
                    svn_revision: row.get(6)?,
                    git_hash: row.get(7)?,
                    status: row.get(8)?,
                    resolution: row.get(9)?,
                    resolved_content: None,
                    resolved_by: row.get(10)?,
                    detected_at: parse_datetime(&created_at_str),
                    resolved_at: resolved_at_str.as_deref().map(parse_datetime),
                    repo_id: row.get(13)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(models::PaginatedResult {
            items,
            total: total as u64,
            page: pagination.page.max(1),
            per_page,
            total_pages: total_pages as u32,
        })
    }

    /// Get a web-layer conflict by ID.
    pub fn get_web_conflict(&self, id: &str) -> Result<Option<models::WebConflict>, DatabaseError> {
        let conn = self.conn();
        let result = conn.query_row(
            "SELECT id, file_path, conflict_type, svn_content, git_content, base_content,
             svn_rev, git_sha, status, resolution, resolved_by, created_at, resolved_at, repo_id
             FROM conflicts WHERE id = ?1",
            params![id],
            |row| {
                let created_at_str: String = row.get(11)?;
                let resolved_at_str: Option<String> = row.get(12)?;
                Ok(models::WebConflict {
                    id: row.get(0)?,
                    file_path: row.get(1)?,
                    conflict_type: row.get(2)?,
                    svn_content: row.get(3)?,
                    git_content: row.get(4)?,
                    base_content: row.get(5)?,
                    diff: None,
                    svn_revision: row.get(6)?,
                    git_hash: row.get(7)?,
                    status: row.get(8)?,
                    resolution: row.get(9)?,
                    resolved_content: None,
                    resolved_by: row.get(10)?,
                    detected_at: parse_datetime(&created_at_str),
                    resolved_at: resolved_at_str.as_deref().map(parse_datetime),
                    repo_id: row.get(13)?,
                })
            },
        );
        match result {
            Ok(conflict) => Ok(Some(conflict)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Update the status and resolution of a conflict.
    pub fn resolve_conflict(
        &self,
        id: &str,
        status: &str,
        resolution: &str,
        resolved_by: &str,
    ) -> Result<(), DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE conflicts SET status = ?1, resolution = ?2, resolved_by = ?3, resolved_at = ?4
             WHERE id = ?5",
            params![status, resolution, resolved_by, now, id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "conflict".into(),
                id: id.to_string(),
            });
        }
        debug!(id, status, resolution, "resolved conflict");
        Ok(())
    }

    /// Resolve a conflict using a ConflictResolution enum.
    pub fn resolve_conflict_web(
        &self,
        id: &str,
        resolution: &models::ConflictResolution,
        content: Option<&str>,
        resolved_by: &str,
    ) -> Result<(), DatabaseError> {
        self.resolve_conflict(id, "resolved", &resolution.to_string(), resolved_by)?;
        if let Some(c) = content {
            self.store_resolved_content(id, c)?;
        }
        Ok(())
    }

    /// Store merged/resolved content for a conflict.
    pub fn store_resolved_content(
        &self,
        conflict_id: &str,
        content: &str,
    ) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE conflicts SET resolved_content = ?1 WHERE id = ?2",
            params![content, conflict_id],
        )?;
        Ok(())
    }

    /// Defer a conflict.
    pub fn defer_conflict(&self, id: &str) -> Result<(), DatabaseError> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE conflicts SET status = 'deferred' WHERE id = ?1",
            params![id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "conflict".into(),
                id: id.to_string(),
            });
        }
        debug!(id, "deferred conflict");
        Ok(())
    }

    /// Count conflicts by status.
    pub fn count_conflicts_by_status(&self, status: &str) -> Result<i64, DatabaseError> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM conflicts WHERE status = ?1",
            params![status],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Count total conflicts.
    pub fn count_all_conflicts(&self) -> Result<i64, DatabaseError> {
        let conn = self.conn();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM conflicts", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Count active (non-resolved, non-deferred) conflicts.
    pub fn count_active_conflicts(&self) -> Result<i64, DatabaseError> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM conflicts WHERE status NOT IN ('resolved', 'deferred')",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Count active (unresolved) conflicts for a specific repository.
    pub fn count_active_conflicts_for_repo(&self, repo_id: &str) -> Result<i64, DatabaseError> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM conflicts WHERE status NOT IN ('resolved', 'deferred') AND repo_id = ?1",
            params![repo_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    // -- watermarks ---------------------------------------------------------

    /// Get the watermark value for a given source.
    pub fn get_watermark(&self, source: &str) -> Result<Option<String>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT value FROM watermarks WHERE source = ?1")?;
        let mut rows = stmt.query_map(params![source], |row| row.get(0))?;
        match rows.next() {
            Some(Ok(val)) => Ok(Some(val)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Upsert a watermark value.
    pub fn set_watermark(&self, source: &str, value: &str) -> Result<(), DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO watermarks (source, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(source) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![source, value, now],
        )?;
        debug!(source, value, "set watermark");
        Ok(())
    }

    /// List all watermarks.
    pub fn list_watermarks(&self) -> Result<Vec<WatermarkEntry>, DatabaseError> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT source, value, updated_at FROM watermarks ORDER BY source")?;
        let entries = stmt
            .query_map([], |row| {
                Ok(WatermarkEntry {
                    source: row.get(0)?,
                    value: row.get(1)?,
                    updated_at: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    // -- audit_log ----------------------------------------------------------

    /// Insert an audit-log entry with explicit success/failure.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_audit_log(
        &self,
        action: &str,
        direction: Option<&str>,
        svn_rev: Option<i64>,
        git_sha: Option<&str>,
        author: Option<&str>,
        details: Option<&str>,
        success: bool,
    ) -> Result<i64, DatabaseError> {
        self.insert_audit_log_with_repo(action, direction, svn_rev, git_sha, author, details, success, None)
    }

    /// Insert an audit log entry tagged with an optional `repo_id`.
    pub fn insert_audit_log_with_repo(
        &self,
        action: &str,
        direction: Option<&str>,
        svn_rev: Option<i64>,
        git_sha: Option<&str>,
        author: Option<&str>,
        details: Option<&str>,
        success: bool,
        repo_id: Option<&str>,
    ) -> Result<i64, DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO audit_log (action, direction, svn_rev, git_sha, author, details, created_at, success, repo_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![action, direction, svn_rev, git_sha, author, details, now, success as i32, repo_id],
        )?;
        let id = conn.last_insert_rowid();
        debug!(id, action, success, "inserted audit_log entry");
        Ok(id)
    }

    /// Insert an audit entry from a model struct.
    pub fn insert_audit_entry(&self, entry: &models::AuditEntry) -> Result<i64, DatabaseError> {
        let id = self.insert_audit_log(
            &entry.action,
            None,
            None,
            None,
            None,
            Some(&entry.details),
            entry.success,
        )?;
        Ok(id)
    }

    /// Delete audit log entries beyond the most recent `keep` rows.
    pub fn prune_audit_log(&self, keep: u32) -> Result<u64, DatabaseError> {
        let conn = self.conn();
        let deleted = conn.execute(
            "DELETE FROM audit_log WHERE id NOT IN (SELECT id FROM audit_log ORDER BY id DESC LIMIT ?1)",
            params![keep],
        )?;
        if deleted > 0 {
            debug!(deleted, keep, "pruned audit_log entries");
        }
        Ok(deleted as u64)
    }

    /// List recent audit-log entries with optional offset for pagination.
    pub fn list_audit_log(&self, limit: u32, offset: u32) -> Result<Vec<AuditLogEntry>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, action, direction, svn_rev, git_sha, author, details, created_at, success, repo_id
             FROM audit_log ORDER BY id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let entries = stmt
            .query_map(params![limit, offset], |row| {
                let success_int: i32 = row.get(8)?;
                Ok(AuditLogEntry {
                    id: row.get(0)?,
                    action: row.get(1)?,
                    direction: row.get(2)?,
                    svn_rev: row.get(3)?,
                    git_sha: row.get(4)?,
                    author: row.get(5)?,
                    details: row.get(6)?,
                    created_at: row.get(7)?,
                    success: success_int != 0,
                    repo_id: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// List audit entries with web-layer types (for the web API).
    pub fn list_audit_entries(
        &self,
        limit: usize,
        since: Option<DateTime<Utc>>,
        action: Option<&str>,
    ) -> Result<Vec<models::WebAuditEntry>, DatabaseError> {
        let conn = self.conn();

        let (sql, bound_params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) =
            match (since, action) {
                (Some(since_dt), Some(act)) => (
                    "SELECT id, action, author, details, created_at, success
                 FROM audit_log WHERE created_at >= ?1 AND action = ?2 ORDER BY id DESC LIMIT ?3"
                        .to_string(),
                    vec![
                        Box::new(since_dt.to_rfc3339()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(act.to_string()),
                        Box::new(limit as i64),
                    ],
                ),
                (Some(since_dt), None) => (
                    "SELECT id, action, author, details, created_at, success
                 FROM audit_log WHERE created_at >= ?1 ORDER BY id DESC LIMIT ?2"
                        .to_string(),
                    vec![
                        Box::new(since_dt.to_rfc3339()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(limit as i64),
                    ],
                ),
                (None, Some(act)) => (
                    "SELECT id, action, author, details, created_at, success
                 FROM audit_log WHERE action = ?1 ORDER BY id DESC LIMIT ?2"
                        .to_string(),
                    vec![
                        Box::new(act.to_string()) as Box<dyn rusqlite::types::ToSql>,
                        Box::new(limit as i64),
                    ],
                ),
                (None, None) => (
                    "SELECT id, action, author, details, created_at, success
                 FROM audit_log ORDER BY id DESC LIMIT ?1"
                        .to_string(),
                    vec![Box::new(limit as i64) as Box<dyn rusqlite::types::ToSql>],
                ),
            };

        let mut stmt = conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            bound_params.iter().map(|p| p.as_ref()).collect();
        let entries = stmt
            .query_map(param_refs.as_slice(), |row| {
                let id: i64 = row.get(0)?;
                let action: String = row.get(1)?;
                let author: Option<String> = row.get(2)?;
                let details: Option<String> = row.get(3)?;
                let created_at: String = row.get(4)?;
                let success_int: i32 = row.get(5)?;
                Ok(models::WebAuditEntry {
                    id: id.to_string(),
                    timestamp: parse_datetime(&created_at),
                    action,
                    details: details.unwrap_or_default(),
                    actor: author,
                    success: success_int != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Count total audit-log entries.
    pub fn count_audit_log(&self) -> Result<i64, DatabaseError> {
        let conn = self.conn();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))?;
        Ok(count)
    }

    /// List audit-log entries filtered by action.
    pub fn list_audit_log_by_action(
        &self,
        action: &str,
        limit: u32,
    ) -> Result<Vec<AuditLogEntry>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, action, direction, svn_rev, git_sha, author, details, created_at, success, repo_id
             FROM audit_log WHERE action = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let entries = stmt
            .query_map(params![action, limit], |row| {
                let success_int: i32 = row.get(8)?;
                Ok(AuditLogEntry {
                    id: row.get(0)?,
                    action: row.get(1)?,
                    direction: row.get(2)?,
                    svn_rev: row.get(3)?,
                    git_sha: row.get(4)?,
                    author: row.get(5)?,
                    details: row.get(6)?,
                    created_at: row.get(7)?,
                    success: success_int != 0,
                    repo_id: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Count audit entries that represent failures in the last 24 hours.
    pub fn count_errors(&self) -> Result<i64, DatabaseError> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM audit_log WHERE success = 0 AND created_at > datetime('now', '-24 hours')",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Return the timestamp of the most recent error, if any.
    pub fn last_error_at(&self) -> Result<Option<String>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT MAX(created_at) FROM audit_log WHERE success = 0",
        )?;
        let result: Option<String> = stmt.query_row([], |row| row.get(0))?;
        Ok(result)
    }

    /// Delete all error entries from the audit log.
    /// Returns the number of rows deleted.
    pub fn clear_errors(&self) -> Result<usize, DatabaseError> {
        let conn = self.conn();
        let deleted = conn.execute("DELETE FROM audit_log WHERE success = 0", [])?;
        Ok(deleted)
    }

    // -- kv_state -----------------------------------------------------------

    /// Get a key-value state entry.
    pub fn get_state(&self, key: &str) -> Result<Option<String>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT value FROM kv_state WHERE key = ?1")?;
        let mut rows = stmt.query_map(params![key], |row| row.get::<_, String>(0))?;
        match rows.next() {
            Some(Ok(val)) => Ok(Some(val)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Set a key-value state entry (upsert).
    pub fn set_state(&self, key: &str, value: &str) -> Result<(), DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, now],
        )?;
        debug!(key, value, "set kv_state");
        Ok(())
    }

    // -- sync_records -------------------------------------------------------

    /// Insert a sync record.
    pub fn insert_sync_record(&self, record: &models::SyncRecord) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO sync_records (id, repo_id, svn_rev, git_sha, direction, author, message, timestamp, synced_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.id,
                record.repo_id,
                record.svn_revision,
                record.git_hash,
                record.direction.to_string(),
                record.author,
                record.message,
                record.timestamp.to_rfc3339(),
                record.synced_at.to_rfc3339(),
                record.status.to_string(),
            ],
        )?;
        debug!(id = %record.id, "inserted sync_record");
        Ok(())
    }

    /// Count total sync records.
    pub fn count_sync_records(&self) -> Result<i64, DatabaseError> {
        let conn = self.conn();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM sync_records", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Get the last SVN revision from the commit map or sync records.
    pub fn get_last_svn_revision(&self) -> Result<Option<i64>, DatabaseError> {
        let conn = self.conn();
        let result: Result<Option<i64>, _> =
            conn.query_row("SELECT MAX(svn_rev) FROM commit_map", [], |row| row.get(0));
        match result {
            Ok(Some(rev)) => Ok(Some(rev)),
            Ok(None) => {
                // Try sync_records as fallback
                let result2: Result<Option<i64>, _> = conn.query_row(
                    "SELECT MAX(svn_rev) FROM sync_records WHERE svn_rev IS NOT NULL",
                    [],
                    |row| row.get(0),
                );
                Ok(result2.unwrap_or(None))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Get the last Git hash from the commit map or sync records.
    pub fn get_last_git_hash(&self) -> Result<Option<String>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT git_sha FROM commit_map ORDER BY id DESC LIMIT 1")?;
        let mut rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        match rows.next() {
            Some(Ok(sha)) => Ok(Some(sha)),
            Some(Err(e)) => Err(e.into()),
            None => {
                drop(rows);
                drop(stmt);
                // Try sync_records as fallback
                let mut stmt2 = conn.prepare(
                    "SELECT git_sha FROM sync_records WHERE git_sha IS NOT NULL ORDER BY synced_at DESC LIMIT 1",
                )?;
                let mut rows2 = stmt2.query_map([], |row| row.get::<_, String>(0))?;
                match rows2.next() {
                    Some(Ok(sha)) => Ok(Some(sha)),
                    Some(Err(e)) => Err(e.into()),
                    None => Ok(None),
                }
            }
        }
    }

    // -- author_mappings (web layer) ----------------------------------------

    /// List all author mappings stored in kv_state with prefix `author_mapping:`.
    pub fn list_author_mappings(&self) -> Result<Vec<models::AuthorMapping>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT key, value, updated_at FROM kv_state WHERE key LIKE 'author_mapping:%' ORDER BY key",
        )?;
        let entries = stmt
            .query_map([], |row| {
                let _key: String = row.get(0)?;
                let value: String = row.get(1)?;
                let _updated_at: String = row.get(2)?;
                Ok(value)
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut mappings = Vec::new();
        for value in entries {
            if let Ok(mapping) = serde_json::from_str::<models::AuthorMapping>(&value) {
                mappings.push(mapping);
            }
        }
        Ok(mappings)
    }

    /// Upsert an author mapping.
    pub fn upsert_author_mapping(
        &self,
        mapping: &models::AuthorMapping,
    ) -> Result<(), DatabaseError> {
        let key = format!("author_mapping:{}", mapping.svn_username);
        let value = serde_json::to_string(mapping).unwrap_or_default();
        self.set_state(&key, &value)
    }

    // -- pr_sync_log (personal branch mode) ---------------------------------

    /// Insert a new PR sync log entry (status = 'pending').
    #[allow(clippy::too_many_arguments)]
    pub fn insert_pr_sync(
        &self,
        pr_number: i64,
        pr_title: &str,
        pr_branch: &str,
        merge_sha: &str,
        merge_strategy: &str,
        commit_count: i64,
    ) -> Result<i64, DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO pr_sync_log (pr_number, pr_title, pr_branch, merge_sha, merge_strategy,
             commit_count, status, detected_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
            params![
                pr_number,
                pr_title,
                pr_branch,
                merge_sha,
                merge_strategy,
                commit_count,
                now
            ],
        )?;
        let id = conn.last_insert_rowid();
        debug!(id, pr_number, merge_sha, "inserted pr_sync_log entry");
        Ok(id)
    }

    /// Mark a PR sync as completed with the SVN revision range.
    pub fn complete_pr_sync(
        &self,
        id: i64,
        svn_rev_start: i64,
        svn_rev_end: i64,
    ) -> Result<(), DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE pr_sync_log SET status = 'completed', svn_rev_start = ?1, svn_rev_end = ?2,
             completed_at = ?3 WHERE id = ?4",
            params![svn_rev_start, svn_rev_end, now, id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "pr_sync_log".into(),
                id: id.to_string(),
            });
        }
        debug!(
            id,
            svn_rev_start, svn_rev_end, "completed pr_sync_log entry"
        );
        Ok(())
    }

    /// Mark a PR sync as failed with an error message.
    pub fn fail_pr_sync(&self, id: i64, error_message: &str) -> Result<(), DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE pr_sync_log SET status = 'failed', error_message = ?1, completed_at = ?2
             WHERE id = ?3",
            params![error_message, now, id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "pr_sync_log".into(),
                id: id.to_string(),
            });
        }
        debug!(id, "failed pr_sync_log entry");
        Ok(())
    }

    /// Check whether a PR merge SHA has already been processed.
    pub fn is_pr_synced(&self, merge_sha: &str) -> Result<bool, DatabaseError> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pr_sync_log WHERE merge_sha = ?1",
            params![merge_sha],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// List PR sync log entries, most recent first.
    pub fn list_pr_syncs(&self, limit: u32) -> Result<Vec<models::PrSyncEntry>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, pr_number, pr_title, pr_branch, merge_sha, merge_strategy,
             svn_rev_start, svn_rev_end, commit_count, status, error_message,
             detected_at, completed_at
             FROM pr_sync_log ORDER BY id DESC LIMIT ?1",
        )?;
        let entries = stmt
            .query_map(params![limit], |row| {
                Ok(models::PrSyncEntry {
                    id: row.get(0)?,
                    pr_number: row.get(1)?,
                    pr_title: row.get(2)?,
                    pr_branch: row.get(3)?,
                    merge_sha: row.get(4)?,
                    merge_strategy: row.get(5)?,
                    svn_rev_start: row.get(6)?,
                    svn_rev_end: row.get(7)?,
                    commit_count: row.get(8)?,
                    status: row.get(9)?,
                    error_message: row.get(10)?,
                    detected_at: row.get(11)?,
                    completed_at: row.get(12)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Get the most recent completed PR sync timestamp.
    pub fn get_last_pr_sync_time(&self) -> Result<Option<String>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT completed_at FROM pr_sync_log WHERE status = 'completed'
             ORDER BY id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([], |row| row.get::<_, Option<String>>(0))?;
        match rows.next() {
            Some(Ok(val)) => Ok(val),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Count PR sync entries by status.
    pub fn count_pr_syncs_by_status(&self, status: &str) -> Result<i64, DatabaseError> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pr_sync_log WHERE status = ?1",
            params![status],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    // -- import_progress ----------------------------------------------------

    /// Persist the current import progress to the singleton row in `import_progress`.
    ///
    /// Uses an upsert (INSERT OR REPLACE) so the row is created on the first
    /// call and updated on subsequent calls.
    pub fn persist_import_progress(
        &self,
        progress: &crate::import::ImportProgress,
    ) -> Result<(), DatabaseError> {
        let phase_str = match progress.phase {
            crate::import::ImportPhase::Idle => "idle",
            crate::import::ImportPhase::Connecting => "connecting",
            crate::import::ImportPhase::Importing => "importing",
            crate::import::ImportPhase::Verifying => "verifying",
            crate::import::ImportPhase::FinalPush => "final_push",
            crate::import::ImportPhase::Completed => "completed",
            crate::import::ImportPhase::Failed => "failed",
            crate::import::ImportPhase::Cancelled => "cancelled",
        };
        let errors_json = serde_json::to_string(&progress.errors).unwrap_or_else(|_| "[]".into());
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO import_progress
             (id, phase, current_rev, total_revs, commits_created, batches_pushed,
              lfs_unique_count, files_skipped, errors_json, started_at, completed_at, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                phase_str,
                progress.current_rev,
                progress.total_revs,
                progress.commits_created as i64,
                progress.batches_pushed as i64,
                progress.lfs_unique_count as i64,
                progress.files_skipped as i64,
                errors_json,
                progress.started_at,
                progress.completed_at,
                now,
            ],
        )?;
        debug!("persisted import_progress (phase={})", phase_str);
        Ok(())
    }

    /// Load the persisted import progress from the singleton row, if it exists.
    ///
    /// Note: `log_lines` are NOT restored (too large); only structural progress
    /// data is loaded.
    pub fn load_import_progress(
        &self,
    ) -> Result<Option<crate::import::ImportProgress>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT phase, current_rev, total_revs, commits_created, batches_pushed,
                    lfs_unique_count, files_skipped, errors_json, started_at, completed_at
             FROM import_progress WHERE id = 1",
        )?;
        let mut rows = stmt.query_map([], |row| {
            let phase_str: String = row.get(0)?;
            let current_rev: i64 = row.get(1)?;
            let total_revs: i64 = row.get(2)?;
            let commits_created: i64 = row.get(3)?;
            let batches_pushed: i64 = row.get(4)?;
            let lfs_unique_count: i64 = row.get(5)?;
            let files_skipped: i64 = row.get(6)?;
            let errors_json: String = row.get(7)?;
            let started_at: Option<String> = row.get(8)?;
            let completed_at: Option<String> = row.get(9)?;
            Ok((
                phase_str,
                current_rev,
                total_revs,
                commits_created,
                batches_pushed,
                lfs_unique_count,
                files_skipped,
                errors_json,
                started_at,
                completed_at,
            ))
        })?;

        match rows.next() {
            Some(Ok((
                phase_str,
                current_rev,
                total_revs,
                commits_created,
                batches_pushed,
                lfs_unique_count,
                files_skipped,
                errors_json,
                started_at,
                completed_at,
            ))) => {
                let phase = match phase_str.as_str() {
                    "idle" => crate::import::ImportPhase::Idle,
                    "connecting" => crate::import::ImportPhase::Connecting,
                    "importing" => crate::import::ImportPhase::Importing,
                    "verifying" => crate::import::ImportPhase::Verifying,
                    "final_push" => crate::import::ImportPhase::FinalPush,
                    "completed" => crate::import::ImportPhase::Completed,
                    "failed" => crate::import::ImportPhase::Failed,
                    "cancelled" => crate::import::ImportPhase::Cancelled,
                    _ => crate::import::ImportPhase::Idle,
                };
                let errors: Vec<String> =
                    serde_json::from_str(&errors_json).unwrap_or_default();
                let mut progress = crate::import::ImportProgress::default();
                progress.phase = phase;
                progress.current_rev = current_rev;
                progress.total_revs = total_revs;
                progress.commits_created = commits_created as u64;
                progress.batches_pushed = batches_pushed as u64;
                progress.lfs_unique_count = lfs_unique_count as u64;
                progress.files_skipped = files_skipped as u64;
                progress.errors = errors;
                progress.started_at = started_at;
                progress.completed_at = completed_at;
                Ok(Some(progress))
            }
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    // -- users ---------------------------------------------------------------

    /// Count total users in the database.
    pub fn count_users(&self) -> Result<i64, DatabaseError> {
        let conn = self.conn();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Insert a new user.
    pub fn insert_user(&self, user: &models::User) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO users (id, username, display_name, email, password_hash, role, enabled, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                user.id,
                user.username,
                user.display_name,
                user.email,
                user.password_hash,
                user.role,
                user.enabled as i32,
                user.created_at,
                user.updated_at,
            ],
        )?;
        debug!(id = %user.id, username = %user.username, "inserted user");
        Ok(())
    }

    /// Get a user by ID.
    pub fn get_user(&self, id: &str) -> Result<Option<models::User>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, username, display_name, email, password_hash, role, enabled, created_at, updated_at
             FROM users WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(models::User {
                id: row.get(0)?,
                username: row.get(1)?,
                display_name: row.get(2)?,
                email: row.get(3)?,
                password_hash: row.get(4)?,
                role: row.get(5)?,
                enabled: row.get::<_, i32>(6)? != 0,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?;
        match rows.next() {
            Some(Ok(user)) => Ok(Some(user)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Get a user by username.
    pub fn get_user_by_username(&self, username: &str) -> Result<Option<models::User>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, username, display_name, email, password_hash, role, enabled, created_at, updated_at
             FROM users WHERE username = ?1",
        )?;
        let mut rows = stmt.query_map(params![username], |row| {
            Ok(models::User {
                id: row.get(0)?,
                username: row.get(1)?,
                display_name: row.get(2)?,
                email: row.get(3)?,
                password_hash: row.get(4)?,
                role: row.get(5)?,
                enabled: row.get::<_, i32>(6)? != 0,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?;
        match rows.next() {
            Some(Ok(user)) => Ok(Some(user)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// List all users.
    pub fn list_users(&self) -> Result<Vec<models::User>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, username, display_name, email, password_hash, role, enabled, created_at, updated_at
             FROM users ORDER BY username",
        )?;
        let entries = stmt
            .query_map([], |row| {
                Ok(models::User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    display_name: row.get(2)?,
                    email: row.get(3)?,
                    password_hash: row.get(4)?,
                    role: row.get(5)?,
                    enabled: row.get::<_, i32>(6)? != 0,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Update a user (display_name, email, role, enabled). Does NOT update password.
    pub fn update_user(
        &self,
        id: &str,
        display_name: &str,
        email: &str,
        role: &str,
        enabled: bool,
    ) -> Result<(), DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE users SET display_name = ?1, email = ?2, role = ?3, enabled = ?4, updated_at = ?5
             WHERE id = ?6",
            params![display_name, email, role, enabled as i32, now, id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "user".into(),
                id: id.into(),
            });
        }
        debug!(id, "updated user");
        Ok(())
    }

    /// Update a user's password hash.
    pub fn update_user_password(&self, id: &str, password_hash: &str) -> Result<(), DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE users SET password_hash = ?1, updated_at = ?2 WHERE id = ?3",
            params![password_hash, now, id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "user".into(),
                id: id.into(),
            });
        }
        debug!(id, "updated user password");
        Ok(())
    }

    /// Disable a user (soft-delete).
    pub fn disable_user(&self, id: &str) -> Result<(), DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE users SET enabled = 0, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "user".into(),
                id: id.into(),
            });
        }
        debug!(id, "disabled user");
        Ok(())
    }

    // -- user_credentials ----------------------------------------------------

    /// Insert a new user credential.
    pub fn insert_user_credential(
        &self,
        cred: &models::UserCredential,
    ) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO user_credentials (id, user_id, service, server_url, username, encrypted_value, nonce, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(user_id, service, server_url) DO UPDATE SET
                username = excluded.username,
                encrypted_value = excluded.encrypted_value,
                nonce = excluded.nonce,
                updated_at = excluded.updated_at",
            params![
                cred.id,
                cred.user_id,
                cred.service,
                cred.server_url,
                cred.username,
                cred.encrypted_value,
                cred.nonce,
                cred.created_at,
                cred.updated_at,
            ],
        )?;
        debug!(id = %cred.id, user_id = %cred.user_id, service = %cred.service, "upserted user credential");
        Ok(())
    }

    /// List credentials for a user (summaries only — encrypted values excluded from display).
    pub fn list_user_credentials(
        &self,
        user_id: &str,
    ) -> Result<Vec<models::UserCredential>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, user_id, service, server_url, username, encrypted_value, nonce, created_at, updated_at
             FROM user_credentials WHERE user_id = ?1 ORDER BY service, server_url",
        )?;
        let entries = stmt
            .query_map(params![user_id], |row| {
                Ok(models::UserCredential {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    service: row.get(2)?,
                    server_url: row.get(3)?,
                    username: row.get(4)?,
                    encrypted_value: row.get(5)?,
                    nonce: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Get a specific credential by ID.
    pub fn get_user_credential(&self, cred_id: &str) -> Result<Option<models::UserCredential>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, user_id, service, server_url, username, encrypted_value, nonce, created_at, updated_at
             FROM user_credentials WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![cred_id], |row| {
            Ok(models::UserCredential {
                id: row.get(0)?,
                user_id: row.get(1)?,
                service: row.get(2)?,
                server_url: row.get(3)?,
                username: row.get(4)?,
                encrypted_value: row.get(5)?,
                nonce: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?;
        match rows.next() {
            Some(Ok(cred)) => Ok(Some(cred)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Delete a credential by ID.
    pub fn delete_user_credential(&self, cred_id: &str) -> Result<(), DatabaseError> {
        let conn = self.conn();
        let changed = conn.execute(
            "DELETE FROM user_credentials WHERE id = ?1",
            params![cred_id],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "user_credential".into(),
                id: cred_id.into(),
            });
        }
        debug!(cred_id, "deleted user credential");
        Ok(())
    }

    // -- sessions ------------------------------------------------------------

    /// Insert a new session.
    pub fn insert_session(&self, session: &models::Session) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO sessions (token, user_id, expires_at, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                session.token,
                session.user_id,
                session.expires_at,
                session.created_at,
            ],
        )?;
        debug!(user_id = %session.user_id, "inserted session");
        Ok(())
    }

    /// Get a session by token (only if not expired).
    pub fn get_session(&self, token: &str) -> Result<Option<models::Session>, DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT token, user_id, expires_at, created_at
             FROM sessions WHERE token = ?1 AND expires_at > ?2",
        )?;
        let mut rows = stmt.query_map(params![token, now], |row| {
            Ok(models::Session {
                token: row.get(0)?,
                user_id: row.get(1)?,
                expires_at: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        match rows.next() {
            Some(Ok(session)) => Ok(Some(session)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Delete a session by token (logout).
    pub fn delete_session(&self, token: &str) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute("DELETE FROM sessions WHERE token = ?1", params![token])?;
        debug!("deleted session");
        Ok(())
    }

    /// Prune expired sessions.
    pub fn prune_expired_sessions(&self) -> Result<usize, DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let deleted = conn.execute(
            "DELETE FROM sessions WHERE expires_at <= ?1",
            params![now],
        )?;
        if deleted > 0 {
            debug!(deleted, "pruned expired sessions");
        }
        Ok(deleted)
    }

    // -- LDAP configuration -------------------------------------------------

    /// Check whether LDAP authentication is enabled.
    pub fn is_ldap_enabled(&self) -> Result<bool, DatabaseError> {
        Ok(self.get_state("ldap_enabled")?.as_deref() == Some("true"))
    }

    /// Load LDAP configuration from the `kv_state` table.
    /// Returns `None` if LDAP has not been configured.
    pub fn load_ldap_config(&self) -> Result<Option<crate::ldap_auth::LdapConfig>, DatabaseError> {
        let url = match self.get_state("ldap_url")? {
            Some(u) if !u.is_empty() => u,
            _ => return Ok(None),
        };

        let base_dn = self.get_state("ldap_base_dn")?.unwrap_or_default();
        let search_filter = self
            .get_state("ldap_search_filter")?
            .unwrap_or_else(|| "(&(objectClass=user)(name={0}))".to_string());
        let display_name_attr = self
            .get_state("ldap_display_name_attr")?
            .unwrap_or_else(|| "displayname".to_string());
        let email_attr = self
            .get_state("ldap_email_attr")?
            .unwrap_or_else(|| "mail".to_string());
        let group_attr = self
            .get_state("ldap_group_attr")?
            .unwrap_or_else(|| "memberOf".to_string());
        let bind_dn = self.get_state("ldap_bind_dn")?.filter(|s| !s.is_empty());

        // Decrypt bind password if stored.
        let bind_password = if let Some(enc_pw) = self.get_state("ldap_bind_password")? {
            if enc_pw.is_empty() {
                None
            } else if let Some(nonce) = self.get_state("ldap_bind_password_nonce")? {
                match crate::crypto::get_or_create_encryption_key(self) {
                    Ok(key) => crate::crypto::decrypt_credential(&enc_pw, &nonce, &key).ok(),
                    Err(_) => None,
                }
            } else {
                None
            }
        } else {
            None
        };

        let tls_verify = self.get_state("ldap_tls_verify")?
            .map(|v| v != "false")
            .unwrap_or(true);

        Ok(Some(crate::ldap_auth::LdapConfig {
            url,
            base_dn,
            search_filter,
            display_name_attr,
            email_attr,
            group_attr,
            bind_dn,
            bind_password,
            tls_verify,
        }))
    }

    /// Save LDAP configuration to the `kv_state` table.
    /// The bind password is encrypted before storage.
    pub fn save_ldap_config(
        &self,
        config: &crate::ldap_auth::LdapConfig,
        enabled: bool,
    ) -> Result<(), DatabaseError> {
        self.set_state("ldap_enabled", if enabled { "true" } else { "false" })?;
        self.set_state("ldap_url", &config.url)?;
        self.set_state("ldap_base_dn", &config.base_dn)?;
        self.set_state("ldap_search_filter", &config.search_filter)?;
        self.set_state("ldap_display_name_attr", &config.display_name_attr)?;
        self.set_state("ldap_email_attr", &config.email_attr)?;
        self.set_state("ldap_group_attr", &config.group_attr)?;
        self.set_state(
            "ldap_bind_dn",
            config.bind_dn.as_deref().unwrap_or(""),
        )?;

        // Encrypt and store bind password.
        if let Some(ref pw) = config.bind_password {
            if !pw.is_empty() {
                let key = crate::crypto::get_or_create_encryption_key(self)
                    .map_err(|e| DatabaseError::Other(format!("encryption key error: {}", e)))?;
                let (encrypted, nonce) = crate::crypto::encrypt_credential(pw, &key)
                    .map_err(|e| DatabaseError::Other(format!("encryption error: {}", e)))?;
                self.set_state("ldap_bind_password", &encrypted)?;
                self.set_state("ldap_bind_password_nonce", &nonce)?;
            }
        } else {
            self.set_state("ldap_bind_password", "")?;
            self.set_state("ldap_bind_password_nonce", "")?;
        }

        self.set_state(
            "ldap_tls_verify",
            if config.tls_verify { "true" } else { "false" },
        )?;

        Ok(())
    }

    // -- repositories -------------------------------------------------------

    /// Insert a new repository.
    pub fn insert_repository(&self, repo: &models::Repository) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO repositories (id, name, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, sync_mode, poll_interval_secs, lfs_threshold_mb, auto_merge, enabled, created_by, created_at, updated_at, last_svn_rev, last_git_sha, last_sync_at, sync_status, total_syncs, total_errors, parent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
            params![
                repo.id,
                repo.name,
                repo.svn_url,
                repo.svn_branch,
                repo.svn_username,
                repo.git_provider,
                repo.git_api_url,
                repo.git_repo,
                repo.git_branch,
                repo.sync_mode,
                repo.poll_interval_secs,
                repo.lfs_threshold_mb,
                repo.auto_merge as i32,
                repo.enabled as i32,
                repo.created_by,
                repo.created_at,
                repo.updated_at,
                repo.last_svn_rev,
                repo.last_git_sha,
                repo.last_sync_at,
                repo.sync_status,
                repo.total_syncs,
                repo.total_errors,
                repo.parent_id,
            ],
        )?;
        debug!(id = %repo.id, name = %repo.name, "inserted repository");
        Ok(())
    }

    /// Get a repository by ID.
    pub fn get_repository(&self, id: &str) -> Result<Option<models::Repository>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, sync_mode, poll_interval_secs, lfs_threshold_mb, auto_merge, enabled, created_by, created_at, updated_at, last_svn_rev, last_git_sha, last_sync_at, sync_status, total_syncs, total_errors, parent_id
             FROM repositories WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(models::Repository {
                id: row.get(0)?,
                name: row.get(1)?,
                svn_url: row.get(2)?,
                svn_branch: row.get(3)?,
                svn_username: row.get(4)?,
                git_provider: row.get(5)?,
                git_api_url: row.get(6)?,
                git_repo: row.get(7)?,
                git_branch: row.get(8)?,
                sync_mode: row.get(9)?,
                poll_interval_secs: row.get(10)?,
                lfs_threshold_mb: row.get(11)?,
                auto_merge: row.get::<_, i32>(12)? != 0,
                enabled: row.get::<_, i32>(13)? != 0,
                created_by: row.get(14)?,
                parent_id: row.get(23)?,
                created_at: row.get(15)?,
                updated_at: row.get(16)?,
                last_svn_rev: row.get(17)?,
                last_git_sha: row.get(18)?,
                last_sync_at: row.get(19)?,
                sync_status: row.get(20)?,
                total_syncs: row.get(21)?,
                total_errors: row.get(22)?,
            })
        })?;
        match rows.next() {
            Some(Ok(repo)) => Ok(Some(repo)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// List all repositories ordered by name.
    pub fn list_repositories(&self) -> Result<Vec<models::Repository>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, sync_mode, poll_interval_secs, lfs_threshold_mb, auto_merge, enabled, created_by, created_at, updated_at, last_svn_rev, last_git_sha, last_sync_at, sync_status, total_syncs, total_errors, parent_id
             FROM repositories ORDER BY name",
        )?;
        let entries = stmt
            .query_map([], |row| {
                Ok(models::Repository {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    svn_url: row.get(2)?,
                    svn_branch: row.get(3)?,
                    svn_username: row.get(4)?,
                    git_provider: row.get(5)?,
                    git_api_url: row.get(6)?,
                    git_repo: row.get(7)?,
                    git_branch: row.get(8)?,
                    sync_mode: row.get(9)?,
                    poll_interval_secs: row.get(10)?,
                    lfs_threshold_mb: row.get(11)?,
                    auto_merge: row.get::<_, i32>(12)? != 0,
                    enabled: row.get::<_, i32>(13)? != 0,
                    created_by: row.get(14)?,
                    parent_id: row.get(23)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                    last_svn_rev: row.get(17)?,
                    last_git_sha: row.get(18)?,
                    last_sync_at: row.get(19)?,
                    sync_status: row.get(20)?,
                    total_syncs: row.get(21)?,
                    total_errors: row.get(22)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// List child repositories (branch pairs) for a given parent.
    pub fn list_child_repositories(&self, parent_id: &str) -> Result<Vec<models::Repository>, DatabaseError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, sync_mode, poll_interval_secs, lfs_threshold_mb, auto_merge, enabled, created_by, created_at, updated_at, last_svn_rev, last_git_sha, last_sync_at, sync_status, total_syncs, total_errors, parent_id
             FROM repositories WHERE parent_id = ?1 ORDER BY name",
        )?;
        let entries = stmt
            .query_map(params![parent_id], |row| {
                Ok(models::Repository {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    svn_url: row.get(2)?,
                    svn_branch: row.get(3)?,
                    svn_username: row.get(4)?,
                    git_provider: row.get(5)?,
                    git_api_url: row.get(6)?,
                    git_repo: row.get(7)?,
                    git_branch: row.get(8)?,
                    sync_mode: row.get(9)?,
                    poll_interval_secs: row.get(10)?,
                    lfs_threshold_mb: row.get(11)?,
                    auto_merge: row.get::<_, i32>(12)? != 0,
                    enabled: row.get::<_, i32>(13)? != 0,
                    created_by: row.get(14)?,
                    parent_id: row.get(23)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                    last_svn_rev: row.get(17)?,
                    last_git_sha: row.get(18)?,
                    last_sync_at: row.get(19)?,
                    sync_status: row.get(20)?,
                    total_syncs: row.get(21)?,
                    total_errors: row.get(22)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(entries)
    }

    /// Update a repository's configuration.
    pub fn update_repository(&self, repo: &models::Repository) -> Result<(), DatabaseError> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE repositories SET name = ?1, svn_url = ?2, svn_branch = ?3, svn_username = ?4, git_provider = ?5, git_api_url = ?6, git_repo = ?7, git_branch = ?8, sync_mode = ?9, poll_interval_secs = ?10, lfs_threshold_mb = ?11, auto_merge = ?12, enabled = ?13, updated_at = ?14, last_svn_rev = ?15, last_git_sha = ?16, last_sync_at = ?17, sync_status = ?18, total_syncs = ?19, total_errors = ?20, parent_id = ?21
             WHERE id = ?22",
            params![
                repo.name,
                repo.svn_url,
                repo.svn_branch,
                repo.svn_username,
                repo.git_provider,
                repo.git_api_url,
                repo.git_repo,
                repo.git_branch,
                repo.sync_mode,
                repo.poll_interval_secs,
                repo.lfs_threshold_mb,
                repo.auto_merge as i32,
                repo.enabled as i32,
                repo.updated_at,
                repo.last_svn_rev,
                repo.last_git_sha,
                repo.last_sync_at,
                repo.sync_status,
                repo.total_syncs,
                repo.total_errors,
                repo.parent_id,
                repo.id,
            ],
        )?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "repository".into(),
                id: repo.id.clone(),
            });
        }
        debug!(id = %repo.id, name = %repo.name, "updated repository");
        Ok(())
    }

    /// Get the watermark (last_svn_rev, last_git_sha) for a repository.
    pub fn get_repo_watermark(&self, repo_id: &str) -> Result<(i64, String), DatabaseError> {
        let conn = self.conn();
        let result = conn.query_row(
            "SELECT last_svn_rev, last_git_sha FROM repositories WHERE id = ?1",
            params![repo_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        );
        match result {
            Ok(pair) => Ok(pair),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((0, String::new())),
            Err(e) => Err(e.into()),
        }
    }

    /// Update the watermark for a repository after a successful sync.
    pub fn update_repo_watermark(
        &self,
        repo_id: &str,
        svn_rev: i64,
        git_sha: &str,
    ) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE repositories SET last_svn_rev = ?1, last_git_sha = ?2, last_sync_at = datetime('now') WHERE id = ?3",
            params![svn_rev, git_sha, repo_id],
        )?;
        debug!(repo_id, svn_rev, git_sha, "updated repo watermark");
        Ok(())
    }

    /// Update the sync status for a repository.
    pub fn update_repo_sync_status(
        &self,
        repo_id: &str,
        status: &str,
    ) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE repositories SET sync_status = ?1 WHERE id = ?2",
            params![status, repo_id],
        )?;
        debug!(repo_id, status, "updated repo sync status");
        Ok(())
    }

    /// Increment the total_syncs counter for a repository.
    pub fn increment_repo_sync_count(&self, repo_id: &str) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE repositories SET total_syncs = total_syncs + 1 WHERE id = ?1",
            params![repo_id],
        )?;
        debug!(repo_id, "incremented repo sync count");
        Ok(())
    }

    /// Increment the total_errors counter for a repository.
    pub fn increment_repo_error_count(&self, repo_id: &str) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute(
            "UPDATE repositories SET total_errors = total_errors + 1 WHERE id = ?1",
            params![repo_id],
        )?;
        debug!(repo_id, "incremented repo error count");
        Ok(())
    }

    /// Delete a repository by ID.
    /// Walk the `parent_id` chain from a given repo upward and return the list
    /// of ancestors: `[parent, grandparent, ..., root]`.  Includes a cycle
    /// guard and a hard depth limit of 4.
    pub fn resolve_ancestor_chain(
        &self,
        repo_id: &str,
    ) -> Result<Vec<models::Repository>, DatabaseError> {
        let mut chain = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut current_pid = self
            .get_repository(repo_id)?
            .and_then(|r| r.parent_id);
        while let Some(pid) = current_pid {
            if !visited.insert(pid.clone()) {
                break; // cycle guard
            }
            if chain.len() >= 4 {
                break; // depth limit
            }
            let ancestor = self
                .get_repository(&pid)?
                .ok_or_else(|| DatabaseError::NotFound {
                    entity: "repository".into(),
                    id: pid.clone(),
                })?;
            current_pid = ancestor.parent_id.clone();
            chain.push(ancestor);
        }
        Ok(chain)
    }

    /// Resolve a credential (e.g. `secret_svn_password`) by walking the
    /// parent chain: repo → parent → grandparent → … → global.
    pub fn resolve_credential_chain(
        &self,
        repo_id: &str,
        key_prefix: &str,
    ) -> Option<String> {
        // 1. Repo-specific key
        if let Some(val) = self
            .get_state(&format!("{}_{}", key_prefix, repo_id))
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
        {
            return Some(val);
        }
        // 2. Walk parent chain
        let mut pid = self
            .get_repository(repo_id)
            .ok()
            .flatten()
            .and_then(|r| r.parent_id);
        let mut visited = std::collections::HashSet::new();
        while let Some(current_pid) = pid {
            if !visited.insert(current_pid.clone()) {
                break;
            }
            if let Some(val) = self
                .get_state(&format!("{}_{}", key_prefix, current_pid))
                .ok()
                .flatten()
                .filter(|v| !v.is_empty())
            {
                return Some(val);
            }
            pid = self
                .get_repository(&current_pid)
                .ok()
                .flatten()
                .and_then(|r| r.parent_id);
        }
        // 3. Fall back to global key
        self.get_state(key_prefix)
            .ok()
            .flatten()
            .filter(|v| !v.is_empty())
    }

    /// Recursively collect all descendant repository IDs for a given repo.
    pub fn list_all_descendants(&self, repo_id: &str) -> Result<Vec<String>, DatabaseError> {
        let mut result = Vec::new();
        let mut stack = vec![repo_id.to_string()];
        while let Some(current_id) = stack.pop() {
            let children = self.list_child_repositories(&current_id)?;
            for child in children {
                result.push(child.id.clone());
                stack.push(child.id);
            }
        }
        Ok(result)
    }

    pub fn delete_repository(&self, id: &str) -> Result<(), DatabaseError> {
        let conn = self.conn();
        let changed = conn.execute("DELETE FROM repositories WHERE id = ?1", params![id])?;
        if changed == 0 {
            return Err(DatabaseError::NotFound {
                entity: "repository".into(),
                id: id.into(),
            });
        }
        debug!(id, "deleted repository");
        Ok(())
    }

    /// Clear all sync data (commit_map, sync_records, audit_log, conflicts,
    /// watermarks, import_progress, etc.) while preserving repository config,
    /// users, sessions, and credentials.
    pub fn clear_sync_data(&self) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute_batch(
            "DELETE FROM commit_map;
             DELETE FROM sync_records;
             DELETE FROM audit_log;
             DELETE FROM conflicts;
             DELETE FROM watermarks;
             DELETE FROM pr_sync_log;
             DELETE FROM import_progress;
             DELETE FROM sync_state;
             DELETE FROM kv_state WHERE key LIKE 'last_%' OR key LIKE 'sync_%';",
        )?;
        info!("cleared all sync data from database");
        Ok(())
    }

    // -- maintenance / retention -----------------------------------------------

    /// Run periodic maintenance: prune old audit log, sync records, and commit map entries.
    pub fn run_maintenance(&self, retention_days: u32) -> Result<(), DatabaseError> {
        let conn = self.conn();
        let cutoff = format!("-{} days", retention_days);

        let audit_deleted: usize = conn.execute(
            "DELETE FROM audit_log WHERE created_at < datetime('now', ?1)",
            params![cutoff],
        )?;
        let sync_deleted: usize = conn.execute(
            "DELETE FROM sync_records WHERE synced_at < datetime('now', ?1)",
            params![cutoff],
        )?;

        if audit_deleted > 0 || sync_deleted > 0 {
            tracing::info!(
                audit_deleted,
                sync_deleted,
                retention_days,
                "periodic maintenance: pruned old records"
            );
        }
        Ok(())
    }

    // -- encrypted_secrets ---------------------------------------------------

    /// Store an encrypted secret (ciphertext + nonce) in the encrypted_secrets table.
    pub fn store_encrypted_secret(
        &self,
        key: &str,
        ciphertext: &str,
        nonce: &str,
    ) -> Result<(), DatabaseError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO encrypted_secrets (key, ciphertext, nonce, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![key, ciphertext, nonce, now],
        )?;
        Ok(())
    }

    /// Retrieve an encrypted secret. Returns `(ciphertext, nonce)` if found.
    pub fn get_encrypted_secret(
        &self,
        key: &str,
    ) -> Result<Option<(String, String)>, DatabaseError> {
        let conn = self.conn();
        match conn.query_row(
            "SELECT ciphertext, nonce FROM encrypted_secrets WHERE key = ?1",
            params![key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ) {
            Ok(pair) => Ok(Some(pair)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete an encrypted secret by key.
    pub fn delete_secret(&self, key: &str) -> Result<(), DatabaseError> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM encrypted_secrets WHERE key = ?1",
            params![key],
        )?;
        Ok(())
    }

    /// Get a consolidated status summary in a single query to reduce mutex contention.
    pub fn get_status_summary(
        &self,
        _repo_id: Option<&str>,
    ) -> Result<StatusSummary, DatabaseError> {
        let conn = self.conn();
        let summary = conn.query_row(
            "SELECT
                (SELECT value FROM kv_state WHERE key = 'sync_state') as sync_state,
                (SELECT value FROM kv_state WHERE key = 'last_sync_at') as last_sync_at,
                (SELECT COUNT(*) FROM sync_records) as total_syncs,
                (SELECT COUNT(*) FROM conflicts) as total_conflicts,
                (SELECT COUNT(*) FROM conflicts WHERE status = 'active') as active_conflicts,
                (SELECT COUNT(*) FROM audit_log WHERE success = 0 AND created_at > datetime('now', '-24 hours')) as recent_errors
            ",
            [],
            |row| {
                Ok(StatusSummary {
                    sync_state: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    last_sync_at: row.get::<_, Option<String>>(1)?,
                    total_syncs: row.get(2)?,
                    total_conflicts: row.get(3)?,
                    active_conflicts: row.get(4)?,
                    recent_errors: row.get(5)?,
                })
            },
        )?;
        Ok(summary)
    }
}

/// Consolidated status summary returned by `get_status_summary`.
#[derive(Debug, Clone)]
pub struct StatusSummary {
    pub sync_state: String,
    pub last_sync_at: Option<String>,
    pub total_syncs: i64,
    pub total_conflicts: i64,
    pub active_conflicts: i64,
    pub recent_errors: i64,
}

/// Parse a datetime string, returning Utc::now() as a fallback if parsing fails.
fn parse_datetime(s: &str) -> DateTime<Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_db() -> Database {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db
    }

    #[test]
    fn test_commit_map_crud() {
        let db = setup_db();
        let id = db
            .insert_commit_map(100, "abc123", "svn_to_git", "alice", "Alice <alice@ex.com>")
            .unwrap();
        assert!(id > 0);
        assert_eq!(
            db.get_git_sha_for_svn_rev(100).unwrap().as_deref(),
            Some("abc123")
        );
        assert_eq!(db.get_svn_rev_for_git_sha("abc123").unwrap(), Some(100));
        assert!(db.is_svn_rev_synced(100).unwrap());
        assert!(!db.is_svn_rev_synced(999).unwrap());
    }

    #[test]
    fn test_sync_state() {
        let db = setup_db();
        let id = db.start_sync_state("running", Some("cycle 1")).unwrap();
        db.complete_sync_state(id, "completed", Some("ok")).unwrap();
        let latest = db.get_latest_sync_state().unwrap().unwrap();
        assert_eq!(latest.state, "completed");
    }

    #[test]
    fn test_conflict_crud() {
        let db = setup_db();
        let id = db
            .insert_conflict_entry(
                "src/main.rs",
                "content",
                Some("svn"),
                Some("git"),
                Some("base"),
                Some(42),
                Some("abc"),
            )
            .unwrap();
        let conflict = db.get_conflict_entry(&id).unwrap();
        assert_eq!(conflict.file_path, "src/main.rs");
        assert_eq!(conflict.status, "detected");

        db.resolve_conflict(&id, "resolved", "accept_svn", "admin")
            .unwrap();
        let resolved = db.get_conflict_entry(&id).unwrap();
        assert_eq!(resolved.status, "resolved");
    }

    #[test]
    fn test_watermark_crud() {
        let db = setup_db();
        assert!(db.get_watermark("svn").unwrap().is_none());
        db.set_watermark("svn", "100").unwrap();
        assert_eq!(db.get_watermark("svn").unwrap().as_deref(), Some("100"));
        db.set_watermark("svn", "200").unwrap();
        assert_eq!(db.get_watermark("svn").unwrap().as_deref(), Some("200"));
    }

    #[test]
    fn test_audit_log() {
        let db = setup_db();
        db.insert_audit_log(
            "sync",
            Some("svn_to_git"),
            Some(42),
            Some("abc"),
            Some("alice"),
            Some("test"),
            true,
        )
        .unwrap();
        let entries = db.list_audit_log(10, 0).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].success);
        assert_eq!(db.count_audit_log().unwrap(), 1);
    }

    #[test]
    fn test_kv_state() {
        let db = setup_db();
        assert!(db.get_state("foo").unwrap().is_none());
        db.set_state("foo", "bar").unwrap();
        assert_eq!(db.get_state("foo").unwrap().as_deref(), Some("bar"));
        db.set_state("foo", "baz").unwrap();
        assert_eq!(db.get_state("foo").unwrap().as_deref(), Some("baz"));
    }

    #[test]
    fn test_pr_sync_log_crud() {
        let db = setup_db();

        // Insert
        let id = db
            .insert_pr_sync(42, "Add search", "feature/search", "abc123", "squash", 3)
            .unwrap();
        assert!(id > 0);

        // Check exists
        assert!(db.is_pr_synced("abc123").unwrap());
        assert!(!db.is_pr_synced("nonexistent").unwrap());

        // List
        let entries = db.list_pr_syncs(10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pr_number, 42);
        assert_eq!(entries[0].status, "pending");
        assert_eq!(entries[0].merge_strategy, "squash");

        // Complete
        db.complete_pr_sync(id, 100, 102).unwrap();
        let entries = db.list_pr_syncs(10).unwrap();
        assert_eq!(entries[0].status, "completed");
        assert_eq!(entries[0].svn_rev_start, Some(100));
        assert_eq!(entries[0].svn_rev_end, Some(102));
        assert!(entries[0].completed_at.is_some());
    }

    #[test]
    fn test_pr_sync_log_fail() {
        let db = setup_db();
        let id = db
            .insert_pr_sync(10, "Broken", "fix/broken", "def456", "merge", 1)
            .unwrap();

        db.fail_pr_sync(id, "SVN conflict on src/main.rs").unwrap();
        let entries = db.list_pr_syncs(10).unwrap();
        assert_eq!(entries[0].status, "failed");
        assert_eq!(
            entries[0].error_message.as_deref(),
            Some("SVN conflict on src/main.rs")
        );
    }

    #[test]
    fn test_pr_sync_count_and_last_time() {
        let db = setup_db();
        assert_eq!(db.count_pr_syncs_by_status("completed").unwrap(), 0);
        assert!(db.get_last_pr_sync_time().unwrap().is_none());

        let id = db
            .insert_pr_sync(1, "PR1", "feature/a", "sha1", "merge", 1)
            .unwrap();
        db.complete_pr_sync(id, 50, 50).unwrap();

        assert_eq!(db.count_pr_syncs_by_status("completed").unwrap(), 1);
        assert!(db.get_last_pr_sync_time().unwrap().is_some());
    }

    // -----------------------------------------------------------------------
    // Issue #36: forensic logging tests
    // -----------------------------------------------------------------------

    /// Migration backward compatibility: a database at schema v2 (no success
    /// column) can be migrated to v3 and existing rows get `success = 1`
    /// (the DEFAULT).
    #[test]
    fn test_migration_backward_compat_audit_success() {
        use rusqlite::Connection;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        // Apply only migrations 1 and 2 (pre-#36 schema).
        crate::db::schema::run_migrations(&conn).unwrap();

        // The column should now exist with the DEFAULT.  Manually insert a
        // row *without* supplying success (simulating a pre-migration row
        // written before the column existed).
        conn.execute(
            "INSERT INTO audit_log (action, created_at) VALUES ('legacy_action', datetime('now'))",
            [],
        )
        .unwrap();

        // Read back and verify the default is true (1).
        let success: i32 = conn
            .query_row(
                "SELECT success FROM audit_log WHERE action = 'legacy_action'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(success, 1, "legacy rows should default to success = 1");
    }

    /// Explicit success/failure persistence: `insert_audit_log` with
    /// `success = false` is queryable and counted by `count_errors`.
    #[test]
    fn test_audit_log_explicit_success_failure() {
        let db = setup_db();

        // Insert a success entry.
        db.insert_audit_log("sync_ok", None, None, None, None, Some("all good"), true)
            .unwrap();

        // Insert a failure entry.
        db.insert_audit_log(
            "sync_cycle",
            None,
            None,
            None,
            None,
            Some("connection refused"),
            false,
        )
        .unwrap();

        // count_errors should return 1 (only the failed one).
        assert_eq!(db.count_errors().unwrap(), 1);

        // list_audit_log should have correct success flags.
        let entries = db.list_audit_log(10, 0).unwrap();
        assert_eq!(entries.len(), 2);
        // Newest first.
        assert!(!entries[0].success); // sync_cycle failure
        assert!(entries[1].success); // sync_ok success
    }

    /// Error counting uses `success = false`, NOT action-name heuristics.
    /// An action named "error_recovery" that succeeded should NOT be counted
    /// as an error.
    #[test]
    fn test_count_errors_ignores_action_name() {
        let db = setup_db();

        // Action name contains "error" but success = true.
        db.insert_audit_log(
            "error_recovery",
            None,
            None,
            None,
            None,
            Some("recovered"),
            true,
        )
        .unwrap();

        // Action name is benign but success = false.
        db.insert_audit_log("sync_cycle", None, None, None, None, Some("timeout"), false)
            .unwrap();

        // Only the actual failure should be counted.
        assert_eq!(db.count_errors().unwrap(), 1);
    }

    /// `insert_audit_entry` uses the model's `success` field, not inferred
    /// from the action name.
    #[test]
    fn test_insert_audit_entry_uses_model_success() {
        let db = setup_db();

        let success_entry = crate::models::AuditEntry::success("sync_cycle", "ok");
        let failure_entry = crate::models::AuditEntry::failure("sync_cycle", "failed: timeout");

        db.insert_audit_entry(&success_entry).unwrap();
        db.insert_audit_entry(&failure_entry).unwrap();

        let entries = db.list_audit_log(10, 0).unwrap();
        assert_eq!(entries.len(), 2);
        // Newest first.
        assert!(!entries[0].success);
        assert!(entries[1].success);
        assert_eq!(db.count_errors().unwrap(), 1);
    }

    /// `list_audit_entries` (web layer) reads persisted success, not
    /// heuristics.
    #[test]
    fn test_list_audit_entries_web_reads_persisted_success() {
        let db = setup_db();

        // Success entry with "error" in action name — must still show success.
        db.insert_audit_log("error_recovery", None, None, None, None, Some("ok"), true)
            .unwrap();
        // Failure entry with benign action name.
        db.insert_audit_log("sync_cycle", None, None, None, None, Some("boom"), false)
            .unwrap();

        let entries = db.list_audit_entries(10, None, None).unwrap();
        assert_eq!(entries.len(), 2);

        let recovery = entries
            .iter()
            .find(|e| e.action == "error_recovery")
            .unwrap();
        assert!(
            recovery.success,
            "error_recovery with success=true should show success"
        );

        let cycle = entries.iter().find(|e| e.action == "sync_cycle").unwrap();
        assert!(
            !cycle.success,
            "sync_cycle with success=false should show failure"
        );
    }

    /// Forced team-mode failure → failed audit entry + error count increments.
    /// This simulates what the team-mode sync engine does when a sync cycle
    /// fails: it writes an `AuditEntry::failure(...)` via `insert_audit_entry`.
    #[test]
    fn test_team_mode_forced_failure_audit_entry() {
        let db = setup_db();

        // Simulate successful sync cycle (team mode).
        let ok = crate::models::AuditEntry::success("sync_cycle", "synced 3 commits");
        db.insert_audit_entry(&ok).unwrap();

        // Simulate failed sync cycle (team mode).
        let fail = crate::models::AuditEntry::failure("sync_cycle", "svn connection timeout");
        db.insert_audit_entry(&fail).unwrap();

        // Another success.
        let ok2 = crate::models::AuditEntry::success("sync_cycle", "synced 1 commit");
        db.insert_audit_entry(&ok2).unwrap();

        // count_errors should be exactly 1.
        assert_eq!(db.count_errors().unwrap(), 1);

        // Total audit entries should be 3.
        assert_eq!(db.count_audit_log().unwrap(), 3);

        // The failed entry should be retrievable with correct details.
        let all = db.list_audit_log(10, 0).unwrap();
        let failures: Vec<_> = all.iter().filter(|e| !e.success).collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].details.as_deref(),
            Some("svn connection timeout")
        );
    }
}
