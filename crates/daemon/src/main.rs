//! RepoSync daemon entry point.
//!
//! Loads configuration, initializes all subsystems, starts the web server
//! and sync scheduler, and handles graceful shutdown.

pub(crate) mod memory;
mod pair_refresh_recovery;
mod scheduler;
mod signals;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use reposync_core::config::AppConfig;
use reposync_core::db::watermark_recovery::{
    recover_repo_watermark_from_git_log_scan, recover_repo_watermark_from_global_migration,
};
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;
use reposync_web::WebServer;

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

/// RepoSync synchronization daemon.
#[derive(Parser, Debug)]
#[command(
    name = "reposync-daemon",
    version,
    about = "Bidirectional SVN/Git synchronization daemon"
)]
struct Args {
    /// Path to the TOML configuration file.
    #[arg(short, long)]
    config: PathBuf,

    /// Override the log level from the config file (trace, debug, info, warn, error).
    #[arg(long)]
    log_level: Option<String>,

    /// Serve only authentication and explicit recovery of a retained pair refresh.
    #[arg(long)]
    pair_refresh_recovery: bool,
}

type TaskHandles = Arc<tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>;

struct RetainedShutdownTask {
    id: u64,
    role: &'static str,
    handle: tokio::task::JoinHandle<()>,
}

static SHUTDOWN_TASK_IDS: AtomicU64 = AtomicU64::new(1);
static RETAINED_SHUTDOWN_TASKS: OnceLock<Mutex<Vec<RetainedShutdownTask>>> = OnceLock::new();
static RETAINED_SHUTDOWN_REGISTRIES: OnceLock<Mutex<Vec<(&'static str, TaskHandles)>>> =
    OnceLock::new();

/// A completed task is still owned until its join result has been observed.
/// Cancellation, timeout and unwind retain the same unconsumed handle.
pub(crate) struct OwnedShutdownTask {
    id: u64,
    role: &'static str,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl OwnedShutdownTask {
    pub(crate) fn new(role: &'static str, handle: tokio::task::JoinHandle<()>) -> Self {
        Self {
            id: SHUTDOWN_TASK_IDS.fetch_add(1, Ordering::Relaxed),
            role,
            handle: Some(handle),
        }
    }

    pub(crate) fn join_observed(&self) -> bool {
        self.handle.is_none()
    }

    #[cfg(test)]
    pub(crate) fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|handle| handle.is_finished())
    }

    /// Reclaim exactly one retained owner for a separately bounded join. No
    /// implicit retry, join, or success is performed by inspecting custody.
    #[allow(dead_code)]
    fn take_retained(id: u64) -> Option<Self> {
        let mut retained = RETAINED_SHUTDOWN_TASKS
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let index = retained.iter().position(|task| task.id == id)?;
        let task = retained.swap_remove(index);
        Some(Self { id: task.id, role: task.role, handle: Some(task.handle) })
    }

    fn abort(&self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }

    pub(crate) async fn join(&mut self) -> Result<(), tokio::task::JoinError> {
        let result = self
            .handle
            .as_mut()
            .expect("shutdown task join was already observed")
            .await;
        // There is no await between consuming Ready and removing the handle.
        self.handle.take();
        result
    }
}

impl Drop for OwnedShutdownTask {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            error!(task_id = self.id, role = self.role, "retaining unjoined shutdown task");
            RETAINED_SHUTDOWN_TASKS
                .get_or_init(|| Mutex::new(Vec::new()))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(RetainedShutdownTask {
                    id: self.id,
                    role: self.role,
                    handle,
                });
        }
    }
}

fn retain_task_registry(role: &'static str, handles: TaskHandles) {
    // If the scheduler join is unresolved, do not snapshot a registry it could
    // still be using. Retain its Arc, including handles registered already.
    RETAINED_SHUTDOWN_REGISTRIES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push((role, handles));
}

/// Keep registry custody even if main unwinds before its shutdown snapshot.
struct TaskRegistryCustody {
    role: &'static str,
    handles: TaskHandles,
}

impl Drop for TaskRegistryCustody {
    fn drop(&mut self) {
        retain_task_registry(self.role, Arc::clone(&self.handles));
    }
}

#[derive(Default)]
struct TaskDrainReport {
    owned: usize,
    unresolved: usize,
    timed_out: bool,
    join_failures: Vec<String>,
}

/// Observe every handle, including handles already finished at the snapshot.
/// Both the initial wait and the post-abort wait have shared deadlines, so the
/// bound does not grow once per child. Dropping any pending owner retains it.
async fn drain_owned_tasks(
    handles: &TaskHandles,
    role: &'static str,
    drain_bound: Duration,
    abort_join_bound: Duration,
) -> TaskDrainReport {
    let tasks: Vec<_> = {
        let mut locked = handles.lock().await;
        locked
            .drain(..)
            .map(|handle| OwnedShutdownTask::new(role, handle))
            .collect()
    };
    let mut report = TaskDrainReport {
        owned: tasks.len(),
        ..TaskDrainReport::default()
    };
    let deadline = tokio::time::Instant::now() + drain_bound;
    let mut pending = tasks.into_iter();
    while let Some(mut task) = pending.next() {
        match tokio::time::timeout_at(deadline, task.join()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => report.join_failures.push(format!("{role}: {error}")),
            Err(_) => {
                report.timed_out = true;
                let mut aborted = vec![task];
                aborted.extend(pending);
                for task in &aborted {
                    task.abort();
                }
                let abort_deadline = tokio::time::Instant::now() + abort_join_bound;
                for mut task in aborted {
                    match tokio::time::timeout_at(abort_deadline, task.join()).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) if error.is_cancelled() => {}
                        Ok(Err(error)) => {
                            report.join_failures.push(format!("{role}: {error}"));
                        }
                        Err(_) => report.unresolved += 1,
                    }
                    // Unobserved handles move to retained storage on drop.
                }
                break;
            }
        }
    }
    report
}

