//! RepoSync daemon entry point.
//!
//! Loads configuration, initializes all subsystems, starts the web server
//! and sync scheduler, and handles graceful shutdown.

mod scheduler;
mod signals;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use reposync_core::config::AppConfig;
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

    // Ensure data directory exists
    std::fs::create_dir_all(&config.daemon.data_dir).context("failed to create data directory")?;

    // Initialize database
    let db_path = config.daemon.data_dir.join("reposync.db");
    let db = Database::new(&db_path).context("failed to open database")?;
    db.initialize()
        .context("failed to initialize database schema")?;
    // Open a second connection for the web server (SQLite supports multiple readers with WAL)
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
                key.strip_prefix("secret_svn_password_").map(|s| s.to_string())
            })();
            if let Some(ref id) = reuse_repo_id {
                info!("Reusing existing repo UUID {} from orphaned credential keys", id);
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
                            let _ = db.set_state(&format!("secret_svn_password_{}", default_repo.id), &pw);
                            info!("Migrated global SVN password to per-repo key for {}", default_repo.name);
                        }
                    }
                    if let Ok(Some(tok)) = db.get_state("secret_git_token") {
                        if !tok.is_empty() {
                            let _ = db.set_state(&format!("secret_git_token_{}", default_repo.id), &tok);
                            info!("Migrated global Git token to per-repo key for {}", default_repo.name);
                        }
                    }
                }
                Err(e) => warn!("Failed to auto-migrate config to repository: {}", e),
            }
        }
    }

    // Ensure repos have per-repo credential keys.
    // For child repos (branch pairs): inherit from PARENT, not global.
    // For parent repos without credentials: only migrate from global
    //   if this is the ONLY parent repo (avoids cross-contamination).
    {
        let repos = db.list_repositories().unwrap_or_default();
        let parent_count = repos.iter().filter(|r| r.parent_id.is_none()).count();

        for repo in &repos {
            let svn_key = format!("secret_svn_password_{}", repo.id);
            if db.get_state(&svn_key).ok().flatten().filter(|v| !v.is_empty()).is_none() {
                // Try parent's credentials first (for branch pairs)
                let source_pw = repo.parent_id.as_ref().and_then(|pid| {
                    db.get_state(&format!("secret_svn_password_{}", pid))
                        .ok().flatten().filter(|v| !v.is_empty())
                });
                // Only fall back to global if this is the sole parent repo
                let source_pw = source_pw.or_else(|| {
                    if repo.parent_id.is_none() && parent_count == 1 {
                        db.get_state("secret_svn_password").ok().flatten().filter(|v| !v.is_empty())
                    } else {
                        None
                    }
                });
                if let Some(pw) = source_pw {
                    let _ = db.set_state(&svn_key, &pw);
                    info!(repo_name = %repo.name, "migrated SVN password to per-repo key");
                }
            }

            let git_key = format!("secret_git_token_{}", repo.id);
            if db.get_state(&git_key).ok().flatten().filter(|v| !v.is_empty()).is_none() {
                let source_tok = repo.parent_id.as_ref().and_then(|pid| {
                    db.get_state(&format!("secret_git_token_{}", pid))
                        .ok().flatten().filter(|v| !v.is_empty())
                });
                let source_tok = source_tok.or_else(|| {
                    if repo.parent_id.is_none() && parent_count == 1 {
                        db.get_state("secret_git_token").ok().flatten().filter(|v| !v.is_empty())
                    } else {
                        None
                    }
                });
                if let Some(tok) = source_tok {
                    let _ = db.set_state(&git_key, &tok);
                    info!(repo_name = %repo.name, "migrated Git token to per-repo key");
                }
            }
        }
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
    // Ensure the remote URL has embedded credentials for reliable HTTP auth.
    git_client
        .ensure_remote_credentials("origin", config.github.token.as_deref())
        .ok(); // Don't crash if this fails

    // Initialize identity mapper
    let identity_mapper = Arc::new(
        IdentityMapper::new(&config.identity).context("failed to initialize identity mapper")?,
    );
    info!("Identity mapper initialized");

    // Initialize sync engine
    let mut engine = SyncEngine::new(
        config.clone(),
        db,
        svn_client,
        git_client,
        identity_mapper,
    );
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
                if let Ok(Some(rev_str)) = engine.db().get_watermark("svn_rev") {
                    if let Ok(rev) = rev_str.parse::<i64>() {
                        if rev > 0 {
                            let sha = engine.db().get_watermark("git_sha")
                                .ok().flatten().unwrap_or_default();
                            match engine.db().update_repo_watermark(&repo.id, rev, &sha) {
                                Ok(()) => {
                                    info!(repo_name = %repo.name, rev, "Recovered watermark from global watermarks table (single-repo migration)");
                                    continue;
                                }
                                Err(e) => warn!("Failed to write watermark for {}: {}", repo.name, e),
                            }
                        }
                    }
                }
            }

            // Per-repo kv_state keys (always safe — these are scoped by repo_id)
            let repo_key = format!("last_svn_rev_{}", repo.id);
            if let Ok(Some(rev_str)) = engine.db().get_state(&repo_key) {
                if let Ok(rev) = rev_str.parse::<i64>() {
                    if rev > 0 {
                        let sha_key = format!("last_git_sha_{}", repo.id);
                        let sha = engine.db().get_state(&sha_key).ok().flatten().unwrap_or_default();
                        match engine.db().update_repo_watermark(&repo.id, rev, &sha) {
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
                // Read git log and scan for sync markers
                let output = tokio::process::Command::new("git")
                    .args(["log", "--oneline", "-200", "--format=%H %s"])
                    .current_dir(git_dir)
                    .output()
                    .await;
                if let Ok(output) = output {
                    if output.status.success() {
                        let log_text = String::from_utf8_lossy(&output.stdout);
                        let re = regex_lite::Regex::new(
                            r"(?i)(?:\[(?:gitsvnsync|reposync)\].*SVN r(\d+)|imported from SVN r(\d+))",
                        )
                        .unwrap();
                        let mut max_rev: i64 = 0;
                        let mut head_sha = String::new();
                        for line in log_text.lines() {
                            // First line is HEAD
                            if head_sha.is_empty() {
                                if let Some(sha) = line.split_whitespace().next() {
                                    head_sha = sha.to_string();
                                }
                            }
                            if let Some(caps) = re.captures(line) {
                                let rev_str = caps
                                    .get(1)
                                    .or_else(|| caps.get(2))
                                    .map(|m| m.as_str())
                                    .unwrap_or("0");
                                if let Ok(rev) = rev_str.parse::<i64>() {
                                    max_rev = max_rev.max(rev);
                                }
                            }
                        }
                        if max_rev > 0 {
                            let sha_for_watermark = if head_sha.is_empty() {
                                String::new()
                            } else {
                                head_sha
                            };
                            match engine.db().update_repo_watermark(
                                &repo.id,
                                max_rev,
                                &sha_for_watermark,
                            ) {
                                Ok(()) => info!(
                                    repo_name = %repo.name,
                                    rev = max_rev,
                                    "Auto-detected watermark r{} for repo {}",
                                    max_rev,
                                    repo.name
                                ),
                                Err(e) => warn!(
                                    "Failed to write auto-detected watermark for {}: {}",
                                    repo.name, e
                                ),
                            }
                        }
                    }
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
        let _ = sync_engine.db().persist_import_progress(&recovered_progress);
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

    // Capture sync handles for graceful shutdown before moving sched
    let sync_handles = sched.sync_handles.clone();

    // Start the scheduler in a background task
    let scheduler_handle = tokio::spawn(async move {
        sched.run(scheduler_shutdown).await;
    });

    // Wait for shutdown signal
    signals::wait_for_shutdown().await;

    info!("Shutdown signal received, stopping...");

    // Signal cooperative shutdown to the scheduler
    shutdown.notify_waiters();

    // Wait for the scheduler to finish its current cycle (up to 10s)
    match tokio::time::timeout(std::time::Duration::from_secs(10), scheduler_handle).await {
        Ok(Ok(())) => info!("scheduler stopped gracefully"),
        Ok(Err(e)) => warn!("scheduler task error: {}", e),
        Err(_) => warn!("scheduler did not stop within 10s, forcing shutdown"),
    }

    // Wait for in-flight sync tasks (up to 30s)
    {
        let handles: Vec<_> = {
            let mut locked = sync_handles.lock().await;
            locked.drain(..).filter(|h| !h.is_finished()).collect()
        };
        if !handles.is_empty() {
            info!(count = handles.len(), "waiting for in-flight sync tasks...");
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            for handle in handles {
                match tokio::time::timeout_at(deadline, handle).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => warn!("sync task error: {}", e),
                    Err(_) => {
                        warn!("remaining sync tasks did not complete within 30s");
                        break;
                    }
                }
            }
            info!("in-flight sync task shutdown complete");
        }
    }

    // Wait for in-flight import tasks (up to 60s — imports are long-running)
    {
        let handles: Vec<_> = {
            let mut locked = app_state_for_shutdown.import_handles.lock().await;
            locked.drain(..).filter(|h| !h.is_finished()).collect()
        };
        if !handles.is_empty() {
            info!(count = handles.len(), "waiting for in-flight import tasks...");
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
            for handle in handles {
                match tokio::time::timeout_at(deadline, handle).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => warn!("import task error: {}", e),
                    Err(_) => {
                        warn!("remaining import tasks did not complete within 60s");
                        break;
                    }
                }
            }
            info!("in-flight import task shutdown complete");
        }
    }

    // The web server uses with_graceful_shutdown and will drain connections
    // when the Tokio runtime shuts down. Give it a moment to finish.
    match tokio::time::timeout(std::time::Duration::from_secs(5), web_handle).await {
        Ok(Ok(())) => info!("web server stopped gracefully"),
        Ok(Err(e)) => warn!("web server task error: {}", e),
        Err(_) => info!("web server shutdown timed out, proceeding"),
    }

    // Checkpoint the SQLite WAL to prevent corruption on unclean exit.
    // Open a fresh connection since the original db/web_db were moved into AppState.
    info!("checkpointing SQLite WAL...");
    let db_path = config.daemon.data_dir.join("reposync.db");
    match rusqlite::Connection::open(&db_path) {
        Ok(conn) => {
            match conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
                Ok(_) => info!("WAL checkpoint completed successfully"),
                Err(e) => warn!("WAL checkpoint failed: {}", e),
            }
        }
        Err(e) => warn!("could not open DB for WAL checkpoint: {}", e),
    }

    info!("RepoSync daemon stopped.");
    Ok(())
}
