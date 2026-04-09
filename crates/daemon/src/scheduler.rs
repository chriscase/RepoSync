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
        let maintenance_ticks = (600 / self.poll_interval.as_secs().max(1)) as u64;
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
                    // Per-repo scheduler handles all repos from the DB.
                    self.maybe_run_repo_cycles().await;
                    // Periodic maintenance (every ~10 minutes)
                    if tick_count % maintenance_ticks == 0 {
                        if let Err(e) = self.db.run_maintenance(90) {
                            warn!("periodic maintenance failed: {}", e);
                        }
                        if let Err(e) = self.db.prune_audit_log(1000) {
                            warn!("audit log pruning failed: {}", e);
                        }
                    }
                }
                // Webhook-triggered immediate sync
                Some(()) = self.sync_rx.recv() => {
                    info!("immediate sync requested via webhook");
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
                    let consecutive = sched_stats.consecutive_errors.fetch_add(1, Ordering::SeqCst) + 1;
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

            // Check if this repo is already busy (another sync cycle OR
            // an import is holding the working tree). The busy lock is
            // process-wide and shared with the web API import handler.
            if reposync_core::busy::is_busy(&repo.id) {
                debug!(repo_name = %repo.name, "skipping: repo is busy (sync or import in progress)");
                continue;
            }

            // Read credentials from kv_state.
            // Chain: repo_id → parent → grandparent → … → global
            let svn_password = self.db.resolve_credential_chain(&repo.id, "secret_svn_password");
            let git_token = self.db.resolve_credential_chain(&repo.id, "secret_git_token");

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
            git_client
                .ensure_head_on_branch(&repo.git_branch)
                .ok();

            // Reuse cached identity mapper when possible (P7 optimization).
            let identity_mapper = match self.cached_identity_mapper.get() {
                Some(cached) => cached.clone(),
                None => {
                    match IdentityMapper::new(&self.app_config.identity) {
                        Ok(m) => {
                            let arc = Arc::new(m);
                            let _ = self.cached_identity_mapper.set(arc.clone());
                            arc
                        }
                        Err(e) => {
                            error!(repo_name = %repo.name, error = %e, "failed to create identity mapper");
                            continue;
                        }
                    }
                }
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

            let repo_id = repo.id.clone();
            let repo_name = repo.name.clone();
            let ws = self.ws_broadcast.clone();

            // Acquire the process-wide busy slot. If we can't, another
            // task beat us to it — skip cleanly.
            let busy_guard = match reposync_core::busy::try_acquire(&repo_id) {
                Some(g) => g,
                None => {
                    debug!(repo_name = %repo_name, "lost race for busy slot, skipping");
                    continue;
                }
            };

            info!(repo_name = %repo_name, repo_id = %repo_id, "starting per-repo sync cycle");

            let sync_handles = self.sync_handles.clone();
            let handle = tokio::spawn(async move {
                // Move the busy guard into the task so it's released when
                // the cycle finishes (including on panic).
                let _busy_guard = busy_guard;
                let result = engine.run_sync_cycle().await;

                match &result {
                    Ok(sync_stats) => {
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
                        });
                        let _ = ws.send(msg.to_string());
                    }
                    Err(e) => {
                        error!(
                            repo_name = %repo_name,
                            error = %e,
                            "per-repo sync cycle failed"
                        );

                        let msg = serde_json::json!({
                            "type": "repo_sync_failed",
                            "repo_id": repo_id,
                            "repo_name": repo_name,
                            "error": e.to_string(),
                        });
                        let _ = ws.send(msg.to_string());
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
