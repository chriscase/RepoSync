//! Bidirectional SVN <-> Git synchronization engine.
//!
//! The [`SyncEngine`] is the heart of RepoSync. It implements a state machine
//! that orchestrates each sync cycle:
//!
//! 1. Fetch new SVN revisions since the last watermark.
//! 2. Fetch new Git commits since the last watermark.
//! 3. Detect conflicts between overlapping changes.
//! 4. If no conflicts (or auto-merge succeeds), apply changes in both directions.
//! 5. Update watermarks, commit map, and audit log.
//!
//! A lock mechanism prevents concurrent sync cycles.

use std::collections::HashSet;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::config::{AppConfig, SvnLayout};
use crate::conflict::detector::git_action_to_change_kind;
use crate::conflict::detector::{ChangeKind, ConflictDetector, FileChange};
use crate::conflict::merger::Merger;
use crate::conflict::Conflict;
use crate::db::git_push_operations::{
    git_push_target_fingerprint, GitPushIntent, GitPushOperation, GitPushOperationState,
};
use crate::db::import_operations::{resolve_repo_import_baseline, RepoImportBaseline};
use crate::db::queries::AuditLogInput;
use crate::db::svn_commit_operations::{
    svn_commit_target_fingerprint, SvnCommitIntent, SvnCommitOperation, SvnCommitOperationState,
};
use crate::db::team_cycle_mapping_operations::{
    team_cycle_mapping_fingerprint, TeamCycleMappingDirection, TeamCycleMappingIntent,
    TeamCycleMappingOperation, TeamCycleMappingOutcome, TeamCycleMappingState,
};
use crate::db::Database;
use crate::echo_suppression::{
    classify_incoming_git_commit, classify_incoming_svn_revision, personal_mode_marker_echo,
    verify_no_target_receipt, EchoDisposition, NoTargetReceiptVerdict, TeamEchoContext,
    SYNC_MARKER,
};
use crate::errors::SyncError;
use crate::git::client::{GitClient, PendingCommitSelection};
use crate::git_push::{
    inspect_svn_to_git_push, observed_git_ref, observed_git_tree, GitPushInspect,
};
use crate::history_inspect::{
    clear_transient_history_block, enforce_durable_history_block, history_block_key,
    inspect_fetched_history, is_full_git_oid, persist_history_block, HistoryInspectReject,
};
use crate::identity::IdentityMapper;
use crate::models::AuditEntry;
use crate::path_projection::{project_git_to_svn_changeset, GitToSvnInputChange};
use crate::pending_frontier::{pending_frontier_is_merge_dag, GitReplayContinuation};
use crate::svn::client::SvnClient;
use crate::svn_commit::{
    hash_regular_file_tree, intended_paths_from_contents, observed_svn_tree_at_revision,
    operation_commit_message,
};

// ---------------------------------------------------------------------------
// Sync state machine
// ---------------------------------------------------------------------------

/// States of a sync cycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    Idle,
    Detecting,
    Applying,
    Committed,
    ConflictFound,
    QueuedForResolution,
    ResolutionApplied,
}

impl std::fmt::Display for SyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Detecting => write!(f, "detecting"),
            Self::Applying => write!(f, "applying"),
            Self::Committed => write!(f, "committed"),
            Self::ConflictFound => write!(f, "conflict_found"),
            Self::QueuedForResolution => write!(f, "queued_for_resolution"),
            Self::ResolutionApplied => write!(f, "resolution_applied"),
        }
    }
}

/// A single commit synced during a cycle (for rich notifications).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncedCommit {
    /// "svn_to_git" or "git_to_svn"
    pub direction: String,
    /// Author name (Git author or SVN committer)
    pub author: String,
    /// Full commit message (first line used as summary)
    pub message: String,
    /// Number of files changed
    pub files_changed: usize,
    /// Short identifier — SVN revision ("r1234") or Git SHA prefix ("abc1234")
    pub revision_id: String,
}

/// Statistics from a single sync cycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncStats {
    pub svn_to_git_count: usize,
    pub git_to_svn_count: usize,
    pub conflicts_detected: usize,
    pub conflicts_auto_resolved: usize,
    pub started_at: String,
    pub completed_at: Option<String>,
    /// Recent commit messages synced in this cycle (for notifications).
    pub recent_messages: Vec<String>,
    /// Rich commit details for Teams notifications (max 10).
    pub synced_commits: Vec<SyncedCommit>,
    /// True when Git replay continuation is incomplete after this cycle.
    pub git_replay_has_more: bool,
    /// Total pending Git commits on the admitted P→R frontier.
    pub git_pending_total: usize,
    /// True when the cycle deferred because Git continuation overlapped SVN work.
    pub deferred_mixed_pending: bool,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Inputs admitted for one team cycle. The Git replay cursor is P, not L or O.
struct TeamHistoryAdmission {
    checkpoint: String,
    remote_tip: String,
}

/// Git changes selected for one sync cycle, including continuation metadata.
struct GitFetchResult {
    replay_batch: Vec<GitChangeSet>,
    conflict_coverage: Vec<GitChangeSet>,
    conflict_coverage_skipped: usize,
    has_more: bool,
    reset_target: String,
    pending_total: usize,
    deferred_mixed_pending: bool,
    continuation_to_persist: Option<GitReplayContinuation>,
    clear_continuation: bool,
}

/// Durable merge-DAG continuation state for one replay batch apply.
struct GitReplayBatchContinuation {
    target: GitReplayContinuation,
    batch_shas: Vec<String>,
}

/// The bidirectional sync engine.
pub struct SyncEngine {
    config: AppConfig,
    db: Database,
    svn_client: std::sync::Mutex<SvnClient>,
    git_client: Arc<std::sync::Mutex<GitClient>>,
    identity_mapper: Arc<IdentityMapper>,
    /// Atomic flag preventing concurrent sync cycles.
    running: Arc<AtomicBool>,
    started_at: chrono::DateTime<Utc>,
    /// Optional repo ID for per-repo credential and watermark keys.
    repo_id: Option<String>,
    /// LFS threshold in bytes. Files larger than this are tracked via Git LFS.
    /// 0 means LFS is disabled.
    lfs_threshold_bytes: u64,
    /// Allowed path prefixes for Git-to-SVN sync. Empty means no restriction.
    allowed_paths: Vec<String>,
    /// Blocked path patterns for Git-to-SVN sync. Empty means no blocked patterns.
    blocked_patterns: Vec<String>,
    /// Fixture-only replay batch cap override (debug builds).
    #[cfg(debug_assertions)]
    pending_commit_cap_override: std::sync::Mutex<Option<usize>>,
    /// Fixture-only fault: truncate conflict coverage one commit short (debug builds).
    #[cfg(debug_assertions)]
    incomplete_conflict_coverage_test_fault: std::sync::Mutex<bool>,
}

impl SyncEngine {
    /// Create a new sync engine with all required dependencies.
    pub fn new(
        config: AppConfig,
        db: Database,
        svn_client: SvnClient,
        git_client: GitClient,
        identity_mapper: Arc<IdentityMapper>,
    ) -> Self {
        info!("initializing sync engine");
        Self {
            config,
            db,
            svn_client: std::sync::Mutex::new(svn_client),
            git_client: Arc::new(std::sync::Mutex::new(git_client)),
            identity_mapper,
            running: Arc::new(AtomicBool::new(false)),
            started_at: Utc::now(),
            repo_id: None,
            lfs_threshold_bytes: 0,
            allowed_paths: Vec::new(),
            blocked_patterns: Vec::new(),
            #[cfg(debug_assertions)]
            pending_commit_cap_override: std::sync::Mutex::new(None),
            #[cfg(debug_assertions)]
            incomplete_conflict_coverage_test_fault: std::sync::Mutex::new(false),
        }
    }

    /// Install a per-engine replay batch cap for fixture tests.
    #[cfg(debug_assertions)]
    pub fn set_pending_commit_cap_override(&self, cap: Option<usize>) {
        *self.pending_commit_cap_override.lock().unwrap() = cap;
    }

    /// Force incomplete conflict coverage on this engine only (fixture tests).
    #[cfg(debug_assertions)]
    pub fn set_incomplete_conflict_coverage_test_fault(&self, enabled: bool) {
        *self.incomplete_conflict_coverage_test_fault.lock().unwrap() = enabled;
    }

    #[cfg(debug_assertions)]
    fn incomplete_conflict_coverage_test_fault_enabled(&self) -> bool {
        *self.incomplete_conflict_coverage_test_fault.lock().unwrap()
    }

    fn replay_batch_cap(&self) -> Option<usize> {
        #[cfg(debug_assertions)]
        {
            return *self.pending_commit_cap_override.lock().unwrap();
        }
        #[cfg(not(debug_assertions))]
        {
            None
        }
    }

    /// Set the repository ID for per-repo credential and watermark keys.
    pub fn set_repo_id(&mut self, id: String) {
        self.repo_id = Some(id);
    }

    /// Set the LFS threshold in bytes. Files larger than this will be
    /// tracked via Git LFS during SVN-to-Git sync.
    pub fn set_lfs_threshold_bytes(&mut self, threshold: u64) {
        self.lfs_threshold_bytes = threshold;
    }

    /// Set path validation rules for Git-to-SVN sync.
    pub fn set_path_rules(&mut self, allowed: Vec<String>, blocked: Vec<String>) {
        self.allowed_paths = allowed;
        self.blocked_patterns = blocked;
    }

    /// Return the kv_state key for the last SVN revision watermark.
    /// Uses per-repo key if repo_id is set, otherwise global key.
    fn svn_rev_key(&self) -> String {
        match &self.repo_id {
            Some(rid) if !rid.is_empty() => format!("last_svn_rev_{}", rid),
            _ => "last_svn_rev".to_string(),
        }
    }

    /// Return the kv_state key for the last Git SHA watermark.
    /// Uses per-repo key if repo_id is set, otherwise global key.
    fn git_sha_key(&self) -> String {
        match &self.repo_id {
            Some(rid) if !rid.is_empty() => format!("last_git_sha_{}", rid),
            _ => "last_git_hash".to_string(),
        }
    }

    /// Return the effective repo_id if set and non-empty, for repo-table watermark operations.
    fn effective_repo_id(&self) -> Option<&str> {
        self.repo_id.as_deref().filter(|id| !id.is_empty())
    }

    /// Persist sync lifecycle state. Managed repos update only their own
    /// `repositories.sync_status`; legacy single-repo callers without a repo id
    /// continue to use the global `kv_state.sync_state` key.
    fn persist_sync_state(&self, state: &str) -> Result<(), SyncError> {
        if let Some(rid) = self.effective_repo_id() {
            self.db
                .update_repo_sync_status(rid, state)
                .map_err(SyncError::DatabaseError)?;
        } else {
            self.db
                .set_state("sync_state", state)
                .map_err(SyncError::DatabaseError)?;
        }
        Ok(())
    }

    /// Return a reference to the database.
    pub fn db(&self) -> &Database {
        &self.db
    }

    /// Return a reference to the configuration.
    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    /// Return a reference to the identity mapper.
    pub fn identity_mapper(&self) -> &IdentityMapper {
        &self.identity_mapper
    }

    /// Check if a sync cycle is currently running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    // -----------------------------------------------------------------------
    // Main entry point
    // -----------------------------------------------------------------------

    /// Execute one full sync cycle.
    ///
    /// Returns statistics about what was synced, or an error if something
    /// went wrong. Conflicts that can be auto-merged are handled inline;
    /// conflicts that require manual resolution are recorded in the database
    /// and the cycle still returns `Ok` (with the conflict count in stats).
    ///
    /// The sync lock is released via a drop guard so it is freed even if
    /// the cycle panics.
    pub async fn run_sync_cycle(&self) -> Result<SyncStats, SyncError> {
        // Acquire the sync lock.
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(SyncError::AlreadyRunning {
                started_at: self.started_at.to_rfc3339(),
            });
        }

        // RAII guard that clears the running flag on drop (even on panic).
        let _guard = SyncLockGuard(self.running.clone());

        // Hot-reload credentials from DB (changed via Setup Wizard).
        self.reload_credentials();

        let mut stats = SyncStats {
            started_at: Utc::now().to_rfc3339(),
            ..Default::default()
        };

        // Store the sync state (per-repo for managed pairs, global otherwise).
        let _ = self.persist_sync_state("detecting");

        let result = self.do_sync_cycle(&mut stats).await;

        // Record completion
        let (final_state, details) = match &result {
            Ok(()) => (
                "idle",
                format!(
                    "svn->git: {}, git->svn: {}, conflicts: {}",
                    stats.svn_to_git_count, stats.git_to_svn_count, stats.conflicts_detected
                ),
            ),
            Err(SyncError::HistoryBlocked { reason, detail, .. })
                if reason == "import_baseline_pending" =>
            {
                ("initializing", format!("awaiting import: {}", detail))
            }
            Err(e @ SyncError::HistoryBlocked { .. })
            | Err(e @ SyncError::SvnCommitHeld { .. })
            | Err(e @ SyncError::GitPushHeld { .. })
            | Err(e @ SyncError::CycleMappingHeld { .. }) => {
                ("reconciliation_required", format!("sync blocked: {}", e))
            }
            Err(e) => ("error", format!("sync failed: {}", e)),
        };

        let _ = self.persist_sync_state(final_state);
        let _ = self.db.set_state("last_sync_at", &Utc::now().to_rfc3339());
        stats.completed_at = Some(Utc::now().to_rfc3339());

        // Update per-repo error count
        if let Some(rid) = self.effective_repo_id() {
            let skip_error_count = matches!(
                &result,
                Err(SyncError::HistoryBlocked { reason, .. })
                    if reason == "import_baseline_pending"
            );
            if result.is_err() && !skip_error_count {
                let _ = self.db.increment_repo_error_count(rid);
            }
        }

        // Audit log
        let audit = if result.is_ok() {
            AuditEntry::success("sync_cycle", &details)
        } else {
            AuditEntry::failure("sync_cycle", &details)
        };
        let _ = self.db.insert_audit_entry(&audit);

        if result.is_ok() {
            let _ = clear_transient_history_block(&self.db, &self.history_block_key());
        }

