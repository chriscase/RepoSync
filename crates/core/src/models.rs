//! Domain model types used throughout RepoSync.
//!
//! These types bridge the sync engine, database layer, and web API.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Sync Status
// ---------------------------------------------------------------------------

/// High-level sync status summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatus {
    pub state: SyncState,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_svn_revision: Option<i64>,
    pub last_git_hash: Option<String>,
    pub total_syncs: i64,
    pub total_conflicts: i64,
    pub active_conflicts: i64,
    pub total_errors: i64,
    pub last_error_at: Option<String>,
    pub uptime_secs: u64,
}

/// Current sync state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    Idle,
    Initializing,
    Syncing,
    Error,
    ErrorPaused,
    ConflictFound,
    ReconciliationRequired,
}

impl SyncState {
    /// Parse a state string into a `SyncState`.
    pub fn from_str_val(s: &str) -> Self {
        match s {
            "initializing" => Self::Initializing,
            "syncing" | "detecting" | "applying" => Self::Syncing,
            "error" => Self::Error,
            "error_paused" => Self::ErrorPaused,
            "conflict_found" => Self::ConflictFound,
            "reconciliation_required" => Self::ReconciliationRequired,
            _ => Self::Idle,
        }
    }
}

impl std::fmt::Display for SyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Initializing => write!(f, "initializing"),
            Self::Syncing => write!(f, "syncing"),
            Self::Error => write!(f, "error"),
            Self::ErrorPaused => write!(f, "error_paused"),
            Self::ConflictFound => write!(f, "conflict_found"),
            Self::ReconciliationRequired => write!(f, "reconciliation_required"),
        }
    }
}

// ---------------------------------------------------------------------------
// Conflict (model-layer)
// ---------------------------------------------------------------------------

/// A conflict record for the model/database layer.
///
/// This is distinct from `conflict::detector::Conflict` which is the
/// in-memory detection-time representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    pub id: String,
    pub file_path: String,
    pub conflict_type: String,
    pub svn_content: Option<String>,
    pub git_content: Option<String>,
    pub base_content: Option<String>,
    pub svn_revision: Option<i64>,
    pub git_hash: Option<String>,
    pub status: String,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    pub repo_id: Option<String>,
}

impl Conflict {
    /// Create a new conflict record with defaults.
    pub fn new(file_path: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            file_path,
            conflict_type: "content".to_string(),
            svn_content: None,
            git_content: None,
            base_content: None,
            svn_revision: None,
            git_hash: None,
            status: "detected".to_string(),
            resolution: None,
            resolved_by: None,
            repo_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Sync Record
// ---------------------------------------------------------------------------

/// A record of a single synchronization action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRecord {
    pub id: String,
    pub repo_id: Option<String>,
    pub svn_revision: Option<i64>,
    pub git_hash: Option<String>,
    pub direction: SyncDirection,
    pub author: String,
    pub message: String,
    pub timestamp: DateTime<Utc>,
    pub synced_at: DateTime<Utc>,
    pub status: SyncRecordStatus,
}

/// Direction of sync.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    SvnToGit,
    GitToSvn,
}

impl std::fmt::Display for SyncDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SvnToGit => write!(f, "svn_to_git"),
            Self::GitToSvn => write!(f, "git_to_svn"),
        }
    }
}

/// Status of a sync record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncRecordStatus {
    Pending,
    Applied,
    Failed,
}

impl std::fmt::Display for SyncRecordStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Applied => write!(f, "applied"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

// ---------------------------------------------------------------------------
// Audit Entry
// ---------------------------------------------------------------------------

/// An audit-log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub action: String,
    pub details: String,
    pub success: bool,
    pub timestamp: DateTime<Utc>,
}

impl AuditEntry {
    /// Create a success audit entry.
    pub fn success(action: &str, details: &str) -> Self {
        Self {
            action: action.to_string(),
            details: details.to_string(),
            success: true,
            timestamp: Utc::now(),
        }
    }

    /// Create a failure audit entry.
    pub fn failure(action: &str, details: &str) -> Self {
        Self {
            action: action.to_string(),
            details: details.to_string(),
            success: false,
            timestamp: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Author mapping (identity)
// ---------------------------------------------------------------------------

/// An SVN-to-Git author identity mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorMapping {
    pub svn_username: String,
    pub git_name: String,
    pub git_email: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Conflict resolution
// ---------------------------------------------------------------------------

/// How a conflict was resolved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolution {
    AcceptSvn,
    AcceptGit,
    Custom,
}

impl std::fmt::Display for ConflictResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AcceptSvn => write!(f, "accept_svn"),
            Self::AcceptGit => write!(f, "accept_git"),
            Self::Custom => write!(f, "custom"),
        }
    }
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

