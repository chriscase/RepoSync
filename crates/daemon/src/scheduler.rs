//! Sync scheduler that runs sync cycles on a configurable interval and
//! supports webhook-triggered immediate syncs.
//!
//! The scheduler manages two kinds of sync:
//! 1. A global SyncEngine (from the TOML config) for backward compatibility.
//! 2. Per-repo sync cycles for every enabled repository in the database,
//!    each honoring its own `poll_interval_secs` and `last_sync_at`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::sync::{broadcast, mpsc, Notify, RwLock};
use tokio::time;
use tracing::{debug, error, info, warn};

use reposync_core::config::AppConfig;
use reposync_core::db::queries::AuditLogInput;
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::{ImportPhase, ImportProgress};
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;

/// Tracks aggregate statistics across sync cycles.
#[allow(dead_code)]
pub struct SchedulerStats {
    pub total_cycles: AtomicU64,
    pub total_conflicts: AtomicU64,
    pub total_errors: AtomicU64,
    pub consecutive_errors: AtomicU64,
}

impl SchedulerStats {
    fn new() -> Self {
        Self {
            total_cycles: AtomicU64::new(0),
            total_conflicts: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
            consecutive_errors: AtomicU64::new(0),
        }
    }
}

/// The sync scheduler.
///
/// Runs sync cycles on a timer and also listens for webhook-triggered
/// immediate sync requests. The sync engine's own lock prevents concurrent
/// cycles, so the scheduler simply skips if the engine reports already running.
#[allow(dead_code)]
pub struct Scheduler {
    /// Global sync engine (TOML-configured, backward compat).
    sync_engine: Arc<SyncEngine>,
    poll_interval: Duration,
    sync_rx: mpsc::Receiver<()>,
    ws_broadcast: broadcast::Sender<String>,
    stats: Arc<SchedulerStats>,
    import_progress: Arc<RwLock<ImportProgress>>,
    /// Database connection for listing repos and reading credentials.
    db: Database,
    /// Global config (for data_dir, identity, etc.).
    app_config: AppConfig,
    /// Handles for in-flight sync tasks, for graceful shutdown.
    pub sync_handles: Arc<tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    /// Cached identity mapper (shared across all repo sync cycles).
    cached_identity_mapper: std::sync::OnceLock<Arc<IdentityMapper>>,
    /// Maximum RSS in bytes. Sync cycles are skipped when exceeded. 0 = disabled.
    memory_limit_bytes: u64,
}

impl Scheduler {
    pub fn new(
        sync_engine: Arc<SyncEngine>,
        poll_interval: Duration,
        sync_rx: mpsc::Receiver<()>,
        ws_broadcast: broadcast::Sender<String>,
        import_progress: Arc<RwLock<ImportProgress>>,
        db: Database,
        app_config: AppConfig,
    ) -> Self {
        let memory_limit_bytes = app_config.daemon.memory_limit_mb * 1024 * 1024;
        Self {
            sync_engine,
            poll_interval,
            sync_rx,
            ws_broadcast,
            stats: Arc::new(SchedulerStats::new()),
            import_progress,
            db,
            app_config,
            sync_handles: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            cached_identity_mapper: std::sync::OnceLock::new(),
            memory_limit_bytes,
        }
    }

    /// Main scheduler loop.
    ///
    /// Runs until the `shutdown` notify fires, then returns so the caller
    /// can perform a clean shutdown.
    pub async fn run(&mut self, shutdown: Arc<Notify>) {
        info!(
            poll_interval_secs = self.poll_interval.as_secs(),
            "scheduler started"
        );

        let mut interval = time::interval(self.poll_interval);
        // The first tick fires immediately; consume it to allow the system
        // time to fully start before the first sync.
        interval.tick().await;

        // Run maintenance (audit pruning, retention) every ~10 minutes.
        let maintenance_ticks = 600 / self.poll_interval.as_secs().max(1);
        let mut tick_count: u64 = 0;

        loop {
            tokio::select! {
                // Shutdown signal takes priority
                _ = shutdown.notified() => {
                    info!("scheduler received shutdown signal");
                    break;
                }
                // Regular polling interval
                _ = interval.tick() => {
                    tick_count += 1;

                    // Memory guard: skip sync cycles if RSS exceeds limit
                    if self.memory_limit_bytes > 0 {
                        let rss = crate::memory::process_rss_bytes();
                        if rss > self.memory_limit_bytes {
                            warn!(
                                rss_mb = rss / 1024 / 1024,
                                limit_mb = self.memory_limit_bytes / 1024 / 1024,
                                "memory limit exceeded ({} MB > {} MB), skipping sync cycle",
                                rss / 1024 / 1024,
                                self.memory_limit_bytes / 1024 / 1024,
                            );
                            continue;
                        }
                    }

                    // Observe-first auto-reconcile for held import and external-write
                    // journals, then per-repo sync cycles.
                    self.maybe_auto_reconcile_held_operations().await;
                    self.maybe_run_repo_cycles().await;
                    // Periodic maintenance (every ~10 minutes)
                    if tick_count.checked_rem(maintenance_ticks).unwrap() == 0 {
                        if let Err(e) = self.db.run_maintenance(90) {
                            warn!("periodic maintenance failed: {}", e);
                        }
                        if let Err(e) = self.db.prune_audit_log(1000) {
                            warn!("audit log pruning failed: {}", e);
                        }
                    }
                }
                // Webhook-triggered immediate sync. GitHub `forced` is only a
                // hint on the webhook; this runs the same inspection as polling.
                Some(()) = self.sync_rx.recv() => {
                    info!("immediate sync requested via webhook (same polling inspection)");
                    self.maybe_auto_reconcile_held_operations().await;
                    self.maybe_run_repo_cycles().await;
                    // Reset the interval so we don't sync again too soon
                    interval.reset();
                }
            }
        }

        info!("scheduler stopped");
    }