pub(crate) struct SchedulerStopReport {
    pub failure: Option<String>,
    pub terminated_before_drain: bool,
    pub drained_sync_tasks: usize,
    pub stop_timeout_recorded: bool,
    pub unresolved_sync_tasks: usize,
    pub sync_join_failures: usize,
}

/// Close admission before notification. A fatal deadline remains fatal even if
/// abort later joins. No child snapshot precedes an observed scheduler join.
pub(crate) async fn stop_scheduler_and_drain_sync_tasks(
    notify: &Arc<tokio::sync::Notify>,
    gate: &Arc<scheduler::SchedulerShutdownGate>,
    scheduler_task: &mut OwnedShutdownTask,
    sync_handles: &TaskHandles,
    stop_timeout: Duration,
    mut failure: Option<String>,
    record_failure: &mut dyn FnMut(reposync_web::SchedulerServiceLevel, &str),
    on_stop_deadline: &mut dyn FnMut(),
    sync_drain_bound: Duration,
) -> SchedulerStopReport {
    gate.request_stop();
    notify.notify_one();
    let mut stop_timeout_recorded = false;
    if !scheduler_task.join_observed() {
        match tokio::time::timeout(stop_timeout, scheduler_task.join()).await {
            Ok(end) => {
                if let Some((level, reason)) = reposync_web::scheduler_failure_from_task_end(
                    true,
                    &reposync_web::SchedulerTaskEnd::from_join(end),
                ) {
                    record_failure(level, &reason);
                    failure.get_or_insert(reason);
                }
            }
            Err(_) => {
                let (level, reason) = reposync_web::scheduler_failure_from_stop_timeout();
                record_failure(level, &reason);
                failure.get_or_insert(reason);
                stop_timeout_recorded = true;
                on_stop_deadline();
                scheduler_task.abort();
                match tokio::time::timeout(stop_timeout, scheduler_task.join()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) if error.is_cancelled() => {}
                    Ok(Err(error)) => error!(%error, "scheduler join failed after stop deadline"),
                    Err(_) => error!("scheduler join remains unresolved; child drains are closed"),
                }
            }
        }
    }
    let terminated = scheduler_task.join_observed();
    let drain = if terminated {
        drain_owned_tasks(sync_handles, "sync task", sync_drain_bound, stop_timeout).await
    } else {
        TaskDrainReport::default()
    };
    let prior_join_failures = gate.sync_join_failures();
    for reason in drain.join_failures.iter().chain(prior_join_failures.iter()) {
        error!(%reason, "observed sync task join failure");
    }
    if drain.timed_out || drain.unresolved != 0 || !drain.join_failures.is_empty()
        || !prior_join_failures.is_empty()
    {
        let reason = format!(
            "sync task shutdown incomplete: timed_out={}, unresolved={}, join_failures={}",
            drain.timed_out,
            drain.unresolved,
            drain.join_failures.len() + prior_join_failures.len()
        );
        record_failure(reposync_web::SchedulerServiceLevel::Fatal, &reason);
        failure.get_or_insert(reason);
    }
    SchedulerStopReport {
        failure,
        terminated_before_drain: terminated,
        drained_sync_tasks: drain.owned,
        stop_timeout_recorded,
        unresolved_sync_tasks: drain.unresolved,
        sync_join_failures: drain.join_failures.len() + prior_join_failures.len(),
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Load and resolve configuration
    let mut config =
        AppConfig::load_from_file(&args.config).context("failed to load configuration file")?;
    config
        .resolve_env_vars()
        .context("failed to resolve environment variables in config")?;
    config
        .validate()
        .context("configuration validation failed")?;

    // Initialize tracing
    let log_level = args
        .log_level
        .as_deref()
        .unwrap_or(&config.daemon.log_level);

    let filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .init();

    // Install a panic hook so unexpected crashes (panics from spawned
    // tasks, libgit2 FFI, etc.) are captured to the log with a backtrace
    // instead of vanishing silently to stderr. Without this, crashes look
    // like the daemon just stopped writing logs and died.
    std::panic::set_hook(Box::new(|panic_info| {
        let location = panic_info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let message = match panic_info.payload().downcast_ref::<&str>() {
            Some(s) => (*s).to_string(),
            None => match panic_info.payload().downcast_ref::<String>() {
                Some(s) => s.clone(),
                None => "<non-string panic payload>".to_string(),
            },
        };
        let backtrace = std::backtrace::Backtrace::force_capture();
        error!(
            location = %location,
            message = %message,
            "!!! DAEMON PANIC !!!"
        );
        error!("panic backtrace:\n{}", backtrace);
        // Also write to stderr so nohup's redirect captures it even if
        // tracing is somehow broken at this point.
        eprintln!("!!! PANIC at {}: {}", location, message);
        eprintln!("{}", backtrace);
    }));

    // Install SIGHUP ignore handler BEFORE doing any other work. This is
    // the critical fix for "daemon silently dies when SSH session ends":
    // systemd-logind sends SIGHUP to the entire user session on SSH
    // disconnect, which by default kills nohup'd processes too. By
    // installing our own async handler that consumes SIGHUP without
    // exiting, the daemon survives.
    signals::ignore_sighup();

    // Startup banner
    info!("========================================");
    info!("  RepoSync Daemon v{}", env!("CARGO_PKG_VERSION"));
    info!("========================================");
    info!("Config file   : {}", args.config.display());
    info!("SVN URL       : {}", config.svn.url);
    info!("GitHub repo   : {}", config.github.repo);
    info!("Poll interval : {}s", config.daemon.poll_interval_secs);
    info!("Web listen    : {}", config.web.listen);
    info!("Data dir      : {}", config.daemon.data_dir.display());
    info!("Log level     : {}", log_level);
    info!("========================================");

    if args.pair_refresh_recovery {
        return pair_refresh_recovery::serve(config, args.config).await;
    }

    // Exclusive data-directory owner (reposync.lock) plus schema init.
    // The lock is held for the lifetime of _lock_guard. On process exit
    // (including SIGKILL/crash), the OS releases the flock automatically.
    let (db, _lock_guard) =
        Database::open_with_exclusive_owner(&config.daemon.data_dir).map_err(|e| {
            error!("{}", e);
            anyhow::anyhow!("{}", e)
        })?;
    info!("Acquired singleton lock (PID {})", std::process::id());

    // Open a second connection for the web server (SQLite supports multiple readers with WAL)
    let db_path = config.daemon.data_dir.join("reposync.db");
    let web_db = Database::new(&db_path).context("failed to open web database connection")?;
    info!("Database initialized at {}", db_path.display());

    // Resolve secrets from DB (fallback when env vars are absent)
    config.resolve_secrets_from_db(&db);

    // Auto-bootstrap admin user if users table is empty
    match db.count_users() {
        Ok(0) => {
            let admin_password = std::env::var("REPOSYNC_ADMIN_PASSWORD")
                .unwrap_or_else(|_| {
                    warn!("No REPOSYNC_ADMIN_PASSWORD set and no users exist — creating admin user with default password 'changeme'. CHANGE THIS IMMEDIATELY!");
                    "changeme".to_string()
                });
            match reposync_core::crypto::hash_password(&admin_password) {
                Ok(hash) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    let admin = reposync_core::models::User {
                        id: uuid::Uuid::new_v4().to_string(),
                        username: "admin".to_string(),
                        display_name: "Administrator".to_string(),
                        email: "admin@localhost".to_string(),
                        password_hash: hash,
                        role: "admin".to_string(),
                        enabled: true,
                        created_at: now.clone(),
                        updated_at: now,
                    };
                    match db.insert_user(&admin) {
                        Ok(()) => info!("Created bootstrap admin user (username: admin)"),
                        Err(e) => error!("Failed to create bootstrap admin user: {}", e),
                    }
                }
                Err(e) => error!("Failed to hash admin password: {}", e),
            }
        }
        Ok(n) => info!("{} user(s) found in database, skipping admin bootstrap", n),
        Err(e) => warn!("Failed to check users table (may not exist yet): {}", e),
    }

    // Auto-migrate existing single-repo config into the repositories table
    // so that existing deployments continue to work without manual changes.
    {
        let repos = db.list_repositories().unwrap_or_default();
        if repos.is_empty() && !config.svn.url.is_empty() {
            let now = chrono::Utc::now().to_rfc3339();
            let provider = match config.github.provider {
                reposync_core::config::GitProvider::GitHub => "github",
                reposync_core::config::GitProvider::Gitea => "gitea",
            };
            let sync_mode = match config.sync.mode {
                reposync_core::config::SyncMode::Direct => "direct",
                reposync_core::config::SyncMode::Pr => "pr",
            };
            // Try to reuse an existing repo UUID from orphaned credential keys.
            // This handles the case where the DB was reset but kv_state still has
            // per-repo credentials stored under the old UUID.
            let reuse_repo_id: Option<String> = (|| {
                let conn = db.conn();
                let mut stmt = conn
                    .prepare("SELECT key FROM kv_state WHERE key LIKE 'secret_svn_password_%' AND key != 'secret_svn_password' LIMIT 1")
                    .ok()?;
                let key: String = stmt.query_row([], |row| row.get(0)).ok()?;
                key.strip_prefix("secret_svn_password_")
                    .map(|s| s.to_string())
            })();
            if let Some(ref id) = reuse_repo_id {
                info!(
                    "Reusing existing repo UUID {} from orphaned credential keys",
                    id
                );
            }

            let default_repo = reposync_core::models::Repository {
                id: reuse_repo_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: config.github.repo.clone(),
                svn_url: config.svn.url.clone(),
                svn_branch: config.svn.trunk_path.clone(),
                svn_username: config.svn.username.clone(),
                git_provider: provider.to_string(),
                git_api_url: config.github.api_url.clone(),
                git_repo: config.github.repo.clone(),
                git_branch: config.github.default_branch.clone(),
                sync_mode: sync_mode.to_string(),
                poll_interval_secs: config.daemon.poll_interval_secs as i64,
                lfs_threshold_mb: 0,
                auto_merge: config.sync.auto_merge,
                enabled: true,
                created_by: None,
                parent_id: None,
                created_at: now.clone(),
                updated_at: now,
                last_svn_rev: 0,
                last_git_sha: String::new(),
                last_sync_at: None,
                sync_status: "idle".to_string(),
                total_syncs: 0,
                total_errors: 0,
                allowed_paths: None,
                blocked_patterns: None,
                consecutive_errors: 0,
                teams_webhook_url: None,
            };
            match db.insert_repository(&default_repo) {
                Ok(()) => {
                    info!(
                        "Auto-migrated existing config to repository: {}",
                        default_repo.name
                    );
                    // Migrate global credentials to per-repo keys
                    if let Ok(Some(pw)) = db.get_state("secret_svn_password") {
                        if !pw.is_empty() {
                            let _ = db.set_state(
                                &format!("secret_svn_password_{}", default_repo.id),
                                &pw,
                            );
                            info!(
                                "Migrated global SVN password to per-repo key for {}",
                                default_repo.name
                            );
                        }
                    }
                    if let Ok(Some(tok)) = db.get_state("secret_git_token") {
                        if !tok.is_empty() {
                            let _ = db
                                .set_state(&format!("secret_git_token_{}", default_repo.id), &tok);
                            info!(
                                "Migrated global Git token to per-repo key for {}",
                                default_repo.name
                            );
                        }
                    }
                }
                Err(e) => warn!("Failed to auto-migrate config to repository: {}", e),
            }
        }
    }

    if let Err(e) = reposync_core::db::credential_seeding::seed_per_repo_credentials(&db) {
        warn!("failed to seed per-repo credentials: {}", e);
    }

    // Initialize SVN client
    let svn_password = config.svn.password.clone().unwrap_or_default();
    let svn_client = SvnClient::new(&config.svn.url, &config.svn.username, &svn_password);
    info!("SVN client initialized for {}", config.svn.url);

    // Initialize Git client (graceful: create empty repo if clone fails)
    let git_repo_path = config.daemon.data_dir.join("git-repo");
    let git_client = if git_repo_path.join(".git").exists() {
        GitClient::new(&git_repo_path).context("failed to open existing Git repository")?
    } else {
        let clone_url = config.github.clone_url();
        let token = config.github.token.as_deref();
        match GitClient::clone_repo(&clone_url, &git_repo_path, token) {
            Ok(client) => {
                info!("Git client cloned at {}", git_repo_path.display());
                client
            }
            Err(e) => {
                // Clone failed — init an empty repo so the daemon can start.
                // The import wizard will populate it later.
                warn!("Clone failed ({}), initializing empty git repo", e);
                std::fs::create_dir_all(&git_repo_path)
                    .context("failed to create git repo directory")?;
                let init_output = tokio::process::Command::new("git")
                    .args(["init", "--initial-branch", &config.github.default_branch])
                    .current_dir(&git_repo_path)
                    .output()
                    .await;
                if init_output.is_err() || !init_output.as_ref().unwrap().status.success() {
                    // Fallback for older git without --initial-branch
                    let _ = tokio::process::Command::new("git")
                        .args(["init"])
                        .current_dir(&git_repo_path)
                        .output()
                        .await;
                }
                let _ = tokio::process::Command::new("git")
                    .args(["remote", "add", "origin", &clone_url])
                    .current_dir(&git_repo_path)
                    .output()
                    .await;
                GitClient::new(&git_repo_path)
                    .context("failed to open newly initialized Git repository")?
            }
        }
    };
    // Config remote credentials come only from config/env — never from a managed repo chain.
    reposync_core::git::apply_config_remote_git_credentials(
        &git_client,
        config.github.token.as_deref(),
    )
    .ok();

    // Initialize identity mapper
    let identity_mapper = Arc::new(
        IdentityMapper::new(&config.identity).context("failed to initialize identity mapper")?,
    );
    info!("Identity mapper initialized");

    // Initialize sync engine
    let mut engine = SyncEngine::new(config.clone(), db, svn_client, git_client, identity_mapper);
    // Set repo_id from the first enabled repository for per-repo keys
    if let Ok(repos) = engine.db().list_repositories() {
        if let Some(repo) = repos.into_iter().find(|r| r.enabled) {
            engine.set_repo_id(repo.id);
        }
    }

    // Auto-detect watermarks for repos where last_svn_rev == 0.
    // Recovery is strictly per-repo: we only restore from sources that are
    // scoped to this specific repo_id. Reading from global / single-repo
    // legacy state would cross-contaminate brand-new sync pairs with
    // watermarks from a totally unrelated repository.
    //
    // Note: only the *first* repo created on a server (when repos was empty)
    // can use the global watermarks table — that's the migration path from
    // single-repo deployments. For all other repos, only per-repo sources
    // are valid.
    {
        let repos = engine.db().list_repositories().unwrap_or_default();
        let single_repo_migration = repos.len() == 1;
        for repo in &repos {
            if repo.last_svn_rev != 0 {
                continue;
            }
            info!(repo_name = %repo.name, "repo has last_svn_rev=0, attempting auto-detect");

            // Only allow recovery from the global watermarks table when this
            // is a single-repo migration scenario (the daemon was previously
            // running as a single-repo deployment and we're upgrading).
            if single_repo_migration {
                match recover_repo_watermark_from_global_migration(engine.db(), &repo.id) {
                    Ok(true) => {
                        if let Ok(Some(rev_str)) = engine.db().get_watermark("svn_rev") {
                            if let Ok(rev) = rev_str.parse::<i64>() {
                                info!(
                                    repo_name = %repo.name,
                                    rev,
                                    "Recovered watermark from global watermarks table (single-repo migration)"
                                );
                            }
                        }
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => warn!("Failed to write watermark for {}: {}", repo.name, e),
                }
            }

            // Per-repo kv_state keys (always safe — these are scoped by repo_id)
            let repo_key = format!("last_svn_rev_{}", repo.id);
            if let Ok(Some(rev_str)) = engine.db().get_state(&repo_key) {
                if let Ok(rev) = rev_str.parse::<i64>() {
                    if rev > 0 {
                        let sha_key = format!("last_git_sha_{}", repo.id);
                        let sha = engine
                            .db()
                            .get_state(&sha_key)
                            .ok()
                            .flatten()
                            .unwrap_or_default();
                        match engine
                            .db()
                            .update_repo_watermark_columns_only(&repo.id, rev, &sha)
                        {
                            Ok(()) => {
                                info!(repo_name = %repo.name, rev, "Recovered watermark from per-repo kv_state");
                                continue;
                            }
                            Err(e) => warn!("Failed to write watermark for {}: {}", repo.name, e),
                        }
                    }
                }
            }
            // Only scan the per-repo git directory. The legacy global
            // /opt/reposync/git-repo holds history from the original
            // single-repo deployment and would cross-contaminate fresh
            // sync pairs with watermarks from a totally unrelated repo.
            let repo_git_dir = config
                .daemon
                .data_dir
                .join("repos")
                .join(&repo.id)
                .join("git-repo");
            let git_dir = if repo_git_dir.join(".git").exists() {
                Some(&repo_git_dir)
            } else {
                None
            };

            if let Some(git_dir) = git_dir {
                match recover_repo_watermark_from_git_log_scan(engine.db(), &repo.id, git_dir) {
                    Ok(true) => {
                        let (max_rev, _) =
                            reposync_core::db::watermark_recovery::scan_git_log_for_svn_watermark(
                                git_dir,
                            );
                        info!(
                            repo_name = %repo.name,
                            rev = max_rev,
                            "Auto-detected watermark r{} for repo {}",
                            max_rev,
                            repo.name
                        );
                    }
                    Ok(false) => {}
                    Err(e) => warn!(
                        "Failed to write auto-detected watermark for {}: {}",
                        repo.name, e
                    ),
                }
            }
        }
    }

    let sync_engine = Arc::new(engine);
    info!("Sync engine initialized");

    // Create sync trigger channel (webhook -> scheduler)
    let (sync_tx, sync_rx) = tokio::sync::mpsc::channel::<()>(16);

    // Create shared import progress — shared between web server and scheduler
    // so the scheduler can pause sync cycles during an active import.
    //
    // Recovery: load the last persisted import state from the DB. If it was
    // in an "active" phase (connecting, importing, verifying, final_push),
    // the daemon must have crashed mid-import — reconcile by marking it as
    // Failed with a note so the user knows why and can re-trigger.
    let mut recovered_progress = sync_engine
        .db()
        .load_import_progress()
        .ok()
        .flatten()
        .unwrap_or_default();
    use reposync_core::import::ImportPhase;
    if matches!(
        recovered_progress.phase,
        ImportPhase::Connecting
            | ImportPhase::Importing
            | ImportPhase::Verifying
            | ImportPhase::FinalPush
    ) {
        let prev_phase = format!("{:?}", recovered_progress.phase);
        warn!(
            previous_phase = %prev_phase,
            current_rev = recovered_progress.current_rev,
            total_revs = recovered_progress.total_revs,
            "import was active when daemon last stopped — marking as failed"
        );
        recovered_progress.phase = ImportPhase::Failed;
        recovered_progress.errors.push(format!(
            "Daemon crashed or was killed while import was in '{}' phase at r{}/r{}. \
             Please re-trigger the import via the repository detail page.",
            prev_phase.to_lowercase(),
            recovered_progress.current_rev,
            recovered_progress.total_revs
        ));
        recovered_progress.push_log(format!(
            "Import marked as failed on daemon startup (previous phase: {})",
            prev_phase
        ));
        // Persist the reconciled state immediately so the web UI reflects it.
        let _ = sync_engine
            .db()
            .persist_import_progress(&recovered_progress);
    }
    let import_progress = std::sync::Arc::new(tokio::sync::RwLock::new(recovered_progress));

    // Initialize web server
    let web_server = WebServer::new(
        config.clone(),
        web_db,
        sync_engine.clone(),
        sync_tx.clone(),
        args.config.clone(),
        import_progress.clone(),
    );
    let ws_broadcast = web_server.broadcast_sender();
    let listen_addr = config.web.listen.clone();

    // Spawn Teams notification listener if configured.
    // Subscribes to the same broadcast channel as WebSocket clients
    // and forwards relevant events to Teams as Adaptive Cards.
    if let Some(ref teams_url) = config.notifications.teams_webhook_url {
        let teams_notifier = reposync_core::notify::teams::TeamsNotifier::new(teams_url.clone());
        let mut teams_rx = ws_broadcast.subscribe();
        tokio::spawn(async move {
            info!("Teams notification listener started");
            loop {
                match teams_rx.recv().await {
                    Ok(msg) => {
                        teams_notifier.process_event(&msg).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(skipped = n, "Teams listener lagged, missed events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("Teams listener: broadcast channel closed, shutting down");
                        break;
                    }
                }
            }
        });
    }

    // Also check for Teams webhook URL from database (kv_state)
    // This allows admin to configure via the Settings UI without restarting
    {
        let check_db =
            reposync_core::db::Database::new(config.daemon.data_dir.join("reposync.db")).ok();
        let db_teams_url = check_db
            .and_then(|db| db.get_state("teams_webhook_url").ok().flatten())
            .filter(|v| !v.is_empty());
        if let Some(ref url) = db_teams_url {
            if config.notifications.teams_webhook_url.is_none() {
                let teams_notifier = reposync_core::notify::teams::TeamsNotifier::new(url.clone());
                let mut teams_rx = ws_broadcast.subscribe();
                tokio::spawn(async move {
                    info!("Teams notification listener started (from DB config)");
                    loop {
                        match teams_rx.recv().await {
                            Ok(msg) => {
                                teams_notifier.process_event(&msg).await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!(skipped = n, "Teams listener lagged");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
            }
        }
    }
    let app_state_for_cleanup = web_server.app_state();
    let app_state_for_shutdown = web_server.app_state();

    // Start web server — runs on the main tokio runtime directly
    // (not spawned) to ensure it gets immediate access to worker threads.
    let web_handle = tokio::spawn(async move {
        if let Err(e) = web_server.start(&listen_addr).await {
            error!("Web server error: {}", e);
        }
    });

    // Spawn background session cleanup task (every 5 minutes)
    {
        let app_state = app_state_for_cleanup;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                let now = chrono::Utc::now();
                let mut sessions = app_state.sessions.write().await;
                let before = sessions.len();
                sessions.retain(|_, expires_at| *expires_at > now);
                let pruned = before - sessions.len();
                if pruned > 0 {
                    tracing::debug!(pruned, "pruned expired in-memory sessions");
                }
            }
        });
    }

    // Create a shutdown notify for cooperative cancellation
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let scheduler_shutdown = shutdown.clone();

    // Open a third DB connection for the scheduler's per-repo sync cycles.
    let scheduler_db =
        Database::new(&db_path).context("failed to open scheduler database connection")?;

    // Create and start the scheduler
    let poll_interval = std::time::Duration::from_secs(config.daemon.poll_interval_secs);
    let mut sched = scheduler::Scheduler::new(
        sync_engine.clone(),
        poll_interval,
        sync_rx,
        ws_broadcast,
        import_progress,
        scheduler_db,
        config.clone(),
    );

    // Capture owners before moving the scheduler. Registry custody survives
    // cancellation/unwind even when no shutdown snapshot was possible.
    let sync_handles = sched.sync_handles.clone();
    let scheduler_shutdown_gate = sched.shutdown_gate();
    let _sync_registry_custody = TaskRegistryCustody {
        role: "sync tasks",
        handles: Arc::clone(&sync_handles),
    };
    let _import_registry_custody = TaskRegistryCustody {
        role: "import tasks",
        handles: Arc::clone(&app_state_for_shutdown.import_handles),
    };
    let scheduler_state = app_state_for_shutdown.clone();

    // Start the scheduler in a background task. The JoinHandle is raced
    // against shutdown so a panic, join error, or early exit is recorded
    // on AppState immediately instead of being ignored until SIGTERM.
    let mut scheduler_handle = OwnedShutdownTask::new(
        "scheduler",
        tokio::spawn(async move {
            sched.run(scheduler_shutdown).await;
        }),
    );
    let mut scheduler_failure: Option<String> = None;

    tokio::select! {
        _ = signals::wait_for_shutdown() => {
            info!("Shutdown signal received, stopping...");
        }
        end = scheduler_handle.join() => {
            if let Some((level, reason)) = reposync_web::scheduler_failure_from_task_end(
                false,
                &reposync_web::SchedulerTaskEnd::from_join(end),
            ) {
                error!(%reason, "scheduler ended before shutdown; marking service fatal");
                scheduler_state.record_scheduler_failure(level, reason.clone());
                scheduler_failure = Some(reason);
            }
            info!("scheduler is stopped; waiting for shutdown so health can report it");
            signals::wait_for_shutdown().await;
            info!("Shutdown signal received after scheduler failure, stopping...");
        }
    }

    let scheduler_stop = stop_scheduler_and_drain_sync_tasks(
        &shutdown,
        &scheduler_shutdown_gate,
        &mut scheduler_handle,
        &sync_handles,
        reposync_web::SCHEDULER_STOP_TIMEOUT,
        scheduler_failure,
        &mut |level, reason| scheduler_state.record_scheduler_failure(level, reason),
        &mut || {},
        Duration::from_secs(30),
    )
    .await;
    scheduler_failure = scheduler_stop.failure;
    info!(
        drained = scheduler_stop.drained_sync_tasks,
        unresolved = scheduler_stop.unresolved_sync_tasks,
        join_failures = scheduler_stop.sync_join_failures,
        stop_timeout = scheduler_stop.stop_timeout_recorded,
        "scheduler shutdown join report"
    );
    let mut unresolved_shutdown = !scheduler_stop.terminated_before_drain
        || scheduler_stop.unresolved_sync_tasks != 0;
    if scheduler_stop.terminated_before_drain {
        let imports = drain_owned_tasks(
            &app_state_for_shutdown.import_handles,
            "import task",
            Duration::from_secs(60),
            reposync_web::SCHEDULER_STOP_TIMEOUT,
        )
        .await;
        unresolved_shutdown |= imports.unresolved != 0;
        for reason in &imports.join_failures {
            error!(%reason, "observed import task join failure");
        }
        if imports.timed_out || imports.unresolved != 0 || !imports.join_failures.is_empty() {
            let reason = format!(
                "import task shutdown incomplete: timed_out={}, unresolved={}, join_failures={}",
                imports.timed_out, imports.unresolved, imports.join_failures.len()
            );
            scheduler_state.record_scheduler_failure(
                reposync_web::SchedulerServiceLevel::Fatal,
                reason.clone(),
            );
            scheduler_failure.get_or_insert(reason);
        }
    } else {
        error!("scheduler join unresolved; sync/import registries remain owned, not drained");
    }

    // The web server uses with_graceful_shutdown and will drain connections
    // when the Tokio runtime shuts down. Give it a moment to finish.
    match tokio::time::timeout(std::time::Duration::from_secs(5), web_handle).await {
        Ok(Ok(())) => info!("web server stopped gracefully"),
        Ok(Err(e)) => warn!("web server task error: {}", e),
        Err(_) => info!("web server shutdown timed out, proceeding"),
    }

    if unresolved_shutdown {
        // A retained join is not a cleanup proof. Do not checkpoint storage
        // while a scheduler or child writer may still be running.
        anyhow::bail!("shutdown retains unjoined work; WAL checkpoint skipped");
    }

    // Checkpoint the SQLite WAL to prevent corruption on unclean exit.
    // Open a fresh connection since the original db/web_db were moved into AppState.
    info!("checkpointing SQLite WAL...");
    let db_path = config.daemon.data_dir.join("reposync.db");
    match rusqlite::Connection::open(&db_path) {
        Ok(conn) => match conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
            Ok(_) => info!("WAL checkpoint completed successfully"),
            Err(e) => warn!("WAL checkpoint failed: {}", e),
        },
        Err(e) => warn!("could not open DB for WAL checkpoint: {}", e),
    }

    info!("RepoSync daemon stopped.");
    if let Some(reason) = scheduler_failure {
        anyhow::bail!("scheduler failed: {reason}");
    }
    Ok(())
}


/// AUTHORED UNRUN. These cases exercise ownership and join observations only;
/// expected injected panic cases are explicitly named and must retain evidence.
#[cfg(test)]
mod shutdown_ownership_tests {
    use super::*;

    fn empty_handles() -> TaskHandles {
        Arc::new(tokio::sync::Mutex::new(Vec::new()))
    }

    /// Intentional injected scheduler panic, including the already-finished path.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finished_scheduler_panic_is_joined_and_classified() {
        let notify = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(scheduler::SchedulerShutdownGate::default());
        let handles = empty_handles();
        let mut task = OwnedShutdownTask::new("finished scheduler panic regression",
            tokio::spawn(async { panic!("RS112 expected scheduler panic"); }));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !task.is_finished() { tokio::task::yield_now().await; }
        }).await.unwrap();
        assert!(!task.join_observed());
        let mut failures = Vec::new();
        let report = stop_scheduler_and_drain_sync_tasks(
            &notify, &gate, &mut task, &handles, Duration::from_secs(2), None,
            &mut |level, reason| failures.push((level, reason.to_string())),
            &mut || panic!("finished scheduler must not time out"),
            Duration::from_secs(2),
        ).await;
        assert!(task.join_observed());
        assert!(report.terminated_before_drain);
        assert!(!report.stop_timeout_recorded);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, reposync_web::SchedulerServiceLevel::Fatal);
        assert!(failures[0].1.contains("panicked during shutdown"));
        assert_eq!(report.failure.as_deref(), Some(failures[0].1.as_str()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn existing_failure_does_not_skip_an_unobserved_scheduler_join() {
        let notify = Arc::new(tokio::sync::Notify::new());
        let wait = Arc::clone(&notify);
        let gate = Arc::new(scheduler::SchedulerShutdownGate::default());
        let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mark_exit = Arc::clone(&exited);
        let mut task = OwnedShutdownTask::new("existing failure regression", tokio::spawn(async move {
            wait.notified().await;
            mark_exit.store(true, Ordering::SeqCst);
        }));
        let report = stop_scheduler_and_drain_sync_tasks(
            &notify, &gate, &mut task, &empty_handles(), Duration::from_secs(2),
            Some("retained earlier fatal reason".to_string()),
            &mut |_level, _reason| panic!("cooperative join must not add failure"),
            &mut || panic!("cooperative join must not time out"),
            Duration::from_secs(2),
        ).await;
        assert_eq!(report.failure.as_deref(), Some("retained earlier fatal reason"));
        assert!(report.terminated_before_drain);
        assert!(task.join_observed());
        assert!(exited.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_deadline_aborts_and_observes_scheduler_before_child_drain() {
        struct MarkExit(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for MarkExit {
            fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); }
        }
        let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mark = MarkExit(Arc::clone(&exited));
        let entered = Arc::new(tokio::sync::Notify::new());
        let announce = Arc::clone(&entered);
        let mut task = OwnedShutdownTask::new("abort join regression", tokio::spawn(async move {
            let _mark = mark;
            announce.notify_one();
            std::future::pending::<()>().await;
        }));
        tokio::time::timeout(Duration::from_secs(2), entered.notified()).await.unwrap();
        let handles = empty_handles();
        let seen_exit = Arc::clone(&exited);
        handles.lock().await.push(tokio::spawn(async move {
            while !seen_exit.load(Ordering::SeqCst) { tokio::task::yield_now().await; }
        }));
        let mut failures = Vec::new();
        let report = stop_scheduler_and_drain_sync_tasks(
            &Arc::new(tokio::sync::Notify::new()),
            &Arc::new(scheduler::SchedulerShutdownGate::default()),
            &mut task, &handles, Duration::from_millis(20), None,
            &mut |_level, reason| failures.push(reason.to_string()),
            &mut || {
                assert!(!exited.load(Ordering::SeqCst));
                assert_eq!(handles.try_lock().unwrap().len(), 1);
            },
            Duration::from_secs(2),
        ).await;
        let (_, reason) = reposync_web::scheduler_failure_from_stop_timeout();
        assert_eq!(failures, vec![reason.clone()]);
        assert_eq!(report.failure.as_deref(), Some(reason.as_str()));
        assert!(report.stop_timeout_recorded);
        assert!(report.terminated_before_drain);
        assert!(task.join_observed());
        assert!(exited.load(Ordering::SeqCst));
        assert_eq!(report.drained_sync_tasks, 1);
        assert_eq!(report.unresolved_sync_tasks, 0);
        assert!(handles.lock().await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abort_timeout_retains_the_same_scheduler_handle_and_closes_child_drain() {
        struct Release(Option<std::sync::mpsc::Sender<()>>);
        impl Drop for Release {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() { let _ = tx.send(()); }
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let release = Release(Some(tx));
        let entered = Arc::new(tokio::sync::Notify::new());
        let announce = Arc::clone(&entered);
        let mut task = OwnedShutdownTask::new("noncancellable scheduler regression",
            tokio::spawn(async move {
                tokio::task::block_in_place(|| {
                    announce.notify_one();
                    rx.recv_timeout(Duration::from_secs(5)).expect("fixture release");
                });
            }));
        let id = task.id;
        tokio::time::timeout(Duration::from_secs(2), entered.notified()).await.unwrap();
        let handles = empty_handles();
        let child_release = Arc::new(tokio::sync::Notify::new());
        let wait = Arc::clone(&child_release);
        handles.lock().await.push(tokio::spawn(async move { wait.notified().await; }));
        let report = stop_scheduler_and_drain_sync_tasks(
            &Arc::new(tokio::sync::Notify::new()),
            &Arc::new(scheduler::SchedulerShutdownGate::default()),
            &mut task, &handles, Duration::from_millis(20), None,
            &mut |_level, _reason| {}, &mut || {}, Duration::from_millis(20),
        ).await;
        let unobserved = !task.join_observed();
        let still_owned_children = handles.lock().await.len();
        drop(task);
        let mut retained = OwnedShutdownTask::take_retained(id).expect("same retained owner");
        assert_eq!(retained.id, id);
        drop(release);
        tokio::time::timeout(Duration::from_secs(2), retained.join()).await
            .expect("released blocking work must join").unwrap();
        child_release.notify_one();
        let children = drain_owned_tasks(
            &handles, "noncancellable regression cleanup",
            Duration::from_secs(2), Duration::from_secs(2),
        ).await;
        assert!(report.stop_timeout_recorded);
        assert!(!report.terminated_before_drain);
        assert_eq!(report.drained_sync_tasks, 0);
        assert_eq!(still_owned_children, 1);
        assert!(unobserved);
        assert!(retained.join_observed());
        assert!(OwnedShutdownTask::take_retained(id).is_none());
        assert_eq!(children.owned, 1);
        assert_eq!(children.unresolved, 0);
        assert!(children.join_failures.is_empty());
    }

    /// Intentional injected child panic; finished is not successful completion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn child_drain_observes_an_already_finished_panic() {
        let handles = empty_handles();
        handles.lock().await.push(tokio::spawn(async { panic!("RS112 expected drain panic"); }));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if handles.lock().await[0].is_finished() { break; }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        let report = drain_owned_tasks(
            &handles, "finished drain panic regression",
            Duration::from_secs(2), Duration::from_secs(2),
        ).await;
        assert_eq!(report.owned, 1);
        assert_eq!(report.join_failures.len(), 1);
        assert!(report.join_failures[0].contains("panic"));
        assert_eq!(report.unresolved, 0);
        assert!(handles.lock().await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_child_drain_retains_current_and_remaining_owners() {
        let handles = empty_handles();
        let mut releases = Vec::new();
        for _ in 0..2 {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            releases.push(tx);
            handles.lock().await.push(tokio::spawn(async move { let _ = rx.await; }));
        }
        let timeout = tokio::time::timeout(Duration::from_millis(20), drain_owned_tasks(
            &handles, "cancelled drain regression",
            Duration::from_secs(5), Duration::from_secs(2),
        )).await;
        assert!(timeout.is_err());
        let ids: Vec<_> = RETAINED_SHUTDOWN_TASKS.get().unwrap().lock().unwrap().iter()
            .filter(|task| task.role == "cancelled drain regression")
            .map(|task| task.id).collect();
        for release in releases { let _ = release.send(()); }
        for id in &ids {
            let mut owner = OwnedShutdownTask::take_retained(*id).expect("retained child owner");
            tokio::time::timeout(Duration::from_secs(2), owner.join()).await.unwrap().unwrap();
            assert!(owner.join_observed());
        }
        assert_eq!(ids.len(), 2);
        assert!(handles.lock().await.is_empty());
        for id in ids { assert!(OwnedShutdownTask::take_retained(id).is_none()); }
    }
}
