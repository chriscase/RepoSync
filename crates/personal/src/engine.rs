//! Personal sync engine — orchestrates SVN↔Git synchronization.
//!
//! Simplified state machine for single-developer sync:
//! `Idle → PollingSvn → ApplyingSvnToGit → PollingGitPRs → ApplyingGitToSvn → Idle`

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use std::sync::Mutex;
use tracing::{error, info, warn};

use reposync_core::db::personal_scope::personal_scope_key;
use reposync_core::db::svn_commit_operations::SvnCommitOperationState;
use reposync_core::db::Database;
use reposync_core::git::client::GitClient;
use reposync_core::git::github::GitHubClient;
use reposync_core::git::{
    resolve_git_http_auth_token_for_workdir, sync_git_http_auth_from_resolution,
};
use reposync_core::history_inspect::inspect_personal_history_with_http_auth;
use reposync_core::models::PersonalSyncStats;
use reposync_core::personal_config::PersonalConfig;
use reposync_core::svn::SvnClient;

use crate::git_to_svn::GitToSvnSync;
use crate::pr_monitor::PrMonitor;
use crate::svn_to_git::SvnToGitSync;

/// Current state of the personal sync engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonalSyncState {
    Idle,
    PollingSvn,
    ApplyingSvnToGit,
    PollingGitPRs,
    ApplyingGitToSvn,
    #[allow(dead_code)]
    ConflictDetected,
    Error,
}

