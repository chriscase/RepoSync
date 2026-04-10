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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::config::{AppConfig, SvnLayout};
use crate::conflict::detector::{ChangeKind, ConflictDetector, FileChange};
use crate::conflict::merger::Merger;
use crate::conflict::Conflict;
use crate::db::Database;
use crate::errors::SyncError;
use crate::git::client::GitClient;
use crate::identity::IdentityMapper;
use crate::models::AuditEntry;
use crate::svn::client::SvnClient;

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

/// Statistics from a single sync cycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncStats {
    pub svn_to_git_count: usize,
    pub git_to_svn_count: usize,
    pub conflicts_detected: usize,
    pub conflicts_auto_resolved: usize,
    pub started_at: String,
    pub completed_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Marker string embedded in sync-generated commit messages for echo detection.
const SYNC_MARKER: &str = "[reposync]";

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

    /// Validate file paths against allowed/blocked rules.
    /// Returns Ok(()) if all paths are valid, Err with list of violations.
    fn validate_file_paths(&self, files: &[(String, String, Option<Vec<u8>>)]) -> Result<(), Vec<String>> {
        if self.allowed_paths.is_empty() && self.blocked_patterns.is_empty() {
            return Ok(());
        }
        let mut violations = Vec::new();
        for (action, path, _) in files {
            if action == "D" { continue; } // deletions are always ok
            // Check allowed paths
            if !self.allowed_paths.is_empty() {
                let allowed = self.allowed_paths.iter().any(|prefix| path.starts_with(prefix));
                if !allowed {
                    violations.push(format!("'{}' not under allowed paths {:?}", path, self.allowed_paths));
                }
            }
            // Check blocked patterns (simple suffix/prefix matching)
            for pattern in &self.blocked_patterns {
                let matches = if pattern.starts_with('*') {
                    // Suffix match: *.exe matches foo.exe
                    path.ends_with(&pattern[1..])
                } else if pattern.ends_with('/') {
                    // Prefix match: temp/ matches temp/foo.txt
                    path.starts_with(pattern)
                } else {
                    path == pattern || path.starts_with(&format!("{}/", pattern))
                };
                if matches {
                    violations.push(format!("'{}' matches blocked pattern '{}'", path, pattern));
                }
            }
        }
        if violations.is_empty() { Ok(()) } else { Err(violations) }
    }

    /// Return the kv_state key for the last SVN revision watermark.
    /// Uses per-repo key if repo_id is set, otherwise global key.
    fn svn_rev_key(&self) -> String {
        match &self.repo_id {
            Some(rid) if !rid.is_empty() => format!("last_svn_rev_{}", rid),
            _ => "last_svn_rev".to_string(),
        }
    }

    /// Return the effective repo_id if set and non-empty, for repo-table watermark operations.
    fn effective_repo_id(&self) -> Option<&str> {
        self.repo_id.as_deref().filter(|id| !id.is_empty())
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

        // Store the sync state
        let _ = self.db.set_state("sync_state", "detecting");

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
            Err(e) => ("error", format!("sync failed: {}", e)),
        };

        let _ = self.db.set_state("sync_state", final_state);
        let _ = self.db.set_state("last_sync_at", &Utc::now().to_rfc3339());
        stats.completed_at = Some(Utc::now().to_rfc3339());

        // Update per-repo sync status and error count
        if let Some(rid) = self.effective_repo_id() {
            let _ = self.db.update_repo_sync_status(rid, final_state);
            if result.is_err() {
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

        // Lock is released by _guard drop (happens here at scope end).
        result.map(|()| stats)
    }

    /// Get a status summary.
    pub fn get_status(&self) -> Result<crate::models::SyncStatus, SyncError> {
        // Use the consolidated summary query to reduce mutex acquisitions
        // (1 query instead of 8+ separate queries).
        let summary = self.db.get_status_summary(self.repo_id.as_deref())
            .map_err(SyncError::DatabaseError)?;

        let last_sync_at = summary.last_sync_at.and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        });

        // SVN rev and git hash need per-repo key lookups not in the summary
        let last_svn_rev = match self
            .db
            .get_state(&self.svn_rev_key())
            .map_err(SyncError::DatabaseError)?
        {
            Some(s) => s.parse::<i64>().ok(),
            None => self
                .db
                .get_last_svn_revision()
                .map_err(SyncError::DatabaseError)?,
        };
        let last_git_hash = match self
            .db
            .get_state("last_git_hash")
            .map_err(SyncError::DatabaseError)?
        {
            Some(s) if !s.is_empty() => Some(s),
            _ => self
                .db
                .get_last_git_hash()
                .map_err(SyncError::DatabaseError)?,
        };
        let last_error_at = self.db.last_error_at().map_err(SyncError::DatabaseError)?;

        let uptime = (Utc::now() - self.started_at).num_seconds().max(0) as u64;

        Ok(crate::models::SyncStatus {
            state: crate::models::SyncState::from_str_val(&summary.sync_state),
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

    async fn do_sync_cycle(&self, stats: &mut SyncStats) -> Result<(), SyncError> {
        // 1. Fetch changes from both sides.
        let svn_changes = self.fetch_svn_changes().await?;
        let git_changes = self.fetch_git_changes().await?;

        // 2. Detect conflicts.
        let conflicts = self.detect_conflicts_internal(&svn_changes, &git_changes);
        stats.conflicts_detected = conflicts.len();

        if !conflicts.is_empty() {
            info!(count = conflicts.len(), "conflicts detected");
            let _ = self.db.set_state("sync_state", "conflict_found");

            for conflict in &conflicts {
                if self.config.sync.auto_merge && self.try_auto_merge(conflict) {
                    stats.conflicts_auto_resolved += 1;
                } else {
                    // Persist unresolved conflict
                    let mut db_conflict = crate::models::Conflict::new(conflict.file_path.clone());
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
        let _ = self.db.set_state("sync_state", "applying");
        stats.svn_to_git_count = self.sync_svn_to_git(&svn_changes).await?;

        // 4. Apply Git -> SVN.
        stats.git_to_svn_count = self.sync_git_to_svn(&git_changes).await?;

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
    async fn sync_svn_to_git(&self, svn_changes: &[SvnChangeSet]) -> Result<usize, SyncError> {
        let mut count = 0;

        for change in svn_changes {
            if self.is_echo_commit(&change.message) {
                debug!(rev = change.revision, "skipping echo SVN revision");
                continue;
            }

            let git_identity = self
                .identity_mapper
                .svn_to_git(&change.author)
                .map_err(SyncError::IdentityError)?;

            // 1. Get the SVN diff for this revision.
            let svn = self.svn_client.lock().unwrap_or_else(|p| p.into_inner()).clone();
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
                    diff.replace(
                        &format!("a/{}/", tp),
                        "a/",
                    ).replace(
                        &format!("b/{}/", tp),
                        "b/",
                    )
                } else {
                    diff
                }
            } else {
                diff
            };
            let diff_applied = if !processed_diff.trim().is_empty() {
                apply_diff_to_path(&repo_path, &processed_diff).await.is_ok()
            } else {
                false
            };

            if !diff_applied {
                // Fallback path: git apply couldn't patch the diff, so
                // reconstruct the target state from an authoritative
                // full-tree snapshot of the revision via `svn export`.
                //
                // The export is the ONLY source of truth here. We do
                // NOT iterate `change.changed_files` — those paths come
                // from the SVN log and are repository-relative (e.g.
                // `/branches/.../trunk/source/SLS/foo.js`), which does
                // not match the export dir layout (`source/SLS/foo.js`
                // because the export is rooted at the branch URL). If
                // we honored those paths we'd either miss files or
                // write them to the wrong place, producing phantom
                // top-level directories in the git tree.
                //
                // Instead we mirror the behavior of `run_full_import`:
                // export the whole tree at this revision, remove any
                // files in the git working tree that are NOT in the
                // export (stale cleanup), then copy-with-policy every
                // file from the export over the working tree. This
                // matches the branch state exactly.
                let export_dir = tempfile::tempdir()
                    .map_err(|e| SyncError::GitError(crate::errors::GitError::IoError(e)))?;
                let export_path = if self.config.svn.layout == SvnLayout::Standard {
                    self.config.svn.trunk_path.trim_matches('/').to_string()
                } else {
                    String::new()
                };
                {
                    let svn = self.svn_client.lock().unwrap_or_else(|p| p.into_inner()).clone();
                    svn.export(&export_path, change.revision, export_dir.path())
                        .await
                        .map_err(SyncError::SvnError)?;
                }

                // Stale cleanup: anything in the git working tree that's
                // not present in the export must go (excluding .git).
                crate::import::remove_stale_files(export_dir.path(), &repo_path).map_err(
                    |e| {
                        SyncError::GitError(crate::errors::GitError::ApplyFailed(format!(
                            "stale file cleanup failed at r{}: {}",
                            change.revision, e
                        )))
                    },
                )?;

                // Mirror export → working tree with LFS enforcement.
                // Use the repo's LFS threshold so large files are
                // tracked via Git LFS instead of committed as blobs.
                let fallback_policy = crate::file_policy::FilePolicy::with_lfs(
                    0,
                    vec![],
                    self.lfs_threshold_bytes,
                    &[],
                );
                crate::import::copy_tree_with_policy(
                    export_dir.path(),
                    &repo_path,
                    &fallback_policy,
                    &self.db,
                )
                .map_err(|e| {
                    SyncError::GitError(crate::errors::GitError::ApplyFailed(format!(
                        "tree copy failed at r{}: {}",
                        change.revision, e
                    )))
                })?;
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
                                cf.path.strip_prefix(&format!("/{}/", tp))
                                    .or_else(|| cf.path.strip_prefix(&format!("{}/", tp)))
                                    .unwrap_or(&cf.path)
                            } else {
                                &cf.path
                            }
                        } else {
                            &cf.path
                        };
                        let clean = path.trim_start_matches('/');
                        if clean.is_empty() { None } else { Some(clean.to_string()) }
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

            // 3. Commit with identity and sync marker.
            let commit_message = format!(
                "{}\n\n{} synced from SVN r{}",
                change.message, SYNC_MARKER, change.revision
            );

            // Wrap commit+push in block_in_place so the synchronous git
            // CLI call doesn't block the tokio async runtime (which would
            // make the web UI unresponsive during pushes).
            let git_sha = tokio::task::block_in_place(|| {
                let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
                let oid = git
                    .commit(
                        &commit_message,
                        &git_identity.name,
                        &git_identity.email,
                        "reposync",
                        "sync@reposync.local",
                    )
                    .map_err(SyncError::GitError)?;

                // 4. Push to remote. Authentication flows through the
                // remote URL credentials set by `reload_credentials` at
                // the start of the cycle, not through a token parameter.
                let branch = &self.config.github.default_branch;
                git.push("origin", branch).map_err(SyncError::GitError)?;

                Ok::<_, SyncError>(oid.to_string())
            })?;

            // 5. Record the sync only after successful write.
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
            debug!(
                record_id = %record.id,
                direction = "svn_to_git",
                svn_rev = change.revision,
                git_sha = %&git_sha[..12.min(git_sha.len())],
                "sync_record created"
            );

            // Update the SVN watermark (dual-write: kv_state + repo table).
            let _ = self
                .db
                .set_state(&self.svn_rev_key(), &change.revision.to_string());
            if let Some(rid) = self.effective_repo_id() {
                debug!(
                    repo_id = %rid,
                    new_svn_rev = change.revision,
                    new_git_sha = %&git_sha[..12.min(git_sha.len())],
                    "watermark updated"
                );
                let _ = self.db.update_repo_watermark(rid, change.revision, &git_sha);
                let _ = self.db.increment_repo_sync_count(rid);
            }

            count += 1;

            // Audit log for successful sync
            let _ = self.db.insert_audit_log_with_repo(
                "sync_cycle",
                Some("svn_to_git"),
                Some(change.revision),
                Some(&git_sha),
                Some(&change.author),
                Some(&format!(
                    "synced SVN r{} -> Git {}",
                    change.revision,
                    &git_sha[..8.min(git_sha.len())]
                )),
                true,
                self.repo_id.as_deref(),
            );

            info!(
                rev = change.revision,
                git_sha = %git_sha,
                git_name = %git_identity.name,
                "synced SVN r{} -> Git {}",
                change.revision,
                &git_sha[..8.min(git_sha.len())]
            );
        }

        Ok(count)
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
    async fn sync_git_to_svn(&self, git_changes: &[GitChangeSet]) -> Result<usize, SyncError> {
        let mut count = 0;

        // Reuse a single SVN working copy across all commits (P4 optimization).
        // Create the tempdir once and use `svn update` between commits instead
        // of a fresh `checkout_head` per commit.
        let svn_wc_dir = tempfile::tempdir()
            .map_err(|e| SyncError::SvnError(crate::errors::SvnError::IoError(e)))?;
        let mut svn_wc_initialized = false;

        for change in git_changes {
            if self.is_echo_commit(&change.message) {
                debug!(sha = %change.sha, "skipping echo Git commit");
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
                let contents: Vec<(String, String, Option<Vec<u8>>)> = change
                    .changed_files
                    .iter()
                    .map(|f| {
                        let (action, path) = (&f.action, &f.path);
                        let content = if action != "D" {
                            git.get_file_content_at_commit(&change.sha, path)
                                .ok()
                                .flatten()
                        } else {
                            None
                        };
                        (action.clone(), path.clone(), content)
                    })
                    .collect();
                contents
            };

            // 2. Prepare SVN working copy: checkout on first use, update thereafter.
            let svn_url_for_log;
            {
                let svn = self.svn_client.lock().unwrap_or_else(|p| p.into_inner()).clone();
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
            let svn = self.svn_client.lock().unwrap_or_else(|p| p.into_inner()).clone();
            if !added_files.is_empty() {
                debug!(
                    sha = %change.sha,
                    files = ?added_files,
                    "running svn add"
                );
                svn.add(svn_wc_dir.path(), &added_files)
                    .await
                    .map_err(SyncError::SvnError)?;
            }
            if !deleted_files.is_empty() {
                debug!(
                    sha = %change.sha,
                    files = ?deleted_files,
                    "running svn rm"
                );
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
                    && !trimmed.starts_with('X')  // externals
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
                // Still advance the Git watermark so we don't retry this
                // commit on the next cycle.
                let _ = self.db.set_state("last_git_hash", &change.sha);
                if let Some(rid) = self.effective_repo_id() {
                    let _ = self.db.set_state(&format!("last_git_sha_{}", rid), &change.sha);
                    // Preserve existing SVN watermark — only update git_sha
                    let current_svn_rev = self.db.get_repo_watermark(rid)
                        .map(|(rev, _)| rev).unwrap_or(0);
                    let _ = self.db.update_repo_watermark(rid, current_svn_rev, &change.sha);
                }
                continue;
            }

            debug!(
                sha = %change.sha,
                svn_status = %svn_status,
                "SVN working copy has pending changes, committing"
            );

            // 4b. Validate file paths against allowed/blocked rules.
            if let Err(violations) = self.validate_file_paths(&file_contents) {
                warn!(
                    sha = %change.sha,
                    violations = ?violations,
                    "skipping Git-to-SVN commit: path validation failed"
                );
                if let Some(rid) = self.effective_repo_id() {
                    let _ = self.db.advance_all_watermarks(rid, &change.sha);
                }
                let _ = self.db.insert_audit_log_with_repo(
                    "path_violation_skipped",
                    Some("git_to_svn"),
                    None,
                    Some(&change.sha),
                    Some(&change.author_name),
                    Some(&format!(
                        "Skipped commit {}: {}",
                        &change.sha[..8.min(change.sha.len())],
                        violations.join("; ")
                    )),
                    false,
                    self.effective_repo_id(),
                );
                continue;
            }

            // 5. Commit to SVN.
            let commit_message = format!(
                "{}\n\n{} synced from Git {}",
                change.message,
                SYNC_MARKER,
                &change.sha[..8.min(change.sha.len())]
            );
            let svn_rev = svn
                .commit(svn_wc_dir.path(), &commit_message, &svn_username)
                .await
                .map_err(SyncError::SvnError)?;

            // 6. Record the sync only after successful write.
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
            let _ = self.db.set_state("last_git_hash", &change.sha);
            if let Some(rid) = self.effective_repo_id() {
                let _ = self.db.set_state(&format!("last_git_sha_{}", rid), &change.sha);
                let _ = self.db.update_repo_watermark(rid, svn_rev, &change.sha);
                let _ = self.db.increment_repo_sync_count(rid);
            }

            count += 1;

            // Audit log for successful sync
            let _ = self.db.insert_audit_log_with_repo(
                "sync_cycle",
                Some("git_to_svn"),
                Some(svn_rev),
                Some(&change.sha),
                Some(&change.author_name),
                Some(&format!(
                    "synced Git {} -> SVN r{}",
                    &change.sha[..8.min(change.sha.len())],
                    svn_rev
                )),
                true,
                self.repo_id.as_deref(),
            );

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
        // Try the repo table watermark first (authoritative), then fall back
        // to kv_state, then to commit_map / sync_records for legacy databases.
        let mut last_rev = 0i64;

        if let Some(rid) = self.effective_repo_id() {
            let (repo_rev, _repo_sha) = self
                .db
                .get_repo_watermark(rid)
                .map_err(SyncError::DatabaseError)?;
            if repo_rev > 0 {
                last_rev = repo_rev;
            }
        }

        if last_rev == 0 {
            last_rev = match self
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
        }

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
                let _ = self.db.set_state(&self.svn_rev_key(), &detected.to_string());
                // Also persist to the repo table if available
                if let Some(rid) = self.effective_repo_id() {
                    let _ = self.db.update_repo_watermark(rid, detected, "");
                }
                last_rev = detected;
            }
        }

        info!(since_rev = last_rev, "fetching SVN changes");

        let svn = self.svn_client.lock().unwrap_or_else(|p| p.into_inner()).clone();
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

        let change_sets: Vec<SvnChangeSet> = entries
            .into_iter()
            .filter(|e| !self.is_echo_commit(&e.message))
            .map(|e| SvnChangeSet {
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
                            if let Some(rest) = raw.strip_prefix(prefix.as_str()) {
                                if rest.is_empty() {
                                    return None; // skip bare trunk/ directory entry
                                }
                                rest.to_string()
                            } else {
                                return None; // skip non-trunk paths (branches/, tags/)
                            }
                        } else {
                            raw.to_string()
                        };
                        Some(ChangedFile {
                            path: mapped_path,
                            action: p.action.clone(),
                            content: None,
                            is_binary: false,
                        })
                    })
                    .collect(),
                diff_content: None,
            })
            .collect();

        debug!(count = change_sets.len(), "fetched SVN change sets");
        Ok(change_sets)
    }

    async fn fetch_git_changes(&self) -> Result<Vec<GitChangeSet>, SyncError> {
        let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());

        // Wrap the blocking git pull in block_in_place to avoid starving
        // the Tokio runtime while holding the lock for network I/O.
        let token = self.config.github.token.as_deref();
        let branch = &self.config.github.default_branch;
        tokio::task::block_in_place(|| {
            git.pull("origin", branch, token)
                .map_err(SyncError::GitError)
        })?;

        // Check per-repo key first, then global key, then commit_map fallback.
        let per_repo_key = self.effective_repo_id()
            .map(|rid| format!("last_git_sha_{}", rid));
        let repo_table_sha = self.effective_repo_id()
            .and_then(|rid| self.db.get_repo_watermark(rid).ok())
            .map(|(_, sha)| sha)
            .filter(|s| !s.is_empty());

        let last_hash = if let Some(sha) = repo_table_sha {
            // Best source: repositories table (set by sync engine + import)
            Some(sha)
        } else if let Some(ref key) = per_repo_key {
            // Per-repo kv_state key
            match self.db.get_state(key).map_err(SyncError::DatabaseError)? {
                Some(s) if !s.is_empty() => Some(s),
                _ => match self.db.get_state("last_git_hash").map_err(SyncError::DatabaseError)? {
                    Some(s) if !s.is_empty() => Some(s),
                    _ => self.db.get_last_git_hash().map_err(SyncError::DatabaseError)?,
                },
            }
        } else {
            // Global fallback
            match self.db.get_state("last_git_hash").map_err(SyncError::DatabaseError)? {
                Some(s) if !s.is_empty() => Some(s),
                _ => self.db.get_last_git_hash().map_err(SyncError::DatabaseError)?,
            }
        };

        info!(since_sha = ?last_hash, "fetching Git changes");

        let commits = git
            .get_commits_since(last_hash.as_deref(), None)
            .map_err(SyncError::GitError)?;

        let mut change_sets: Vec<GitChangeSet> = Vec::new();
        for c in commits {
            if self.is_echo_commit(&c.message) {
                continue;
            }
            // Populate changed_files from the commit's diff.
            let files = git.get_changed_files(&c.sha).map_err(SyncError::GitError)?;
            let changed_files: Vec<ChangedFile> = files
                .into_iter()
                .map(|(action, path)| ChangedFile {
                    path,
                    action,
                    content: None,
                    is_binary: false,
                })
                .collect();
            change_sets.push(GitChangeSet {
                sha: c.sha,
                author_name: c.author_name,
                author_email: c.author_email,
                message: c.message,
                changed_files,
            });
        }

        // Reverse so oldest commits are replayed first.  get_commits_since
        // returns newest-first (revwalk order), but sync_git_to_svn must
        // apply changes chronologically so the final SVN tree matches the
        // latest Git state and intermediate revisions map correctly.
        change_sets.reverse();

        debug!(count = change_sets.len(), "fetched Git change sets");
        Ok(change_sets)
    }

    // -----------------------------------------------------------------------
    // Conflict detection
    // -----------------------------------------------------------------------

    fn detect_conflicts_internal(
        &self,
        svn_changes: &[SvnChangeSet],
        git_changes: &[GitChangeSet],
    ) -> Vec<Conflict> {
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
            .flat_map(|cs| {
                cs.changed_files.iter().map(|f| FileChange {
                    path: f.path.trim_start_matches('/').to_string(),
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

        ConflictDetector::detect(&svn_file_changes, &git_file_changes)
    }

    // -----------------------------------------------------------------------
    // Credential hot-reload
    // -----------------------------------------------------------------------

    /// Re-read SVN password and Git token from the DB so that credentials
    /// saved via the repo detail page take effect without a daemon restart.
    /// Tries per-repo keys first (secret_svn_password_{repo_id}), then falls
    /// back to global keys for backward compatibility.
    fn reload_credentials(&self) {
        // SVN password — per-repo key first, then global
        let svn_pw = self
            .repo_id
            .as_ref()
            .and_then(|rid| {
                self.db
                    .get_state(&format!("secret_svn_password_{}", rid))
                    .ok()
                    .flatten()
                    .filter(|v| !v.is_empty())
            })
            .or_else(|| {
                self.db
                    .get_state("secret_svn_password")
                    .ok()
                    .flatten()
                    .filter(|v| !v.is_empty())
            });

        if let Some(pw) = svn_pw {
            let mut svn = self.svn_client.lock().unwrap_or_else(|p| p.into_inner());
            svn.set_password(pw);
            debug!("reloaded SVN password from database");
        }

        // Git token — per-repo key first, then global
        let git_tok = self
            .repo_id
            .as_ref()
            .and_then(|rid| {
                self.db
                    .get_state(&format!("secret_git_token_{}", rid))
                    .ok()
                    .flatten()
                    .filter(|v| !v.is_empty())
            })
            .or_else(|| {
                self.db
                    .get_state("secret_git_token")
                    .ok()
                    .flatten()
                    .filter(|v| !v.is_empty())
            });

        if let Some(token) = git_tok {
            let git = self.git_client.lock().unwrap_or_else(|p| p.into_inner());
            let _ = git.ensure_remote_credentials("origin", Some(&token));
            debug!("reloaded Git token from database");
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
    // Echo detection
    // -----------------------------------------------------------------------

    fn is_echo_commit(&self, message: &str) -> bool {
        message.contains(SYNC_MARKER)
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
    use std::process::Stdio;
    use tokio::process::Command;
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_path)
        .args(["apply", "--3way", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(crate::errors::GitError::IoError)?;
    if let Some(ref mut stdin) = child.stdin {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(diff_content.as_bytes())
            .await
            .map_err(crate::errors::GitError::IoError)?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(crate::errors::GitError::IoError)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_echo_commit() {
        let message_with_marker = "Fix bug\n\n[reposync] synced from SVN r42";
        assert!(message_with_marker.contains(SYNC_MARKER));

        let normal_message = "Fix bug in authentication";
        assert!(!normal_message.contains(SYNC_MARKER));
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
}
