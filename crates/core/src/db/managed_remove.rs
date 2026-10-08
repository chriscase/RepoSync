//! Durable v12 journal for explicit managed removal (#65).
//!
//! Documents live in `kv_state` under `managed_remove_v1:`. Ordinary schema
//! stays v12. Legacy root DELETE does not write this journal.
//!
//! Removal disables the registration, waits until #64 import / Git→SVN /
//! SVN→Git ownership is quiet, then deletes only exact per-repo secret keys and the
//! repository row. Commit mappings, audit rows, and remote history are kept.
//! Completed removals keep a tombstone and retained mappings for optional
//! restore while recovery data exists. A tombstone blocks a stale job from
//! inserting the same id again until restore or purge.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Database;
use crate::errors::DatabaseError;
use crate::models::Repository;

const PREFIX: &str = "managed_remove_v1:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedRemoveState {
    Queued,
    Cancelling,
    Running,
    Completed,
    Failed,
    ReconciliationRequired,
}

impl ManagedRemoveState {
    pub fn is_terminal_success(&self) -> bool {
        matches!(self, Self::Completed)
    }

    pub fn blocks_success_response(&self) -> bool {
        !self.is_terminal_success()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedRemoveOperation {
    pub version: u8,
    pub id: String,
    pub repo_id: String,
    pub operation_type: String,
    pub initiator_id: String,
    pub request_id: String,
    pub target_fingerprint: String,
    pub created_at: String,
    pub updated_at: String,
    pub state: ManagedRemoveState,
    pub outcome_detail: Option<String>,
    pub last_svn_rev: i64,
    pub last_git_sha: String,
    pub remote_git: String,
    pub remote_svn: String,
    pub restore_supported: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalTombstone {
    pub version: u8,
    pub repo_id: String,
    pub operation_id: String,
    pub name: String,
    pub svn_url: String,
    pub svn_branch: String,
    pub git_api_url: String,
    pub git_repo: String,
    pub git_branch: String,
    pub parent_id: Option<String>,
    pub last_svn_rev: i64,
    pub last_git_sha: String,
    pub commit_map_count: i64,
    pub remote_git: String,
    pub remote_svn: String,
    pub restore_supported: bool,
    pub retention: String,
    #[serde(default)]
    pub svn_username: String,
    #[serde(default = "default_git_provider")]
    pub git_provider: String,
    #[serde(default = "default_sync_mode")]
    pub sync_mode: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: i64,
    #[serde(default)]
    pub lfs_threshold_mb: i64,
    #[serde(default = "default_auto_merge")]
    pub auto_merge: bool,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub allowed_paths: Option<String>,
    #[serde(default)]
    pub blocked_patterns: Option<String>,
    #[serde(default)]
    pub consecutive_errors: i64,
    #[serde(default)]
    pub teams_webhook_url: Option<String>,
    #[serde(default)]
    pub total_syncs: i64,
    #[serde(default)]
    pub total_errors: i64,
    #[serde(default = "default_sync_status")]
    pub sync_status: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

fn default_git_provider() -> String {
    "gitea".into()
}

fn default_sync_mode() -> String {
    "direct".into()
}

fn default_poll_interval() -> i64 {
    60
}

fn default_auto_merge() -> bool {
    true
}

fn default_sync_status() -> String {
    "idle".into()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemovalBlocker {
    ImportRunning,
    ImportReconciliationRequired,
    SvnCommitRunning,
    SvnCommitReconciliationRequired,
    GitPushRunning,
    GitPushReconciliationRequired,
    ChildRegistrations { count: i64 },
}

impl RemovalBlocker {
    pub fn detail(&self) -> String {
        match self {
            Self::ImportRunning => {
                "in-flight import still owns the repository; local data was not deleted".into()
            }
            Self::ImportReconciliationRequired => {
                "import has an unresolved external effect; registration and local data were kept"
                    .into()
            }
            Self::SvnCommitRunning => {
                "in-flight Git-to-SVN commit still owns the repository; local data was not deleted"
                    .into()
            }
            Self::SvnCommitReconciliationRequired => {
                "Git-to-SVN commit has an unresolved external effect; registration and local data were kept"
                    .into()
            }
            Self::GitPushRunning => {
                "in-flight SVN-to-Git push still owns the repository; local data was not deleted"
                    .into()
            }
            Self::GitPushReconciliationRequired => {
                "SVN-to-Git push has an unresolved external effect; registration and local data were kept"
                    .into()
            }
            Self::ChildRegistrations { count } => format!(
                "parent removal is blocked while {count} child registration(s) exist; remove child branch pairs first — children are not removed automatically"
            ),
        }
    }

    pub fn waiting_state(&self) -> ManagedRemoveState {
        match self {
            Self::ImportReconciliationRequired
            | Self::SvnCommitReconciliationRequired
            | Self::GitPushReconciliationRequired => ManagedRemoveState::ReconciliationRequired,
            Self::ChildRegistrations { .. } => ManagedRemoveState::Failed,
            Self::ImportRunning | Self::SvnCommitRunning | Self::GitPushRunning => {
                ManagedRemoveState::Cancelling
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum RestoreAdvance {
    Restored { repo: Repository },
    AlreadyListed { repo: Repository },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemovalAdvance {
    NotFound,
    ParentBlocked {
        child_count: i64,
    },
    Waiting {
        operation: ManagedRemoveOperation,
        blocker: RemovalBlocker,
    },
    Cleanup {
        operation: ManagedRemoveOperation,
    },
    Completed {
        operation: ManagedRemoveOperation,
    },
}

fn key(kind: &str, id: &str) -> String {
    format!("{PREFIX}{kind}:{id}")
}

fn read_value(conn: &Connection, name: &str) -> Result<Option<String>, DatabaseError> {
    Ok(conn
        .query_row("SELECT value FROM kv_state WHERE key=?1", [name], |row| {
            row.get(0)
        })
        .optional()?)
}

fn write_value(conn: &Connection, name: &str, value: &str) -> Result<(), DatabaseError> {
    conn.execute(
        "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        params![name, value, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn parse_op(raw: &str) -> Result<ManagedRemoveOperation, DatabaseError> {
    let op: ManagedRemoveOperation = serde_json::from_str(raw)
        .map_err(|error| DatabaseError::Other(format!("invalid managed removal: {error}")))?;
    if op.version != 1 || op.operation_type != "managed_remove" {
        return Err(DatabaseError::Other(
            "unsupported managed removal document".into(),
        ));
    }
    Ok(op)
}

pub fn new_work_blocked(conn: &Connection, repo_id: &str) -> Result<bool, DatabaseError> {
    Ok(read_value(conn, &key("active", repo_id))?.is_some() || tombstone_present(conn, repo_id)?)
}

pub fn tombstone_present(conn: &Connection, repo_id: &str) -> Result<bool, DatabaseError> {
    Ok(read_value(conn, &key("tombstone", repo_id))?.is_some())
}

/// True when a non-terminal managed removal still owns the registration id.
pub fn removal_in_progress(conn: &Connection, repo_id: &str) -> Result<bool, DatabaseError> {
    let Some(op_id) = read_value(conn, &key("active", repo_id))? else {
        return Ok(false);
    };
    let Some(raw) = read_value(conn, &key("document", &op_id))? else {
        return Ok(true);
    };
    let op = parse_op(&raw)?;
    Ok(!op.state.is_terminal_success())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CredentialHolder {
    Revoked,
    Repo(String),
    Global,
    None,
}

fn credential_holder(conn: &Connection, repo_id: &str, key_prefix: &str) -> CredentialHolder {
    let repo_key = format!("{}_{}", key_prefix, repo_id);
    match read_value(conn, &repo_key) {
        Ok(Some(val)) if val.is_empty() => return CredentialHolder::Revoked,
        Ok(Some(_)) => return CredentialHolder::Repo(repo_id.to_string()),
        Ok(None) => {}
        Err(_) => return CredentialHolder::None,
    }
    let mut pid = load_repo(conn, repo_id)
        .ok()
        .flatten()
        .and_then(|r| r.parent_id);
    let mut visited = std::collections::HashSet::new();
    while let Some(current_pid) = pid {
        if !visited.insert(current_pid.clone()) {
            break;
        }
        let ancestor_key = format!("{}_{}", key_prefix, current_pid);
        match read_value(conn, &ancestor_key) {
            Ok(Some(val)) if val.is_empty() => return CredentialHolder::Revoked,
            Ok(Some(_)) => return CredentialHolder::Repo(current_pid),
            Ok(None) => {}
            Err(_) => return CredentialHolder::None,
        }
        pid = load_repo(conn, &current_pid)
            .ok()
            .flatten()
            .and_then(|r| r.parent_id);
    }
    match read_value(conn, key_prefix) {
        Ok(Some(val)) if !val.is_empty() => CredentialHolder::Global,
        _ => CredentialHolder::None,
    }
}

fn list_all_repo_ids(conn: &Connection) -> Result<Vec<String>, DatabaseError> {
    let mut stmt = conn.prepare("SELECT id FROM repositories ORDER BY id")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(DatabaseError::from)
}

fn repos_sharing_credential_holder(
    conn: &Connection,
    holder: &CredentialHolder,
    key_prefix: &str,
) -> Result<Vec<String>, DatabaseError> {
    let mut out = Vec::new();
    for id in list_all_repo_ids(conn)? {
        let theirs = credential_holder(conn, &id, key_prefix);
        let matches = match holder {
            CredentialHolder::Repo(owner) => {
                matches!(&theirs, CredentialHolder::Repo(h) if h == owner)
            }
            CredentialHolder::Global => theirs == CredentialHolder::Global,
            _ => false,
        };
        if matches {
            out.push(id);
        }
    }
    Ok(out)
}

fn git_remote_identity_nonempty(repo: &Repository) -> bool {
    !repo.git_api_url.trim().is_empty() && !repo.git_repo.trim().is_empty()
}

fn owned_secret_keys(repo_id: &str) -> [String; 2] {
    [
        format!("secret_svn_password_{repo_id}"),
        format!("secret_git_token_{repo_id}"),
    ]
}

fn load_repo(conn: &Connection, repo_id: &str) -> Result<Option<Repository>, DatabaseError> {
    conn.query_row(
        "SELECT id, name, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, sync_mode, poll_interval_secs, lfs_threshold_mb, auto_merge, enabled, created_by, created_at, updated_at, last_svn_rev, last_git_sha, last_sync_at, sync_status, total_syncs, total_errors, parent_id, allowed_paths, blocked_patterns, consecutive_errors, teams_webhook_url
         FROM repositories WHERE id=?1",
        [repo_id],
        |row| {
            Ok(Repository {
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
                allowed_paths: row.get(24)?,
                blocked_patterns: row.get(25)?,
                consecutive_errors: row.get(26)?,
                teams_webhook_url: row.get(27)?,
                created_at: row.get(15)?,
                updated_at: row.get(16)?,
                last_svn_rev: row.get(17)?,
                last_git_sha: row.get(18)?,
                last_sync_at: row.get(19)?,
                sync_status: row.get(20)?,
                total_syncs: row.get(21)?,
                total_errors: row.get(22)?,
            })
        },
    )
    .optional()
    .map_err(DatabaseError::from)
}

fn child_count(conn: &Connection, repo_id: &str) -> Result<i64, DatabaseError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM repositories WHERE parent_id=?1",
        [repo_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalChildRef {
    pub id: String,
    pub name: String,
    pub git_branch: String,
    pub svn_branch: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalParentRef {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedGitRegistrationRef {
    pub id: String,
    pub name: String,
    pub git_branch: String,
    pub svn_branch: String,
    pub relationship: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialKeyPreview {
    pub key: String,
    pub action: String,
    pub retained_for_repo_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalDependencyPreview {
    pub repo_id: String,
    pub repo_name: String,
    pub parent: Option<RemovalParentRef>,
    pub children: Vec<RemovalChildRef>,
    pub parent_removal_blocked: bool,
    pub block_reason: Option<String>,
    pub credentials: Vec<CredentialKeyPreview>,
    pub managed_local_path: String,
    pub sibling_local_paths_preserved: Vec<String>,
    pub shared_git_registrations: Vec<SharedGitRegistrationRef>,
}

fn list_children(conn: &Connection, repo_id: &str) -> Result<Vec<RemovalChildRef>, DatabaseError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, git_branch, svn_branch, enabled FROM repositories WHERE parent_id=?1 ORDER BY name",
    )?;
    let rows = stmt.query_map([repo_id], |row| {
        Ok(RemovalChildRef {
            id: row.get(0)?,
            name: row.get(1)?,
            git_branch: row.get(2)?,
            svn_branch: row.get(3)?,
            enabled: row.get::<_, i32>(4)? != 0,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(DatabaseError::from)
}

fn shared_git_registrations(
    conn: &Connection,
    repo: &Repository,
) -> Result<Vec<SharedGitRegistrationRef>, DatabaseError> {
    if !git_remote_identity_nonempty(repo) {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, name, git_branch, svn_branch, parent_id FROM repositories
         WHERE git_api_url=?1 AND git_repo=?2 AND id!=?3
         ORDER BY name",
    )?;
    let rows = stmt.query_map(params![repo.git_api_url, repo.git_repo, repo.id], |row| {
        let id: String = row.get(0)?;
        let parent_id: Option<String> = row.get(4)?;
        let relationship = if parent_id.as_deref() == Some(repo.id.as_str()) {
            "child_branch_pair"
        } else if repo.parent_id.as_deref() == Some(id.as_str()) {
            "parent_registration"
        } else if parent_id == repo.parent_id && parent_id.is_some() {
            "sibling_branch_pair"
        } else {
            "shared_remote_registration"
        };
        Ok(SharedGitRegistrationRef {
            id,
            name: row.get(1)?,
            git_branch: row.get(2)?,
            svn_branch: row.get(3)?,
            relationship: relationship.to_string(),
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(DatabaseError::from)
}

fn credential_key_nonempty(conn: &Connection, key: &str) -> Result<bool, DatabaseError> {
    Ok(read_value(conn, key)?
        .map(|v| !v.is_empty())
        .unwrap_or(false))
}

fn build_credential_preview(
    conn: &Connection,
    repo: &Repository,
) -> Result<Vec<CredentialKeyPreview>, DatabaseError> {
    let mut out = Vec::new();
    let svn_key = format!("secret_svn_password_{}", repo.id);
    let git_key = format!("secret_git_token_{}", repo.id);
    if credential_key_nonempty(conn, &svn_key)? {
        let mut retained = Vec::new();
        if let Some(parent_id) = repo.parent_id.as_deref() {
            let parent_key = format!("secret_svn_password_{parent_id}");
            if credential_key_nonempty(conn, &parent_key)? {
                retained.push(parent_id.to_string());
            }
        }
        for child in list_children(conn, &repo.id)? {
            let child_key = format!("secret_svn_password_{}", child.id);
            if credential_key_nonempty(conn, &child_key)? {
                retained.push(child.id);
            }
        }
        for id in repos_sharing_credential_holder(
            conn,
            &CredentialHolder::Repo(repo.id.clone()),
            "secret_svn_password",
        )? {
            if id != repo.id && !retained.contains(&id) {
                retained.push(id);
            }
        }
        out.push(CredentialKeyPreview {
            key: svn_key,
            action: "delete_if_present".into(),
            retained_for_repo_ids: retained,
        });
    }
    if credential_key_nonempty(conn, &git_key)? {
        let mut retained = Vec::new();
        if let Some(parent_id) = repo.parent_id.as_deref() {
            let parent_key = format!("secret_git_token_{parent_id}");
            if credential_key_nonempty(conn, &parent_key)? {
                retained.push(parent_id.to_string());
            }
        }
        for child in list_children(conn, &repo.id)? {
            let child_key = format!("secret_git_token_{}", child.id);
            if credential_key_nonempty(conn, &child_key)? {
                retained.push(child.id);
            }
        }
        for id in repos_sharing_credential_holder(
            conn,
            &CredentialHolder::Repo(repo.id.clone()),
            "secret_git_token",
        )? {
            if id != repo.id && !retained.contains(&id) {
                retained.push(id);
            }
        }
        out.push(CredentialKeyPreview {
            key: git_key,
            action: "delete_if_present".into(),
            retained_for_repo_ids: retained,
        });
    }
    for global in ["secret_svn_password", "secret_git_token"] {
        if credential_key_nonempty(conn, global)? {
            let retained =
                repos_sharing_credential_holder(conn, &CredentialHolder::Global, global)?;
            out.push(CredentialKeyPreview {
                key: global.to_string(),
                action: "never_deleted_by_managed_remove".into(),
                retained_for_repo_ids: retained,
            });
        }
    }
    Ok(out)
}

fn sibling_local_paths(conn: &Connection, repo: &Repository) -> Result<Vec<String>, DatabaseError> {
    let mut paths = Vec::new();
    if let Some(parent_id) = repo.parent_id.as_deref() {
        paths.push(format!("repos/{parent_id}"));
        for child in list_children(conn, parent_id)? {
            if child.id != repo.id {
                paths.push(format!("repos/{}", child.id));
            }
        }
    } else {
        for child in list_children(conn, &repo.id)? {
            paths.push(format!("repos/{}", child.id));
        }
    }
    Ok(paths)
}

pub fn build_removal_dependency_preview(
    conn: &Connection,
    repo_id: &str,
) -> Result<Option<RemovalDependencyPreview>, DatabaseError> {
    let Some(repo) = load_repo(conn, repo_id)? else {
        return Ok(None);
    };
    let children = list_children(conn, repo_id)?;
    let blocked = !children.is_empty();
    let block_reason = if blocked {
        Some(
            RemovalBlocker::ChildRegistrations {
                count: children.len() as i64,
            }
            .detail(),
        )
    } else {
        None
    };
    let parent = if let Some(parent_id) = repo.parent_id.clone() {
        load_repo(conn, &parent_id)?.map(|p| RemovalParentRef {
            id: p.id,
            name: p.name,
        })
    } else {
        None
    };
    Ok(Some(RemovalDependencyPreview {
        repo_id: repo.id.clone(),
        repo_name: repo.name.clone(),
        parent,
        children,
        parent_removal_blocked: blocked,
        block_reason,
        credentials: build_credential_preview(conn, &repo)?,
        managed_local_path: format!("repos/{}", repo.id),
        sibling_local_paths_preserved: sibling_local_paths(conn, &repo)?,
        shared_git_registrations: shared_git_registrations(conn, &repo)?,
    }))
}

fn fingerprint(repo: &Repository) -> String {
    let source = serde_json::json!({
        "repo_id": repo.id,
        "svn_url": repo.svn_url,
        "svn_branch": repo.svn_branch,
        "git_api_url": repo.git_api_url,
        "git_repo": repo.git_repo,
        "git_branch": repo.git_branch,
        "managed_rel": format!("repos/{}", repo.id),
    })
    .to_string();
    hex::encode(Sha256::digest(source.as_bytes()))
}

fn read_op(
    conn: &Connection,
    repo_id: &str,
) -> Result<Option<ManagedRemoveOperation>, DatabaseError> {
    let Some(op_id) =
        read_value(conn, &key("active", repo_id))?.or(read_value(conn, &key("latest", repo_id))?)
    else {
        return Ok(None);
    };
    let Some(raw) = read_value(conn, &key("document", &op_id))? else {
        return Ok(None);
    };
    let op = parse_op(&raw)?;
    if op.repo_id != repo_id {
        return Err(DatabaseError::Other(
            "managed removal repository mismatch".into(),
        ));
    }
    Ok(Some(op))
}

fn store_op(conn: &Connection, op: &ManagedRemoveOperation) -> Result<(), DatabaseError> {
    write_value(
        conn,
        &key("document", &op.id),
        &serde_json::to_string(op).map_err(|error| {
            DatabaseError::Other(format!("managed removal serialization failed: {error}"))
        })?,
    )?;
    write_value(conn, &key("latest", &op.repo_id), &op.id)?;
    if op.state.is_terminal_success() {
        conn.execute(
            "DELETE FROM kv_state WHERE key=?1 AND value=?2",
            params![key("active", &op.repo_id), op.id],
        )?;
    } else {
        write_value(conn, &key("active", &op.repo_id), &op.id)?;
    }
    Ok(())
}

fn audit(
    conn: &Connection,
    repo_id: &str,
    action: &str,
    details: &str,
    success: bool,
) -> Result<(), DatabaseError> {
    conn.execute(
        "INSERT INTO audit_log (action, direction, svn_rev, git_sha, author, details, created_at, success, repo_id)
         VALUES (?1, NULL, NULL, NULL, 'managed_remove', ?2, ?3, ?4, ?5)",
        params![
            action,
            details,
            Utc::now().to_rfc3339(),
            success as i32,
            repo_id
        ],
    )?;
    Ok(())
}

fn import_blocker(
    conn: &Connection,
    repo_id: &str,
) -> Result<Option<RemovalBlocker>, DatabaseError> {
    let Some(op_id) = read_value(conn, &format!("import_operation_v1:active:{repo_id}"))? else {
        return Ok(None);
    };
    let Some(raw) = read_value(conn, &format!("import_operation_v1:document:{op_id}"))? else {
        return Ok(Some(RemovalBlocker::ImportRunning));
    };
    let state = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|value| value.get("state")?.as_str().map(str::to_owned));
    Ok(match state.as_deref() {
        Some("completed" | "cancelled" | "failed") => None,
        Some("reconciliation_required") => Some(RemovalBlocker::ImportReconciliationRequired),
        _ => Some(RemovalBlocker::ImportRunning),
    })
}

fn svn_commit_blocker(
    conn: &Connection,
    repo_id: &str,
) -> Result<Option<RemovalBlocker>, DatabaseError> {
    let Some(op_id) = read_value(conn, &format!("git_to_svn_commit_v1:active:{repo_id}"))? else {
        return Ok(None);
    };
    let Some(raw) = read_value(conn, &format!("git_to_svn_commit_v1:document:{op_id}"))? else {
        return Ok(Some(RemovalBlocker::SvnCommitRunning));
    };
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    let state = value.get("state").and_then(|s| s.as_str());
    Ok(match state {
        Some("completed" | "failed") => None,
        Some("reconciliation_required") => Some(RemovalBlocker::SvnCommitReconciliationRequired),
        _ => Some(RemovalBlocker::SvnCommitRunning),
    })
}

fn git_push_blocker(
    conn: &Connection,
    repo_id: &str,
) -> Result<Option<RemovalBlocker>, DatabaseError> {
    let Some(op_id) = read_value(conn, &format!("svn_to_git_push_v1:active:{repo_id}"))? else {
        return Ok(None);
    };
    let Some(raw) = read_value(conn, &format!("svn_to_git_push_v1:document:{op_id}"))? else {
        return Ok(Some(RemovalBlocker::GitPushRunning));
    };
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    let state = value.get("state").and_then(|s| s.as_str());
    Ok(match state {
        Some("completed" | "failed") => None,
        Some("reconciliation_required") => Some(RemovalBlocker::GitPushReconciliationRequired),
        _ => Some(RemovalBlocker::GitPushRunning),
    })
}

fn writer_blocker(
    conn: &Connection,
    repo_id: &str,
) -> Result<Option<RemovalBlocker>, DatabaseError> {
    if let Some(blocker) = import_blocker(conn, repo_id)? {
        return Ok(Some(blocker));
    }
    if let Some(blocker) = svn_commit_blocker(conn, repo_id)? {
        return Ok(Some(blocker));
    }
    git_push_blocker(conn, repo_id)
}

fn fresh_op(repo: &Repository, initiator_id: &str, request_id: &str) -> ManagedRemoveOperation {
    let now = Utc::now().to_rfc3339();
    ManagedRemoveOperation {
        version: 1,
        id: Uuid::new_v4().to_string(),
        repo_id: repo.id.clone(),
        operation_type: "managed_remove".into(),
        initiator_id: initiator_id.into(),
        request_id: request_id.into(),
        target_fingerprint: fingerprint(repo),
        created_at: now.clone(),
        updated_at: now,
        state: ManagedRemoveState::Queued,
        outcome_detail: None,
        last_svn_rev: repo.last_svn_rev,
        last_git_sha: repo.last_git_sha.clone(),
        remote_git: "untouched".into(),
        remote_svn: "untouched".into(),
        restore_supported: false,
    }
}

impl Database {
    pub fn managed_remove_blocks_new_work(&self, repo_id: &str) -> Result<bool, DatabaseError> {
        let conn = self.conn();
        new_work_blocked(&conn, repo_id)
    }

    pub fn managed_removal(
        &self,
        repo_id: &str,
    ) -> Result<Option<ManagedRemoveOperation>, DatabaseError> {
        let conn = self.conn();
        read_op(&conn, repo_id)
    }

    pub fn removal_dependency_preview(
        &self,
        repo_id: &str,
    ) -> Result<Option<RemovalDependencyPreview>, DatabaseError> {
        let conn = self.conn();
        build_removal_dependency_preview(&conn, repo_id)
    }

    pub fn removal_tombstone(
        &self,
        repo_id: &str,
    ) -> Result<Option<RemovalTombstone>, DatabaseError> {
        let conn = self.conn();
        let Some(raw) = read_value(&conn, &key("tombstone", repo_id))? else {
            return Ok(None);
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| DatabaseError::Other(format!("invalid removal tombstone: {error}")))
    }

    /// Narrow legacy DELETE: clear `enabled` only. Does not create a removal
    /// journal and does not delete mappings, secrets, files, or remotes.
    pub fn legacy_disable_repository(&self, repo_id: &str) -> Result<bool, DatabaseError> {
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE repositories SET enabled=0, updated_at=?1 WHERE id=?2",
            params![Utc::now().to_rfc3339(), repo_id],
        )?;
        Ok(changed == 1)
    }

    pub fn prepare_managed_remove(
        &self,
        repo_id: &str,
        initiator_id: &str,
        request_id: &str,
    ) -> Result<RemovalAdvance, DatabaseError> {
        self.transaction(|tx| {
            if let Some(op) = read_op(tx, repo_id)? {
                if op.state.is_terminal_success() {
                    return Ok(RemovalAdvance::Completed { operation: op });
                }
            }
            let children = child_count(tx, repo_id)?;
            if children > 0 {
                return Ok(RemovalAdvance::ParentBlocked {
                    child_count: children,
                });
            }
            let Some(repo) = load_repo(tx, repo_id)? else {
                return Ok(match read_op(tx, repo_id)? {
                    Some(op) if op.state.is_terminal_success() => {
                        RemovalAdvance::Completed { operation: op }
                    }
                    Some(_) => RemovalAdvance::NotFound,
                    None => RemovalAdvance::NotFound,
                });
            };
            let mut op = match read_op(tx, repo_id)? {
                Some(op) => op,
                None => {
                    let op = fresh_op(&repo, initiator_id, request_id);
                    audit(
                        tx,
                        repo_id,
                        "managed_remove_accepted",
                        "explicit managed removal accepted; remote Git and SVN history will not be modified; restore is available after completion when recovery metadata remains",
                        true,
                    )?;
                    op
                }
            };
            tx.execute(
                "UPDATE repositories SET enabled=0, updated_at=?1 WHERE id=?2",
                params![Utc::now().to_rfc3339(), repo_id],
            )?;
            if let Some(blocker) = writer_blocker(tx, repo_id)? {
                op.state = blocker.waiting_state();
                op.outcome_detail = Some(blocker.detail());
                op.updated_at = Utc::now().to_rfc3339();
                store_op(tx, &op)?;
                return Ok(RemovalAdvance::Waiting { operation: op, blocker });
            }
            op.state = ManagedRemoveState::Running;
            op.outcome_detail = Some(
                "writers are quiet; owned local cleanup has not finished".into(),
            );
            op.updated_at = Utc::now().to_rfc3339();
            op.last_svn_rev = repo.last_svn_rev;
            op.last_git_sha = repo.last_git_sha;
            store_op(tx, &op)?;
            Ok(RemovalAdvance::Cleanup { operation: op })
        })
    }

    pub fn note_removal_waiting(
        &self,
        repo_id: &str,
        op_id: &str,
        detail: &str,
    ) -> Result<ManagedRemoveOperation, DatabaseError> {
        self.transaction(|tx| {
            let mut op = require_active(tx, repo_id, op_id)?;
            if op.state.is_terminal_success() {
                return Ok(op);
            }
            op.state = ManagedRemoveState::Cancelling;
            op.outcome_detail = Some(detail.into());
            op.updated_at = Utc::now().to_rfc3339();
            store_op(tx, &op)?;
            Ok(op)
        })
    }

    pub fn fail_managed_remove(
        &self,
        repo_id: &str,
        op_id: &str,
        detail: &str,
    ) -> Result<ManagedRemoveOperation, DatabaseError> {
        self.transaction(|tx| {
            let mut op = require_active(tx, repo_id, op_id)?;
            if op.state.is_terminal_success() {
                return Err(DatabaseError::Other(
                    "completed removal cannot be marked failed".into(),
                ));
            }
            op.state = ManagedRemoveState::Failed;
            op.outcome_detail = Some(detail.into());
            op.updated_at = Utc::now().to_rfc3339();
            store_op(tx, &op)?;
            audit(tx, repo_id, "managed_remove_failed", detail, false)?;
            Ok(op)
        })
    }

    /// Finish removal only after owned-path cleanup has returned success.
    /// Partial failure must use [`Database::fail_managed_remove`] instead.
    pub fn complete_managed_remove(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> Result<ManagedRemoveOperation, DatabaseError> {
        self.transaction(|tx| {
            let mut op = require_active(tx, repo_id, op_id)?;
            if op.state.is_terminal_success() {
                return Ok(op);
            }
            if child_count(tx, repo_id)? > 0 {
                return Err(DatabaseError::Other(
                    "child registrations appeared before removal finished".into(),
                ));
            }
            if let Some(blocker) = writer_blocker(tx, repo_id)? {
                return Err(DatabaseError::Other(blocker.detail()));
            }
            let Some(repo) = load_repo(tx, repo_id)? else {
                return Err(DatabaseError::Other(
                    "registration disappeared before removal could record its tombstone".into(),
                ));
            };
            let commit_map_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM commit_map WHERE repo_id=?1",
                [repo_id],
                |row| row.get(0),
            )?;
            let tombstone = RemovalTombstone {
                version: 1,
                repo_id: repo.id.clone(),
                operation_id: op.id.clone(),
                name: repo.name.clone(),
                svn_url: repo.svn_url.clone(),
                svn_branch: repo.svn_branch.clone(),
                git_api_url: repo.git_api_url.clone(),
                git_repo: repo.git_repo.clone(),
                git_branch: repo.git_branch.clone(),
                parent_id: repo.parent_id.clone(),
                last_svn_rev: repo.last_svn_rev,
                last_git_sha: repo.last_git_sha.clone(),
                commit_map_count,
                remote_git: "untouched".into(),
                remote_svn: "untouched".into(),
                restore_supported: true,
                retention: "tombstone_audit_mappings_retained_restore_available".into(),
                svn_username: repo.svn_username.clone(),
                git_provider: repo.git_provider.clone(),
                sync_mode: repo.sync_mode.clone(),
                poll_interval_secs: repo.poll_interval_secs,
                lfs_threshold_mb: repo.lfs_threshold_mb,
                auto_merge: repo.auto_merge,
                created_by: repo.created_by.clone(),
                allowed_paths: repo.allowed_paths.clone(),
                blocked_patterns: repo.blocked_patterns.clone(),
                consecutive_errors: repo.consecutive_errors,
                teams_webhook_url: repo.teams_webhook_url.clone(),
                total_syncs: repo.total_syncs,
                total_errors: repo.total_errors,
                sync_status: repo.sync_status.clone(),
                created_at: repo.created_at.clone(),
                updated_at: repo.updated_at.clone(),
            };
            write_value(
                tx,
                &key("tombstone", repo_id),
                &serde_json::to_string(&tombstone).map_err(|error| {
                    DatabaseError::Other(format!("tombstone serialization failed: {error}"))
                })?,
            )?;
            for secret_key in owned_secret_keys(repo_id) {
                tx.execute("DELETE FROM kv_state WHERE key=?1", [&secret_key])?;
                tx.execute("DELETE FROM encrypted_secrets WHERE key=?1", [&secret_key])?;
            }
            if tx.execute("DELETE FROM repositories WHERE id=?1", [repo_id])? != 1 {
                return Err(DatabaseError::Other(
                    "registration row was not removed".into(),
                ));
            }
            op.state = ManagedRemoveState::Completed;
            op.restore_supported = true;
            op.outcome_detail = Some(
                "removed from active listings; owned local data cleaned; remote Git and SVN history were not modified; registration can be restored while recovery metadata remains"
                    .into(),
            );
            op.updated_at = Utc::now().to_rfc3339();
            op.last_svn_rev = repo.last_svn_rev;
            op.last_git_sha = repo.last_git_sha;
            store_op(tx, &op)?;
            audit(
                tx,
                repo_id,
                "managed_remove_completed",
                op.outcome_detail.as_deref().unwrap_or("completed"),
                true,
            )?;
            Ok(op)
        })
    }

    /// Restore a completed managed removal while its tombstone and retained
    /// mappings still exist. The registration returns disabled until the user
    /// explicitly enables sync again.
    pub fn restore_managed_registration(
        &self,
        repo_id: &str,
    ) -> Result<RestoreAdvance, DatabaseError> {
        self.transaction(|tx| {
            if removal_in_progress(tx, repo_id)? {
                return Err(DatabaseError::Other(
                    "managed removal is still in progress; restore is not available".into(),
                ));
            }
            if let Some(existing) = load_repo(tx, repo_id)? {
                return Ok(RestoreAdvance::AlreadyListed { repo: existing });
            }
            let Some(raw) = read_value(tx, &key("tombstone", repo_id))? else {
                return Err(DatabaseError::NotFound {
                    entity: "removal_tombstone".into(),
                    id: repo_id.into(),
                });
            };
            let tombstone: RemovalTombstone = serde_json::from_str(&raw).map_err(|error| {
                DatabaseError::Other(format!("invalid removal tombstone: {error}"))
            })?;
            if !tombstone.restore_supported || tombstone.retention.contains("purged") {
                return Err(DatabaseError::Other(
                    "recovery data was purged; restore is not supported".into(),
                ));
            }
            if let Some(parent_id) = tombstone.parent_id.as_deref() {
                if load_repo(tx, parent_id)?.is_none() {
                    return Err(DatabaseError::Other(
                        "parent registration is missing; child restore is not available".into(),
                    ));
                }
            }
            let maps: i64 = tx.query_row(
                "SELECT COUNT(*) FROM commit_map WHERE repo_id=?1",
                [repo_id],
                |row| row.get(0),
            )?;
            if maps < tombstone.commit_map_count {
                return Err(DatabaseError::Other(
                    "retained commit mappings were purged; restore is not available".into(),
                ));
            }
            let now = Utc::now().to_rfc3339();
            let restored = Repository {
                id: tombstone.repo_id.clone(),
                name: tombstone.name.clone(),
                svn_url: tombstone.svn_url.clone(),
                svn_branch: tombstone.svn_branch.clone(),
                svn_username: tombstone.svn_username.clone(),
                git_provider: tombstone.git_provider.clone(),
                git_api_url: tombstone.git_api_url.clone(),
                git_repo: tombstone.git_repo.clone(),
                git_branch: tombstone.git_branch.clone(),
                sync_mode: tombstone.sync_mode.clone(),
                poll_interval_secs: tombstone.poll_interval_secs,
                lfs_threshold_mb: tombstone.lfs_threshold_mb,
                auto_merge: tombstone.auto_merge,
                enabled: false,
                created_by: tombstone.created_by.clone(),
                parent_id: tombstone.parent_id.clone(),
                created_at: if tombstone.created_at.is_empty() {
                    now.clone()
                } else {
                    tombstone.created_at.clone()
                },
                updated_at: now.clone(),
                last_svn_rev: tombstone.last_svn_rev,
                last_git_sha: tombstone.last_git_sha.clone(),
                last_sync_at: None,
                sync_status: "idle".into(),
                total_syncs: tombstone.total_syncs,
                total_errors: tombstone.total_errors,
                allowed_paths: tombstone.allowed_paths.clone(),
                blocked_patterns: tombstone.blocked_patterns.clone(),
                consecutive_errors: tombstone.consecutive_errors,
                teams_webhook_url: tombstone.teams_webhook_url.clone(),
            };
            tx.execute(
                "INSERT INTO repositories (id, name, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, sync_mode, poll_interval_secs, lfs_threshold_mb, auto_merge, enabled, created_by, created_at, updated_at, last_svn_rev, last_git_sha, last_sync_at, sync_status, total_syncs, total_errors, parent_id, allowed_paths, blocked_patterns, consecutive_errors, teams_webhook_url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28)",
                params![
                    restored.id,
                    restored.name,
                    restored.svn_url,
                    restored.svn_branch,
                    restored.svn_username,
                    restored.git_provider,
                    restored.git_api_url,
                    restored.git_repo,
                    restored.git_branch,
                    restored.sync_mode,
                    restored.poll_interval_secs,
                    restored.lfs_threshold_mb,
                    restored.auto_merge as i32,
                    0i32,
                    restored.created_by,
                    restored.created_at,
                    restored.updated_at,
                    restored.last_svn_rev,
                    restored.last_git_sha,
                    restored.last_sync_at,
                    restored.sync_status,
                    restored.total_syncs,
                    restored.total_errors,
                    restored.parent_id,
                    restored.allowed_paths,
                    restored.blocked_patterns,
                    restored.consecutive_errors,
                    restored.teams_webhook_url,
                ],
            )?;
            tx.execute(
                "DELETE FROM kv_state WHERE key=?1",
                [key("tombstone", repo_id)],
            )?;
            audit(
                tx,
                repo_id,
                "managed_remove_restored",
                "registration restored from tombstone; sync remains disabled until explicitly enabled",
                true,
            )?;
            Ok(RestoreAdvance::Restored { repo: restored })
        })
    }
}

fn require_active(
    conn: &Connection,
    repo_id: &str,
    op_id: &str,
) -> Result<ManagedRemoveOperation, DatabaseError> {
    let active = read_value(conn, &key("active", repo_id))?;
    if active.as_deref() != Some(op_id) {
        if let Some(op) = read_op(conn, repo_id)? {
            if op.id == op_id && op.state.is_terminal_success() {
                return Ok(op);
            }
        }
        return Err(DatabaseError::Other(
            "stale or inactive managed removal".into(),
        ));
    }
    let raw = read_value(conn, &key("document", op_id))?
        .ok_or_else(|| DatabaseError::Other("missing managed removal document".into()))?;
    let op = parse_op(&raw)?;
    if op.repo_id != repo_id {
        return Err(DatabaseError::Other(
            "managed removal repository mismatch".into(),
        ));
    }
    Ok(op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::import_operations::ImportOperationState;

    fn setup() -> Database {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db
    }

    fn repo(id: &str, name: &str, parent: Option<&str>) -> Repository {
        Repository {
            id: id.into(),
            name: name.into(),
            svn_url: "file:///tmp/svn".into(),
            svn_branch: "trunk".into(),
            svn_username: "user".into(),
            git_provider: "gitea".into(),
            git_api_url: "http://127.0.0.1/api/v1".into(),
            git_repo: "local/fixture".into(),
            git_branch: "main".into(),
            sync_mode: "direct".into(),
            poll_interval_secs: 60,
            lfs_threshold_mb: 0,
            auto_merge: true,
            enabled: true,
            created_by: None,
            parent_id: parent.map(str::to_owned),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            last_svn_rev: 3,
            last_git_sha: "abc".into(),
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 1,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        }
    }

    #[test]
    fn candidate_r02_journal_failure_is_retryable_and_stale_job_cannot_resurrect() {
        let db = setup();
        let parent = repo("parent-1", "Parent", None);
        db.insert_repository(&parent).unwrap();
        db.insert_repository(&repo("child-1", "Child", Some("parent-1")))
            .unwrap();
        match db
            .prepare_managed_remove("parent-1", "admin", "req")
            .unwrap()
        {
            RemovalAdvance::ParentBlocked { child_count } => assert_eq!(child_count, 1),
            other => panic!("expected parent block, got {other:?}"),
        }
        assert!(db.get_repository("parent-1").unwrap().unwrap().enabled);
        assert!(db.managed_removal("parent-1").unwrap().is_none());

        db.set_state("secret_svn_password_child-1", "child-secret")
            .unwrap();
        db.set_state("secret_svn_password_parent-1", "owned-secret")
            .unwrap();
        match db
            .prepare_managed_remove("child-1", "admin", "child")
            .unwrap()
        {
            RemovalAdvance::Cleanup { operation } => {
                db.complete_managed_remove("child-1", &operation.id)
                    .unwrap();
            }
            other => panic!("expected child cleanup, got {other:?}"),
        }
        assert!(db.get_repository("child-1").unwrap().is_none());
        assert!(db.get_repository("parent-1").unwrap().unwrap().enabled);
        assert_eq!(
            db.get_state("secret_svn_password_parent-1")
                .unwrap()
                .as_deref(),
            Some("owned-secret")
        );
        assert!(db
            .get_state("secret_svn_password_child-1")
            .unwrap()
            .is_none());
        db.set_state("secret_svn_password_parent-1", "owned-secret")
            .unwrap();
        db.set_state("secret_git_token_parent-1", "owned-token")
            .unwrap();
        db.set_state("secret_svn_password", "global-secret")
            .unwrap();
        db.set_state("secret_svn_password_sibling", "sibling-secret")
            .unwrap();
        db.set_state("last_svn_rev_parent-1", "3").unwrap();
        db.conn()
            .execute(
                "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
                 VALUES (3, 'abc', 'svn_to_git', 't', 'a', 'b', 'parent-1')",
                [],
            )
            .unwrap();

        let data = tempfile::tempdir().unwrap();
        let owned = data.path().join("repos").join("parent-1");
        std::fs::create_dir_all(owned.join("git-repo")).unwrap();
        std::fs::write(owned.join("git-repo").join("owned.txt"), "owned\n").unwrap();
        std::fs::create_dir_all(data.path().join("repos").join("sibling")).unwrap();
        std::fs::write(
            data.path().join("repos").join("sibling").join("keep.txt"),
            "keep\n",
        )
        .unwrap();

        db.create_import_operation("parent-1", "admin", "imp", "fingerprint")
            .unwrap();
        match db
            .prepare_managed_remove("parent-1", "admin", "req-1")
            .unwrap()
        {
            RemovalAdvance::Waiting { blocker, operation } => {
                assert_eq!(blocker, RemovalBlocker::ImportRunning);
                assert_eq!(operation.state, ManagedRemoveState::Cancelling);
                assert!(!operation.state.is_terminal_success());
            }
            other => panic!("expected import wait, got {other:?}"),
        }
        assert!(owned.join("git-repo").join("owned.txt").exists());
        assert_eq!(
            db.get_state("secret_svn_password_parent-1")
                .unwrap()
                .as_deref(),
            Some("owned-secret")
        );
        let op = db.managed_removal("parent-1").unwrap().unwrap();
        db.finish_import_operation(
            "parent-1",
            &db.active_import_operation("parent-1").unwrap().unwrap().id,
            ImportOperationState::Cancelled,
            "cancelled for removal test",
        )
        .unwrap();

        let failed = db
            .fail_managed_remove(&op.repo_id, &op.id, "injected cleanup failure")
            .unwrap();
        assert_eq!(failed.state, ManagedRemoveState::Failed);
        assert!(db.get_repository("parent-1").unwrap().is_some());
        assert_eq!(
            db.get_state("secret_svn_password_parent-1")
                .unwrap()
                .as_deref(),
            Some("owned-secret")
        );

        match db
            .prepare_managed_remove("parent-1", "admin", "req-2")
            .unwrap()
        {
            RemovalAdvance::Cleanup { operation } => {
                assert_eq!(operation.id, op.id);
                crate::managed_remove::remove_owned_repo_tree(data.path(), "parent-1").unwrap();
                let done = db
                    .complete_managed_remove("parent-1", &operation.id)
                    .unwrap();
                assert_eq!(done.state, ManagedRemoveState::Completed);
                assert!(done.restore_supported);
                assert_eq!(done.remote_git, "untouched");
                assert_eq!(done.remote_svn, "untouched");
            }
            other => panic!("expected cleanup, got {other:?}"),
        }

        assert!(!owned.exists());
        assert_eq!(
            std::fs::read_to_string(data.path().join("repos").join("sibling").join("keep.txt"))
                .unwrap(),
            "keep\n"
        );
        assert!(db.get_repository("parent-1").unwrap().is_none());
        assert!(db
            .get_state("secret_svn_password_parent-1")
            .unwrap()
            .is_none());
        assert!(db.get_state("secret_git_token_parent-1").unwrap().is_none());
        assert_eq!(
            db.get_state("secret_svn_password").unwrap().as_deref(),
            Some("global-secret")
        );
        assert_eq!(
            db.get_state("secret_svn_password_sibling")
                .unwrap()
                .as_deref(),
            Some("sibling-secret")
        );
        assert_eq!(
            db.get_state("last_svn_rev_parent-1").unwrap().as_deref(),
            Some("3")
        );
        let maps: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM commit_map WHERE repo_id='parent-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(maps, 1);
        let audits: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE repo_id='parent-1' AND action='managed_remove_failed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(audits >= 1);
        let tombstone = db.removal_tombstone("parent-1").unwrap().unwrap();
        assert!(tombstone.restore_supported);
        assert_eq!(tombstone.commit_map_count, 1);
        assert_eq!(tombstone.remote_svn, "untouched");

        let again = db
            .prepare_managed_remove("parent-1", "admin", "req-3")
            .unwrap();
        assert!(matches!(again, RemovalAdvance::Completed { .. }));
        let error = db.insert_repository(&parent).unwrap_err();
        assert!(error.to_string().contains("cannot recreate"), "{error}");
        assert!(db.list_repositories().unwrap().is_empty());
    }

    #[test]
    fn removal_blocked_while_svn_to_git_push_running() {
        use crate::db::git_push_operations::{GitPushIntent, GitPushOperationState};

        let db = setup();
        let r = repo("push-repo", "Push", None);
        db.insert_repository(&r).unwrap();

        let intent = GitPushIntent {
            repo_id: "push-repo",
            initiator_id: "worker",
            request_id: "req-1",
            target_fingerprint: "fp",
            source_svn_rev: 3,
            source_svn_author: "dev",
            source_svn_message: "add feature",
            pre_push_git_remote: "origin",
            pre_push_git_branch: "main",
            pre_push_git_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            pre_push_git_tree: Some("cccccccccccccccccccccccccccccccccccccccc"),
            intended_local_git_sha: "dddddddddddddddddddddddddddddddddddddddd",
            intended_local_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            intended_local_git_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        };
        let push = db.begin_svn_to_git_push(intent).unwrap();
        assert_eq!(push.state, GitPushOperationState::Running);

        match db
            .prepare_managed_remove("push-repo", "admin", "req")
            .unwrap()
        {
            RemovalAdvance::Waiting { blocker, operation } => {
                assert_eq!(blocker, RemovalBlocker::GitPushRunning);
                assert_eq!(operation.state, ManagedRemoveState::Cancelling);
            }
            other => panic!("expected git-push wait, got {other:?}"),
        }
        assert!(!db.get_repository("push-repo").unwrap().unwrap().enabled);
    }

    #[test]
    fn removal_blocked_while_svn_to_git_push_held_for_reconcile() {
        use crate::db::git_push_operations::{GitPushIntent, GitPushOperationState};

        let db = setup();
        let r = repo("held-repo", "Held", None);
        db.insert_repository(&r).unwrap();

        let intent = GitPushIntent {
            repo_id: "held-repo",
            initiator_id: "worker",
            request_id: "req-1",
            target_fingerprint: "fp",
            source_svn_rev: 3,
            source_svn_author: "dev",
            source_svn_message: "add feature",
            pre_push_git_remote: "origin",
            pre_push_git_branch: "main",
            pre_push_git_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            pre_push_git_tree: Some("cccccccccccccccccccccccccccccccccccccccc"),
            intended_local_git_sha: "dddddddddddddddddddddddddddddddddddddddd",
            intended_local_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            intended_local_git_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        };
        let push = db.begin_svn_to_git_push(intent).unwrap();
        db.hold_svn_to_git_reconciliation("held-repo", &push.id, "lost reply")
            .unwrap();
        assert_eq!(
            db.active_git_push_operation("held-repo")
                .unwrap()
                .unwrap()
                .state,
            GitPushOperationState::ReconciliationRequired
        );

        match db
            .prepare_managed_remove("held-repo", "admin", "req")
            .unwrap()
        {
            RemovalAdvance::Waiting { blocker, operation } => {
                assert_eq!(blocker, RemovalBlocker::GitPushReconciliationRequired);
                assert_eq!(operation.state, ManagedRemoveState::ReconciliationRequired);
            }
            other => panic!("expected git-push reconcile wait, got {other:?}"),
        }
    }

    #[test]
    fn removal_allowed_when_no_active_git_push() {
        use crate::db::git_push_operations::{GitPushIntent, GitPushOperationState};

        let db = setup();
        let r = repo("quiet-repo", "Quiet", None);
        db.insert_repository(&r).unwrap();

        let intent = GitPushIntent {
            repo_id: "quiet-repo",
            initiator_id: "worker",
            request_id: "req-1",
            target_fingerprint: "fp",
            source_svn_rev: 3,
            source_svn_author: "dev",
            source_svn_message: "add feature",
            pre_push_git_remote: "origin",
            pre_push_git_branch: "main",
            pre_push_git_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            pre_push_git_tree: Some("cccccccccccccccccccccccccccccccccccccccc"),
            intended_local_git_sha: "dddddddddddddddddddddddddddddddddddddddd",
            intended_local_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            intended_local_git_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        };
        let push = db.begin_svn_to_git_push(intent).unwrap();
        let done = db
            .confirm_svn_to_git_push(
                "quiet-repo",
                &push.id,
                "dddddddddddddddddddddddddddddddddddddddd",
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            )
            .unwrap();
        assert_eq!(done.state, GitPushOperationState::Completed);
        assert!(db
            .active_git_push_operation("quiet-repo")
            .unwrap()
            .is_none());

        match db
            .prepare_managed_remove("quiet-repo", "admin", "req")
            .unwrap()
        {
            RemovalAdvance::Cleanup { operation } => {
                assert_eq!(operation.state, ManagedRemoveState::Running);
            }
            other => panic!("expected cleanup, got {other:?}"),
        }
    }

    #[test]
    fn removal_still_blocked_by_git_to_svn_commit() {
        use crate::db::svn_commit_operations::{
            IntendedPath, SvnCommitIntent, SvnCommitOperationState,
        };

        let db = setup();
        let r = repo("svn-repo", "Svn", None);
        db.insert_repository(&r).unwrap();

        let paths = vec![IntendedPath {
            action: "A".into(),
            path: "feature.txt".into(),
            content_sha256: Some("d".repeat(64)),
        }];
        let intent = SvnCommitIntent {
            repo_id: "svn-repo",
            initiator_id: "worker",
            request_id: "req-1",
            target_fingerprint: "fp",
            source_git_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            source_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            source_git_tree: "cccccccccccccccccccccccccccccccccccccccc",
            target_svn_uuid: "uuid",
            target_svn_path: "file:///svn",
            target_svn_root_url: "file:///svn",
            target_svn_branch_path: "",
            pre_write_svn_rev: 2,
            pre_write_svn_tree: "pre-tree",
            projection: "{}",
            intended_changed_paths: paths,
            intended_svn_tree: "post-tree",
            author: "dev",
            source_message: "add feature",
        };
        let commit = db.begin_git_to_svn_commit(intent).unwrap();
        assert_eq!(commit.state, SvnCommitOperationState::Running);

        match db
            .prepare_managed_remove("svn-repo", "admin", "req")
            .unwrap()
        {
            RemovalAdvance::Waiting { blocker, operation } => {
                assert_eq!(blocker, RemovalBlocker::SvnCommitRunning);
                assert_eq!(operation.state, ManagedRemoveState::Cancelling);
            }
            other => panic!("expected svn-commit wait, got {other:?}"),
        }
    }

    #[test]
    fn removal_still_blocked_by_git_to_svn_reconcile_hold() {
        use crate::db::svn_commit_operations::{
            IntendedPath, SvnCommitIntent, SvnCommitOperationState,
        };

        let db = setup();
        let r = repo("svn-held", "SvnHeld", None);
        db.insert_repository(&r).unwrap();

        let paths = vec![IntendedPath {
            action: "A".into(),
            path: "feature.txt".into(),
            content_sha256: Some("d".repeat(64)),
        }];
        let intent = SvnCommitIntent {
            repo_id: "svn-held",
            initiator_id: "worker",
            request_id: "req-1",
            target_fingerprint: "fp",
            source_git_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            source_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            source_git_tree: "cccccccccccccccccccccccccccccccccccccccc",
            target_svn_uuid: "uuid",
            target_svn_path: "file:///svn",
            target_svn_root_url: "file:///svn",
            target_svn_branch_path: "",
            pre_write_svn_rev: 2,
            pre_write_svn_tree: "pre-tree",
            projection: "{}",
            intended_changed_paths: paths,
            intended_svn_tree: "post-tree",
            author: "dev",
            source_message: "add feature",
        };
        let commit = db.begin_git_to_svn_commit(intent).unwrap();
        db.hold_git_to_svn_reconciliation("svn-held", &commit.id, "lost reply")
            .unwrap();
        assert_eq!(
            db.active_svn_commit_operation("svn-held")
                .unwrap()
                .unwrap()
                .state,
            SvnCommitOperationState::ReconciliationRequired
        );

        match db
            .prepare_managed_remove("svn-held", "admin", "req")
            .unwrap()
        {
            RemovalAdvance::Waiting { blocker, operation } => {
                assert_eq!(blocker, RemovalBlocker::SvnCommitReconciliationRequired);
                assert_eq!(operation.state, ManagedRemoveState::ReconciliationRequired);
            }
            other => panic!("expected svn-commit reconcile wait, got {other:?}"),
        }
    }

    #[test]
    fn credential_chain_inheritance_lists_dependent_repos_in_retained_for_repo_ids() {
        let db = setup();
        db.insert_repository(&repo("inherit-parent", "Parent", None))
            .unwrap();
        db.insert_repository(&repo("inherit-child", "Child", Some("inherit-parent")))
            .unwrap();
        db.set_state("secret_svn_password_inherit-parent", "shared")
            .unwrap();
        let preview = db
            .removal_dependency_preview("inherit-parent")
            .unwrap()
            .unwrap();
        let svn = preview
            .credentials
            .iter()
            .find(|c| c.key == "secret_svn_password_inherit-parent")
            .expect("parent svn credential preview");
        assert!(
            svn.retained_for_repo_ids
                .contains(&"inherit-child".to_string()),
            "child inheriting parent credential must be listed as retained: {:?}",
            svn.retained_for_repo_ids
        );
    }

    #[test]
    fn blank_git_remote_does_not_surface_shared_registrations() {
        let db = setup();
        let blank = repo("blank-remote", "Blank", None);
        let mut blank = blank;
        blank.git_api_url = String::new();
        blank.git_repo = String::new();
        db.insert_repository(&blank).unwrap();
        db.insert_repository(&repo("other-remote", "Other", None))
            .unwrap();
        let preview = db
            .removal_dependency_preview("blank-remote")
            .unwrap()
            .unwrap();
        assert!(preview.shared_git_registrations.is_empty());
    }

    #[test]
    fn restore_managed_registration_is_idempotent_and_disabled() {
        let db = setup();
        let r = repo("restore-me", "Restore", None);
        db.insert_repository(&r).unwrap();
        db.conn()
            .execute(
                "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
                 VALUES (1, 'abc', 'svn_to_git', 't', 'a', 'b', 'restore-me')",
                [],
            )
            .unwrap();
        let RemovalAdvance::Cleanup { operation } = db
            .prepare_managed_remove("restore-me", "admin", "req")
            .unwrap()
        else {
            panic!("expected cleanup");
        };
        crate::managed_remove::remove_owned_repo_tree(
            tempfile::tempdir().unwrap().path(),
            "restore-me",
        )
        .unwrap();
        db.complete_managed_remove("restore-me", &operation.id)
            .unwrap();
        assert!(db.get_repository("restore-me").unwrap().is_none());
        let RestoreAdvance::Restored { repo: restored } =
            db.restore_managed_registration("restore-me").unwrap()
        else {
            panic!("expected restore");
        };
        assert!(!restored.enabled);
        assert_eq!(restored.name, "Restore");
        assert!(db.removal_tombstone("restore-me").unwrap().is_none());
        let again = db.restore_managed_registration("restore-me").unwrap();
        assert!(matches!(again, RestoreAdvance::AlreadyListed { .. }));
    }

    #[test]
    fn restore_rejects_while_removal_in_progress() {
        let db = setup();
        let r = repo("busy-restore", "Busy", None);
        db.insert_repository(&r).unwrap();
        db.create_import_operation("busy-restore", "admin", "imp", "fp")
            .unwrap();
        match db
            .prepare_managed_remove("busy-restore", "admin", "req")
            .unwrap()
        {
            RemovalAdvance::Waiting { .. } => {}
            other => panic!("expected waiting, got {other:?}"),
        };
        assert!(removal_in_progress(&db.conn(), "busy-restore").unwrap());
        let err = db
            .restore_managed_registration("busy-restore")
            .unwrap_err()
            .to_string();
        assert!(err.contains("in progress"), "{err}");
        let import = db.active_import_operation("busy-restore").unwrap().unwrap();
        db.finish_import_operation(
            "busy-restore",
            &import.id,
            ImportOperationState::Cancelled,
            "cancel",
        )
        .unwrap();
    }
}