impl std::fmt::Display for PersonalSyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::PollingSvn => write!(f, "polling_svn"),
            Self::ApplyingSvnToGit => write!(f, "applying_svn_to_git"),
            Self::PollingGitPRs => write!(f, "polling_git_prs"),
            Self::ApplyingGitToSvn => write!(f, "applying_git_to_svn"),
            Self::ConflictDetected => write!(f, "conflict_detected"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// The personal sync engine orchestrates all sync operations.
pub struct PersonalSyncEngine {
    config: PersonalConfig,
    db: Arc<Database>,
    svn_client: SvnClient,
    git_client: Arc<Mutex<GitClient>>,
    github_client: Arc<GitHubClient>,
    running: Arc<AtomicBool>,
    state: Arc<std::sync::Mutex<PersonalSyncState>>,
    svn_wc_path: PathBuf,
    git_repo_path: PathBuf,
}

impl PersonalSyncEngine {
    /// Create a new personal sync engine.
    pub fn new(
        config: PersonalConfig,
        db: Database,
        svn_client: SvnClient,
        git_client: GitClient,
        github_client: GitHubClient,
    ) -> Self {
        let data_dir = &config.personal.data_dir;
        let svn_wc_path = data_dir.join("svn-wc");
        let git_repo_path = git_client.repo_path().to_path_buf();

        // LFS preflight: log warning (non-fatal) if LFS is configured but not available.
        if config.options.lfs_threshold > 0 {
            match reposync_core::lfs::preflight_check() {
                Ok(version) => {
                    info!(
                        version = %version,
                        lfs_threshold = config.options.lfs_threshold,
                        "Git LFS is available"
                    );
                }
                Err(e) => {
                    warn!(
                        lfs_threshold = config.options.lfs_threshold,
                        error = %e,
                        "Git LFS is configured but not available — \
                         LFS-threshold files will be synced as regular blobs"
                    );
                }
            }
        }

        Self {
            config,
            db: Arc::new(db),
            svn_client,
            git_client: Arc::new(Mutex::new(git_client)),
            github_client: Arc::new(github_client),
            running: Arc::new(AtomicBool::new(false)),
            state: Arc::new(std::sync::Mutex::new(PersonalSyncState::Idle)),
            svn_wc_path,
            git_repo_path,
        }
    }

    /// Run a single sync cycle (SVN→Git then Git→SVN via PRs).
    pub async fn run_cycle(&self) -> Result<PersonalSyncStats> {
        if self.running.swap(true, Ordering::SeqCst) {
            anyhow::bail!("sync cycle already in progress");
        }

        let _guard = RunningGuard {
            flag: self.running.clone(),
        };

        let mut stats = PersonalSyncStats {
            started_at: Some(chrono::Utc::now()),
            ..Default::default()
        };

        if !self.config.options.projected_history
            && !self.config.options.allowed_paths.is_empty()
            && self.db.active_personal_svn_commit_operation()?.is_some()
        {
            anyhow::bail!("pending Personal Git→SVN journal predates scoped inbound sync; reconcile it under its original configuration");
        }

        reposync_core::personal_projection::admit_sync(&self.db, &self.config)?;

        if self.config.options.projected_history {
            let svn = self.outbound_svn()?;
            reposync_core::personal_projection::require_projected_source(&self.db, &svn).await?;
            let git = self.git_client.lock().unwrap();
            let expected = self.config.github.clone_url();
            let origin = git.repo().find_remote("origin")?;
            anyhow::ensure!(
                origin.url() == Some(expected.as_str()),
                "Personal projected Git origin changed; preserve the mirror"
            );
        }

        // Prove and finalize any held Git→SVN journal before P/O/R/L inspection.
        // Promotion uses SVN inspect-before-author (GitHub fallback only after proof).
        if let Err(e) = self.promote_held_git_to_svn_before_history_inspect().await {
            self.set_state(PersonalSyncState::Error);
            error!(error = %e, "held personal git-to-svn promotion failed");
            self.db
                .insert_audit_log(
                    "git_to_svn",
                    Some("git_to_svn"),
                    None,
                    None,
                    None,
                    Some(&e.to_string()),
                    false,
                )
                .ok();
            return Err(e);
        }

        // Same P/O/R/L inspection as team Git→SVN, before any reset/replay.
        let admission = match self.inspect_git_history() {
            Ok(admission) => admission,
            Err(e) => {
                self.set_state(PersonalSyncState::Error);
                error!(error = %e, "personal Git history inspection blocked");
                self.db
                    .insert_audit_log(
                        "history_blocked",
                        Some("git_to_svn"),
                        None,
                        None,
                        None,
                        Some(&e.to_string()),
                        false,
                    )
                    .ok();
                return Err(e);
            }
        };

        if self.config.options.projected_history {
            let scope = &self.config;
            let admission =
                admission.context("missing admitted Git history for projected Personal mirror")?;
            let divergent = reposync_core::personal_projection::git_path_divergence(
                &self.git_repo_path,
                scope,
            )?;
            let remote = reposync_core::personal_projection::git_path_divergence_at(
                &self.git_repo_path,
                &admission.remote_tip,
                scope,
            )?;
            anyhow::ensure!(divergent.is_empty() && remote.is_empty(),
                "Git tree diverges from the admitted Personal path projection; preserve the mirror and re-import into a fresh destination");
        }

        // Phase 1: SVN → Git
        self.set_state(PersonalSyncState::PollingSvn);
        match self.sync_svn_to_git().await {
            Ok(count) => {
                stats.svn_to_git_count = count;
                info!(count, "SVN→Git sync completed");
            }
            Err(e) => {
                self.set_state(PersonalSyncState::Error);
                error!(error = %e, "SVN→Git sync failed");
                self.db
                    .insert_audit_log(
                        "svn_to_git",
                        Some("svn_to_git"),
                        None,
                        None,
                        None,
                        Some(&e.to_string()),
                        false,
                    )
                    .ok();
                if !self.config.options.allowed_paths.is_empty() {
                    // Never bypass a held scoped inbound revision to write SVN.
                    return Err(e);
                }
                // Don't abort — still try Git→SVN
                warn!("continuing to Git→SVN despite SVN→Git error");
            }
        }

        // Phase 2: Git → SVN (via merged PRs)
        self.set_state(PersonalSyncState::PollingGitPRs);
        match self.sync_git_to_svn().await {
            Ok((pr_count, commit_count)) => {
                stats.prs_processed = pr_count;
                stats.git_to_svn_count = commit_count;
                info!(
                    prs = pr_count,
                    commits = commit_count,
                    "Git→SVN sync completed"
                );
            }
            Err(e) => {
                self.set_state(PersonalSyncState::Error);
                error!(error = %e, "Git→SVN sync failed");
                self.db
                    .insert_audit_log(
                        "git_to_svn",
                        Some("git_to_svn"),
                        None,
                        None,
                        None,
                        Some(&e.to_string()),
                        false,
                    )
                    .ok();
                if self.config.options.projected_history {
                    return Err(e);
                }
            }
        }

        stats.completed_at = Some(chrono::Utc::now());
        self.set_state(PersonalSyncState::Idle);

        // Audit log
        let detail = format!(
            "svn→git: {}, git→svn: {} ({} PRs)",
            stats.svn_to_git_count, stats.git_to_svn_count, stats.prs_processed
        );
        self.db
            .insert_audit_log("sync_cycle", None, None, None, None, Some(&detail), true)
            .ok();

        Ok(stats)
    }

    /// SVN → Git sync phase.
    async fn sync_svn_to_git(&self) -> Result<u64> {
        self.set_state(PersonalSyncState::ApplyingSvnToGit);

        let syncer = SvnToGitSync::new(
            self.svn_client.clone(),
            self.git_client.clone(),
            self.db.clone(),
            self.config.clone(),
        );

        let count = syncer.sync().await?;
        Ok(count as u64)
    }

    /// Git → SVN sync phase (via merged PRs).
    async fn sync_git_to_svn(&self) -> Result<(u64, u64)> {
        if !self.config.options.projected_history && !self.config.options.allowed_paths.is_empty() {
            // Older scoped configurations intentionally remain inbound-only.
            return Ok((0, 0));
        }
        let held_git_to_svn_needs_promotion = self
            .db
            .active_personal_svn_commit_operation()
            .context("failed to read active personal git-to-svn commit")?
            .is_some_and(|op| op.state == SvnCommitOperationState::ReconciliationRequired);

        let monitor = PrMonitor::new(&self.github_client, &self.db, &self.config);
        let merged_prs = if held_git_to_svn_needs_promotion {
            match monitor.check_for_merged_prs().await {
                Ok(prs) => prs,
                Err(error) => {
                    warn!(
                        error = %error,
                        "PR monitor failed while a held git-to-svn journal needs promotion; continuing with idle promotion only"
                    );
                    Vec::new()
                }
            }
        } else {
            monitor
                .check_for_merged_prs()
                .await
                .context("failed to poll merged pull requests")?
        };

        if merged_prs.is_empty() && !held_git_to_svn_needs_promotion {
            return Ok((0, 0));
        }

        self.set_state(PersonalSyncState::ApplyingGitToSvn);

        let syncer = GitToSvnSync::new(
            self.outbound_svn()?,
            (*self.github_client).clone(),
            self.db.clone(),
            &self.config,
            self.svn_wc_path.clone(),
            self.git_repo_path.clone(),
        );

        let result = syncer.sync().await?;
        Ok((result.prs_synced, result.commits_synced))
    }

    /// Get the current engine state.
    #[allow(dead_code)]
    pub fn get_state(&self) -> PersonalSyncState {
        // unwrap_or_else handles the (rare) case where the mutex was poisoned
        // by a panic in another thread; we recover the inner value.
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Check if a sync cycle is currently running.
    #[allow(dead_code)]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn set_state(&self, new_state: PersonalSyncState) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        info!(from = %*state, to = %new_state, "state transition");
        *state = new_state;
    }

    fn outbound_svn(&self) -> Result<SvnClient> {
        if self.config.options.projected_history {
            let svn = reposync_core::personal_projection::scoped_source(
                &self.svn_client,
                &self.config.options,
            )?;
            reposync_core::personal_projection::require_https_trust(&self.db, &svn)?;
            Ok(svn)
        } else {
            Ok(self.svn_client.clone())
        }
    }

    async fn promote_held_git_to_svn_before_history_inspect(&self) -> Result<()> {
        let syncer = GitToSvnSync::new(
            self.outbound_svn()?,
            (*self.github_client).clone(),
            self.db.clone(),
            &self.config,
            self.svn_wc_path.clone(),
            self.git_repo_path.clone(),
        );
        syncer
            .promote_held_git_to_svn_reconciliation_if_proven()
            .await?;
        Ok(())
    }

    /// Inspect the configured Git branch against the last handled SHA before
    /// SVN→Git or Git→SVN writes. Webhook `forced` is not consulted here.
    fn inspect_git_history(
        &self,
    ) -> Result<Option<reposync_core::history_inspect::HistoryInspectAdmission>> {
        let config_token = self.config.github.token.as_deref();
        let (git_path, http_auth_token) = {
            let git = self
                .git_client
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let git_path = git.repo_path().to_path_buf();
            sync_git_http_auth_from_resolution(
                &git,
                &git_path,
                &self.db,
                personal_scope_key(),
                config_token,
            )?;
            let http_auth_token = resolve_git_http_auth_token_for_workdir(
                &self.db,
                personal_scope_key(),
                config_token,
                &git_path,
            );
            (git_path, http_auth_token)
        };
        inspect_personal_history_with_http_auth(
            &self.db,
            &git_path,
            &self.config.github.default_branch,
            personal_scope_key(),
            http_auth_token.as_deref(),
            config_token,
        )
        .map_err(|e| anyhow::anyhow!(e))
    }
}

/// RAII guard that resets the running flag when dropped.
struct RunningGuard {
    flag: Arc<AtomicBool>,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}