        // Lock is released by _guard drop (happens here at scope end).
        result.map(|()| stats)
    }

    /// Get a status summary.
    pub fn get_status(&self) -> Result<crate::models::SyncStatus, SyncError> {
        // Use the consolidated summary query to reduce mutex acquisitions
        // (1 query instead of 8+ separate queries).
        let summary = self
            .db
            .get_status_summary(self.repo_id.as_deref())
            .map_err(SyncError::DatabaseError)?;

        let last_sync_at = summary.last_sync_at.and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        });

        // SVN rev and git hash need per-repo key lookups not in the summary.
        // Managed engines scope both cursors to the repo; legacy callers keep
        // the global fallback chain.
        let last_svn_rev = if let Some(rid) = self.effective_repo_id() {
            let scoped = self
                .db
                .get_state(&self.svn_rev_key())
                .map_err(SyncError::DatabaseError)?
                .and_then(|s| s.parse::<i64>().ok());
            if scoped.is_some() {
                scoped
            } else {
                self.db
                    .get_repository(rid)
                    .map_err(SyncError::DatabaseError)?
                    .and_then(|repo| {
                        if repo.last_svn_rev > 0 {
                            Some(repo.last_svn_rev)
                        } else {
                            None
                        }
                    })
            }
        } else {
            match self
                .db
                .get_state(&self.svn_rev_key())
                .map_err(SyncError::DatabaseError)?
            {
                Some(s) => s.parse::<i64>().ok(),
                None => self
                    .db
                    .get_last_svn_revision()
                    .map_err(SyncError::DatabaseError)?,
            }
        };
        let last_git_hash = if let Some(rid) = self.effective_repo_id() {
            let column = self
                .db
                .get_repository(rid)
                .map_err(SyncError::DatabaseError)?
                .and_then(|repo| {
                    if repo.last_git_sha.is_empty() {
                        None
                    } else {
                        Some(repo.last_git_sha)
                    }
                });
            if column.is_some() {
                column
            } else {
                self.db
                    .get_state(&self.git_sha_key())
                    .map_err(SyncError::DatabaseError)?
                    .filter(|s| !s.is_empty())
            }
        } else {
            crate::sync_status::resolve_unscoped_last_git_hash(&self.db)
                .map_err(SyncError::DatabaseError)?
        };
        let last_error_at = self.db.last_error_at().map_err(SyncError::DatabaseError)?;

        let uptime = (Utc::now() - self.started_at).num_seconds().max(0) as u64;

        let state_str = if let Some(rid) = self.effective_repo_id() {
            self.db
                .get_repository(rid)
                .map_err(SyncError::DatabaseError)?
                .map(|repo| repo.sync_status)
                .unwrap_or(summary.sync_state)
        } else {
            summary.sync_state
        };

        Ok(crate::models::SyncStatus {
            state: crate::models::SyncState::from_str_val(&state_str),
            last_sync_at,
            last_svn_revision: last_svn_rev,
            last_git_hash,
            total_syncs: summary.total_syncs,
            total_conflicts: summary.total_conflicts,
            active_conflicts: summary.active_conflicts,
            total_errors: summary.recent_errors,
            last_error_at,
            uptime_secs: uptime,
        })
    }

    // -----------------------------------------------------------------------
    // Inner sync cycle logic
    // -----------------------------------------------------------------------

    fn history_block_key(&self) -> String {
        history_block_key(self.effective_repo_id())
    }

    fn record_history_block(
        &self,
        reason: &str,
        detail: &str,
        p: Option<&str>,
        o: Option<&str>,
        r: Option<&str>,
        l: Option<&str>,
    ) -> SyncError {
        let reject = HistoryInspectReject {
            reason: reason.to_string(),
            detail: detail.to_string(),
            o: o.map(str::to_string),
            r: r.map(str::to_string),
            l: l.map(str::to_string),
        };
        match persist_history_block(
            &self.db,
            &self.history_block_key(),
            self.effective_repo_id(),
            &reject,
            p,
        ) {
            Ok(()) => SyncError::HistoryBlocked {
                reason: reason.to_string(),
                detail: detail.to_string(),
            },
            Err(error) => SyncError::DatabaseError(error),
        }
    }

    /// Read a repository-owned legacy Git cursor without borrowing another
    /// pair's global maximum. A missing or conflicting cursor is not a new
    /// baseline. The schema transition remains #63.
    fn team_git_checkpoint(&self) -> Result<Option<String>, SyncError> {
        if let Some(rid) = self.effective_repo_id() {
            let column: Option<String> = {
                let conn = self.db.conn();
                conn.query_row(
                    "SELECT last_git_sha FROM repositories WHERE id = ?1",
                    [rid],
                    |row| row.get(0),
                )
                .optional()
                .map_err(crate::errors::DatabaseError::from)?
            };
            let column = column.filter(|value| !value.is_empty());
            let kv = self
                .db
                .get_state(&format!("last_git_sha_{}", rid))
                .map_err(SyncError::DatabaseError)?
                .filter(|value| !value.is_empty());
            // A no-target decision is authority for the handled frontier even
            // when the old cursor copies happen to agree. Check every present
            // copy before choosing between equal, split, or KV-only shapes.
            if let Some(ref sha) = column {
                self.checked_no_target_receipt(rid, sha)?;
            }
            let kv_no_target = if let Some(ref sha) = kv {
                self.checked_no_target_receipt(rid, sha)?
            } else {
                false
            };
            if column.is_some() && kv.is_some() && column != kv {
                let (emitted, applied_outbound, svn_origin) = {
                    let conn = self.db.conn();
                    let emitted: i64 = conn.query_row(
                        "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1 AND git_sha = ?2 AND direction = 'svn_to_git' AND status = 'applied'",
                        rusqlite::params![rid, column.as_deref()], |row| row.get(0),
                    ).map_err(crate::errors::DatabaseError::from)?;
                    let applied_outbound: i64 = conn.query_row(
                        "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1 AND git_sha = ?2 AND direction = 'git_to_svn' AND status = 'applied'",
                        rusqlite::params![rid, kv.as_deref()], |row| row.get(0),
                    ).map_err(crate::errors::DatabaseError::from)?;
                    let svn_origin: i64 = conn.query_row(
                        "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1 AND git_sha = ?2 AND direction = 'svn_to_git' AND status = 'applied' AND svn_rev <= (SELECT last_svn_rev FROM repositories WHERE id = ?1)",
                        rusqlite::params![rid, kv.as_deref()], |row| row.get(0),
                    ).map_err(crate::errors::DatabaseError::from)?;
                    (emitted, applied_outbound, svn_origin)
                };
                // The column is also used by the old SVN->Git writer to hold
                // its emitted tip. That is not the inbound handled cursor P.
                // Keep the older handled cursor when its outcome (applied
                // outbound, imported SVN origin, or no-target receipt) and
                // ancestry to the emitted tip are both proved. Pending Git
                // ancestors remain in the replay range.
                let old_import_projection =
                    self.allowed_paths.is_empty() && self.blocked_patterns.is_empty();
                if emitted > 0
                    && (applied_outbound > 0
                        || (old_import_projection && svn_origin > 0)
                        || kv_no_target)
                    && is_full_git_oid(column.as_deref().unwrap())
                    && is_full_git_oid(kv.as_deref().unwrap())
                {
                    let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
                    let ancestry = Command::new("git")
                        .args([
                            "merge-base",
                            "--is-ancestor",
                            kv.as_deref().unwrap(),
                            column.as_deref().unwrap(),
                        ])
                        .current_dir(git.repo_path())
                        .output();
                    match ancestry {
                        Ok(output) if output.status.code() == Some(0) => return Ok(kv),
                        Ok(output) if output.status.code() == Some(1) => (),
                        _ => {
                            return Err(self.record_history_block(
                                "ancestry_command_failed",
                                "repository cursor copies could not be reconciled",
                                kv.as_deref(),
                                None,
                                None,
                                column.as_deref(),
                            ))
                        }
                    }
                }
                return Err(self.record_history_block(
                    "ambiguous_checkpoint",
                    "repository column and repository-scoped legacy cursor disagree",
                    column.as_deref(),
                    None,
                    None,
                    None,
                ));
            }
            if let Some(ref emitted_tip) = column {
                if kv.is_none() {
                    let (emitted, last_handled): (i64, Option<String>) = {
                        let conn = self.db.conn();
                        let emitted = conn.query_row(
                            "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1 AND git_sha = ?2 AND direction = 'svn_to_git' AND status = 'applied'",
                            rusqlite::params![rid, emitted_tip], |row| row.get(0),
                        ).map_err(crate::errors::DatabaseError::from)?;
                        let last_handled = conn.query_row(
                            "SELECT git_sha FROM sync_records WHERE repo_id = ?1 AND direction = 'git_to_svn' AND status = 'applied' ORDER BY rowid DESC LIMIT 1",
                            [rid], |row| row.get(0),
                        ).optional().map_err(crate::errors::DatabaseError::from)?;
                        (emitted, last_handled)
                    };
                    if emitted > 0 {
                        // A retained first row is not a baseline: maintenance
                        // may already have deleted earlier applied rows.
                        let baseline = self
                            .db
                            .get_state(&format!("handled_git_baseline_{}", rid))
                            .map_err(SyncError::DatabaseError)?
                            .and_then(|value| {
                                serde_json::from_str::<serde_json::Value>(&value).ok()
                            });
                        let baseline_sha = baseline
                            .as_ref()
                            .and_then(|record| record["git_sha"].as_str());
                        let baseline_revision = baseline
                            .as_ref()
                            .and_then(|record| record["svn_rev"].as_i64());
                        let baseline_valid = baseline.as_ref().is_some_and(|record| {
                            record["version"] == 1
                                && record["repo_id"] == rid
                                && record["projection"] == self.no_target_projection()
                        }) && baseline_sha.is_some_and(is_full_git_oid)
                            && baseline_revision.is_some_and(|rev| rev > 0);
                        if !baseline_valid {
                            return Err(self.record_history_block(
                                "ambiguous_checkpoint",
                                "missing durable handled Git baseline for absent repository cursor",
                                None,
                                None,
                                None,
                                Some(emitted_tip),
                            ));
                        }
                        let baseline_sha = baseline_sha.unwrap();
                        let baseline_revision = baseline_revision.unwrap();
                        let baseline_mapping: i64 = {
                            let conn = self.db.conn();
                            conn.query_row(
                                "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1 AND svn_rev = ?2 AND git_sha = ?3 AND direction = 'svn_to_git' AND status = 'applied'",
                                rusqlite::params![rid, baseline_revision, baseline_sha], |row| row.get(0),
                            ).map_err(crate::errors::DatabaseError::from)?
                        };
                        if baseline_mapping == 0 {
                            return Err(self.record_history_block(
                                "ambiguous_checkpoint",
                                "durable baseline mapping is missing",
                                Some(baseline_sha),
                                None,
                                None,
                                Some(emitted_tip),
                            ));
                        }
                        let handled = last_handled.unwrap_or_else(|| baseline_sha.to_string());
                        if !is_full_git_oid(&handled) || !is_full_git_oid(emitted_tip) {
                            return Err(self.record_history_block(
                                "ambiguous_checkpoint",
                                "mapped legacy cursor is malformed",
                                Some(&handled),
                                None,
                                None,
                                Some(emitted_tip),
                            ));
                        }
                        let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
                        let baseline_ancestry = Command::new("git")
                            .args(["merge-base", "--is-ancestor", baseline_sha, &handled])
                            .current_dir(git.repo_path())
                            .output();
                        if !matches!(baseline_ancestry, Ok(ref output) if output.status.code() == Some(0))
                        {
                            return Err(self.record_history_block(
                                "ambiguous_checkpoint",
                                "handled Git row is not descended from verified baseline",
                                Some(&handled),
                                None,
                                None,
                                Some(emitted_tip),
                            ));
                        }
                        let ancestry = Command::new("git")
                            .args(["merge-base", "--is-ancestor", &handled, emitted_tip])
                            .current_dir(git.repo_path())
                            .output();
                        match ancestry {
                            Ok(output) if output.status.code() == Some(0) => {
                                return Ok(Some(handled))
                            }
                            Ok(output) if output.status.code() == Some(1) => {
                                return Err(self.record_history_block(
                                    "ambiguous_checkpoint",
                                    "mapped legacy cursor is unrelated to emitted tip",
                                    Some(&handled),
                                    None,
                                    None,
                                    Some(emitted_tip),
                                ))
                            }
                            _ => {
                                return Err(self.record_history_block(
                                    "ancestry_command_failed",
                                    "mapped legacy cursor ancestry could not be established",
                                    Some(&handled),
                                    None,
                                    None,
                                    Some(emitted_tip),
                                ))
                            }
                        }
                    }
                }
            }
            return Ok(column.or(kv));
        }

        // Older single-repository callers use a global key. It is never
        // borrowed when multiple repository rows can own that key.
        let registered: i64 = {
            let conn = self.db.conn();
            conn.query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))
                .map_err(crate::errors::DatabaseError::from)?
        };
        if registered > 1 {
            return Err(self.record_history_block(
                "ambiguous_checkpoint",
                "global legacy cursor has multiple possible repository owners",
                None,
                None,
                None,
                None,
            ));
        }
        self.db
            .get_state("last_git_hash")
            .map_err(SyncError::DatabaseError)
            .map(|value| value.filter(|sha| !sha.is_empty()))
    }

    fn no_target_projection(&self) -> String {
        serde_json::json!({"allowed_paths": self.allowed_paths, "blocked_patterns": self.blocked_patterns}).to_string()
    }

    fn checked_no_target_receipt(&self, rid: &str, sha: &str) -> Result<bool, SyncError> {
        let key = format!("handled_git_no_target_{}_{}", rid, sha);
        let Some(raw) = self.db.get_state(&key).map_err(SyncError::DatabaseError)? else {
            return Ok(false);
        };
        let receipt = serde_json::from_str::<serde_json::Value>(&raw).ok();
        let Some(record) = receipt else {
            return Err(self.record_history_block(
                "unverified_no_target_receipt",
                "no-target receipt is malformed; reconcile before replay",
                Some(sha),
                None,
                None,
                None,
            ));
        };
        let projection = self.no_target_projection();
        match verify_no_target_receipt(&record, rid, sha, &projection) {
            NoTargetReceiptVerdict::Accepted => Ok(true),
            NoTargetReceiptVerdict::RepoOrShaMismatch => Err(self.record_history_block(
                "unverified_no_target_receipt",
                "no-target receipt does not identify this repository and Git commit",
                Some(sha),
                None,
                None,
                None,
            )),
            NoTargetReceiptVerdict::ProjectionMismatch => {
                let reason = if record["outcome"] == "empty_commit" {
                    // Preserve the accepted legacy empty-commit rejection shape.
                    "ambiguous_checkpoint"
                } else {
                    "receipt_policy_changed"
                };
                Err(self.record_history_block(
                    reason,
                    "no-target decision belongs to a different path policy; reconcile before writes",
                    Some(sha),
                    None,
                    None,
                    None,
                ))
            }
            NoTargetReceiptVerdict::UnverifiedOutcome => Err(self.record_history_block(
                "unverified_no_target_receipt",
                "no-target receipt lacks verified outcome evidence",
                Some(sha),
                None,
                None,
                None,
            )),
        }
    }

    /// A clean working-copy status is not proof that a nonempty Git delta is
    /// represented by SVN. Compare the exact selected paths with an exported,
    /// pinned target revision before creating durable handled evidence.
    async fn verify_no_svn_delta(
        &self,
        svn: &SvnClient,
        files: &[(String, String, Option<Vec<u8>>)],
        sha: &str,
    ) -> Result<serde_json::Value, SyncError> {
        #[cfg(debug_assertions)]
        self.test_outbound_pause("REPOSYNC_TEST_BEFORE_NO_TARGET_VERIFY", sha)
            .await?;
        let before = svn.info().await.map_err(SyncError::SvnError)?;
        let snapshot = tempfile::tempdir()
            .map_err(|error| SyncError::SvnError(crate::errors::SvnError::IoError(error)))?;
        let target = snapshot.path().join("target");
        svn.export("", before.latest_rev, &target)
            .await
            .map_err(SyncError::SvnError)?;
        let mut paths = serde_json::Map::new();
        for (action, path, content) in files {
            let relative = std::path::Path::new(path);
            if relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(self.record_history_block(
                    "unverified_no_target",
                    "Git delta contains a non-relative target path",
                    Some(sha),
                    None,
                    None,
                    None,
                ));
            }
            let actual = target.join(relative);
            // An SVN special file may export as a symlink. Check every existing
            // component before any read or absence decision so the verifier
            // cannot follow an exported link outside the pinned snapshot.
            let mut checked = target.clone();
            for component in relative.components() {
                checked.push(component.as_os_str());
                match std::fs::symlink_metadata(&checked) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        return Err(self.record_history_block(
                            "unverified_no_target",
                            "pinned SVN target contains unsupported symlink semantics",
                            Some(sha),
                            None,
                            None,
                            None,
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Err(_) => {
                        return Err(self.record_history_block(
                            "unverified_no_target",
                            "pinned SVN target metadata is unreadable",
                            Some(sha),
                            None,
                            None,
                            None,
                        ));
                    }
                }
            }
            if action == "D" {
                if std::fs::symlink_metadata(&actual).is_ok() {
                    return Err(self.record_history_block(
                        "unverified_no_target",
                        "deleted Git path still exists at pinned SVN target",
                        Some(sha),
                        None,
                        None,
                        None,
                    ));
                }
                paths.insert(path.clone(), serde_json::Value::Null);
            } else {
                let Some(expected) = content else {
                    return Err(self.record_history_block(
                        "unverified_no_target",
                        "non-delete Git content is missing",
                        Some(sha),
                        None,
                        None,
                        None,
                    ));
                };
                if !std::fs::symlink_metadata(&actual)
                    .map_err(|_| {
                        self.record_history_block(
                            "unverified_no_target",
                            "pinned target type is unreadable",
                            Some(sha),
                            None,
                            None,
                            None,
                        )
                    })?
                    .file_type()
                    .is_file()
                {
                    return Err(self.record_history_block(
                        "unverified_no_target",
                        "pinned SVN target is not a regular file",
                        Some(sha),
                        None,
                        None,
                        None,
                    ));
                }
                let actual_bytes = std::fs::read(&actual).map_err(|_| {
                    self.record_history_block(
                        "unverified_no_target",
                        "Git path is absent or unreadable at pinned SVN target",
                        Some(sha),
                        None,
                        None,
                        None,
                    )
                })?;
                if actual_bytes != *expected {
                    return Err(self.record_history_block(
                        "unverified_no_target",
                        "pinned SVN target content differs from Git delta",
                        Some(sha),
                        None,
                        None,
                        None,
                    ));
                }
                let props = svn
                    .file_properties_at_rev(path, before.latest_rev)
                    .await
                    .map_err(SyncError::SvnError)?;
                if props.contains("<property ") || props.contains("<property>") {
                    return Err(self.record_history_block(
                        "unverified_no_target",
                        "pinned SVN target has file properties outside the regular-byte projection",
                        Some(sha),
                        None,
                        None,
                        None,
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if std::fs::metadata(&actual)
                        .map_err(|_| {
                            self.record_history_block(
                                "unverified_no_target",
                                "pinned target metadata is unreadable",
                                Some(sha),
                                None,
                                None,
                                None,
                            )
                        })?
                        .permissions()
                        .mode()
                        & 0o111
                        != 0
                    {
                        return Err(self.record_history_block("unverified_no_target",
                            "pinned SVN target has executable semantics absent from Git regular file",
                            Some(sha), None, None, None));
                    }
                }
                #[cfg(not(unix))]
                return Err(self.record_history_block(
                    "unverified_no_target",
                    "SVN executable semantics are unqualified on this platform",
                    Some(sha),
                    None,
                    None,
                    None,
                ));
                paths.insert(
                    path.clone(),
                    serde_json::json!({
                        "sha256": hex::encode(Sha256::digest(expected)),
                        "git_mode": 33188, "svn_executable": false,
                    }),
                );
            }
        }
        let after = svn.info().await.map_err(SyncError::SvnError)?;
        if before.uuid != after.uuid
            || before.url != after.url
            || before.latest_rev != after.latest_rev
        {
            return Err(self.record_history_block(
                "target_changed_during_verification",
                "SVN target changed while no-delta proof was checked",
                Some(sha),
                None,
                None,
                None,
            ));
        }
        Ok(serde_json::json!({
            "svn_revision": before.latest_rev, "svn_uuid": before.uuid,
            "svn_url": before.url, "paths": paths,
            "semantic_projection": "regular_file_bytes_no_properties_v1",
        }))
    }

    #[cfg(debug_assertions)]
    async fn test_outbound_pause(&self, key: &str, sha: &str) -> Result<(), SyncError> {
        let bridge = self
            .git_client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .repo_path()
            .to_string_lossy()
            .to_string();
        let scoped_key = format!(
            "{key}_{sha}_{}",
            hex::encode(Sha256::digest(bridge.as_bytes()))
        );
        let Some(dir) = std::env::var(&scoped_key).ok() else {
            return Ok(());
        };
        let dir = std::path::Path::new(&dir);
        std::fs::write(dir.join("ready"), b"")
            .map_err(|error| SyncError::GitError(crate::errors::GitError::IoError(error)))?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while !dir.join("release").exists() {
            if tokio::time::Instant::now() >= deadline {
                return Err(SyncError::GitError(crate::errors::GitError::IoError(
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "test outbound pause timed out",
                    ),
                )));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Ok(())
    }

    fn materialize_git_baseline(&self, sha: &str, revision: i64) -> Result<(), SyncError> {
        let Some(rid) = self.effective_repo_id() else {
            return Ok(());
        };
        // The pinned old import route used the unfiltered projection. Old
        // applied rows do not encode a policy, so a changed projection cannot
        // inherit an import baseline without a separate qualification.
        if !self.allowed_paths.is_empty() || !self.blocked_patterns.is_empty() {
            return Ok(());
        }
        if !is_full_git_oid(sha) || revision <= 0 {
            return Ok(());
        }
        let key = format!("handled_git_baseline_{}", rid);
        if self
            .db
            .get_state(&key)
            .map_err(SyncError::DatabaseError)?
            .is_some()
        {
            return Ok(());
        }
        let mapped: i64 = {
            let conn = self.db.conn();
            conn.query_row(
                "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1 AND svn_rev = ?2 AND git_sha = ?3 AND direction = 'svn_to_git' AND status = 'applied'",
                rusqlite::params![rid, revision, sha], |row| row.get(0),
            ).map_err(crate::errors::DatabaseError::from)?
        };
        if mapped != 1 {
            return Ok(());
        }
        let value = serde_json::json!({
            "version": 1, "repo_id": rid, "git_sha": sha,
            "svn_rev": revision, "projection": self.no_target_projection(),
        });
        self.db
            .set_state(&key, &value.to_string())
            .map_err(SyncError::DatabaseError)
    }

    /// Fetch the exact configured branch into an inspection ref and admit
    /// only a complete, linear path from the durably handled P to fresh R.
    /// This runs before SVN legacy adoption or any checkout reset.
    fn inspect_team_history(&self) -> Result<TeamHistoryAdmission, SyncError> {
        enforce_durable_history_block(&self.db, &self.history_block_key())?;
        let p = self.team_git_checkpoint()?;
        let git = self
            .git_client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match inspect_fetched_history(
            git.repo_path(),
            &self.config.github.default_branch,
            p.clone(),
        ) {
            Ok(admission) => Ok(TeamHistoryAdmission {
                checkpoint: admission.checkpoint,
                remote_tip: admission.remote_tip,
            }),
            Err(reject) => Err(self.record_history_block(
                &reject.reason,
                &reject.detail,
                p.as_deref(),
                reject.o.as_deref(),
                reject.r.as_deref(),
                reject.l.as_deref(),
            )),
        }
    }

    async fn do_sync_cycle(&self, stats: &mut SyncStats) -> Result<(), SyncError> {
        if let Some(rid) = self.effective_repo_id() {
            if let Some(op) = self
                .db
                .active_team_cycle_mapping_operation(rid)
                .map_err(SyncError::DatabaseError)?
            {
                if let Some(error) = self.blocking_cycle_mapping_hold(&op) {
                    return Err(error);
                }
            }
            if let Some(op) = self
                .db
                .active_svn_commit_operation(rid)
                .map_err(SyncError::DatabaseError)?
            {
                if let Some(error) = self.blocking_svn_commit_hold(&op) {
                    return Err(error);
                }
            }
            if let Some(op) = self
                .db
                .active_git_push_operation(rid)
                .map_err(SyncError::DatabaseError)?
            {
                if let Some(error) = self.blocking_git_push_hold(&op) {
                    return Err(error);
                }
            }
        }
        // Authorized SVN→Git resume must publish the recorded local commit
        // before history inspect. The unpushed bridge tip is the intended
        // SHA; inspect would otherwise classify it as unpublished_local_history
        // and never issue that one push.
        self.resume_authorized_svn_to_git_push(stats)?;
        // SVN inspection may adopt a legacy checkpoint. Admit Git history
        // before that call, not merely before the later destructive reset.
        let admission = tokio::task::block_in_place(|| self.inspect_team_history())?;

        // On a pinned old import, both legacy copies point to the imported
        // SVN mapping. Materialize that verified baseline before diagnostics
        // can expire; never derive it from a later retained row.
        if let Some(rid) = self.effective_repo_id() {
            let (revision, column) = self
                .db
                .get_repo_watermark(rid)
                .map_err(SyncError::DatabaseError)?;
            let kv = self
                .db
                .get_state(&format!("last_git_sha_{}", rid))
                .map_err(SyncError::DatabaseError)?;
            if column == admission.checkpoint && kv.as_deref() == Some(column.as_str()) {
                self.materialize_git_baseline(&column, revision)?;
            }
        }

        // 1. Fetch changes from both sides.
        let svn_changes = self.fetch_svn_changes().await?;
        let git_fetch = self.fetch_git_changes(&admission, &svn_changes).await?;

        stats.git_replay_has_more = git_fetch.has_more;
        stats.git_pending_total = git_fetch.pending_total;
        stats.deferred_mixed_pending = git_fetch.deferred_mixed_pending;

        if git_fetch.deferred_mixed_pending {
            info!(
                pending_git_total = git_fetch.pending_total,
                svn_pending = svn_changes.len(),
                reset_target = %git_fetch.reset_target,
                "Git replay continuation incomplete with pending SVN work; deferring cycle with no mutation"
            );
            return Ok(());
        }

        let batch_continuation =
            git_fetch
                .continuation_to_persist
                .as_ref()
                .map(|target| GitReplayBatchContinuation {
                    target: target.clone(),
                    batch_shas: git_fetch
                        .replay_batch
                        .iter()
                        .map(|change| change.sha.clone())
                        .collect(),
                });
        if let Some(continuation) = batch_continuation.as_ref() {
            self.persist_pre_batch_git_replay_continuation(continuation)?;
        }

        // 2. Detect conflicts against the full admitted P→R path, not only the replay batch.
        self.ensure_conflict_coverage_before_detection(
            git_fetch.has_more,
            git_fetch
                .pending_total
                .saturating_sub(git_fetch.conflict_coverage_skipped),
            git_fetch.conflict_coverage.len(),
        )?;
        let conflicts =
            self.detect_conflicts_internal(&svn_changes, &git_fetch.conflict_coverage)?;
        stats.conflicts_detected = conflicts.len();

        if !conflicts.is_empty() {
            info!(count = conflicts.len(), "conflicts detected");
            let _ = self.persist_sync_state("conflict_found");

            for conflict in &conflicts {
                if self.config.sync.auto_merge && self.try_auto_merge(conflict) {
                    stats.conflicts_auto_resolved += 1;
                } else {
                    // Persist unresolved conflict
                    let mut db_conflict = crate::models::Conflict::new(conflict.file_path.clone());
                    db_conflict.conflict_type = conflict.conflict_type.to_string();
                    db_conflict.svn_content = conflict.svn_content.clone();
                    db_conflict.git_content = conflict.git_content.clone();
                    db_conflict.base_content = conflict.base_content.clone();
                    db_conflict.svn_revision = conflict.svn_rev;
                    db_conflict.git_hash = conflict.git_sha.clone();
                    db_conflict.repo_id = self.repo_id.clone();
                    let _ = self.db.insert_conflict(&db_conflict);
                }
            }
        }

        // 3. Apply SVN -> Git.
        let _ = self.persist_sync_state("applying");
        self.sync_svn_to_git(&svn_changes, &mut stats.svn_to_git_count)
            .await?;

        // The exact remote Git tip had no pending commits before this SVN
        // publication, so its newly mapped tip is a handled frontier.
        if git_fetch.replay_batch.is_empty()
            && !git_fetch.has_more
            && admission.checkpoint == admission.remote_tip
            && stats.svn_to_git_count > 0
        {
            if let Some(rid) = self.effective_repo_id() {
                let (revision, emitted) = self
                    .db
                    .get_repo_watermark(rid)
                    .map_err(SyncError::DatabaseError)?;
                self.materialize_git_baseline(&emitted, revision)?;
            }
        }

        // 4. Apply Git -> SVN.
        stats.git_to_svn_count = self
            .sync_git_to_svn(&git_fetch.replay_batch, batch_continuation.as_ref())
            .await?;

        // Collect commit details for notifications (max 10 commits)
        for change in &svn_changes {
            let first_line = change.message.lines().next().unwrap_or("").to_string();
            if !first_line.is_empty() && stats.recent_messages.len() < 5 {
                stats.recent_messages.push(first_line);
            }
            if stats.synced_commits.len() < 10 {
                stats.synced_commits.push(SyncedCommit {
                    direction: "svn_to_git".to_string(),
                    author: change.author.clone(),
                    message: change.message.clone(),
                    files_changed: change.changed_files.len(),
                    revision_id: format!("r{}", change.revision),
                });
            }
        }
        for change in &git_fetch.replay_batch {
            let first_line = change.message.lines().next().unwrap_or("").to_string();
            if !first_line.is_empty() && stats.recent_messages.len() < 5 {
                stats.recent_messages.push(first_line);
            }
            if stats.synced_commits.len() < 10 {
                stats.synced_commits.push(SyncedCommit {
                    direction: "git_to_svn".to_string(),
                    author: change.author_name.clone(),
                    message: change.message.clone(),
                    files_changed: change.changed_files.len(),
                    revision_id: if change.sha.len() >= 7 {
                        change.sha[..7].to_string()
                    } else {
                        change.sha.clone()
                    },
                });
            }
        }

        if git_fetch.clear_continuation {
            self.clear_git_replay_continuation()?;
        }

        info!(
            svn_to_git = stats.svn_to_git_count,
            git_to_svn = stats.git_to_svn_count,
            "sync cycle completed"
        );

        Ok(())
    }

    // -----------------------------------------------------------------------
    // SVN -> Git
    // -----------------------------------------------------------------------

    /// Apply SVN changes to the Git repository.
    ///
    /// For each SVN revision:
    /// 1. Get the unified diff from SVN.
    /// 2. Apply the diff to the Git working tree.
    /// 3. Commit with the mapped Git identity and a `[reposync]` marker.
    /// 4. Push to the remote.
    /// 5. Only then record the sync in the database.
    async fn sync_svn_to_git(
        &self,
        svn_changes: &[SvnChangeSet],
        applied: &mut usize,
    ) -> Result<(), SyncError> {
        let resume_svn_rev = if let Some(rid) = self.effective_repo_id() {
            match self
                .db
                .active_git_push_operation(rid)
                .map_err(SyncError::DatabaseError)?
            {
                Some(op) => {
                    if let Some(error) = self.blocking_git_push_hold(&op) {
                        return Err(error);
                    }
                    if op.state == GitPushOperationState::ReconciliationRequired
                        && op.resume_authorized
                    {
                        Some(op.source_svn_rev)
                    } else {
                        None
                    }
                }
                None => None,
            }
        } else {
            None
        };

        for change in svn_changes {
            if let Some(allowed) = resume_svn_rev {
                if change.revision != allowed {
                    debug!(
                        rev = change.revision,
                        allowed,
                        "skipping SVN revision while one held push is authorized to resume"
                    );
                    continue;
                }
            }
            if self.should_skip_incoming_svn_revision(change.revision, &change.message)? {
                continue;
            }

            let git_identity = self
                .identity_mapper
                .svn_to_git(&change.author)
                .map_err(SyncError::IdentityError)?;

            // 1. Get the SVN diff for this revision.
            let svn = self
                .svn_client
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            let diff = svn
                .diff_full(change.revision)
                .await
                .map_err(SyncError::SvnError)?;

            // Get the git repo path before locking, for apply_diff_to_path.
            let repo_path = {
                let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
                git.repo_path().to_path_buf()
            };

            // 2. Apply the diff to the Git working tree.
            // Try git apply first; fall back to export-based copy if the diff
            // is empty or in a format git cannot parse (e.g. SVN property-only
            // changes or initial adds).
            // When using standard layout, strip the trunk prefix from diff paths
            // so they match the git repository structure.
            let processed_diff = if self.config.svn.layout == SvnLayout::Standard {
                let tp = self.config.svn.trunk_path.trim_matches('/');
                if !tp.is_empty() {
                    diff.replace(&format!("a/{}/", tp), "a/")
                        .replace(&format!("b/{}/", tp), "b/")
                } else {
                    diff
                }
            } else {
                diff
            };
            // Convert SVN diff format to git-compatible format:
            // - SVN uses "(nonexistent)" for deleted files; git needs "/dev/null"
            // - SVN uses "(revision N)" for existing files; git needs actual paths
            // Without this, git apply treats deletions as "truncate to empty"
            // instead of removing the file.
            let git_diff = convert_svn_diff_to_git(&processed_diff);

            debug!(
                rev = change.revision,
                raw_diff_lines = processed_diff.lines().count(),
                converted_diff_preview = %git_diff.lines().take(6).collect::<Vec<_>>().join(" | "),
                "SVN diff converted for git apply"
            );

            let mut apply_error = None;
            let diff_applied = if !git_diff.trim().is_empty() {
                let result =
                    apply_diff_to_path_revision(&repo_path, &git_diff, Some(change.revision), None)
                        .await;
                if result.is_ok() {
                    // Verify: check that files were created at the correct paths
                    for cf in &change.changed_files {
                        let expected = repo_path.join(&cf.path);
                        if expected.exists() {
                            info!(path = %cf.path, "git apply created file at correct path");
                        } else {
                            warn!(path = %cf.path, "git apply succeeded but file NOT at expected path");
                        }
                    }
                } else if let Err(error) = &result {
                    apply_error = Some(error.to_string());
                    warn!(
                        rev = change.revision,
                        error = %error,
                        "git apply failed"
                    );
                }
                result.is_ok()
            } else {
                false
            };

            // SVN properties and empty directories have no Git file-content
            // delta. Prove that separately using SVN's content-only diff;
            // a failed nonempty patch may never be treated as filtered work.
            let no_target_content = if !diff_applied {
                svn.diff_content_only(change.revision)
                    .await
                    .map_err(SyncError::SvnError)?
                    .trim()
                    .is_empty()
            } else {
                false
            };
            if !diff_applied && no_target_content {
                info!(
                    rev = change.revision,
                    "recording SVN revision with no Git target content (metadata-only)"
                );
                if let Some(rid) = self.effective_repo_id() {
                    let (pre_svn_rev, pre_git_sha) =
                        self.cycle_mapping_pre_write_watermarks(rid)?;
                    let projection = self.no_target_projection();
                    let fingerprint = team_cycle_mapping_fingerprint(rid, &projection);
                    self.record_team_cycle_mapping(
                        TeamCycleMappingIntent {
                            repo_id: rid,
                            initiator_id: "team_worker",
                            request_id: &format!("svn-r{}", change.revision),
                            target_fingerprint: &fingerprint,
                            direction: TeamCycleMappingDirection::SvnToGit,
                            outcome: TeamCycleMappingOutcome::SvnNoTargetContent,
                            source_svn_rev: Some(change.revision),
                            source_git_sha: None,
                            pre_write_svn_rev: pre_svn_rev,
                            pre_write_git_sha: &pre_git_sha,
                            projection: &projection,
                            intended_target_proof: None,
                        },
                        None,
                    )?;
                } else {
                    let per_repo_key = self
                        .effective_repo_id()
                        .map(|rid| format!("last_svn_rev_{}", rid));
                    if let Some(ref key) = per_repo_key {
                        self.db
                            .set_state(key, &change.revision.to_string())
                            .map_err(SyncError::DatabaseError)?;
                    }
                    if let Some(ref rid) = self.repo_id {
                        self.db
                            .advance_svn_watermark(rid, change.revision)
                            .map_err(SyncError::DatabaseError)?;
                    }
                }
                self.db
                    .insert_audit_log_with_repo(AuditLogInput {
                        action: "svn_to_git_no_target",
                        direction: Some("svn_to_git"),
                        svn_rev: Some(change.revision),
                        git_sha: None,
                        author: Some(&change.author),
                        details: Some("No file-content delta under active SVN path"),
                        success: true,
                        repo_id: self.effective_repo_id(),
                    })
                    .map_err(SyncError::DatabaseError)?;
                continue;
            }

            if !diff_applied {
                // A nonempty revision that failed to apply remains pending.
                // Stop at this revision: continuing could acknowledge later
                // dependent work while leaving this tree change unapplied.
                warn!(
                    rev = change.revision,
                    message = %change.message,
                    files = change.changed_files.len(),
                    "git apply failed for SVN revision; stopping at durable frontier"
                );
                let _ = self.db.insert_audit_log_with_repo(AuditLogInput {
                    action: "svn_to_git_apply_failed",
                    direction: Some("svn_to_git"),
                    svn_rev: Some(change.revision),
                    git_sha: None,
                    author: Some(&change.author),
                    details: Some(&format!(
                        "Stopped at unapplied r{}: git apply failed. Message: {}",
                        change.revision,
                        change.message.lines().next().unwrap_or("")
                    )),
                    success: false,
                    repo_id: self.effective_repo_id(),
                });
                return Err(SyncError::GitError(crate::errors::GitError::ApplyFailed(
                    format!(
                        "SVN r{} could not be applied; later revisions remain pending: {}",
                        change.revision,
                        apply_error.unwrap_or_else(
                            || "nonempty SVN change produced no Git patch".to_string()
                        )
                    ),
                )));
            }

            // 2b. LFS enforcement: after applying changes, scan modified files
            // and ensure any above the LFS threshold are tracked via .gitattributes.
            if self.lfs_threshold_bytes > 0 {
                let changed_files: Vec<String> = change
                    .changed_files
                    .iter()
                    .filter_map(|cf| {
                        // Strip trunk prefix for standard layout
                        let path = if self.config.svn.layout == SvnLayout::Standard {
                            let tp = self.config.svn.trunk_path.trim_matches('/');
                            if !tp.is_empty() {
                                cf.path
                                    .strip_prefix(&format!("/{}/", tp))
                                    .or_else(|| cf.path.strip_prefix(&format!("{}/", tp)))
                                    .unwrap_or(&cf.path)
                            } else {
                                &cf.path
                            }
                        } else {
                            &cf.path
                        };
                        let clean = path.trim_start_matches('/');
                        if clean.is_empty() {
                            None
                        } else {
                            Some(clean.to_string())
                        }
                    })
                    .collect();

                for rel_path in &changed_files {
                    let full_path = repo_path.join(rel_path);
                    if let Ok(meta) = std::fs::metadata(&full_path) {
                        if meta.len() > self.lfs_threshold_bytes {
                            let pattern = crate::lfs::pattern_for_path(rel_path);
                            if let Err(e) = crate::lfs::ensure_lfs_tracked(&repo_path, &pattern) {
                                warn!(
                                    path = rel_path.as_str(),
                                    size = meta.len(),
                                    threshold = self.lfs_threshold_bytes,
                                    error = %e,
                                    "failed to update .gitattributes for LFS tracking"
                                );
                            } else {
                                info!(
                                    path = rel_path.as_str(),
                                    size = meta.len(),
                                    threshold = self.lfs_threshold_bytes,
                                    pattern = pattern.as_str(),
                                    "LFS: large file detected during sync, .gitattributes updated"
                                );
                            }
                        }
                    }
                }
            }

            // A revision's changed paths are a delta, not a full-tree keep
            // list. Only the explicit SVN delete operations in git_diff may
            // remove tracked content; old wrong-path history needs separate
            // reviewed reconciliation.

            // 3. Commit with identity and sync marker, persist durable intent,
            // push, then confirm only with observed Git ref/tree evidence.
            let commit_message = format!(
                "{}\n\n{} synced from SVN r{}",
                change.message, SYNC_MARKER, change.revision
            );
            let branch = self.config.github.default_branch.clone();
            let remote = "origin".to_string();
            let repo_id = self.effective_repo_id().map(str::to_owned);
            let (git_sha, intended_parent, intended_tree, pre_push_sha, pre_push_tree) =
                tokio::task::block_in_place(|| {
                    let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
                    let pre_push_sha = git
                        .ls_remote_ref(&remote, &branch)
                        .map_err(SyncError::GitError)?
                        .unwrap_or_default();
                    let pre_push_tree = if pre_push_sha.is_empty() {
                        None
                    } else {
                        Some(
                            git.commit_parent_and_tree(&pre_push_sha)
                                .map_err(SyncError::GitError)?
                                .1,
                        )
                    };
                    let oid = git
                        .commit_via_cli(
                            &commit_message,
                            &git_identity.name,
                            &git_identity.email,
                            "reposync",
                            "sync@reposync.local",
                        )
                        .map_err(SyncError::GitError)?;
                    let local_sha = oid.to_string();
                    let (parent, tree) = git
                        .commit_parent_and_tree(&local_sha)
                        .map_err(SyncError::GitError)?;
                    Ok::<_, SyncError>((local_sha, parent, tree, pre_push_sha, pre_push_tree))
                })?;

            let mut intent: Option<GitPushOperation> = None;
            if let Some(ref rid) = repo_id {
                intent = Some(
                    self.db
                        .begin_svn_to_git_push(GitPushIntent {
                            repo_id: rid,
                            initiator_id: "team_worker",
                            request_id: &format!("svn-r{}", change.revision),
                            target_fingerprint: &git_push_target_fingerprint(rid, &remote, &branch),
                            source_svn_rev: change.revision,
                            source_svn_author: &change.author,
                            source_svn_message: &change.message,
                            pre_push_git_remote: &remote,
                            pre_push_git_branch: &branch,
                            pre_push_git_sha: &pre_push_sha,
                            pre_push_git_tree: pre_push_tree.as_deref(),
                            intended_local_git_sha: &git_sha,
                            intended_local_git_parent: intended_parent.as_deref(),
                            intended_local_git_tree: &intended_tree,
                        })
                        .map_err(SyncError::DatabaseError)?,
                );
                #[cfg(debug_assertions)]
                if self.git_push_fixture_flag("REPOSYNC_GIT_PUSH_CRASH_BEFORE", rid) {
                    let op = intent.as_ref().unwrap();
                    let _ = self.db.hold_svn_to_git_reconciliation(
                        rid,
                        &op.id,
                        "intent recorded; planned Git push was not issued",
                    );
                    return Err(SyncError::GitPushHeld {
                        reason: "intent_recorded_push_not_issued".into(),
                        detail: "fixture: crash after durable intent and before git push".into(),
                    });
                }
            }

            let push_result = tokio::task::block_in_place(|| {
                let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
                git.push(&remote, &branch).map_err(SyncError::GitError)
            });

            if let Err(push_err) = push_result {
                if let (Some(ref rid), Some(ref op)) = (&repo_id, &intent) {
                    let _ = self.db.hold_svn_to_git_reconciliation(
                        rid,
                        &op.id,
                        &format!("git push failed after intent was recorded: {push_err}"),
                    );
                    return Err(SyncError::GitPushHeld {
                        reason: "push_outcome_uncertain".into(),
                        detail: format!("git push failed after durable intent: {push_err}"),
                    });
                }
                // Legacy path without durable intent: roll back the local commit.
                tokio::task::block_in_place(|| {
                    let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
                    if let Ok(prev) = git.head_sha() {
                        if prev == git_sha {
                            if let Ok(Some(prev_sha)) = git
                                .commit_parent_and_tree(&git_sha)
                                .map(|(parent, _)| parent)
                            {
                                let _ = git.reset_hard(&prev_sha);
                            }
                        }
                    }
                });
                return Err(push_err);
            }

            if let (Some(ref rid), Some(ref op)) = (&repo_id, &intent) {
                #[cfg(debug_assertions)]
                if self.git_push_fixture_flag("REPOSYNC_GIT_PUSH_LOST_REPLY", rid) {
                    let _ = self.db.hold_svn_to_git_reconciliation(
                        rid,
                        &op.id,
                        "Git accepted the push but the reply was lost before local checkpoint",
                    );
                    return Err(SyncError::GitPushHeld {
                        reason: "lost_push_reply".into(),
                        detail: "fixture: accepted Git push reply lost before verification".into(),
                    });
                }
                let (observed_sha, observed_tree) = tokio::task::block_in_place(|| {
                    let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
                    let observed_sha =
                        observed_git_ref(&git, &remote, &branch).map_err(SyncError::GitError)?;
                    let observed_tree =
                        observed_git_tree(&git, &observed_sha).map_err(SyncError::GitError)?;
                    Ok::<_, SyncError>((observed_sha, observed_tree))
                })?;
                #[cfg(debug_assertions)]
                let observed_tree = if self
                    .git_push_fixture_flag("REPOSYNC_GIT_PUSH_OBSERVED_TREE_MISMATCH", rid)
                {
                    "ffffffffffffffffffffffffffffffffffffffff".into()
                } else {
                    observed_tree
                };
                if observed_sha != git_sha {
                    let detail = format!(
                        "remote ref {branch} is {observed_sha} but intended local commit was {git_sha}"
                    );
                    let _ = self.db.hold_svn_to_git_reconciliation(rid, &op.id, &detail);
                    return Err(SyncError::GitPushHeld {
                        reason: "observed_ref_mismatch".into(),
                        detail,
                    });
                }
                if observed_tree != intended_tree {
                    let detail = format!(
                        "remote commit tree {observed_tree} does not match intended local tree {intended_tree}"
                    );
                    let _ = self.db.hold_svn_to_git_reconciliation(rid, &op.id, &detail);
                    return Err(SyncError::GitPushHeld {
                        reason: "observed_tree_mismatch".into(),
                        detail,
                    });
                }
                match self
                    .db
                    .confirm_svn_to_git_push(rid, &op.id, &observed_sha, &observed_tree)
                {
                    Ok(confirmed) => {
                        debug!(
                            operation_id = %confirmed.id,
                            direction = "svn_to_git",
                            svn_rev = change.revision,
                            git_sha = %&observed_sha[..12.min(observed_sha.len())],
                            git_tree = %&observed_tree[..12.min(observed_tree.len())],
                            "svn-to-git push verified and checkpointed"
                        );
                    }
                    Err(error) => {
                        let _ = self.db.hold_svn_to_git_reconciliation(
                            rid,
                            &op.id,
                            &format!(
                                "Git push verified but the local checkpoint write failed: {error}"
                            ),
                        );
                        return Err(SyncError::GitPushHeld {
                            reason: "checkpoint_write_failed".into(),
                            detail: format!("Git push verified but checkpoint failed: {error}"),
                        });
                    }
                }
            } else {
                // Legacy path when no repository scope is pinned.
                let record = crate::models::SyncRecord {
                    id: uuid::Uuid::new_v4().to_string(),
                    repo_id: self.effective_repo_id().map(|s| s.to_string()),
                    svn_revision: Some(change.revision),
                    git_hash: Some(git_sha.clone()),
                    direction: crate::models::SyncDirection::SvnToGit,
                    author: change.author.clone(),
                    message: change.message.clone(),
                    timestamp: Utc::now(),
                    synced_at: Utc::now(),
                    status: crate::models::SyncRecordStatus::Applied,
                };
                self.db
                    .insert_sync_record(&record)
                    .map_err(SyncError::DatabaseError)?;
                self.db
                    .set_state(&self.svn_rev_key(), &change.revision.to_string())
                    .map_err(SyncError::DatabaseError)?;
                if let Some(rid) = self.effective_repo_id() {
                    self.db
                        .update_repo_watermark(rid, change.revision, &git_sha)
                        .map_err(SyncError::DatabaseError)?;
                    self.db
                        .increment_repo_sync_count(rid)
                        .map_err(SyncError::DatabaseError)?;
                }
            }

            *applied += 1;

            // Audit log for successful sync
            let _ = self.db.insert_audit_log_with_repo(AuditLogInput {
                action: "sync_cycle",
                direction: Some("svn_to_git"),
                svn_rev: Some(change.revision),
                git_sha: Some(&git_sha),
                author: Some(&change.author),
                details: Some(&format!(
                    "synced SVN r{} -> Git {}",
                    change.revision,
                    &git_sha[..8.min(git_sha.len())]
                )),
                success: true,
                repo_id: self.repo_id.as_deref(),
            });

            info!(
                rev = change.revision,
                git_sha = %git_sha,
                git_name = %git_identity.name,
                "synced SVN r{} -> Git {}",
                change.revision,
                &git_sha[..8.min(git_sha.len())]
            );
        }

        Ok(())
    }

    /// Issue the one recorded SVN→Git push after observe-first resume, or
    /// finalize a unique match that landed before the worker ran.
    ///
    /// Re-reads the remote first. Conflict and unavailable stay held without a
    /// push, watermark, mapping, or remote mutation. The local branch tip must
    /// still be the recorded intended commit — this is not a new SHA and not a
    /// blind re-push.
    fn resume_authorized_svn_to_git_push(&self, stats: &mut SyncStats) -> Result<(), SyncError> {
        let Some(rid) = self.effective_repo_id() else {
            return Ok(());
        };
        let Some(op) = self
            .db
            .active_git_push_operation(rid)
            .map_err(SyncError::DatabaseError)?
        else {
            return Ok(());
        };
        if op.state != GitPushOperationState::ReconciliationRequired || !op.resume_authorized {
            return Ok(());
        }

        let inspect = {
            let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
            inspect_svn_to_git_push(&git, &op)
        };
        match inspect {
            GitPushInspect::UniqueMatch { git_sha, git_tree } => {
                let fingerprint = git_push_target_fingerprint(
                    rid,
                    &op.pre_push_git_remote,
                    &op.pre_push_git_branch,
                );
                self.db
                    .finalize_verified_svn_to_git_push(
                        rid,
                        &op.id,
                        &git_sha,
                        &git_tree,
                        &fingerprint,
                    )
                    .map_err(SyncError::DatabaseError)?;
                self.record_resumed_svn_to_git_push(&op, &git_sha, false);
                stats.svn_to_git_count += 1;
                return Ok(());
            }
            GitPushInspect::Conflict { reason } | GitPushInspect::Unavailable { reason } => {
                let _ = self
                    .db
                    .note_git_push_reconciliation_reason(rid, &op.id, &reason);
                return Err(SyncError::GitPushHeld {
                    reason: "reconciliation_required".into(),
                    detail: reason,
                });
            }
            GitPushInspect::AbsentUnchanged => {}
        }

        let local_tip = {
            let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
            let spec = format!("refs/heads/{}", op.pre_push_git_branch);
            let output = Command::new("git")
                .args(["rev-parse", "--verify", &spec])
                .current_dir(git.repo_path())
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .map_err(|e| SyncError::GitError(crate::errors::GitError::IoError(e)))?;
            if !output.status.success() {
                let detail = format!(
                    "could not read local branch {} for the recorded push",
                    op.pre_push_git_branch
                );
                let _ = self
                    .db
                    .note_git_push_reconciliation_reason(rid, &op.id, &detail);
                return Err(SyncError::GitPushHeld {
                    reason: "intended_commit_unreadable".into(),
                    detail,
                });
            }
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        if local_tip != op.intended_local_git_sha {
            let detail = format!(
                "bridge branch {} is {local_tip} but the recorded intended commit was {}",
                op.pre_push_git_branch, op.intended_local_git_sha
            );
            let _ = self
                .db
                .note_git_push_reconciliation_reason(rid, &op.id, &detail);
            return Err(SyncError::GitPushHeld {
                reason: "intended_commit_not_at_branch_tip".into(),
                detail,
            });
        }
        let local_tree = {
            let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
            observed_git_tree(&git, &op.intended_local_git_sha).map_err(SyncError::GitError)?
        };
        if local_tree != op.intended_local_git_tree {
            let detail = format!(
                "recorded intended tree {} does not match local tree {local_tree}",
                op.intended_local_git_tree
            );
            let _ = self
                .db
                .note_git_push_reconciliation_reason(rid, &op.id, &detail);
            return Err(SyncError::GitPushHeld {
                reason: "intended_tree_mismatch".into(),
                detail,
            });
        }

        self.db
            .begin_svn_to_git_push(op.intent())
            .map_err(SyncError::DatabaseError)?;

        let push_result = {
            let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
            git.push(&op.pre_push_git_remote, &op.pre_push_git_branch)
                .map_err(SyncError::GitError)
        };
        if let Err(push_err) = push_result {
            let detail = format!("git push failed after resume was authorized: {push_err}");
            let _ = self.db.hold_svn_to_git_reconciliation(rid, &op.id, &detail);
            return Err(SyncError::GitPushHeld {
                reason: "push_outcome_uncertain".into(),
                detail,
            });
        }

        let (observed_sha, observed_tree) = {
            let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
            let observed_sha =
                observed_git_ref(&git, &op.pre_push_git_remote, &op.pre_push_git_branch)
                    .map_err(SyncError::GitError)?;
            let observed_tree =
                observed_git_tree(&git, &observed_sha).map_err(SyncError::GitError)?;
            (observed_sha, observed_tree)
        };
        if observed_sha != op.intended_local_git_sha {
            let detail = format!(
                "remote ref {} is {observed_sha} but intended local commit was {}",
                op.pre_push_git_branch, op.intended_local_git_sha
            );
            let _ = self.db.hold_svn_to_git_reconciliation(rid, &op.id, &detail);
            return Err(SyncError::GitPushHeld {
                reason: "observed_ref_mismatch".into(),
                detail,
            });
        }
        if observed_tree != op.intended_local_git_tree {
            let detail = format!(
                "remote commit tree {observed_tree} does not match intended local tree {}",
                op.intended_local_git_tree
            );
            let _ = self.db.hold_svn_to_git_reconciliation(rid, &op.id, &detail);
            return Err(SyncError::GitPushHeld {
                reason: "observed_tree_mismatch".into(),
                detail,
            });
        }
        match self
            .db
            .confirm_svn_to_git_push(rid, &op.id, &observed_sha, &observed_tree)
        {
            Ok(_) => {
                self.record_resumed_svn_to_git_push(&op, &observed_sha, true);
                stats.svn_to_git_count += 1;
                Ok(())
            }
            Err(error) => {
                let _ = self.db.hold_svn_to_git_reconciliation(
                    rid,
                    &op.id,
                    &format!("Git push verified but the local checkpoint write failed: {error}"),
                );
                Err(SyncError::GitPushHeld {
                    reason: "checkpoint_write_failed".into(),
                    detail: format!("Git push verified but checkpoint failed: {error}"),
                })
            }
        }
    }

    fn record_resumed_svn_to_git_push(
        &self,
        op: &GitPushOperation,
        git_sha: &str,
        issued_push: bool,
    ) {
        let details = if issued_push {
            format!(
                "resumed the one recorded SVN r{} -> Git {} push",
                op.source_svn_rev,
                &git_sha[..8.min(git_sha.len())]
            )
        } else {
            format!(
                "finalized the recorded SVN r{} -> Git {} push without a second push",
                op.source_svn_rev,
                &git_sha[..8.min(git_sha.len())]
            )
        };
        let _ = self.db.insert_audit_log_with_repo(AuditLogInput {
            action: "sync_cycle",
            direction: Some("svn_to_git"),
            svn_rev: Some(op.source_svn_rev),
            git_sha: Some(git_sha),
            author: Some(&op.source_svn_author),
            details: Some(&details),
            success: true,
            repo_id: self.repo_id.as_deref(),
        });
        info!(
            rev = op.source_svn_rev,
            git_sha = %git_sha,
            issued_push,
            "resumed recorded SVN-to-Git push"
        );
    }

    // -----------------------------------------------------------------------
    // Git -> SVN
    // -----------------------------------------------------------------------

    /// Apply Git changes to the SVN repository.
    ///
    /// For each Git commit:
    /// 1. Get the changed files from the commit.
    /// 2. Copy changed files into the SVN working copy.
    /// 3. Stage additions/deletions with `svn add`/`svn rm`.
    /// 4. Commit to SVN with a `[reposync]` marker.
    /// 5. Only then record the sync in the database.
    fn blocking_git_push_hold(&self, op: &GitPushOperation) -> Option<SyncError> {
        if op.state == GitPushOperationState::ReconciliationRequired && !op.resume_authorized {
            return Some(SyncError::GitPushHeld {
                reason: "reconciliation_required".into(),
                detail: op
                    .outcome_detail
                    .clone()
                    .unwrap_or_else(|| "held SVN-to-Git push requires explicit reconcile".into()),
            });
        }
        if !op.state.is_terminal() && op.state != GitPushOperationState::Running {
            return Some(SyncError::GitPushHeld {
                reason: "unfinished_write".into(),
                detail: "an unfinished SVN-to-Git push is still active".into(),
            });
        }
        None
    }

    #[cfg(debug_assertions)]
    fn git_push_fixture_flag(&self, var: &str, repo_id: &str) -> bool {
        let scoped = format!("{}__{}", var, repo_id);
        if std::env::var(&scoped).is_ok() {
            return true;
        }
        std::env::var(var).ok().as_deref() == Some(repo_id)
    }

    fn blocking_svn_commit_hold(&self, op: &SvnCommitOperation) -> Option<SyncError> {
        if op.state == SvnCommitOperationState::ReconciliationRequired && !op.resume_authorized {
            return Some(SyncError::SvnCommitHeld {
                reason: "reconciliation_required".into(),
                detail: op
                    .outcome_detail
                    .clone()
                    .unwrap_or_else(|| "held Git-to-SVN commit requires explicit reconcile".into()),
            });
        }
        if !op.state.is_terminal() && op.state != SvnCommitOperationState::Running {
            return Some(SyncError::SvnCommitHeld {
                reason: "unfinished_write".into(),
                detail: "an unfinished Git-to-SVN commit is still active".into(),
            });
        }
        None
    }

    fn blocking_cycle_mapping_hold(&self, op: &TeamCycleMappingOperation) -> Option<SyncError> {
        if op.state == TeamCycleMappingState::ReconciliationRequired && !op.resume_authorized {
            return Some(SyncError::CycleMappingHeld {
                reason: "reconciliation_required".into(),
                detail: op.outcome_detail.clone().unwrap_or_else(|| {
                    "held team cycle mapping requires explicit reconcile".into()
                }),
            });
        }
        if !op.state.is_terminal() && op.state != TeamCycleMappingState::Running {
            return Some(SyncError::CycleMappingHeld {
                reason: "unfinished_mapping".into(),
                detail: "an unfinished team cycle mapping is still active".into(),
            });
        }
        None
    }

    fn cycle_mapping_pre_write_watermarks(&self, rid: &str) -> Result<(i64, String), SyncError> {
        let (svn_rev, git_sha) = self
            .db
            .get_repo_watermark(rid)
            .map_err(SyncError::DatabaseError)?;
        Ok((svn_rev, git_sha))
    }

    fn record_team_cycle_mapping(
        &self,
        intent: TeamCycleMappingIntent<'_>,
        observed_proof: Option<serde_json::Value>,
    ) -> Result<(), SyncError> {
        let rid = intent.repo_id;
        let op = self
            .db
            .begin_team_cycle_mapping(intent)
            .map_err(SyncError::DatabaseError)?;
        #[cfg(debug_assertions)]
        if self.cycle_mapping_fixture_flag("REPOSYNC_CYCLE_MAPPING_CHECKPOINT_FAIL", rid) {
            let _ = self.db.hold_team_cycle_mapping_reconciliation(
                rid,
                &op.id,
                "mapping intent recorded but the local checkpoint write failed",
            );
            return Err(SyncError::CycleMappingHeld {
                reason: "checkpoint_write_failed".into(),
                detail: "fixture: mapping intent could not be checkpointed".into(),
            });
        }
        match self
            .db
            .confirm_team_cycle_mapping(rid, &op.id, observed_proof)
        {
            Ok(_) => Ok(()),
            Err(error) => {
                let _ = self.db.hold_team_cycle_mapping_reconciliation(
                    rid,
                    &op.id,
                    &format!("mapping verified but the local checkpoint write failed: {error}"),
                );
                Err(SyncError::CycleMappingHeld {
                    reason: "checkpoint_write_failed".into(),
                    detail: error.to_string(),
                })
            }
        }
    }

    #[cfg(debug_assertions)]
    fn cycle_mapping_fixture_flag(&self, var: &str, repo_id: &str) -> bool {
        let scoped = format!("{}__{}", var, repo_id);
        if std::env::var(&scoped).is_ok() {
            return true;
        }
        std::env::var(var).ok().as_deref() == Some(repo_id)
    }

    #[cfg(debug_assertions)]
    fn svn_commit_fixture_flag(&self, var: &str, repo_id: &str) -> bool {
        let scoped = format!("{}__{}", var, repo_id);
        if std::env::var(&scoped).is_ok() {
            return true;
        }
        // Legacy single-threaded harness: one global var names the target repo.
        std::env::var(var).ok().as_deref() == Some(repo_id)
    }

    async fn persist_git_to_svn_intent(
        &self,
        change: &GitChangeSet,
        file_contents: &[(String, String, Option<Vec<u8>>)],
        svn_wc: &std::path::Path,
        svn: &SvnClient,
    ) -> Result<Option<SvnCommitOperation>, SyncError> {
        let Some(rid) = self.effective_repo_id() else {
            return Ok(None);
        };
        let (parent, tree) = {
            let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
            git.commit_parent_and_tree(&change.sha)
                .map_err(SyncError::GitError)?
        };
        let info = svn.info().await.map_err(SyncError::SvnError)?;
        let snapshot = tempfile::tempdir()
            .map_err(|e| SyncError::SvnError(crate::errors::SvnError::IoError(e)))?;
        let exported = snapshot.path().join("pre-write");
        svn.export("", info.latest_rev, &exported)
            .await
            .map_err(SyncError::SvnError)?;
        let pre_write_tree = hash_regular_file_tree(&exported)
            .map_err(|e| SyncError::SvnError(crate::errors::SvnError::IoError(e)))?;
        let intended_tree = hash_regular_file_tree(svn_wc)
            .map_err(|e| SyncError::SvnError(crate::errors::SvnError::IoError(e)))?;
        // `file_contents` is the pre-mutation projected set — the same paths
        // that were staged into this working copy.
        let projection = self.no_target_projection();
        let fingerprint =
            svn_commit_target_fingerprint(rid, &info.uuid, svn.url(), &info.url, &projection);
        let identity = crate::path_projection::svn_path_identity(&info.root_url, &info.url);
        let op = self
            .db
            .begin_git_to_svn_commit(SvnCommitIntent {
                repo_id: rid,
                initiator_id: "team_worker",
                request_id: &change.sha,
                target_fingerprint: &fingerprint,
                source_git_sha: &change.sha,
                source_git_parent: parent.as_deref(),
                source_git_tree: &tree,
                target_svn_uuid: &info.uuid,
                target_svn_path: &info.url,
                target_svn_root_url: &identity.root_url,
                target_svn_branch_path: &identity.branch_path,
                pre_write_svn_rev: info.latest_rev,
                pre_write_svn_tree: &pre_write_tree,
                projection: &projection,
                intended_changed_paths: intended_paths_from_contents(file_contents),
                intended_svn_tree: &intended_tree,
                author: &change.author_name,
                source_message: &change.message,
            })
            .map_err(SyncError::DatabaseError)?;
        Ok(Some(op))
    }

    async fn sync_git_to_svn(
        &self,
        git_changes: &[GitChangeSet],
        batch_continuation: Option<&GitReplayBatchContinuation>,
    ) -> Result<usize, SyncError> {
        let mut count = 0;
        let resume_sha = if let Some(rid) = self.effective_repo_id() {
            match self
                .db
                .active_svn_commit_operation(rid)
                .map_err(SyncError::DatabaseError)?
            {
                Some(op) => {
                    if let Some(error) = self.blocking_svn_commit_hold(&op) {
                        return Err(error);
                    }
                    if op.state == SvnCommitOperationState::ReconciliationRequired
                        && op.resume_authorized
                    {
                        Some(op.source_git_sha.clone())
                    } else {
                        None
                    }
                }
                None => None,
            }
        } else {
            None
        };

        // Reuse a single SVN working copy across all commits (P4 optimization).
        // Create the tempdir once and use `svn update` between commits instead
        // of a fresh `checkout_head` per commit.
        let svn_wc_dir = tempfile::tempdir()
            .map_err(|e| SyncError::SvnError(crate::errors::SvnError::IoError(e)))?;
        let mut svn_wc_initialized = false;

        for change in git_changes {
            if let Some(allowed) = resume_sha.as_deref() {
                if change.sha != allowed {
                    debug!(
                        sha = %change.sha,
                        allowed,
                        "skipping Git commit while one held write is authorized to resume"
                    );
                    continue;
                }
            }
            if self.should_skip_incoming_git_commit(&change.sha, &change.message)? {
                continue;
            }

            let svn_username = self
                .identity_mapper
                .git_to_svn(&change.author_name, &change.author_email)
                .map_err(SyncError::IdentityError)?;

            // 1. Get changed files and their contents from the Git commit.
            //    Lock is scoped in a block so the guard is dropped before any
            //    .await (std::sync::MutexGuard is !Send).
            let file_contents = {
                let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
                // Use the pre-populated changed_files from fetch_git_changes
                // instead of re-calling get_changed_files (P5 optimization).
                let contents: Result<Vec<_>, SyncError> = change
                    .changed_files
                    .iter()
                    .map(|f| -> Result<_, SyncError> {
                        let content = if f.action != "D" {
                            #[cfg(debug_assertions)]
                            let fault = std::env::var("REPOSYNC_TEST_GIT_CONTENT_FAULT")
                                .ok()
                                .is_some_and(|value| {
                                    value == format!("{}|{}", change.sha, git.repo_path().display())
                                });
                            #[cfg(debug_assertions)]
                            let read = if fault {
                                Err(crate::errors::GitError::RefNotFound(f.path.clone()))
                            } else {
                                git.get_file_content_at_commit(&change.sha, &f.path)
                            };
                            #[cfg(not(debug_assertions))]
                            let read = git.get_file_content_at_commit(&change.sha, &f.path);
                            Some(read.map_err(SyncError::GitError)?.ok_or_else(|| {
                                SyncError::GitError(crate::errors::GitError::RefNotFound(
                                    f.path.clone(),
                                ))
                            })?)
                        } else {
                            None
                        };
                        Ok(GitToSvnInputChange {
                            action: f.action.clone(),
                            path: f.path.clone(),
                            content,
                            rename_from: f.rename_from.clone(),
                        })
                    })
                    .collect();
                contents?
            };

            // 1b. Project the typed changeset BEFORE any SVN working-copy
            // mutation. Allow prefixes, blocked patterns, and deletes share
            // one component-aware matcher. Staging, verification, journal,
            // and receipts below consume this same included set.
            let projected = project_git_to_svn_changeset(
                file_contents,
                &self.allowed_paths,
                &self.blocked_patterns,
            )
            .map_err(|err| {
                SyncError::GitError(crate::errors::GitError::ApplyFailed(err.to_string()))
            })?;
            if !projected.excluded.is_empty() {
                let violations =
                    projected.exclusion_messages(&self.allowed_paths, &self.blocked_patterns);
                for (action, path) in &projected.excluded {
                    debug!(
                        sha = %change.sha,
                        action = %action,
                        path = %path,
                        "projection: excluding path from Git→SVN write"
                    );
                }
                if projected.is_empty() {
                    warn!(
                        sha = %change.sha,
                        violations = ?violations,
                        "skipping entire commit: all files are outside the active projection"
                    );
                    let _ = self.db.insert_audit_log_with_repo(AuditLogInput {
                        action: "path_violation_skipped",
                        direction: Some("git_to_svn"),
                        svn_rev: None,
                        git_sha: Some(&change.sha),
                        author: Some(&change.author_name),
                        details: Some(&format!(
                            "Skipped entire commit {}: {}",
                            &change.sha[..8.min(change.sha.len())],
                            violations.join("; ")
                        )),
                        success: false,
                        repo_id: self.effective_repo_id(),
                    });
                } else {
                    warn!(
                        sha = %change.sha,
                        violations = ?violations,
                        valid_files = projected.included.len(),
                        "projection: {} file(s) outside scope, {} in-scope file(s) remain",
                        projected.excluded.len(),
                        projected.included.len(),
                    );
                    let _ = self.db.insert_audit_log_with_repo(AuditLogInput {
                        action: "path_violation_filtered",
                        direction: Some("git_to_svn"),
                        svn_rev: None,
                        git_sha: Some(&change.sha),
                        author: Some(&change.author_name),
                        details: Some(&format!(
                            "Commit {} filtered: removed {} violating file(s), synced {} valid file(s). Violations: {}",
                            &change.sha[..8.min(change.sha.len())],
                            projected.excluded.len(),
                            projected.included.len(),
                            violations.join("; ")
                        )),
                        success: true,
                        repo_id: self.effective_repo_id(),
                    });
                }
            }
            let file_contents = projected.into_file_contents();
            if let Err(violations) = validate_file_paths_impl(
                &self.allowed_paths,
                &self.blocked_patterns,
                &file_contents,
            ) {
                return Err(self.record_history_block(
                    "projected_changeset_inconsistent",
                    &format!(
                        "projected Git→SVN set still contains out-of-scope paths: {}",
                        violations.join("; ")
                    ),
                    Some(&change.sha),
                    None,
                    None,
                    None,
                ));
            }

            // The current bridge maps regular-file bytes only. Mode, type,
            // symlink and executable changes cannot be acknowledged by an
            // empty SVN status or by a content-only no-target receipt.
            {
                let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
                for (_, path, _) in &file_contents {
                    let (previous, current) = git
                        .changed_entry_modes(&change.sha, path)
                        .map_err(SyncError::GitError)?;
                    if (previous.is_none() && current.is_none())
                        || previous
                            .into_iter()
                            .chain(current)
                            .any(|mode| mode != 33188)
                    {
                        return Err(self.record_history_block("unsupported_git_semantics",
                            "changed Git tree entry has mode or type unsupported by the SVN byte bridge",
                            Some(&change.sha), None, None, None));
                    }
                }
            }

            if file_contents.is_empty() {
                // All files were filtered out — record a durable no-target mapping.
                if let Some(rid) = self.effective_repo_id() {
                    let (pre_svn_rev, pre_git_sha) =
                        self.cycle_mapping_pre_write_watermarks(rid)?;
                    let projection = self.no_target_projection();
                    let fingerprint = team_cycle_mapping_fingerprint(rid, &projection);
                    let mapping_outcome = if change.changed_files.is_empty() {
                        TeamCycleMappingOutcome::GitEmptyCommit
                    } else {
                        TeamCycleMappingOutcome::GitFiltered
                    };
                    self.record_team_cycle_mapping(
                        TeamCycleMappingIntent {
                            repo_id: rid,
                            initiator_id: "team_worker",
                            request_id: &change.sha,
                            target_fingerprint: &fingerprint,
                            direction: TeamCycleMappingDirection::GitToSvn,
                            outcome: mapping_outcome,
                            source_svn_rev: None,
                            source_git_sha: Some(&change.sha),
                            pre_write_svn_rev: pre_svn_rev,
                            pre_write_git_sha: &pre_git_sha,
                            projection: &projection,
                            intended_target_proof: None,
                        },
                        None,
                    )?;
                    self.append_git_replay_handled_commit(batch_continuation, &change.sha)?;
                } else {
                    self.db
                        .set_state("last_git_hash", &change.sha)
                        .map_err(SyncError::DatabaseError)?;
                }
                continue;
            }

            #[cfg(debug_assertions)]
            self.test_outbound_pause("REPOSYNC_TEST_BEFORE_GIT_TO_SVN_CHECKOUT", &change.sha)
                .await?;

            // 2. Prepare SVN working copy: checkout on first use, update thereafter.
            let svn_url_for_log;
            {
                let svn = self
                    .svn_client
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                svn_url_for_log = svn.url().to_string();
                if !svn_wc_initialized {
                    debug!(
                        sha = %change.sha,
                        svn_url = %svn_url_for_log,
                        wc_path = %svn_wc_dir.path().display(),
                        "checking out SVN HEAD into temp working copy"
                    );
                    svn.checkout_head(svn_wc_dir.path())
                        .await
                        .map_err(SyncError::SvnError)?;
                    svn_wc_initialized = true;
                } else {
                    debug!(sha = %change.sha, "updating SVN working copy to HEAD");
                    svn.update(svn_wc_dir.path())
                        .await
                        .map_err(SyncError::SvnError)?;
                }
            }

            // 3. Copy changed files from Git into the SVN working copy.
            //    If a file is marked as modified ("M") in Git but does not
            //    exist in the SVN working copy, treat it as an add so that
            //    `svn add` is called.  This handles the case where the SVN
            //    repo has fewer files than Git (e.g. freshly created repo).
            let mut added_files = Vec::new();
            let mut deleted_files = Vec::new();

            for (action, file_path, content) in &file_contents {
                let dst = svn_wc_dir.path().join(file_path);
                debug!(
                    sha = %change.sha,
                    action = %action,
                    file_path = %file_path,
                    dst = %dst.display(),
                    dst_exists = dst.exists(),
                    "processing file change"
                );
                match action.as_str() {
                    "D" => {
                        if dst.exists() {
                            deleted_files.push(file_path.as_str());
                        } else {
                            debug!(
                                file_path = %file_path,
                                "skipping delete: file does not exist in SVN working copy"
                            );
                        }
                    }
                    "A" => {
                        if let Some(content) = content {
                            if let Some(parent) = dst.parent() {
                                std::fs::create_dir_all(parent).map_err(|e| {
                                    SyncError::GitError(crate::errors::GitError::IoError(e))
                                })?;
                            }
                            std::fs::write(&dst, content).map_err(|e| {
                                SyncError::GitError(crate::errors::GitError::IoError(e))
                            })?;
                            added_files.push(file_path.as_str());
                        }
                    }
                    _ => {
                        // Modified: overwrite content.
                        if let Some(content) = content {
                            let file_is_new = !dst.exists();
                            if let Some(parent) = dst.parent() {
                                std::fs::create_dir_all(parent).map_err(|e| {
                                    SyncError::GitError(crate::errors::GitError::IoError(e))
                                })?;
                            }
                            std::fs::write(&dst, content).map_err(|e| {
                                SyncError::GitError(crate::errors::GitError::IoError(e))
                            })?;
                            // If the file didn't exist in the SVN working copy,
                            // it must be `svn add`ed even though Git says "M".
                            if file_is_new {
                                debug!(
                                    file_path = %file_path,
                                    "file marked as modified in Git but missing in SVN WC; treating as add"
                                );
                                added_files.push(file_path.as_str());
                            }
                        }
                    }
                }
            }

            // 4. Stage changes in SVN.
            let svn = self
                .svn_client
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();

            // Stage ALL changes at once using `svn add --force .` from the
            // WC root. This recursively adds every unversioned file and
            // directory in one atomic operation, avoiding all E150000
            // parent-node issues. The --force flag makes it a no-op for
            // already-versioned items.
            //
            // A failed add is an operational failure, even if some other
            // nodes were staged. Status alone cannot turn that into a
            // durable no-target outcome.
            if !added_files.is_empty() {
                debug!(sha = %change.sha, count = added_files.len(), "staging additions");

                // Collect all parent directories that need adding, sorted
                // shallowest-first. Then add directories top-down, then files.
                // This guarantees every parent is registered in SVN's WC
                // database before any child is added.
                let mut dirs_to_add: Vec<String> = Vec::new();
                for file_path in &added_files {
                    let p = std::path::Path::new(file_path);
                    let mut cur = p.parent();
                    while let Some(dir) = cur {
                        if dir.as_os_str().is_empty() {
                            break;
                        }
                        let ds = dir.to_string_lossy().to_string();
                        if !dirs_to_add.contains(&ds) {
                            dirs_to_add.push(ds);
                        }
                        cur = dir.parent();
                    }
                }
                // Sort by depth (shallowest first = fewest separators)
                dirs_to_add.sort_by_key(|d| d.matches('/').count());

                // Add each directory individually, shallowest first.
                // --force makes it a no-op for already-versioned dirs.
                for dir in &dirs_to_add {
                    let full = svn_wc_dir.path().join(dir);
                    if full.is_dir() {
                        svn.run_svn_in_dir_public(
                            svn_wc_dir.path(),
                            &["add", "--force", "--depth", "empty", dir],
                        )
                        .await
                        .map_err(SyncError::SvnError)?;
                    }
                }

                // Now add the actual files — parents are guaranteed registered.
                for file in &added_files {
                    #[cfg(debug_assertions)]
                    let fault = std::env::var("REPOSYNC_TEST_SVN_STAGE_FAULT")
                        .ok()
                        .is_some_and(|value| {
                            value
                                == format!(
                                    "{}|{}",
                                    change.sha,
                                    self.git_client
                                        .lock()
                                        .unwrap_or_else(|p| p.into_inner())
                                        .repo_path()
                                        .display()
                                )
                        });
                    #[cfg(debug_assertions)]
                    if fault {
                        return Err(SyncError::SvnError(
                            crate::errors::SvnError::WorkingCopyError {
                                path: file.to_string(),
                                detail: "injected SVN staging failure".into(),
                            },
                        ));
                    }
                    svn.run_svn_in_dir_public(svn_wc_dir.path(), &["add", "--force", file])
                        .await
                        .map_err(SyncError::SvnError)?;
                }
            }
            if !deleted_files.is_empty() {
                debug!(sha = %change.sha, count = deleted_files.len(), "staging deletions");
                svn.rm(svn_wc_dir.path(), &deleted_files)
                    .await
                    .map_err(SyncError::SvnError)?;
            }

            // 4b. Check `svn status` to verify there are actual pending changes.
            //     If SVN sees no modifications, skip this commit gracefully
            //     instead of failing to parse an empty commit output.
            let svn_status = svn
                .status(svn_wc_dir.path())
                .await
                .map_err(SyncError::SvnError)?;
            let has_changes = svn_status.lines().any(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty()
                    && !trimmed.starts_with('?')  // unversioned
                    && !trimmed.starts_with('X') // externals
            });
            if !has_changes {
                warn!(
                    sha = %change.sha,
                    svn_url = %svn_url_for_log,
                    svn_status = %svn_status,
                    file_count = file_contents.len(),
                    added = added_files.len(),
                    deleted = deleted_files.len(),
                    "no pending SVN changes after copying files — skipping commit \
                     (files may already be in sync or paths may be misaligned)"
                );
                let proof = self
                    .verify_no_svn_delta(&svn, &file_contents, &change.sha)
                    .await?;
                if let Some(rid) = self.effective_repo_id() {
                    let (pre_svn_rev, pre_git_sha) =
                        self.cycle_mapping_pre_write_watermarks(rid)?;
                    let projection = self.no_target_projection();
                    let fingerprint = team_cycle_mapping_fingerprint(rid, &projection);
                    self.record_team_cycle_mapping(
                        TeamCycleMappingIntent {
                            repo_id: rid,
                            initiator_id: "team_worker",
                            request_id: &change.sha,
                            target_fingerprint: &fingerprint,
                            direction: TeamCycleMappingDirection::GitToSvn,
                            outcome: TeamCycleMappingOutcome::GitNoSvnDelta,
                            source_svn_rev: None,
                            source_git_sha: Some(&change.sha),
                            pre_write_svn_rev: pre_svn_rev,
                            pre_write_git_sha: &pre_git_sha,
                            projection: &projection,
                            intended_target_proof: Some(&proof),
                        },
                        Some(proof.clone()),
                    )?;
                    self.append_git_replay_handled_commit(batch_continuation, &change.sha)?;
                } else {
                    self.db
                        .set_state("last_git_hash", &change.sha)
                        .map_err(SyncError::DatabaseError)?;
                }
                continue;
            }

            debug!(
                sha = %change.sha,
                svn_status = %svn_status,
                "SVN working copy has pending changes, committing"
            );

            // 5. Commit to SVN. The working copy contains only the projected
            // changeset; excluded/blocked paths were never written.
            // Run svn update immediately before commit to minimize the window
            // for E155011 "out of date" errors. The WC may be stale if a
            // previous commit in this batch advanced the server HEAD.
            {
                let svn = self
                    .svn_client
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                svn.update(svn_wc_dir.path())
                    .await
                    .map_err(SyncError::SvnError)?;
            }

            let intent = self
                .persist_git_to_svn_intent(change, &file_contents, svn_wc_dir.path(), &svn)
                .await?;
            #[cfg(debug_assertions)]
            if let (Some(rid), Some(op)) = (self.effective_repo_id(), intent.as_ref()) {
                if self.svn_commit_fixture_flag("REPOSYNC_SVN_COMMIT_CRASH_BEFORE", rid) {
                    let _ = self.db.hold_git_to_svn_reconciliation(
                        rid,
                        &op.id,
                        "intent recorded; planned SVN write was not issued",
                    );
                    return Err(SyncError::SvnCommitHeld {
                        reason: "intent_recorded_write_not_issued".into(),
                        detail: "fixture: crash after durable intent and before svn commit".into(),
                    });
                }
            }
            let commit_message = match &intent {
                Some(op) => operation_commit_message(&change.message, &change.sha, &op.id),
                None => format!(
                    "{}\n\n{} synced from Git {}",
                    change.message,
                    SYNC_MARKER,
                    &change.sha[..8.min(change.sha.len())]
                ),
            };
            let mut svn_commit_result = svn
                .commit(svn_wc_dir.path(), &commit_message, &svn_username)
                .await;

            // Retry once on E155011 only when no durable intent exists.
            // A journaled write cannot blindly retry after the pre-write snapshot.
            if intent.is_none() {
                if let Err(ref e) = svn_commit_result {
                    let err_str = e.to_string();
                    if err_str.contains("E155011") || err_str.contains("out of date") {
                        warn!(
                            sha = %change.sha,
                            "svn commit got 'out of date' — updating WC and retrying"
                        );
                        let svn = self
                            .svn_client
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .clone();
                        svn.update(svn_wc_dir.path())
                            .await
                            .map_err(SyncError::SvnError)?;
                        svn_commit_result = svn
                            .commit(svn_wc_dir.path(), &commit_message, &svn_username)
                            .await;
                    }
                }
            }

            // Handle "nothing to commit" gracefully — the SVN working copy
            // was already in sync (e.g. files were already synced by a prior
            // cycle, or this is an echo commit the detection didn't catch).
            let svn_rev = match svn_commit_result {
                Ok(rev) => rev,
                Err(crate::errors::SvnError::NothingToCommit) if intent.is_none() => {
                    info!(
                        sha = %change.sha,
                        "svn commit: nothing to commit — files already in sync, advancing watermark"
                    );
                    let proof = self
                        .verify_no_svn_delta(&svn, &file_contents, &change.sha)
                        .await?;
                    if let Some(rid) = self.effective_repo_id() {
                        let (pre_svn_rev, pre_git_sha) =
                            self.cycle_mapping_pre_write_watermarks(rid)?;
                        let projection = self.no_target_projection();
                        let fingerprint = team_cycle_mapping_fingerprint(rid, &projection);
                        self.record_team_cycle_mapping(
                            TeamCycleMappingIntent {
                                repo_id: rid,
                                initiator_id: "team_worker",
                                request_id: &change.sha,
                                target_fingerprint: &fingerprint,
                                direction: TeamCycleMappingDirection::GitToSvn,
                                outcome: TeamCycleMappingOutcome::GitNoSvnDelta,
                                source_svn_rev: None,
                                source_git_sha: Some(&change.sha),
                                pre_write_svn_rev: pre_svn_rev,
                                pre_write_git_sha: &pre_git_sha,
                                projection: &projection,
                                intended_target_proof: Some(&proof),
                            },
                            Some(proof.clone()),
                        )?;
                        self.append_git_replay_handled_commit(batch_continuation, &change.sha)?;
                    } else {
                        self.db
                            .set_state("last_git_hash", &change.sha)
                            .map_err(SyncError::DatabaseError)?;
                    }
                    continue;
                }
                Err(e) => {
                    if let Some(op) = &intent {
                        if let Some(rid) = self.effective_repo_id() {
                            let _ = self.db.hold_git_to_svn_reconciliation(
                                rid,
                                &op.id,
                                &format!("svn commit failed after intent was recorded: {e}"),
                            );
                        }
                    }
                    return Err(SyncError::SvnError(e));
                }
            };

            if let (Some(rid), Some(op)) = (self.effective_repo_id(), &intent) {
                #[cfg(debug_assertions)]
                if self.svn_commit_fixture_flag("REPOSYNC_SVN_COMMIT_LOST_REPLY", rid) {
                    let _ = self.db.hold_git_to_svn_reconciliation(
                        rid,
                        &op.id,
                        "SVN accepted the commit but the reply was lost before local checkpoint",
                    );
                    return Err(SyncError::SvnCommitHeld {
                        reason: "lost_commit_reply".into(),
                        detail: "fixture: accepted SVN commit reply lost before verification"
                            .into(),
                    });
                }
                #[cfg(debug_assertions)]
                if self.svn_commit_fixture_flag("REPOSYNC_SVN_COMMIT_CHECKPOINT_FAIL", rid) {
                    let _ = self.db.hold_git_to_svn_reconciliation(
                        rid,
                        &op.id,
                        "SVN accepted the commit but the local checkpoint write failed",
                    );
                    return Err(SyncError::SvnCommitHeld {
                        reason: "checkpoint_write_failed".into(),
                        detail: "fixture: accepted SVN commit could not be checkpointed".into(),
                    });
                }
                let observed_svn_tree = match observed_svn_tree_at_revision(&svn, svn_rev).await {
                    Ok(tree) => tree,
                    Err(error) => {
                        let detail = format!(
                            "SVN accepted the commit but the observed tree could not be re-read: {error}"
                        );
                        let _ = self.db.hold_git_to_svn_reconciliation(rid, &op.id, &detail);
                        return Err(SyncError::SvnCommitHeld {
                            reason: "observed_tree_unavailable".into(),
                            detail,
                        });
                    }
                };
                if observed_svn_tree != op.intended_svn_tree {
                    let detail = format!(
                        "SVN revision {svn_rev} tree does not match the intended Git-to-SVN tree"
                    );
                    let _ = self.db.hold_git_to_svn_reconciliation(rid, &op.id, &detail);
                    return Err(SyncError::SvnCommitHeld {
                        reason: "observed_tree_mismatch".into(),
                        detail,
                    });
                }
                match self
                    .db
                    .confirm_git_to_svn_commit(rid, &op.id, svn_rev, &observed_svn_tree)
                {
                    Ok(_) => {
                        self.append_git_replay_handled_commit(batch_continuation, &change.sha)?;
                    }
                    Err(error) => {
                        let _ = self.db.hold_git_to_svn_reconciliation(
                            rid,
                            &op.id,
                            &format!("SVN accepted the commit but the local checkpoint write failed: {error}"),
                        );
                        return Err(SyncError::SvnCommitHeld {
                            reason: "checkpoint_write_failed".into(),
                            detail: error.to_string(),
                        });
                    }
                }
            } else {
                // Legacy path without a pair identity: retain the previous writers.
                let record = crate::models::SyncRecord {
                    id: uuid::Uuid::new_v4().to_string(),
                    repo_id: self.effective_repo_id().map(|s| s.to_string()),
                    svn_revision: Some(svn_rev),
                    git_hash: Some(change.sha.clone()),
                    direction: crate::models::SyncDirection::GitToSvn,
                    author: change.author_name.clone(),
                    message: change.message.clone(),
                    timestamp: Utc::now(),
                    synced_at: Utc::now(),
                    status: crate::models::SyncRecordStatus::Applied,
                };
                self.db
                    .insert_sync_record(&record)
                    .map_err(SyncError::DatabaseError)?;

                // Update the Git watermark (dual-write: kv_state + repo table).
                // IMPORTANT: Only advance the git SHA here, NOT the SVN rev.
                if let Some(rid) = self.effective_repo_id() {
                    self.db
                        .advance_all_watermarks(rid, &change.sha)
                        .map_err(SyncError::DatabaseError)?;
                    self.db
                        .increment_repo_sync_count(rid)
                        .map_err(SyncError::DatabaseError)?;
                } else {
                    self.db
                        .set_state("last_git_hash", &change.sha)
                        .map_err(SyncError::DatabaseError)?;
                }
            }

            count += 1;

            // Audit log for successful sync
            let _ = self.db.insert_audit_log_with_repo(AuditLogInput {
                action: "sync_cycle",
                direction: Some("git_to_svn"),
                svn_rev: Some(svn_rev),
                git_sha: Some(&change.sha),
                author: Some(&change.author_name),
                details: Some(&format!(
                    "synced Git {} -> SVN r{}",
                    &change.sha[..8.min(change.sha.len())],
                    svn_rev
                )),
                success: true,
                repo_id: self.repo_id.as_deref(),
            });

            info!(
                sha = %change.sha,
                svn_rev,
                "synced Git {} -> SVN r{}",
                &change.sha[..8.min(change.sha.len())],
                svn_rev
            );
        }

        Ok(count)
    }

    // -----------------------------------------------------------------------
    // Change fetching
    // -----------------------------------------------------------------------

    async fn fetch_svn_changes(&self) -> Result<Vec<SvnChangeSet>, SyncError> {
        let last_rev = if let Some(rid) = self.effective_repo_id() {
            // Team mode: scoped import baseline is the only SVN checkpoint
            // authority. Global `last_svn_rev`, git-log auto-detect, and
            // revision zero are never adopted (#63).
            if self
                .db
                .get_repository(rid)
                .map_err(SyncError::DatabaseError)?
                .is_some()
            {
                match resolve_repo_import_baseline(&self.db, rid)
                    .map_err(SyncError::DatabaseError)?
                {
                    RepoImportBaseline::Verified { svn_rev, .. } => svn_rev,
                    RepoImportBaseline::Pending => {
                        return Err(SyncError::HistoryBlocked {
                            reason: "import_baseline_pending".into(),
                            detail: "repository has no verified scoped SVN checkpoint; complete import before syncing"
                                .into(),
                        });
                    }
                    RepoImportBaseline::ReconciliationRequired { reason, detail } => {
                        return Err(SyncError::HistoryBlocked { reason, detail });
                    }
                }
            } else {
                // Legacy pair-creation transition before a repository row
                // exists: honor scoped KV only; never borrow global state.
                match self
                    .db
                    .get_state(&self.svn_rev_key())
                    .map_err(SyncError::DatabaseError)?
                    .and_then(|value| value.parse::<i64>().ok())
                {
                    Some(rev) if rev > 0 => rev,
                    _ => {
                        return Err(SyncError::HistoryBlocked {
                            reason: "import_baseline_pending".into(),
                            detail: "repository has no verified scoped SVN checkpoint; complete import before syncing"
                                .into(),
                        });
                    }
                }
            }
        } else {
            // Personal/single-repo callers keep the legacy fallback chain.
            let mut last_rev = match self
                .db
                .get_state(&self.svn_rev_key())
                .map_err(SyncError::DatabaseError)?
            {
                Some(s) => s.parse::<i64>().unwrap_or(0),
                None => self
                    .db
                    .get_last_svn_revision()
                    .map_err(SyncError::DatabaseError)?
                    .unwrap_or(0),
            };

            // On a fresh DB connecting to a repo with existing commits, auto-detect
            // the highest SVN revision already synced by scanning git log for
            // sync markers like "[reposync] synced from SVN rNNN".
            if last_rev == 0 {
                let detected = self.detect_last_svn_rev_from_git();
                if detected > 0 {
                    info!(
                        detected_rev = detected,
                        "Auto-detected last synced revision from existing git history"
                    );
                    let _ = crate::db::watermark_recovery::persist_git_log_auto_detect_watermark(
                        &self.db,
                        self.effective_repo_id(),
                        detected,
                    );
                    last_rev = detected;
                }
            }
            last_rev
        };

        info!(since_rev = last_rev, "fetching SVN changes");

        let svn = self
            .svn_client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let svn_info = svn.info().await.map_err(SyncError::SvnError)?;
        let head_rev = svn_info.latest_rev;

        if head_rev <= last_rev {
            debug!("SVN is up to date");
            return Ok(Vec::new());
        }

        let entries = svn
            .log(last_rev + 1, head_rev)
            .await
            .map_err(SyncError::SvnError)?;

        // Determine the trunk prefix to filter/strip when using standard layout.
        let trunk_prefix = if self.config.svn.layout == SvnLayout::Standard {
            let tp = self.config.svn.trunk_path.trim_matches('/');
            if tp.is_empty() {
                None
            } else {
                Some(format!("{}/", tp))
            }
        } else {
            None
        };

        let mut change_sets: Vec<SvnChangeSet> = Vec::new();
        for e in entries {
            if self.should_skip_incoming_svn_revision(e.revision, &e.message)? {
                continue;
            }
            change_sets.push(SvnChangeSet {
                revision: e.revision,
                author: e.author,
                date: e.date,
                message: e.message,
                changed_files: e
                    .changed_paths
                    .iter()
                    .filter_map(|p| {
                        let raw = p.path.strip_prefix('/').unwrap_or(&p.path);
                        // When using standard layout, only sync files under trunk/
                        // and strip the trunk prefix so git paths are repo-relative.
                        let mapped_path = if let Some(ref prefix) = trunk_prefix {
                            let rest = raw.strip_prefix(prefix.as_str())?;
                            if rest.is_empty() {
                                return None; // skip bare trunk/ directory entry
                            }
                            rest.to_string()
                        } else {
                            raw.to_string()
                        };
                        Some(ChangedFile {
                            path: mapped_path,
                            action: p.action.clone(),
                            content: None,
                            is_binary: false,
                            rename_from: None,
                        })
                    })
                    .collect(),
                diff_content: None,
            });
        }

        debug!(count = change_sets.len(), "fetched SVN change sets");
        Ok(change_sets)
    }

    fn load_git_replay_continuation(&self) -> Result<Option<GitReplayContinuation>, SyncError> {
        let Some(rid) = self.effective_repo_id() else {
            return Ok(None);
        };
        let key = GitReplayContinuation::state_key(rid);
        let raw = self.db.get_state(&key).map_err(SyncError::DatabaseError)?;
        raw.as_deref()
            .map(|value| {
                serde_json::from_str(value).map_err(|error| SyncError::HistoryBlocked {
                    reason: "invalid_git_replay_continuation".into(),
                    detail: format!("stored continuation state is unreadable: {error}"),
                })
            })
            .transpose()
    }

    fn persist_git_replay_continuation(
        &self,
        continuation: &GitReplayContinuation,
    ) -> Result<(), SyncError> {
        let Some(rid) = self.effective_repo_id() else {
            return Err(SyncError::HistoryBlocked {
                reason: "missing_repo_identity".into(),
                detail: "git replay continuation requires a repository identity".into(),
            });
        };
        let key = GitReplayContinuation::state_key(rid);
        let value =
            serde_json::to_string(continuation).map_err(|error| SyncError::HistoryBlocked {
                reason: "invalid_git_replay_continuation".into(),
                detail: format!("continuation state could not be encoded: {error}"),
            })?;
        self.db
            .set_state(&key, &value)
            .map_err(SyncError::DatabaseError)
    }

    fn clear_git_replay_continuation(&self) -> Result<(), SyncError> {
        if let Some(rid) = self.effective_repo_id() {
            let key = GitReplayContinuation::state_key(rid);
            self.db
                .conn()
                .execute("DELETE FROM kv_state WHERE key = ?1", [&key])
                .map_err(|error| {
                    SyncError::DatabaseError(crate::errors::DatabaseError::from(error))
                })?;
        }
        Ok(())
    }

    fn persist_pre_batch_git_replay_continuation(
        &self,
        ctx: &GitReplayBatchContinuation,
    ) -> Result<(), SyncError> {
        let batch_set: HashSet<&str> = ctx.batch_shas.iter().map(String::as_str).collect();
        let pre_batch = GitReplayContinuation {
            handled_shas: ctx
                .target
                .handled_shas
                .iter()
                .filter(|sha| !batch_set.contains(sha.as_str()))
                .cloned()
                .collect(),
            ..ctx.target.clone()
        };
        self.persist_git_replay_continuation(&pre_batch)
    }

    fn append_git_replay_handled_commit(
        &self,
        ctx: Option<&GitReplayBatchContinuation>,
        sha: &str,
    ) -> Result<(), SyncError> {
        let Some(ctx) = ctx else {
            return Ok(());
        };
        if !ctx.target.merge_dag || !ctx.batch_shas.iter().any(|batch_sha| batch_sha == sha) {
            return Ok(());
        }
        let stored = self
            .load_git_replay_continuation()?
            .unwrap_or_else(|| ctx.target.clone());
        if stored.handled_shas.iter().any(|handled| handled == sha) {
            return Ok(());
        }
        let updated = GitReplayContinuation {
            handled_shas: stored
                .handled_shas
                .into_iter()
                .chain(std::iter::once(sha.to_string()))
                .collect(),
            ..stored
        };
        self.persist_git_replay_continuation(&updated)
    }

    async fn svn_changes_block_git_continuation(
        &self,
        svn_changes: &[SvnChangeSet],
    ) -> Result<bool, SyncError> {
        if svn_changes.is_empty() {
            return Ok(false);
        }
        let svn = self
            .svn_client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        for change in svn_changes {
            if change.changed_files.is_empty() {
                continue;
            }
            let content = svn
                .diff_content_only(change.revision)
                .await
                .map_err(SyncError::SvnError)?;
            if !content.trim().is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn ensure_conflict_coverage_before_detection(
        &self,
        has_more: bool,
        pending_total: usize,
        coverage_len: usize,
    ) -> Result<(), SyncError> {
        if has_more && coverage_len != pending_total {
            return Err(SyncError::HistoryBlocked {
                reason: "incomplete_conflict_coverage".into(),
                detail: format!(
                    "conflict detection refused on partial batch state: coverage={coverage_len} pending_total={pending_total}"
                ),
            });
        }
        Ok(())
    }

    fn pending_selection_is_merge_dag(
        git: &GitClient,
        since_sha: &str,
        tip_sha: &str,
    ) -> Result<bool, SyncError> {
        let repo = git2::Repository::open(git.repo_path())
            .map_err(|error| SyncError::GitError(crate::errors::GitError::Git2Error(error)))?;
        pending_frontier_is_merge_dag(&repo, since_sha, tip_sha).map_err(SyncError::GitError)
    }

    fn next_git_replay_continuation(
        &self,
        admission: &TeamHistoryAdmission,
        stored: Option<&GitReplayContinuation>,
        selection: &PendingCommitSelection,
        merge_dag: bool,
    ) -> (Option<GitReplayContinuation>, bool) {
        if selection.has_more {
            let mut handled = stored
                .map(|state| state.handled_shas.clone())
                .unwrap_or_default();
            if merge_dag {
                handled.extend(selection.commits.iter().map(|commit| commit.sha.clone()));
            }
            let continuation = GitReplayContinuation {
                p_origin: stored
                    .map(|state| state.p_origin.clone())
                    .unwrap_or_else(|| admission.checkpoint.clone()),
                r_admitted: admission.remote_tip.clone(),
                handled_shas: handled,
                merge_dag,
            };
            (Some(continuation), false)
        } else {
            (None, stored.is_some())
        }
    }

    async fn fetch_git_changes(
        &self,
        admission: &TeamHistoryAdmission,
        svn_changes: &[SvnChangeSet],
    ) -> Result<GitFetchResult, SyncError> {
        info!(since_sha = %admission.checkpoint, remote_sha = %admission.remote_tip, "fetching admitted Git changes");

        // Select P..R from the pinned inspection objects before reset so a
        // visited-order HEAD walk cannot skip older pending work.
        let batch_cap = self.replay_batch_cap();
        let stored = self.load_git_replay_continuation()?;
        if let Some(continuation) = &stored {
            if continuation.r_admitted != admission.remote_tip {
                return Err(SyncError::HistoryBlocked {
                    reason: "continuation_remote_drift".into(),
                    detail: "admitted remote tip changed during incomplete Git replay continuation"
                        .into(),
                });
            }
        }

        let selection_origin = stored
            .as_ref()
            .map(|state| state.p_origin.as_str())
            .unwrap_or(admission.checkpoint.as_str());
        let (selection, merge_dag): (PendingCommitSelection, bool) = {
            let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
            let selection = if let Some(continuation) = &stored {
                if continuation.merge_dag {
                    git.pending_commits_continuation_batch(
                        &continuation.p_origin,
                        &admission.remote_tip,
                        &continuation.handled_shas,
                        batch_cap,
                    )
                } else {
                    git.pending_commits_between(
                        &admission.checkpoint,
                        &admission.remote_tip,
                        batch_cap,
                    )
                }
            } else {
                git.pending_commits_between(&admission.checkpoint, &admission.remote_tip, batch_cap)
            }
            .map_err(|error| match error {
                crate::errors::GitError::UnsupportedHistory { reason, detail } => self
                    .record_history_block(
                        &reason,
                        &detail,
                        Some(selection_origin),
                        None,
                        Some(&admission.remote_tip),
                        None,
                    ),
                other => SyncError::GitError(other),
            })?;
            let merge_dag = if let Some(state) = stored.as_ref() {
                state.merge_dag
            } else if selection.has_more {
                Self::pending_selection_is_merge_dag(&git, selection_origin, &admission.remote_tip)?
            } else {
                false
            };
            (selection, merge_dag)
        };
        let pending_total = if stored.is_some() {
            let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
            git.pending_commits_between(selection_origin, &admission.remote_tip, batch_cap)
                .map_err(SyncError::GitError)?
                .total
        } else {
            selection.total
        };
        let (continuation_to_persist, clear_continuation) =
            self.next_git_replay_continuation(admission, stored.as_ref(), &selection, merge_dag);

        if selection.has_more && self.svn_changes_block_git_continuation(svn_changes).await? {
            return Ok(GitFetchResult {
                replay_batch: Vec::new(),
                conflict_coverage: Vec::new(),
                conflict_coverage_skipped: 0,
                has_more: true,
                reset_target: admission.checkpoint.clone(),
                pending_total,
                deferred_mixed_pending: true,
                continuation_to_persist: None,
                clear_continuation: false,
            });
        }

        let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
        let conflict_commits = git
            .pending_commits_for_conflict_coverage(
                selection_origin,
                &admission.remote_tip,
                batch_cap,
            )
            .map_err(SyncError::GitError)?;
        #[cfg(debug_assertions)]
        let conflict_commits = if selection.has_more
            && self.incomplete_conflict_coverage_test_fault_enabled()
            && conflict_commits.len() == pending_total
            && conflict_commits.len() > 1
        {
            conflict_commits[..conflict_commits.len() - 1].to_vec()
        } else {
            conflict_commits
        };
        self.ensure_conflict_coverage_before_detection(
            selection.has_more,
            pending_total,
            conflict_commits.len(),
        )?;

        let reset_target = if selection.has_more {
            selection.batch_tip.clone()
        } else {
            admission.remote_tip.clone()
        };

        // Reset only through the replay batch tip while continuation is
        // incomplete. A hard reset to the full admitted R would leave unreplayed
        // Git history on the bridge and invite unsafe opposite-direction writes.
        let reset = tokio::task::block_in_place(|| {
            Command::new("git")
                .args(["reset", "--hard", &reset_target])
                .current_dir(git.repo_path())
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
        })
        .map_err(crate::errors::GitError::IoError)?;
        if !reset.status.success() {
            return Err(SyncError::GitError(crate::errors::GitError::Git2Error(
                git2::Error::from_str("git reset to replay target failed"),
            )));
        }
        if git.head_sha().map_err(SyncError::GitError)? != reset_target {
            return Err(SyncError::HistoryBlocked {
                reason: "post_reset_tip_mismatch".into(),
                detail: "bridge did not reach the replay checkout target".into(),
            });
        }

        let replay_batch = self.git_change_sets_from_commits(&git, &selection.commits)?;
        let conflict_coverage = self.git_change_sets_from_commits(&git, &conflict_commits)?;
        let conflict_coverage_skipped = conflict_commits.len() - conflict_coverage.len();

        debug!(
            replay = replay_batch.len(),
            conflict_coverage = conflict_coverage.len(),
            conflict_coverage_skipped,
            has_more = selection.has_more,
            reset_target = %reset_target,
            "fetched Git change sets"
        );
        Ok(GitFetchResult {
            replay_batch,
            conflict_coverage,
            conflict_coverage_skipped,
            has_more: selection.has_more,
            reset_target,
            pending_total,
            deferred_mixed_pending: false,
            continuation_to_persist,
            clear_continuation,
        })
    }

    fn git_change_sets_from_commits(
        &self,
        git: &GitClient,
        commits: &[crate::git::client::GitCommitInfo],
    ) -> Result<Vec<GitChangeSet>, SyncError> {
        let mut change_sets: Vec<GitChangeSet> = Vec::new();
        for c in commits {
            if self.should_skip_incoming_git_commit(&c.sha, &c.message)? {
                continue;
            }
            if let Some(rid) = self.effective_repo_id() {
                if crate::skip_commit::is_commit_excluded(&self.db, rid, &c.sha)
                    .map_err(SyncError::DatabaseError)?
                {
                    debug!(repo_id = rid, git_sha = %c.sha, "skipping excluded Git commit");
                    continue;
                }
            }
            let files = git.get_changed_files(&c.sha).map_err(SyncError::GitError)?;
            let changed_files: Vec<ChangedFile> = files
                .into_iter()
                .map(|change| ChangedFile {
                    path: change.path,
                    action: change.action,
                    content: None,
                    is_binary: false,
                    rename_from: change.rename_from,
                })
                .collect();
            change_sets.push(GitChangeSet {
                sha: c.sha.clone(),
                author_name: c.author_name.clone(),
                author_email: c.author_email.clone(),
                message: c.message.clone(),
                changed_files,
            });
        }
        Ok(change_sets)
    }

    // -----------------------------------------------------------------------
    // Conflict detection
    // -----------------------------------------------------------------------

    fn detect_conflicts_internal(
        &self,
        svn_changes: &[SvnChangeSet],
        git_changes: &[GitChangeSet],
    ) -> Result<Vec<Conflict>, SyncError> {
        // SVN paths from the changeset include the branch prefix
        // (e.g. `trunk/README.md`), but Git paths are relative to the repo
        // root (e.g. `README.md`). Strip the SVN branch prefix so the
        // detector can match paths correctly.
        let trunk_path = self.config.svn.trunk_path.trim_matches('/');
        let strip_prefix = |p: &str| -> String {
            let p = p.trim_start_matches('/');
            if !trunk_path.is_empty() {
                if let Some(rest) = p.strip_prefix(trunk_path) {
                    return rest.trim_start_matches('/').to_string();
                }
            }
            // Also handle hard-coded "trunk/" prefix as fallback for repos
            // configured with empty trunk_path but synced from a trunk URL.
            if let Some(rest) = p.strip_prefix("trunk/") {
                return rest.to_string();
            }
            p.to_string()
        };

        let svn_file_changes: Vec<FileChange> = svn_changes
            .iter()
            .flat_map(|cs| {
                cs.changed_files.iter().map(|f| FileChange {
                    path: strip_prefix(&f.path),
                    change_kind: match f.action.as_str() {
                        "A" => ChangeKind::Added,
                        "D" => ChangeKind::Deleted,
                        "M" => ChangeKind::Modified,
                        _ => ChangeKind::Modified,
                    },
                    content: f.content.clone(),
                    is_binary: f.is_binary,
                })
            })
            .collect();

        let git_file_changes: Vec<FileChange> = git_changes
            .iter()
            .flat_map(|cs| cs.changed_files.iter())
            .map(|f| {
                let change_kind =
                    git_action_to_change_kind(&f.action, &f.path, f.rename_from.as_deref())
                        .map_err(|err| {
                            SyncError::GitError(crate::errors::GitError::ApplyFailed(
                                err.to_string(),
                            ))
                        })?;
                Ok(FileChange {
                    path: f.path.trim_start_matches('/').to_string(),
                    change_kind,
                    content: f.content.clone(),
                    is_binary: f.is_binary,
                })
            })
            .collect::<Result<Vec<_>, SyncError>>()?;

        Ok(ConflictDetector::detect(
            &svn_file_changes,
            &git_file_changes,
        ))
    }

    // -----------------------------------------------------------------------
    // Credential hot-reload
    // -----------------------------------------------------------------------

    /// Fixture-only accessor for credential isolation proofs.
    #[doc(hidden)]
    pub fn fixture_svn_password_marker(&self) -> String {
        self.svn_client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .fixture_password_marker()
            .to_string()
    }

    /// Re-read SVN password and Git token from the DB so that credentials
    /// saved via the repo detail page take effect without a daemon restart.
    /// Uses the same scoped resolver as the scheduler: repo → parent chain →
    /// global, with explicit per-repo revocation blocking broader fallback.
    fn reload_credentials(&self) {
        use crate::db::queries::CredentialChainState;

        let svn_state = match self.repo_id.as_deref() {
            Some(rid) => self
                .db
                .resolve_credential_chain_state(rid, "secret_svn_password"),
            None => match self.db.get_state("secret_svn_password") {
                Ok(Some(pw)) if !pw.is_empty() => CredentialChainState::resolved(pw),
                Ok(Some(_)) | Ok(None) | Err(_) => CredentialChainState::not_found(),
            },
        };
        {
            let mut svn = self.svn_client.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(pw) = svn_state.value {
                svn.set_password(pw);
                debug!("reloaded SVN password from database");
            } else if svn_state.explicitly_revoked {
                svn.set_password("");
                debug!("cleared SVN password after explicit revocation");
            }
        }

        let git_state = match self.repo_id.as_deref() {
            Some(rid) => self
                .db
                .resolve_credential_chain_state(rid, "secret_git_token"),
            None => match self.db.get_state("secret_git_token") {
                Ok(Some(tok)) if !tok.is_empty() => CredentialChainState::resolved(tok),
                Ok(Some(_)) | Ok(None) | Err(_) => CredentialChainState::not_found(),
            },
        };
        {
            let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
            match crate::git::apply_git_credential_chain_state(&git, "origin", &git_state) {
                Ok(()) if git_state.value.is_some() => {
                    debug!("reloaded Git token from database");
                }
                Ok(()) if git_state.explicitly_revoked => {
                    debug!("cleared embedded git credentials after explicit revocation");
                }
                Ok(()) => {}
                Err(e) => warn!("failed to apply git credential chain state: {e}"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Watermark auto-detection
    // -----------------------------------------------------------------------

    /// Scan the git log for sync markers to find the highest SVN revision
    /// already present. This prevents duplicate commits on clean installs
    /// connecting to a repo that already has synced history.
    fn detect_last_svn_rev_from_git(&self) -> i64 {
        let repo_path = {
            let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
            git.repo_path().to_path_buf()
        };

        let output = match std::process::Command::new("git")
            .args(["log", "--oneline", "-200", "--format=%s"])
            .current_dir(&repo_path)
            .output()
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => return 0,
        };

        static RE: std::sync::OnceLock<regex_lite::Regex> = std::sync::OnceLock::new();
        let re = RE.get_or_init(|| {
            regex_lite::Regex::new(r"(?i)(?:synced from |from )SVN r(\d+)").unwrap()
        });
        let mut max_rev: i64 = 0;
        for line in output.lines() {
            if let Some(caps) = re.captures(line) {
                if let Ok(rev) = caps[1].parse::<i64>() {
                    max_rev = max_rev.max(rev);
                }
            }
        }
        max_rev
    }

    // -----------------------------------------------------------------------
    // Echo detection (team: receipt-backed; personal: marker hint only)
    // -----------------------------------------------------------------------

    fn should_skip_incoming_svn_revision(
        &self,
        svn_rev: i64,
        message: &str,
    ) -> Result<bool, SyncError> {
        if let Some(repo_id) = self.effective_repo_id() {
            let projection = self.no_target_projection();
            let ctx = TeamEchoContext {
                db: &self.db,
                repo_id,
                no_target_projection: &projection,
            };
            return match classify_incoming_svn_revision(&ctx, svn_rev, message)? {
                EchoDisposition::SkipEcho => Ok(true),
                EchoDisposition::DeferPendingJournal => Err(SyncError::SvnCommitHeld {
                    reason: "pending_journal_finalize".into(),
                    detail: format!(
                        "SVN revision {svn_rev} matches a Running git-to-SVN journal without a receipt for repository {repo_id}; wait for the in-flight emit to finish, or restart the worker to hold the journal for reconciliation"
                    ),
                }),
                EchoDisposition::ApplyGenuineWithMarkerHint | EchoDisposition::ApplyGenuine => {
                    Ok(false)
                }
            };
        }
        Ok(personal_mode_marker_echo(message))
    }

    fn should_skip_incoming_git_commit(
        &self,
        git_sha: &str,
        message: &str,
    ) -> Result<bool, SyncError> {
        if let Some(repo_id) = self.effective_repo_id() {
            let projection = self.no_target_projection();
            let ctx = TeamEchoContext {
                db: &self.db,
                repo_id,
                no_target_projection: &projection,
            };
            return match classify_incoming_git_commit(&ctx, git_sha, message)
                .map_err(SyncError::DatabaseError)??
            {
                EchoDisposition::SkipEcho => Ok(true),
                EchoDisposition::DeferPendingJournal => Err(SyncError::GitPushHeld {
                    reason: "pending_journal_finalize".into(),
                    detail: format!(
                        "Git commit {git_sha} matches a Running svn-to-Git journal without a receipt for repository {repo_id}; wait for the in-flight emit to finish, or restart the worker to hold the journal for reconciliation"
                    ),
                }),
                EchoDisposition::ApplyGenuineWithMarkerHint | EchoDisposition::ApplyGenuine => {
                    Ok(false)
                }
            };
        }
        Ok(personal_mode_marker_echo(message))
    }

    fn try_auto_merge(&self, conflict: &Conflict) -> bool {
        let (base, ours, theirs) = match (
            &conflict.base_content,
            &conflict.svn_content,
            &conflict.git_content,
        ) {
            (Some(b), Some(o), Some(t)) => (b.as_str(), o.as_str(), t.as_str()),
            _ => return false,
        };

        if Merger::can_auto_merge(base, ours, theirs) {
            match Merger::three_way_merge(base, ours, theirs) {
                Ok(result) if !result.has_conflicts => {
                    info!(file = %conflict.file_path, "auto-merged conflict");
                    true
                }
                _ => false,
            }
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// SVN diff → git diff conversion
// ---------------------------------------------------------------------------

/// Convert SVN unified diff format to git-compatible format.
///
/// Key differences:
/// - SVN deletion: `+++ path\t(nonexistent)` → git: `+++ /dev/null`
/// - SVN new file: `--- path\t(nonexistent)` → git: `--- /dev/null`
/// - SVN revision: `--- path\t(revision N)` → git: `--- path` (strip annotation)
///
/// Without this conversion, `git apply` treats SVN deletions as "truncate
/// to zero bytes" instead of actually deleting the file, because it doesn't
/// recognize `(nonexistent)` as a deletion marker.
fn convert_svn_diff_to_git(svn_diff: &str) -> String {
    let mut result = String::with_capacity(svn_diff.len());
    for line in svn_diff.lines() {
        if line.starts_with("--- ") && line.contains("\t(nonexistent)") {
            // New file: source didn't exist → /dev/null
            result.push_str("--- /dev/null");
        } else if line.starts_with("+++ ") && line.contains("\t(nonexistent)") {
            // Deleted file: target doesn't exist → /dev/null
            result.push_str("+++ /dev/null");
        } else if line.starts_with("--- ") && line.contains("\t(revision ") {
            // Existing file: strip the "(revision N)" annotation
            if let Some(tab_pos) = line.find('\t') {
                result.push_str(&line[..tab_pos]);
            } else {
                result.push_str(line);
            }
        } else if line.starts_with("+++ ") && line.contains("\t(revision ") {
            // Modified file target: strip annotation
            if let Some(tab_pos) = line.find('\t') {
                result.push_str(&line[..tab_pos]);
            } else {
                result.push_str(line);
            }
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }
    result
}

// ---------------------------------------------------------------------------
// Standalone diff application (avoids holding GitClient across await points)
// ---------------------------------------------------------------------------

/// Apply a unified diff to a git repository at the given path.
///
/// This is a standalone async function that does not hold a reference to
/// `GitClient`, avoiding `Send` issues with `git2::Repository`.
pub async fn apply_diff_to_path(
    repo_path: &std::path::Path,
    diff_content: &str,
) -> Result<(), crate::errors::GitError> {
    apply_diff_to_path_revision(repo_path, diff_content, None, None).await
}

/// The existing Git apply path with import-scoped subprocess supervision.
pub async fn apply_diff_to_path_for_import(
    repo_path: &std::path::Path,
    diff_content: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<(), crate::errors::GitError> {
    apply_diff_to_path_revision(repo_path, diff_content, None, Some(cancel)).await
}

async fn apply_diff_to_path_revision(
    repo_path: &std::path::Path,
    diff_content: &str,
    revision: Option<i64>,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<(), crate::errors::GitError> {
    use std::process::Stdio;
    use tokio::process::Command;
    let mut cmd = if cancel.is_some() {
        crate::process::import_git_command().map_err(crate::errors::GitError::IoError)?
    } else {
        Command::new("git")
    };
    cmd.current_dir(repo_path)
        .args(["apply", "--3way", "-p0", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Only debug fixture runs may replace one exact SVN revision's patch
    // bytes. The production git-apply subprocess and error path still run.
    #[cfg(debug_assertions)]
    let injected = std::env::var("REPOSYNC_TEST_SVN_APPLY_FAULT")
        .ok()
        .and_then(|value| {
            value
                .split_once('|')
                .map(|(rev, path)| (rev.to_string(), path.to_string()))
        })
        .is_some_and(|(rev, path)| {
            revision.is_some_and(|actual| rev == actual.to_string())
                && std::path::Path::new(&path) == repo_path
        });
    #[cfg(not(debug_assertions))]
    let _ = revision;
    #[cfg(not(debug_assertions))]
    let injected = false;
    let input = if injected {
        b"invalid fixture patch\n".as_slice()
    } else {
        diff_content.as_bytes()
    };
    let output = if let Some(cancel) = cancel {
        // A regular temporary file replaces the pipe. Git can stall before
        // reading input without blocking this worker's stdin delivery.
        crate::process::run_with_input(
            cmd,
            input,
            std::time::Duration::from_secs(120),
            Some(cancel),
        )
        .await
        .map_err(crate::errors::GitError::IoError)?
    } else {
        cmd.stdin(Stdio::piped());
        let mut child = cmd.spawn().map_err(crate::errors::GitError::IoError)?;
        // Write diff to stdin and explicitly close it so git apply sees EOF
        // and begins processing. Without closing, git apply may hang forever.
        {
            let mut stdin = child.stdin.take().ok_or_else(|| {
                crate::errors::GitError::IoError(std::io::Error::other(
                    "failed to open git apply stdin",
                ))
            })?;
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(input)
                .await
                .map_err(crate::errors::GitError::IoError)?;
        }
        child
            .wait_with_output()
            .await
            .map_err(crate::errors::GitError::IoError)?
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        tracing::warn!(%stderr, "git apply failed");
        return Err(crate::errors::GitError::ApplyFailed(stderr));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sync lock RAII guard
// ---------------------------------------------------------------------------

/// Drop guard that resets the `running` flag to `false`.
///
/// This ensures the sync lock is always released, even if a sync cycle panics.
struct SyncLockGuard(Arc<AtomicBool>);

impl Drop for SyncLockGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// Internal change-set types
// ---------------------------------------------------------------------------

/// A set of changes from a single SVN revision.
#[derive(Debug, Clone)]
pub struct SvnChangeSet {
    pub revision: i64,
    pub author: String,
    pub date: String,
    pub message: String,
    pub changed_files: Vec<ChangedFile>,
    pub diff_content: Option<String>,
}

/// A set of changes from a single Git commit.
#[derive(Debug, Clone)]
pub struct GitChangeSet {
    pub sha: String,
    pub author_name: String,
    pub author_email: String,
    pub message: String,
    pub changed_files: Vec<ChangedFile>,
}

/// A single file changed in a commit.
#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    pub action: String,
    pub content: Option<String>,
    pub is_binary: bool,
    pub rename_from: Option<String>,
}

/// Validate file paths against allowed/blocked rules.
///
/// Uses the same component-aware matcher as the pre-mutation projector,
/// including deletes. Extracted for testability.
pub(crate) fn validate_file_paths_impl(
    allowed_paths: &[String],
    blocked_patterns: &[String],
    files: &[(String, String, Option<Vec<u8>>)],
) -> Result<(), Vec<String>> {
    if allowed_paths.is_empty() && blocked_patterns.is_empty() {
        return Ok(());
    }
    let mut violations = Vec::new();
    for (_action, path, _) in files {
        if !crate::path_projection::path_is_projected(path, allowed_paths, blocked_patterns) {
            violations.push(crate::path_projection::exclusion_message(
                path,
                allowed_paths,
                blocked_patterns,
            ));
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::IdentityConfig;
    use crate::db::git_push_operations::{git_push_target_fingerprint, GitPushIntent};
    use crate::db::svn_commit_operations::{svn_commit_target_fingerprint, SvnCommitIntent};
    use crate::identity::IdentityMapper;

    fn team_echo_engine(repo_id: &str) -> SyncEngine {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
                 VALUES (?1,'p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','idle',0,0)",
                [repo_id],
            )
            .unwrap();
        let config: AppConfig = toml::from_str(
            r#"
[daemon]
[svn]
url = "file:///svn"
username = ""
[github]
repo = "test/test-repo"
"#,
        )
        .unwrap();
        let git_dir = tempfile::tempdir().unwrap();
        let git_client = GitClient::init(git_dir.path()).unwrap();
        let mapper = IdentityMapper::new(&IdentityConfig {
            email_domain: Some("example.com".into()),
            ..Default::default()
        })
        .unwrap();
        let mut engine = SyncEngine::new(
            config,
            db,
            SvnClient::new("file:///svn", "", ""),
            git_client,
            Arc::new(mapper),
        );
        engine.set_repo_id(repo_id.into());
        engine
    }

    fn begin_running_svn_to_git_push(db: &Database, repo_id: &str, git_sha: &str) {
        let fingerprint = git_push_target_fingerprint(repo_id, "origin", "main");
        db.begin_svn_to_git_push(GitPushIntent {
            repo_id,
            initiator_id: "worker",
            request_id: "req-1",
            target_fingerprint: &fingerprint,
            source_svn_rev: 3,
            source_svn_author: "dev",
            source_svn_message: "add feature",
            pre_push_git_remote: "origin",
            pre_push_git_branch: "main",
            pre_push_git_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            pre_push_git_tree: Some("cccccccccccccccccccccccccccccccccccccccc"),
            intended_local_git_sha: git_sha,
            intended_local_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            intended_local_git_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        })
        .unwrap();
    }

    fn begin_running_git_to_svn_commit(db: &Database, repo_id: &str, pre_write_svn_rev: i64) {
        let git_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let fingerprint = svn_commit_target_fingerprint(repo_id, "uuid", "/repo", "/repo", "{}");
        db.begin_git_to_svn_commit(SvnCommitIntent {
            repo_id,
            initiator_id: "worker",
            request_id: "req-1",
            target_fingerprint: &fingerprint,
            source_git_sha: git_sha,
            source_git_parent: Some("0000000000000000000000000000000000000000"),
            source_git_tree: "cccccccccccccccccccccccccccccccccccccccc",
            target_svn_uuid: "uuid",
            target_svn_path: "/repo",
            target_svn_root_url: "/repo",
            target_svn_branch_path: "",
            pre_write_svn_rev,
            pre_write_svn_tree: "dddddddddddddddddddddddddddddddddddddddd",
            projection: "{}",
            intended_changed_paths: vec![],
            intended_svn_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            author: "dev",
            source_message: "feature",
        })
        .unwrap();
    }

    #[test]
    fn pending_journal_finalize_surfaces_for_git_echo() {
        let git_sha = "dddddddddddddddddddddddddddddddddddddddd";
        let engine = team_echo_engine("pair");
        begin_running_svn_to_git_push(engine.db(), "pair", git_sha);
        let marker = format!("synced\n\n{SYNC_MARKER} synced from SVN r3");
        let err = engine
            .should_skip_incoming_git_commit(git_sha, &marker)
            .unwrap_err();
        assert!(matches!(
            err,
            SyncError::GitPushHeld {
                reason,
                detail,
            } if reason == "pending_journal_finalize"
                && detail.contains("matches a Running svn-to-Git journal")
                && detail.contains("wait for the in-flight emit")
        ));
    }

    #[test]
    fn pending_journal_finalize_surfaces_for_svn_echo() {
        let engine = team_echo_engine("pair");
        begin_running_git_to_svn_commit(engine.db(), "pair", 4);
        let marker = format!(
            "synced\n\n{SYNC_MARKER} synced from Git {}",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        let err = engine
            .should_skip_incoming_svn_revision(5, &marker)
            .unwrap_err();
        assert!(matches!(
            err,
            SyncError::SvnCommitHeld {
                reason,
                detail,
            } if reason == "pending_journal_finalize"
                && detail.contains("matches a Running git-to-SVN journal")
                && detail.contains("wait for the in-flight emit")
        ));
    }

    #[test]
    fn running_git_journal_with_different_sha_applies_as_genuine() {
        let journal_sha = "dddddddddddddddddddddddddddddddddddddddd";
        let other_sha = "ffffffffffffffffffffffffffffffffffffffff";
        let engine = team_echo_engine("pair");
        begin_running_svn_to_git_push(engine.db(), "pair", journal_sha);
        let marker = format!("user work\n\n{SYNC_MARKER} synced from SVN r3");
        assert!(!engine
            .should_skip_incoming_git_commit(other_sha, &marker)
            .unwrap());
        assert!(!engine
            .should_skip_incoming_git_commit(other_sha, "no marker")
            .unwrap());
    }

    #[test]
    fn running_git_journal_without_marker_defers_only_matching_sha() {
        let git_sha = "dddddddddddddddddddddddddddddddddddddddd";
        let other_sha = "ffffffffffffffffffffffffffffffffffffffff";
        let engine = team_echo_engine("pair");
        begin_running_svn_to_git_push(engine.db(), "pair", git_sha);
        assert!(matches!(
            engine.should_skip_incoming_git_commit(git_sha, "edited away marker"),
            Err(SyncError::GitPushHeld {
                reason,
                ..
            }) if reason == "pending_journal_finalize"
        ));
        assert!(!engine
            .should_skip_incoming_git_commit(other_sha, "no marker")
            .unwrap());
    }

    #[test]
    fn running_svn_journal_without_marker_defers_only_matching_rev() {
        let engine = team_echo_engine("pair");
        begin_running_git_to_svn_commit(engine.db(), "pair", 4);
        assert!(matches!(
            engine.should_skip_incoming_svn_revision(5, "edited away marker"),
            Err(SyncError::SvnCommitHeld {
                reason,
                detail,
            }) if reason == "pending_journal_finalize"
                && detail.contains("matches a Running git-to-SVN journal")
                && detail.contains("wait for the in-flight emit")
        ));
        assert!(!engine
            .should_skip_incoming_svn_revision(6, "no marker")
            .unwrap());
    }

    #[test]
    fn running_svn_journal_with_different_rev_applies_as_genuine() {
        let engine = team_echo_engine("pair");
        begin_running_git_to_svn_commit(engine.db(), "pair", 4);
        let marker = format!(
            "synced\n\n{SYNC_MARKER} synced from Git {}",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(!engine
            .should_skip_incoming_svn_revision(6, &marker)
            .unwrap());
        assert!(!engine
            .should_skip_incoming_svn_revision(6, "no marker")
            .unwrap());
    }

    #[test]
    fn svn_no_target_receipt_skips_only_when_admission_scoped() {
        let engine = team_echo_engine("pair");
        let svn_rev = 9_i64;
        let projection = engine.no_target_projection();
        let receipt = serde_json::json!({
            "version": 1,
            "repo_id": "pair",
            "svn_revision": svn_rev,
            "outcome": "no_git_content",
            "projection": projection,
        });
        engine
            .db()
            .set_state(
                &format!("handled_svn_no_target_pair_{svn_rev}"),
                &receipt.to_string(),
            )
            .unwrap();
        assert!(engine
            .should_skip_incoming_svn_revision(svn_rev, "no marker")
            .unwrap());

        let other_rev = 10_i64;
        let cross_repo = serde_json::json!({
            "version": 1,
            "repo_id": "other",
            "svn_revision": other_rev,
            "outcome": "no_git_content",
            "projection": projection,
        });
        engine
            .db()
            .set_state(
                &format!("handled_svn_no_target_other_{other_rev}"),
                &cross_repo.to_string(),
            )
            .unwrap();
        assert!(!engine
            .should_skip_incoming_svn_revision(other_rev, "no marker")
            .unwrap());

        let malformed_rev = 11_i64;
        engine
            .db()
            .set_state(
                &format!("handled_svn_no_target_pair_{malformed_rev}"),
                "not-json",
            )
            .unwrap();
        assert!(!engine
            .should_skip_incoming_svn_revision(malformed_rev, "no marker")
            .unwrap());

        let weak_rev = 12_i64;
        let weak = serde_json::json!({
            "version": 3,
            "repo_id": "pair",
            "svn_revision": weak_rev,
            "outcome": "no_git_content",
            "projection": projection,
        });
        engine
            .db()
            .set_state(
                &format!("handled_svn_no_target_pair_{weak_rev}"),
                &weak.to_string(),
            )
            .unwrap();
        assert!(!engine
            .should_skip_incoming_svn_revision(weak_rev, "no marker")
            .unwrap());
    }

    #[test]
    fn test_personal_mode_marker_echo() {
        assert!(personal_mode_marker_echo(
            "Fix bug\n\n[reposync] synced from SVN r42"
        ));
        assert!(!personal_mode_marker_echo("Fix bug in authentication"));
    }

    #[test]
    fn test_sync_state_display() {
        assert_eq!(SyncState::Idle.to_string(), "idle");
        assert_eq!(SyncState::Detecting.to_string(), "detecting");
        assert_eq!(SyncState::Applying.to_string(), "applying");
        assert_eq!(SyncState::Committed.to_string(), "committed");
        assert_eq!(SyncState::ConflictFound.to_string(), "conflict_found");
        assert_eq!(
            SyncState::QueuedForResolution.to_string(),
            "queued_for_resolution"
        );
        assert_eq!(
            SyncState::ResolutionApplied.to_string(),
            "resolution_applied"
        );
    }

    // ---- validate_file_paths tests ----

    fn make_file(action: &str, path: &str) -> (String, String, Option<Vec<u8>>) {
        (action.to_string(), path.to_string(), None)
    }

    #[test]
    fn test_no_rules_allows_everything() {
        let files = vec![make_file("A", "anything.txt")];
        assert!(validate_file_paths_impl(&[], &[], &files).is_ok());
    }

    #[test]
    fn test_allowed_paths_valid_file() {
        let allowed = vec!["source/".to_string()];
        let files = vec![make_file("A", "source/foo.txt")];
        assert!(validate_file_paths_impl(&allowed, &[], &files).is_ok());
    }

    #[test]
    fn test_allowed_paths_invalid_file() {
        let allowed = vec!["source/".to_string()];
        let files = vec![make_file("A", "root.txt")];
        let result = validate_file_paths_impl(&allowed, &[], &files);
        assert!(result.is_err());
        let violations = result.unwrap_err();
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("root.txt"));
    }

    #[test]
    fn test_allowed_paths_multiple_prefixes() {
        let allowed = vec!["source/".to_string(), "config/".to_string()];
        let files = vec![
            make_file("A", "source/foo.rs"),
            make_file("M", "config/app.toml"),
        ];
        assert!(validate_file_paths_impl(&allowed, &[], &files).is_ok());
    }

    #[test]
    fn test_allowed_paths_mixed_valid_invalid() {
        let allowed = vec!["source/".to_string()];
        let files = vec![
            make_file("A", "source/good.rs"),
            make_file("A", "bad.txt"),
            make_file("A", "source/also-good.rs"),
        ];
        let result = validate_file_paths_impl(&allowed, &[], &files);
        assert!(result.is_err());
        let violations = result.unwrap_err();
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("bad.txt"));
    }

    #[test]
    fn test_delete_out_of_scope_is_excluded() {
        let allowed = vec!["source/".to_string()];
        let files = vec![make_file("D", "root-level-file.txt")];
        let result = validate_file_paths_impl(&allowed, &[], &files);
        assert!(result.is_err());
        let violations = result.unwrap_err();
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("root-level-file.txt"));
    }

    #[test]
    fn test_delete_in_scope_passes() {
        let allowed = vec!["source/".to_string()];
        let files = vec![make_file("D", "source/gone.txt")];
        assert!(validate_file_paths_impl(&allowed, &[], &files).is_ok());
    }

    #[test]
    fn test_allowed_prefix_does_not_match_sibling() {
        let allowed = vec!["team".to_string()];
        let files = vec![
            make_file("A", "team/ok.txt"),
            make_file("A", "team-other/leak.txt"),
        ];
        let result = validate_file_paths_impl(&allowed, &[], &files);
        assert!(result.is_err());
        let violations = result.unwrap_err();
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("team-other/leak.txt"));
    }

    #[test]
    fn test_blocked_suffix_pattern() {
        let blocked = vec!["*.exe".to_string()];
        let files = vec![make_file("A", "program.exe")];
        let result = validate_file_paths_impl(&[], &blocked, &files);
        assert!(result.is_err());
    }

    #[test]
    fn test_blocked_prefix_pattern() {
        let blocked = vec!["temp/".to_string()];
        let files = vec![make_file("A", "temp/data.txt")];
        let result = validate_file_paths_impl(&[], &blocked, &files);
        assert!(result.is_err());
    }

    #[test]
    fn test_blocked_no_match_passes() {
        let blocked = vec!["*.exe".to_string()];
        let files = vec![make_file("A", "source/foo.rs")];
        assert!(validate_file_paths_impl(&[], &blocked, &files).is_ok());
    }

    #[test]
    fn incomplete_conflict_coverage_fails_closed() {
        let engine = team_echo_engine("pair");
        let err = engine.ensure_conflict_coverage_before_detection(true, 4, 3);
        assert!(matches!(
            err,
            Err(SyncError::HistoryBlocked {
                reason,
                ..
            }) if reason == "incomplete_conflict_coverage"
        ));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn incomplete_conflict_coverage_test_fault_is_per_engine() {
        let left = team_echo_engine("left");
        let right = team_echo_engine("right");
        left.set_incomplete_conflict_coverage_test_fault(true);
        assert!(left.incomplete_conflict_coverage_test_fault_enabled());
        assert!(!right.incomplete_conflict_coverage_test_fault_enabled());
        left.set_incomplete_conflict_coverage_test_fault(false);
        assert!(!left.incomplete_conflict_coverage_test_fault_enabled());
    }
}