/// Pagination parameters for list queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pagination {
    pub page: u32,
    pub per_page: u32,
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            page: 1,
            per_page: 20,
        }
    }
}

/// A paginated result set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedResult<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub per_page: u32,
    pub total_pages: u32,
}

// ---------------------------------------------------------------------------
// Web-layer conflict view
// ---------------------------------------------------------------------------

/// A conflict record as seen by the web layer with DateTime fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConflict {
    pub id: String,
    pub file_path: String,
    pub conflict_type: String,
    pub svn_content: Option<String>,
    pub git_content: Option<String>,
    pub base_content: Option<String>,
    pub diff: Option<String>,
    pub svn_revision: Option<i64>,
    pub git_hash: Option<String>,
    pub status: String,
    pub resolution: Option<String>,
    pub resolved_content: Option<String>,
    pub resolved_by: Option<String>,
    pub detected_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub repo_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Web-layer audit entry
// ---------------------------------------------------------------------------

/// An audit-log entry as seen by the web layer with typed fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAuditEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub action: String,
    pub details: String,
    pub actor: Option<String>,
    pub success: bool,
}

// ---------------------------------------------------------------------------
// Multi-user auth types
// ---------------------------------------------------------------------------

/// A user account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub email: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub role: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// An encrypted credential stored for a user (e.g. SVN password, API token).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserCredential {
    pub id: String,
    pub user_id: String,
    pub service: String,
    pub server_url: String,
    pub username: String,
    #[serde(skip_serializing)]
    pub encrypted_value: String,
    #[serde(skip_serializing)]
    pub nonce: String,
    pub created_at: String,
    pub updated_at: String,
}

/// An active user session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub user_id: String,
    pub expires_at: String,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Repository (multi-repo support)
// ---------------------------------------------------------------------------

/// A configured repository for multi-repo sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: String,
    pub name: String,
    pub svn_url: String,
    pub svn_branch: String,
    pub svn_username: String,
    pub git_provider: String,
    pub git_api_url: String,
    pub git_repo: String,
    pub git_branch: String,
    pub sync_mode: String,
    pub poll_interval_secs: i64,
    pub lfs_threshold_mb: i64,
    pub auto_merge: bool,
    pub enabled: bool,
    pub created_by: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub last_svn_rev: i64,
    #[serde(default)]
    pub last_git_sha: String,
    #[serde(default)]
    pub last_sync_at: Option<String>,
    #[serde(default = "default_sync_status")]
    pub sync_status: String,
    #[serde(default)]
    pub total_syncs: i64,
    #[serde(default)]
    pub total_errors: i64,
    /// JSON array of allowed path prefixes (e.g., `["source/", "config/"]`).
    /// NULL means no restriction.
    #[serde(default)]
    pub allowed_paths: Option<String>,
    /// JSON array of blocked glob patterns (e.g., `["*.exe", "temp/"]`).
    /// NULL means no blocked patterns.
    #[serde(default)]
    pub blocked_patterns: Option<String>,
    /// Consecutive permanent error count for circuit breaker.
    #[serde(default)]
    pub consecutive_errors: i64,
    /// Per-repo Teams webhook URL (overrides global setting).
    #[serde(default)]
    pub teams_webhook_url: Option<String>,
}

fn default_sync_status() -> String {
    "idle".to_string()
}

/// The largest LFS threshold a pair accepts: 1 TiB, far above any file a
/// repository would hold, and small enough that the byte value cannot
/// overflow.
pub const MAX_LFS_THRESHOLD_MB: i64 = 1024 * 1024;

/// Why `mb` is not a usable LFS threshold, if it is not. Zero disables LFS.
pub fn lfs_threshold_mb_problem(mb: i64) -> Option<String> {
    if mb < 0 {
        Some("must not be negative (0 disables LFS)".into())
    } else if mb > MAX_LFS_THRESHOLD_MB {
        Some(format!("must be at most {MAX_LFS_THRESHOLD_MB} MB"))
    } else {
        None
    }
}

impl Repository {
    /// The LFS threshold in bytes; 0 when LFS is disabled. A stored value
    /// from before the bound saturates instead of overflowing.
    pub fn lfs_threshold_bytes(&self) -> u64 {
        u64::try_from(self.lfs_threshold_mb)
            .unwrap_or(0)
            .saturating_mul(1024 * 1024)
    }