    /// Attempt to run a sync cycle for the global engine.
    /// If the engine is already running or an import is in progress, skip.
    #[allow(dead_code)]
    async fn maybe_run_cycle(&self, trigger: &str) {
        // The legacy global engine has no repository identity to compare to a
        // held per-repository import, so fail closed while any hold exists.
        match self.db.has_any_active_import_operation() {
            Ok(true) => {
                info!(trigger, "skipping global sync while import work is held");
                return;
            }
            Err(e) => {
                error!(trigger, error = %e, "cannot establish import holds; refusing global sync");
                return;
            }
            Ok(false) => {}
        }
        match self.db.has_any_blocking_svn_commit_hold() {
            Ok(true) => {
                info!(
                    trigger,
                    "skipping global sync while a git-to-svn commit is held"
                );
                return;
            }
            Err(e) => {
                error!(
                    trigger,
                    error = %e,
                    "cannot establish git-to-svn holds; refusing global sync"
                );
                return;
            }
            Ok(false) => {}
        }
        // Skip sync cycles while an import is active to avoid concurrent
        // git repo access ("file changed before we could read it" errors).
        {
            let phase = self.import_progress.read().await.phase.clone();
            if !matches!(
                phase,
                ImportPhase::Idle
                    | ImportPhase::Completed
                    | ImportPhase::Failed
                    | ImportPhase::Cancelled
            ) {
                info!(trigger, ?phase, "skipping sync cycle: import in progress");
                return;
            }
        }

        // The sync engine has its own atomic lock; check it first.
        if self.sync_engine.is_running() {
            warn!(trigger, "skipping sync cycle: previous cycle still running");
            return;
        }

        let cycle_num = self.stats.total_cycles.fetch_add(1, Ordering::SeqCst) + 1;
        info!(cycle = cycle_num, trigger, "starting sync cycle");

        // Broadcast sync started
        let start_msg = serde_json::json!({
            "type": "sync_started",
            "cycle": cycle_num,
            "trigger": trigger,
        });
        let _ = self.ws_broadcast.send(start_msg.to_string());

        // Run the sync cycle directly on the main tokio runtime. The sync
        // engine's DB is separate from the web DB, so there's no mutex
        // contention. The only blocking I/O is libgit2 (brief) and SVN CLI
        // (async via tokio::process::Command).
        let engine = self.sync_engine.clone();
        let sched_stats = self.stats.clone();
        let ws = self.ws_broadcast.clone();

        tokio::spawn(async move {
            match engine.run_sync_cycle().await {
                Ok(sync_stats) => {
                    sched_stats.consecutive_errors.store(0, Ordering::SeqCst);
                    sched_stats
                        .total_conflicts
                        .fetch_add(sync_stats.conflicts_detected as u64, Ordering::SeqCst);

                    info!(
                        cycle = cycle_num,
                        svn_to_git = sync_stats.svn_to_git_count,
                        git_to_svn = sync_stats.git_to_svn_count,
                        conflicts = sync_stats.conflicts_detected,
                        auto_resolved = sync_stats.conflicts_auto_resolved,
                        "sync cycle completed successfully"
                    );

                    let end_msg = serde_json::json!({
                        "type": "sync_completed",
                        "cycle": cycle_num,
                        "svn_to_git": sync_stats.svn_to_git_count,
                        "git_to_svn": sync_stats.git_to_svn_count,
                        "conflicts": sync_stats.conflicts_detected,
                    });
                    let _ = ws.send(end_msg.to_string());
                }
                Err(e) => {
                    let errors = sched_stats.total_errors.fetch_add(1, Ordering::SeqCst) + 1;
                    let consecutive = sched_stats
                        .consecutive_errors
                        .fetch_add(1, Ordering::SeqCst)
                        + 1;
                    error!(
                        cycle = cycle_num,
                        error = %e,
                        total_errors = errors,
                        consecutive_errors = consecutive,
                        "sync cycle failed"
                    );

                    let err_msg = serde_json::json!({
                        "type": "sync_failed",
                        "cycle": cycle_num,
                        "error": e.to_string(),
                    });
                    let _ = ws.send(err_msg.to_string());
                }
            }
        });
    }