    /// Why scheduled sync will refuse this pair as it is stored, or `None`
    /// when its stored settings are usable. These are the settings the
    /// scheduler checks before it starts a pair's cycle; a pair saved before
    /// a check existed can still hold a value that fails it.
    pub fn sync_configuration_problem(&self) -> Option<String> {
        if let Some(problem) = crate::git::git_branch_name_problem(&self.git_branch) {
            return Some(format!("git_branch {problem}"));
        }
        if let Err(error) =
            crate::svn::operand::configured_branch_url(&self.svn_url, &self.svn_branch)
        {
            // The URL is built from both settings, so either can be at fault.
            let detail = match &error {
                crate::errors::SvnError::WorkingCopyError { detail, .. } => detail.clone(),
                other => other.to_string(),
            };
            return Some(format!(
                "svn_url or svn_branch is not usable: {}",
                crate::errors::redact_vcs_error_detail(&detail)
            ));
        }
        for (setting, raw) in [
            ("allowed_paths", self.allowed_paths.as_deref()),
            ("blocked_patterns", self.blocked_patterns.as_deref()),
        ] {
            if raw.is_some_and(|raw| serde_json::from_str::<Vec<String>>(raw).is_err()) {
                return Some(format!("{setting} must be a JSON array of strings"));
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Personal Branch Mode types
// ---------------------------------------------------------------------------

/// A record of a processed PR merge (personal branch mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrSyncEntry {
    pub id: i64,
    pub pr_number: i64,
    pub pr_title: String,
    pub pr_branch: String,
    pub merge_sha: String,
    pub merge_strategy: String,
    pub svn_rev_start: Option<i64>,
    pub svn_rev_end: Option<i64>,
    pub commit_count: i64,
    pub status: String,
    pub error_message: Option<String>,
    pub detected_at: String,
    pub completed_at: Option<String>,
}

/// Statistics for a personal sync session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersonalSyncStats {
    /// Number of SVN→Git commits synced.
    pub svn_to_git_count: u64,
    /// Number of Git→SVN commits synced.
    pub git_to_svn_count: u64,
    /// Number of merged PRs processed.
    pub prs_processed: u64,
    /// Timestamp when the sync session started.
    pub started_at: Option<DateTime<Utc>>,
    /// Timestamp when the sync session completed.
    pub completed_at: Option<DateTime<Utc>>,
    /// Number of conflicts detected.
    pub conflicts_detected: u64,
    /// Number of conflicts auto-resolved.
    pub conflicts_auto_resolved: u64,
}

/// Merge strategy detected from a PR merge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Standard merge commit (2 parents).
    Merge,
    /// Squash merge (1 parent, single commit).
    Squash,
    /// Rebase merge (1 parent, multiple commits).
    Rebase,
    /// Could not determine strategy.
    Unknown,
}

impl std::fmt::Display for MergeStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Merge => write!(f, "merge"),
            Self::Squash => write!(f, "squash"),
            Self::Rebase => write!(f, "rebase"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl MergeStrategy {
    /// Parse a strategy string.
    pub fn from_str_val(s: &str) -> Self {
        match s {
            "merge" => Self::Merge,
            "squash" => Self::Squash,
            "rebase" => Self::Rebase,
            _ => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod lfs_threshold_tests {
    use super::*;

    #[test]
    fn the_lfs_threshold_bound_and_byte_conversion() {
        assert!(lfs_threshold_mb_problem(0).is_none());
        assert!(lfs_threshold_mb_problem(MAX_LFS_THRESHOLD_MB).is_none());
        assert!(lfs_threshold_mb_problem(-1).is_some());
        assert!(lfs_threshold_mb_problem(MAX_LFS_THRESHOLD_MB + 1).is_some());
        let bytes = |mb: i64| {
            let mut repo: Repository = serde_json::from_value(serde_json::json!({
                "id": "r", "name": "r", "svn_url": "u", "svn_branch": "", "svn_username": "",
                "git_provider": "github", "git_api_url": "", "git_repo": "", "git_branch": "main",
                "sync_mode": "direct", "poll_interval_secs": 60, "lfs_threshold_mb": 0,
                "auto_merge": true, "enabled": true, "created_by": null,
                "created_at": "", "updated_at": ""
            }))
            .unwrap();
            repo.lfs_threshold_mb = mb;
            repo.lfs_threshold_bytes()
        };
        assert_eq!(bytes(0), 0);
        assert_eq!(bytes(-3), 0);
        assert_eq!(bytes(5), 5 * 1024 * 1024);
        assert_eq!(bytes(MAX_LFS_THRESHOLD_MB), 1 << 40);
        assert_eq!(bytes(i64::MAX), u64::MAX);
    }
}