    /// On each scheduler tick, attempt observe-first reconciliation for held
    /// `import_operation_v1`, `svn_commit`, and `svn_to_git_push` journals
    /// before ordinary sync cycles.
    ///
    /// Frequency is bounded by the scheduler poll interval and the per-repository
    /// busy slot: at most one reconcile attempt per tick per repository, and no
    /// attempt while another writer holds the slot.
    async fn maybe_auto_reconcile_held_operations(&self) {
        {
            let phase = self.import_progress.read().await.phase.clone();
            if !matches!(
                phase,
                ImportPhase::Idle
                    | ImportPhase::Completed
                    | ImportPhase::Failed
                    | ImportPhase::Cancelled
            ) {
                debug!("skipping auto-reconcile: import in progress");
                return;
            }
        }

        let repos = match self.db.list_repositories() {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "failed to list repositories for auto-reconcile");
                return;
            }
        };

        for repo in repos {
            if !repo.enabled {
                continue;
            }
            match reposync_core::auto_reconcile::repo_has_reconciliation_hold(&self.db, &repo.id) {
                Ok(true) => {}
                Err(e) => {
                    error!(repo_name = %repo.name, error = %e,
                        "cannot establish reconciliation hold; skipping auto-reconcile");
                    continue;
                }
                Ok(false) => continue,
            }
            match self.db.managed_remove_blocks_new_work(&repo.id) {
                Ok(true) => {
                    debug!(repo_name = %repo.name, "skipping auto-reconcile: managed removal");
                    continue;
                }
                Err(e) => {
                    error!(repo_name = %repo.name, error = %e,
                        "cannot establish removal hold; skipping auto-reconcile");
                    continue;
                }
                Ok(false) => {}
            }

            let busy_guard = match reposync_core::busy::try_acquire(&repo.id) {
                Some(g) => g,
                None => {
                    debug!(
                        repo_name = %repo.name,
                        "skipping auto-reconcile: writer busy"
                    );
                    continue;
                }
            };

            let svn_password = self
                .db
                .resolve_credential_chain(&repo.id, "secret_svn_password");
            let svn_url = if repo.svn_branch.is_empty() {
                repo.svn_url.clone()
            } else {
                format!(
                    "{}/{}",
                    repo.svn_url.trim_end_matches('/'),
                    repo.svn_branch.trim_start_matches('/')
                )
            };
            let svn_client = SvnClient::new(
                &svn_url,
                &repo.svn_username,
                svn_password.as_deref().unwrap_or(""),
            );

            let git_repo_path = self
                .app_config
                .daemon
                .data_dir
                .join("repos")
                .join(&repo.id)
                .join("git-repo");

            let reconciled = reposync_core::auto_reconcile::reconcile_held_external_writes(
                &self.db,
                &repo,
                &svn_client,
                Some(git_repo_path.as_path()),
            )
            .await;

            match reconciled {
                Ok(result) => {
                    for attempt in &result.attempts {
                        let action = if attempt.skipped {
                            "auto_reconcile_skipped"
                        } else if attempt.finalized {
                            "auto_reconcile_finalized"
                        } else if attempt.resume_authorized {
                            "auto_reconcile_resume_authorized"
                        } else {
                            "auto_reconcile_still_held"
                        };
                        let details = serde_json::json!({
                            "kind": match attempt.kind {
                                reposync_core::auto_reconcile::HeldExternalWriteKind::ImportOperation =>
                                    "import_operation",
                                reposync_core::auto_reconcile::HeldExternalWriteKind::GitToSvnCommit =>
                                    "git_to_svn_commit",
                                reposync_core::auto_reconcile::HeldExternalWriteKind::SvnToGitPush =>
                                    "svn_to_git_push",
                            },
                            "operation_id": attempt.operation_id,
                            "finalized": attempt.finalized,
                            "resume_authorized": attempt.resume_authorized,
                            "skipped": attempt.skipped,
                            "skip_reason": attempt.skip_reason,
                        })
                        .to_string();
                        let _ = self.db.insert_audit_log_with_repo(AuditLogInput {
                            action,
                            direction: None,
                            svn_rev: None,
                            git_sha: None,
                            author: Some("scheduler"),
                            details: Some(&details),
                            success: attempt.finalized || attempt.resume_authorized,
                            repo_id: Some(&repo.id),
                        });
                        info!(
                            repo_name = %repo.name,
                            operation_id = %attempt.operation_id,
                            finalized = attempt.finalized,
                            resume_authorized = attempt.resume_authorized,
                            skipped = attempt.skipped,
                            "auto-reconcile attempt completed"
                        );
                    }
                }
                Err(e) => {
                    error!(
                        repo_name = %repo.name,
                        error = %e,
                        "auto-reconcile failed"
                    );
                    let _ = self.db.insert_audit_log_with_repo(AuditLogInput {
                        action: "auto_reconcile_failed",
                        direction: None,
                        svn_rev: None,
                        git_sha: None,
                        author: Some("scheduler"),
                        details: Some(&e.to_string()),
                        success: false,
                        repo_id: Some(&repo.id),
                    });
                }
            }

            drop(busy_guard);
        }
    }

    /// Check all enabled repositories and spawn sync cycles for those that
    /// are due (based on `poll_interval_secs` and `last_sync_at`).
    async fn maybe_run_repo_cycles(&self) {
        // Skip while an import is active.
        {
            let phase = self.import_progress.read().await.phase.clone();
            if !matches!(
                phase,
                ImportPhase::Idle
                    | ImportPhase::Completed
                    | ImportPhase::Failed
                    | ImportPhase::Cancelled
            ) {
                debug!("skipping per-repo sync: import in progress");
                return;
            }
        }

        let repos = match self.db.list_repositories() {
            Ok(r) => r,
            Err(e) => {
                error!(error = %e, "failed to list repositories for per-repo sync");
                return;
            }
        };

        let now = Utc::now();

        for repo in repos {
            if !repo.enabled {
                continue;
            }
            match self.db.managed_remove_blocks_new_work(&repo.id) {
                Ok(true) => {
                    debug!(repo_name = %repo.name, "skipping repository held by managed removal");
                    continue;
                }
                Err(e) => {
                    error!(repo_name = %repo.name, error = %e,
                        "cannot establish removal hold; refusing repository sync");
                    continue;
                }
                Ok(false) => {}
            }
            match self.db.active_import_operation(&repo.id) {
                Ok(Some(op)) => {
                    debug!(repo_name = %repo.name, operation_id = %op.id,
                        "skipping repository held by import operation");
                    continue;
                }
                Err(e) => {
                    error!(repo_name = %repo.name, error = %e,
                        "cannot establish import hold; refusing repository sync");
                    continue;
                }
                Ok(None) => {}
            }
            match self.db.active_svn_commit_operation(&repo.id) {
                Ok(Some(op))
                    if op.state
                        == reposync_core::db::svn_commit_operations::SvnCommitOperationState::ReconciliationRequired
                        && !op.resume_authorized =>
                {
                    debug!(repo_name = %repo.name, operation_id = %op.id,
                        "skipping repository held by git-to-svn commit reconciliation");
                    continue;
                }
                Ok(Some(op)) if !op.state.is_terminal() => {
                    debug!(repo_name = %repo.name, operation_id = %op.id,
                        "skipping repository with an unfinished git-to-svn commit");
                    continue;
                }
                Err(e) => {
                    error!(repo_name = %repo.name, error = %e,
                        "cannot establish git-to-svn hold; refusing repository sync");
                    continue;
                }
                Ok(_) => {}
            }
            match self.db.active_git_push_operation(&repo.id) {
                Ok(Some(op))
                    if op.state
                        == reposync_core::db::git_push_operations::GitPushOperationState::ReconciliationRequired
                        && !op.resume_authorized =>
                {
                    debug!(repo_name = %repo.name, operation_id = %op.id,
                        "skipping repository held by svn-to-git push reconciliation");
                    continue;
                }
                Ok(Some(op)) if !op.state.is_terminal() => {
                    debug!(repo_name = %repo.name, operation_id = %op.id,
                        "skipping repository with an unfinished svn-to-git push");
                    continue;
                }
                Err(e) => {
                    error!(repo_name = %repo.name, error = %e,
                        "cannot establish svn-to-git hold; refusing repository sync");
                    continue;
                }
                Ok(_) => {}
            }
            // Circuit breaker: skip repos that have been paused due to permanent errors
            if repo.sync_status == "error_paused" {
                debug!(repo_name = %repo.name, "skipping: circuit breaker active (error_paused)");
                continue;
            }

            // A repo that has never been initialized must not be touched
            // by the scheduler. The scheduler only knows how to do
            // *incremental* sync from a watermark; bootstrapping a fresh
            // repo (initial clone, full history replay, credential
            // setup) is the job of the /api/repos/:id/import handler
            // and its `run_full_import` path. If last_sync_at is NULL
            // AND last_svn_rev is 0, the user hasn't kicked an import
            // yet — leave it alone, don't clone, don't sync, don't
            // take the busy slot.
            if repo.last_sync_at.is_none() && repo.last_svn_rev == 0 {
                debug!(
                    repo_name = %repo.name,
                    "skipping: repo has never been initialized (awaiting import)"
                );
                continue;
            }

            // Check if it's time to sync based on poll_interval_secs and last_sync_at.
            let interval_secs = if repo.poll_interval_secs > 0 {
                repo.poll_interval_secs
            } else {
                self.poll_interval.as_secs() as i64
            };

            if let Some(ref last_sync) = repo.last_sync_at {
                if let Ok(last) = chrono::DateTime::parse_from_rfc3339(last_sync) {
                    let elapsed = now.signed_duration_since(last);
                    if elapsed.num_seconds() < interval_secs {
                        debug!(
                            repo_name = %repo.name,
                            elapsed_secs = elapsed.num_seconds(),
                            interval_secs,
                            "repo not due for sync yet"
                        );
                        continue;
                    }
                }
            }

            let repo_id = repo.id.clone();
            let repo_name = repo.name.clone();

            // RS-C07 (#64): acquire exclusive writer ownership before any
            // mutable prep that touches this repo's Git/SVN working tree.
            let busy_guard = match reposync_core::busy::try_acquire(&repo_id) {
                Some(g) => g,
                None => {
                    debug!(
                        repo_name = %repo_name,
                        "refusing sync prep: writer busy (sync or import in progress)"
                    );
                    let _ = self.db.insert_audit_log_with_repo(AuditLogInput {
                        action: "sync_refused",
                        direction: None,
                        svn_rev: None,
                        git_sha: None,
                        author: Some("scheduler"),
                        details: Some("writer busy; refused mutable prep"),
                        success: false,
                        repo_id: Some(&repo_id),
                    });
                    continue;
                }
            };

            // Read credentials from kv_state.
            // Chain: repo_id → parent → grandparent → … → global
            let svn_password = self
                .db
                .resolve_credential_chain(&repo.id, "secret_svn_password");
            let git_token = self
                .db
                .resolve_credential_chain(&repo.id, "secret_git_token");

            debug!(
                repo_name = %repo.name,
                svn_password_found = svn_password.is_some(),
                git_token_found = git_token.is_some(),
                "resolved credentials via chain"
            );

            // Build SVN URL: repo.svn_url + repo.svn_branch
            let svn_url = if repo.svn_branch.is_empty() {
                repo.svn_url.clone()
            } else {
                format!(
                    "{}/{}",
                    repo.svn_url.trim_end_matches('/'),
                    repo.svn_branch.trim_start_matches('/')
                )
            };

            if svn_password.is_none() {
                warn!(
                    repo_name = %repo.name,
                    repo_id = %repo.id,
                    parent_id = ?repo.parent_id,
                    "SVN password not found via credential chain"
                );
            }

            let svn_client = SvnClient::new(
                &svn_url,
                &repo.svn_username,
                svn_password.as_deref().unwrap_or(""),
            );

            // Git repo path: {data_dir}/repos/{repo_id}/git-repo
            let git_repo_path = self
                .app_config
                .daemon
                .data_dir
                .join("repos")
                .join(&repo.id)
                .join("git-repo");

            // Derive the clone URL from the repo's git_api_url and git_repo.
            let clone_url = reposync_core::git::remote_url::derive_git_remote_url(
                &repo.git_api_url,
                None,
                &repo.git_repo,
            );

            let git_client = if git_repo_path.join(".git").exists() {
                match GitClient::new(&git_repo_path) {
                    Ok(c) => c,
                    Err(e) => {
                        error!(repo_name = %repo.name, error = %e, "failed to open git repo");
                        continue;
                    }
                }
            } else {
                // Ensure parent dir exists, then clone or init.
                if let Err(e) = std::fs::create_dir_all(&git_repo_path) {
                    error!(repo_name = %repo.name, error = %e, "failed to create git repo dir");
                    continue;
                }
                match GitClient::clone_repo(&clone_url, &git_repo_path, git_token.as_deref()) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(
                            repo_name = %repo.name,
                            error = %e,
                            "clone failed, initializing empty git repo"
                        );
                        let _ = tokio::process::Command::new("git")
                            .args(["init", "--initial-branch", &repo.git_branch])
                            .current_dir(&git_repo_path)
                            .output()
                            .await;
                        let _ = tokio::process::Command::new("git")
                            .args(["remote", "add", "origin", &clone_url])
                            .current_dir(&git_repo_path)
                            .output()
                            .await;
                        match GitClient::new(&git_repo_path) {
                            Ok(c) => c,
                            Err(e) => {
                                error!(
                                    repo_name = %repo.name,
                                    error = %e,
                                    "failed to open newly initialized git repo"
                                );
                                continue;
                            }
                        }
                    }
                }
            };

            // Ensure remote has credentials embedded.
            git_client
                .ensure_remote_credentials("origin", git_token.as_deref())
                .ok();

            // Ensure HEAD is aligned with the configured branch. After
            // cloning an empty remote, libgit2 may leave HEAD pointing
            // at the wrong default branch name (e.g. master instead of
            // main). This is a no-op once a real commit exists.
            git_client.ensure_head_on_branch(&repo.git_branch).ok();

            // Reuse cached identity mapper when possible (P7 optimization).
            let identity_mapper = match self.cached_identity_mapper.get() {
                Some(cached) => cached.clone(),
                None => match IdentityMapper::new(&self.app_config.identity) {
                    Ok(m) => {
                        let arc = Arc::new(m);
                        let _ = self.cached_identity_mapper.set(arc.clone());
                        arc
                    }
                    Err(e) => {
                        error!(repo_name = %repo.name, error = %e, "failed to create identity mapper");
                        continue;
                    }
                },
            };

            // Open a per-engine DB connection.
            let db_path = self.app_config.daemon.data_dir.join("reposync.db");
            let engine_db = match Database::new(&db_path) {
                Ok(d) => d,
                Err(e) => {
                    error!(repo_name = %repo.name, error = %e, "failed to open DB for repo sync");
                    continue;
                }
            };

            // Override global config with per-repo settings.
            // The trunk_path must be empty because the branch path is already
            // baked into the SVN URL (svn_url + svn_branch).
            let mut repo_config = self.app_config.clone();
            repo_config.svn.trunk_path = String::new();
            repo_config.svn.layout = reposync_core::config::SvnLayout::Custom;
            // Override the git branch so the sync engine pulls the correct branch
            // for this repo (child branch pairs have a different git_branch than
            // the global config's default_branch).
            repo_config.github.default_branch = repo.git_branch.clone();

            let mut engine = SyncEngine::new(
                repo_config,
                engine_db,
                svn_client,
                git_client,
                identity_mapper,
            );
            engine.set_repo_id(repo.id.clone());
            if repo.lfs_threshold_mb > 0 {
                let threshold_bytes = (repo.lfs_threshold_mb as u64) * 1024 * 1024;
                engine.set_lfs_threshold_bytes(threshold_bytes);
                debug!(
                    repo_name = %repo.name,
                    lfs_threshold_mb = repo.lfs_threshold_mb,
                    lfs_threshold_bytes = threshold_bytes,
                    "LFS enforcement enabled for sync engine"
                );
            }

            // Parse and set path validation rules
            let allowed_paths: Vec<String> = repo
                .allowed_paths
                .as_ref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let blocked_patterns: Vec<String> = repo
                .blocked_patterns
                .as_ref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            if !allowed_paths.is_empty() || !blocked_patterns.is_empty() {
                engine.set_path_rules(allowed_paths, blocked_patterns);
                debug!(repo_name = %repo.name, "path validation rules configured");
            }

            let ws = self.ws_broadcast.clone();

            info!(repo_name = %repo_name, repo_id = %repo_id, "starting per-repo sync cycle");

            let sync_handles = self.sync_handles.clone();
            let handle = tokio::spawn(async move {
                // Move the busy guard into the task so it's released when
                // the cycle finishes (including on panic).
                let _busy_guard = busy_guard;
                let result = engine.run_sync_cycle().await;

                // Access the engine's DB for circuit breaker updates
                let engine_db = engine.db();

                match &result {
                    Ok(sync_stats) => {
                        // Reset consecutive errors on success
                        let _ = engine_db.reset_consecutive_errors(&repo_id);

                        info!(
                            repo_name = %repo_name,
                            svn_to_git = sync_stats.svn_to_git_count,
                            git_to_svn = sync_stats.git_to_svn_count,
                            conflicts = sync_stats.conflicts_detected,
                            "per-repo sync cycle completed"
                        );

                        let msg = serde_json::json!({
                            "type": "repo_sync_completed",
                            "repo_id": repo_id,
                            "repo_name": repo_name,
                            "svn_to_git": sync_stats.svn_to_git_count,
                            "git_to_svn": sync_stats.git_to_svn_count,
                            "conflicts": sync_stats.conflicts_detected,
                            "messages": sync_stats.recent_messages,
                            "commits": sync_stats.synced_commits,
                        });
                        let _ = ws.send(msg.to_string());
                    }
                    Err(e) => {
                        error!(
                            repo_name = %repo_name,
                            error = %e,
                            "per-repo sync cycle failed"
                        );

                        // Circuit breaker: only count permanent errors
                        if e.is_permanent() {
                            if let Ok(count) = engine_db.increment_consecutive_errors(&repo_id) {
                                if count >= 3 {
                                    warn!(
                                        repo_name = %repo_name,
                                        repo_id = %repo_id,
                                        consecutive_errors = count,
                                        "circuit breaker triggered: pausing repo after {} permanent errors",
                                        count
                                    );
                                    let _ = engine_db.conn().execute(
                                        "UPDATE repositories SET sync_status = 'error_paused' WHERE id = ?1",
                                        rusqlite::params![&repo_id],
                                    );
                                }
                            }
                        }

                        // Suppress Teams notifications for transient push failures
                        // (non-fast-forward). These auto-resolve on the next cycle
                        // after pulling the remote changes. Sending error cards for
                        // every transient conflict creates noise.
                        let error_str = e.to_string();
                        let is_transient = error_str.contains("non-fast-forward")
                            || error_str.contains("E155011")  // SVN "out of date"
                            || error_str.contains("E155010")  // SVN "node not found"
                            || error_str.contains("E150000")  // SVN "can't find parent"
                            || error_str.contains("out of date")
                            || error_str.contains("parent directory");

                        if is_transient {
                            info!(
                                repo_name = %repo_name,
                                "transient sync error — will retry next cycle: {}",
                                &error_str[..error_str.len().min(100)]
                            );
                        } else {
                            let sanitized_error =
                                reposync_core::errors::sanitize_error_message(&error_str);
                            let msg = serde_json::json!({
                                "type": "repo_sync_failed",
                                "repo_id": repo_id,
                                "repo_name": repo_name,
                                "error": sanitized_error,
                                "is_permanent": e.is_permanent(),
                            });
                            let _ = ws.send(msg.to_string());
                        }
                    }
                }

                // busy guard drops here, releasing the slot.
            });

            // Track the handle for graceful shutdown.
            {
                let mut handles = sync_handles.lock().await;
                // Clean up completed handles while we're here.
                handles.retain(|h| !h.is_finished());
                handles.push(handle);
            }
        }
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use reposync_core::config::IdentityConfig;
    use reposync_core::models::Repository;

    #[tokio::test]
    async fn held_partial_import_is_skipped_on_actual_scheduler_tick() {
        let tmp = tempfile::tempdir().unwrap();
        let config_file = tmp.path().join("config.toml");
        std::fs::write(&config_file, format!("[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"file:///nonexistent\"\nusername = \"fixture\"\n[github]\nrepo = \"local/fixture\"\n", tmp.path().display())).unwrap();
        let config = AppConfig::load_from_file(&config_file).unwrap();
        let db_path = tmp.path().join("reposync.db");
        let db = Database::new(&db_path).unwrap();
        db.initialize().unwrap();
        let now = Utc::now().to_rfc3339();
        db.insert_repository(&Repository {
            id: "held-repo".into(),
            name: "held".into(),
            svn_url: "file:///nonexistent".into(),
            svn_branch: "trunk".into(),
            svn_username: "fixture".into(),
            git_provider: "gitea".into(),
            git_api_url: "file:///nonexistent".into(),
            git_repo: "local/fixture".into(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 1,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now,
            last_svn_rev: 2,
            last_git_sha: "verified-old".into(),
            last_sync_at: Some("2000-01-01T00:00:00Z".into()),
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        })
        .unwrap();
        let operation = db
            .create_import_operation("held-repo", "legacy", "request", "target")
            .unwrap();
        db.finish_import_operation(
            "held-repo",
            &operation.id,
            reposync_core::db::import_operations::ImportOperationState::Cancelled,
            "verified partial prefix remains",
        )
        .unwrap();
        let dummy_git = tmp.path().join("dummy-git");
        assert!(std::process::Command::new("git")
            .args(["init", dummy_git.to_str().unwrap()])
            .status()
            .unwrap()
            .success());
        let engine = SyncEngine::new(
            config.clone(),
            Database::in_memory().unwrap(),
            SvnClient::new("file:///nonexistent", "fixture", ""),
            GitClient::new(&dummy_git).unwrap(),
            Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
        );
        let (_, rx) = mpsc::channel(1);
        let (ws, _) = broadcast::channel(1);
        let scheduler = Scheduler::new(
            Arc::new(engine),
            Duration::from_secs(1),
            rx,
            ws,
            Arc::new(RwLock::new(ImportProgress::default())),
            db,
            config,
        );
        scheduler.maybe_run_repo_cycles().await;
        assert!(scheduler.sync_handles.lock().await.is_empty());
        assert!(!tmp.path().join("repos/held-repo/git-repo").exists());
        assert_eq!(
            scheduler.db.get_repo_watermark("held-repo").unwrap(),
            (2, "verified-old".into())
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64A_SCHEDULER",
            "operation_id":operation.id,"checkpoint":2,"worker_spawned":false,"workdir_created":false})
        );
    }

    fn scheduler_fixture(
        tmp: &tempfile::TempDir,
        repo_id: &str,
        last_sync_at: &str,
    ) -> (Scheduler, std::path::PathBuf) {
        let config_file = tmp.path().join("config.toml");
        std::fs::write(
            &config_file,
            format!(
                "[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"file:///nonexistent\"\nusername = \"fixture\"\n[github]\nrepo = \"local/fixture\"\n",
                tmp.path().display()
            ),
        )
        .unwrap();
        let config = AppConfig::load_from_file(&config_file).unwrap();
        let db_path = tmp.path().join("reposync.db");
        let db = Database::new(&db_path).unwrap();
        db.initialize().unwrap();
        let now = Utc::now().to_rfc3339();
        db.insert_repository(&Repository {
            id: repo_id.into(),
            name: "fixture".into(),
            svn_url: "file:///nonexistent".into(),
            svn_branch: "trunk".into(),
            svn_username: "fixture".into(),
            git_provider: "gitea".into(),
            git_api_url: "file:///nonexistent".into(),
            git_repo: "local/fixture".into(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 1,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now,
            last_svn_rev: 2,
            last_git_sha: "verified-old".into(),
            last_sync_at: Some(last_sync_at.into()),
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        })
        .unwrap();
        let dummy_git = tmp.path().join("dummy-git");
        assert!(std::process::Command::new("git")
            .args(["init", dummy_git.to_str().unwrap()])
            .status()
            .unwrap()
            .success());
        let engine = SyncEngine::new(
            config.clone(),
            Database::in_memory().unwrap(),
            SvnClient::new("file:///nonexistent", "fixture", ""),
            GitClient::new(&dummy_git).unwrap(),
            Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
        );
        let (_, rx) = mpsc::channel(1);
        let (ws, _) = broadcast::channel(1);
        let git_repo_path = tmp.path().join("repos").join(repo_id).join("git-repo");
        let scheduler = Scheduler::new(
            Arc::new(engine),
            Duration::from_secs(1),
            rx,
            ws,
            Arc::new(RwLock::new(ImportProgress::default())),
            db,
            config,
        );
        (scheduler, git_repo_path)
    }

    #[tokio::test]
    async fn candidate_64c07_busy_refusal_skips_mutable_prep_without_workdir() {
        let tmp = tempfile::tempdir().unwrap();
        let (scheduler, git_repo_path) =
            scheduler_fixture(&tmp, "busy-repo", "2000-01-01T00:00:00Z");
        let guard = reposync_core::busy::try_acquire("busy-repo").unwrap();
        assert!(!git_repo_path.exists());

        scheduler.maybe_run_repo_cycles().await;

        assert!(scheduler.sync_handles.lock().await.is_empty());
        assert!(
            !git_repo_path.exists(),
            "scheduler must not create git workdir while writer is busy"
        );
        assert_eq!(
            scheduler.db.get_repo_watermark("busy-repo").unwrap(),
            (2, "verified-old".into())
        );
        let audit = scheduler
            .db
            .list_audit_log(10, 0)
            .unwrap()
            .into_iter()
            .find(|entry| entry.action == "sync_refused")
            .expect("busy refusal must be recorded durably");
        assert!(audit
            .details
            .as_deref()
            .unwrap_or("")
            .contains("writer busy"));
        drop(guard);
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64C07_BUSY_SKIP",
            "workdir_created":false,"worker_spawned":false,"audit_action":"sync_refused"})
        );
    }

    #[tokio::test]
    async fn candidate_64c07_busy_refusal_leaves_existing_workdir_pristine() {
        let tmp = tempfile::tempdir().unwrap();
        let (scheduler, git_repo_path) =
            scheduler_fixture(&tmp, "pristine-repo", "2000-01-01T00:00:00Z");
        std::fs::create_dir_all(&git_repo_path).unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "--initial-branch", "main"])
            .current_dir(&git_repo_path)
            .status()
            .unwrap()
            .success());
        std::fs::write(git_repo_path.join("sentinel.txt"), "unchanged").unwrap();
        assert!(std::process::Command::new("git")
            .args(["add", "sentinel.txt"])
            .current_dir(&git_repo_path)
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args(["commit", "-m", "baseline"])
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@test")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@test")
            .current_dir(&git_repo_path)
            .status()
            .unwrap()
            .success());
        let head_before = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&git_repo_path)
            .output()
            .unwrap();
        let guard = reposync_core::busy::try_acquire("pristine-repo").unwrap();

        scheduler.maybe_run_repo_cycles().await;

        assert!(scheduler.sync_handles.lock().await.is_empty());
        let head_after = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&git_repo_path)
            .output()
            .unwrap();
        assert_eq!(head_before.stdout, head_after.stdout);
        assert_eq!(
            std::fs::read_to_string(git_repo_path.join("sentinel.txt")).unwrap(),
            "unchanged"
        );
        drop(guard);
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64C07_PRISTINE_WC",
            "head_unchanged":true,"sentinel_unchanged":true,"worker_spawned":false})
        );
    }

    #[tokio::test]
    async fn candidate_64e_import_auto_reconcile_skips_when_writer_busy() {
        let tmp = tempfile::tempdir().unwrap();
        let (scheduler, _) = scheduler_fixture(&tmp, "held-import-auto", "2000-01-01T00:00:00Z");
        let operation = scheduler
            .db
            .create_import_operation("held-import-auto", "legacy", "request", "fp")
            .unwrap();
        scheduler
            .db
            .finish_import_operation(
                "held-import-auto",
                &operation.id,
                reposync_core::db::import_operations::ImportOperationState::ReconciliationRequired,
                "lost reply",
            )
            .unwrap();
        assert_eq!(
            scheduler
                .db
                .active_import_operation("held-import-auto")
                .unwrap()
                .unwrap()
                .state,
            reposync_core::db::import_operations::ImportOperationState::ReconciliationRequired
        );
        let guard = reposync_core::busy::try_acquire("held-import-auto").unwrap();

        scheduler.maybe_auto_reconcile_held_operations().await;

        let audit = scheduler.db.list_audit_log(20, 0).unwrap();
        assert!(
            !audit
                .iter()
                .any(|entry| entry.action.starts_with("auto_reconcile")),
            "busy writer must defer import auto-reconcile until the slot is free"
        );
        drop(guard);
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64E_IMPORT_BUSY_DEFER","auto_reconcile_attempted":false})
        );
    }

    #[tokio::test]
    async fn candidate_64f_partial_import_with_resume_authorized_still_blocks_scheduler() {
        let tmp = tempfile::tempdir().unwrap();
        let (scheduler, _) =
            scheduler_fixture(&tmp, "held-import-resume-auth", "2000-01-01T00:00:00Z");
        let operation = scheduler
            .db
            .create_import_operation("held-import-resume-auth", "legacy", "request", "fp")
            .unwrap();
        scheduler
            .db
            .start_import_operation("held-import-resume-auth", &operation.id)
            .unwrap();
        scheduler
            .db
            .note_import_total("held-import-resume-auth", &operation.id, 52)
            .unwrap();
        scheduler
            .db
            .note_import_local(
                "held-import-resume-auth",
                &operation.id,
                50,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                50,
                50,
            )
            .unwrap();
        scheduler
            .db
            .begin_import_publication(
                "held-import-resume-auth",
                &operation.id,
                "refs/heads/main",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap();
        scheduler
            .db
            .confirm_import_publication(
                "held-import-resume-auth",
                &operation.id,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap();
        scheduler
            .db
            .finish_import_operation(
                "held-import-resume-auth",
                &operation.id,
                reposync_core::db::import_operations::ImportOperationState::ReconciliationRequired,
                "partial import held",
            )
            .unwrap();
        let mut held = scheduler
            .db
            .get_import_operation("held-import-resume-auth", &operation.id)
            .unwrap()
            .unwrap();
        held.resume_authorized = true;
        held.last_confirmed_svn_rev = Some(50);
        held.last_confirmed_git_sha = Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into());
        scheduler
            .db
            .conn()
            .execute(
                "UPDATE kv_state SET value=?1 WHERE key=?2",
                rusqlite::params![
                    serde_json::to_string(&held).unwrap(),
                    format!("import_operation_v1:document:{}", operation.id)
                ],
            )
            .unwrap();

        scheduler.maybe_run_repo_cycles().await;

        assert!(scheduler.sync_handles.lock().await.is_empty());
        assert_eq!(
            scheduler
                .db
                .get_repo_watermark("held-import-resume-auth")
                .unwrap(),
            (2, "verified-old".into())
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({
                "case":"64F_PARTIAL_RESUME_AUTH_STILL_BLOCKED",
                "operation_id":operation.id,
                "resume_authorized":true,
                "checkpoint":2,
                "worker_spawned":false
            })
        );
    }

    #[tokio::test]
    async fn candidate_64e_held_import_blocks_scheduler_sync_without_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let (scheduler, _) = scheduler_fixture(&tmp, "held-import-block", "2000-01-01T00:00:00Z");
        let operation = scheduler
            .db
            .create_import_operation("held-import-block", "legacy", "request", "fp")
            .unwrap();
        scheduler
            .db
            .finish_import_operation(
                "held-import-block",
                &operation.id,
                reposync_core::db::import_operations::ImportOperationState::ReconciliationRequired,
                "lost reply",
            )
            .unwrap();

        scheduler.maybe_run_repo_cycles().await;

        assert!(scheduler.sync_handles.lock().await.is_empty());
        assert_eq!(
            scheduler
                .db
                .get_repo_watermark("held-import-block")
                .unwrap(),
            (2, "verified-old".into())
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({
                "case":"64E_IMPORT_CYCLE_BLOCKED",
                "operation_id":operation.id,
                "resume_authorized":false,
                "worker_spawned":false
            })
        );
    }

    #[tokio::test]
    async fn candidate_64d_auto_reconcile_skips_when_writer_busy() {
        let tmp = tempfile::tempdir().unwrap();
        let (scheduler, _) = scheduler_fixture(&tmp, "held-auto", "2000-01-01T00:00:00Z");
        use reposync_core::db::git_push_operations::{GitPushIntent, GitPushOperationState};
        let intent = GitPushIntent {
            repo_id: "held-auto",
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
        let push = scheduler.db.begin_svn_to_git_push(intent).unwrap();
        scheduler
            .db
            .hold_svn_to_git_reconciliation("held-auto", &push.id, "lost reply")
            .unwrap();
        assert_eq!(
            scheduler
                .db
                .active_git_push_operation("held-auto")
                .unwrap()
                .unwrap()
                .state,
            GitPushOperationState::ReconciliationRequired
        );
        let guard = reposync_core::busy::try_acquire("held-auto").unwrap();

        scheduler.maybe_auto_reconcile_held_operations().await;

        let audit = scheduler.db.list_audit_log(20, 0).unwrap();
        assert!(
            !audit
                .iter()
                .any(|entry| entry.action.starts_with("auto_reconcile")),
            "busy writer must defer auto-reconcile until the slot is free"
        );
        drop(guard);
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64D_BUSY_DEFER","auto_reconcile_attempted":false})
        );
    }

    #[tokio::test]
    async fn candidate_64d_auto_reconcile_records_attempt_without_workdir() {
        let tmp = tempfile::tempdir().unwrap();
        let (scheduler, _) = scheduler_fixture(&tmp, "held-no-wc", "2000-01-01T00:00:00Z");
        use reposync_core::db::git_push_operations::{GitPushIntent, GitPushOperationState};
        let intent = GitPushIntent {
            repo_id: "held-no-wc",
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
        let push = scheduler.db.begin_svn_to_git_push(intent).unwrap();
        scheduler
            .db
            .hold_svn_to_git_reconciliation("held-no-wc", &push.id, "lost reply")
            .unwrap();
        assert_eq!(
            scheduler
                .db
                .active_git_push_operation("held-no-wc")
                .unwrap()
                .unwrap()
                .state,
            GitPushOperationState::ReconciliationRequired
        );

        scheduler.maybe_auto_reconcile_held_operations().await;

        let audit = scheduler
            .db
            .list_audit_log(20, 0)
            .unwrap()
            .into_iter()
            .find(|entry| entry.action == "auto_reconcile_skipped")
            .expect("auto-reconcile must record a skipped attempt when git workdir is absent");
        assert!(audit
            .details
            .as_deref()
            .unwrap_or("")
            .contains("git workdir unavailable"));
        assert_eq!(
            scheduler
                .db
                .active_git_push_operation("held-no-wc")
                .unwrap()
                .unwrap()
                .state,
            GitPushOperationState::ReconciliationRequired
        );
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64D_NO_WORKDIR","audit_action":"auto_reconcile_skipped"})
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn candidate_64c07_acquire_precedes_clone_prep_on_due_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let (scheduler, git_repo_path) =
            scheduler_fixture(&tmp, "due-repo", "2000-01-01T00:00:00Z");
        assert!(!git_repo_path.exists());

        scheduler.maybe_run_repo_cycles().await;

        // Without a competing writer, ownership is acquired before clone/init
        // prep and the cycle proceeds far enough to create the workdir.
        assert!(
            git_repo_path.exists(),
            "due repo should acquire ownership then perform mutable prep"
        );
        let handles: Vec<_> = scheduler.sync_handles.lock().await.drain(..).collect();
        assert!(
            !handles.is_empty(),
            "sync worker should spawn only after ownership is held through prep"
        );
        for handle in handles {
            let _ = handle.await;
        }
        eprintln!(
            "RELIABILITY_EVIDENCE {}",
            serde_json::json!({"case":"64C07_ACQUIRE_BEFORE_PREP",
            "workdir_created":true,"worker_spawned":true})
        );
    }
}
