//! RepoSync Personal Branch Mode daemon.
//!
//! A lightweight, single-developer tool that mirrors an SVN repo to a personal
//! GitHub repository. Supports feature branches, PRs, and automatic bidirectional
//! commit sync.

mod commit_format;
mod daemon;
mod engine;
mod git_to_svn;
mod initial_import;
mod pr_monitor;
mod scheduler;
mod signals;
mod svn_to_git;

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use reposync_core::db::Database;
use reposync_core::config::GitProvider;
use reposync_core::git::github::GitHubClient;
use reposync_core::git::GitClient;
use reposync_core::personal_config::PersonalConfig;
use reposync_core::svn::SvnClient;

use crate::engine::PersonalSyncEngine;
use crate::initial_import::{ImportMode, InitialImport};

/// RepoSync Personal Branch Mode — individual SVN↔Git sync daemon.
#[derive(Parser)]
#[command(name = "reposync-personal", version, about)]
struct Cli {
    /// Path to the personal config file.
    #[arg(short, long, default_value = "~/.config/reposync/personal.toml")]
    config: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the sync daemon.
    Start {
        /// Run in the foreground (default is background).
        #[arg(long)]
        foreground: bool,
    },

    /// Stop the running daemon.
    Stop,

    /// Check daemon status.
    Status,

    /// Run a single sync cycle and exit.
    Sync,

    /// Import SVN history into Git.
    Import {
        /// Import only a snapshot of HEAD (one commit).
        #[arg(long, conflicts_with = "full")]
        snapshot: bool,

        /// Import full SVN history (one commit per revision).
        #[arg(long, conflicts_with = "snapshot")]
        full: bool,
    },

    /// Emit log messages at every level and exit.  Used by integration tests
    /// to verify file sink and level filtering without requiring a running
    /// SVN/Git environment.
    #[command(name = "log-probe", hide = true)]
    LogProbe,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Resolve config path (expand ~).
    let config_path = expand_tilde(&cli.config);

    // Load config early so we can use log_level and data_dir for tracing init.
    // If the config file doesn't exist (e.g. stop/status on unconfigured system),
    // fall back to simple tracing with default "info" level and no file appender.
    let config_opt = PersonalConfig::load_from_file(&config_path).ok();

    // Initialize tracing with config-aware log level and file appender.
    init_tracing(config_opt.as_ref());

    match cli.command {
        Commands::Start { foreground } => cmd_start(&config_path, foreground).await,
        Commands::Stop => cmd_stop(&config_path),
        Commands::Status => cmd_status(&config_path),
        Commands::Sync => cmd_sync(&config_path).await,
        Commands::Import { snapshot, full: _ } => {
            let mode = if snapshot {
                ImportMode::Snapshot
            } else {
                // Default to full if neither flag specified
                ImportMode::Full
            };
            cmd_import(&config_path, mode).await
        }
        Commands::LogProbe => {
            cmd_log_probe();
            Ok(())
        }
    }
}

/// Initialize tracing with the personal config's `log_level` and file appender.
///
/// - If `RUST_LOG` is set, it overrides `personal.log_level`.
/// - When a valid config is available, a non-blocking file appender writes to
///   `{data_dir}/personal.log`. The file appender is NOT rolling — the file is
///   appended to across daemon restarts.  Operators should use external log
///   rotation (e.g. `logrotate`) for long-running deployments.
/// - Console (stderr) output is always enabled.
fn init_tracing(config: Option<&PersonalConfig>) {
    let log_level = config
        .map(|c| c.personal.log_level.as_str())
        .unwrap_or("info");

    // RUST_LOG takes precedence; otherwise use config log_level.
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));

    // Console layer (always present).
    let console_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false);

    // Optionally add a file appender layer when config provides a valid data_dir.
    if let Some(cfg) = config {
        let data_dir = &cfg.personal.data_dir;
        // Best-effort: create data_dir if it doesn't exist yet.
        let _ = std::fs::create_dir_all(data_dir);

        let file_appender = tracing_appender::rolling::never(data_dir, "personal.log");
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

        // Leak the guard so the non-blocking writer lives for the process lifetime.
        // This is intentional — the daemon runs until exit, and we need the writer
        // to stay alive.  The OS reclaims the file handle on process exit.
        std::mem::forget(_guard);

        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(non_blocking)
            .with_target(true)
            .with_ansi(false);

        tracing_subscriber::registry()
            .with(filter)
            .with(console_layer)
            .with(file_layer)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(console_layer)
            .init();
    }
}

/// Load config and build the sync engine.
async fn build_engine(config_path: &str) -> Result<(PersonalSyncEngine, PersonalConfig)> {
    let config =
        PersonalConfig::load_and_resolve(config_path).context("failed to load personal config")?;
    config.validate().context("invalid personal config")?;

    // Ensure data directory exists.
    let data_dir = &config.personal.data_dir;
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create data directory: {}", data_dir.display()))?;

    // Initialize database.
    let db_path = data_dir.join("personal.db");
    let db = Database::new(&db_path).context("failed to initialize database")?;

    // Create SVN client.
    let svn_client = SvnClient::new(
        &config.svn.url,
        &config.svn.username,
        config.svn.password.as_deref().unwrap_or(""),
    );

    // Set up Git repository.
    let git_repo_path = data_dir.join("git-repo");
    let git_client = if git_repo_path.exists() {
        GitClient::new(&git_repo_path).context("failed to open git repository")?
    } else {
        std::fs::create_dir_all(&git_repo_path).context("failed to create git repo directory")?;
        let remote_url = config.github.clone_url();
        GitClient::clone_repo(&remote_url, &git_repo_path, config.github.token.as_deref())
            .context("failed to clone git repository")?
    };

    // Create GitHub client.
    let github_token = config.github.token.as_deref().unwrap_or("");
    let github_client = GitHubClient::new(&config.github.api_url, github_token, GitProvider::default());

    let engine = PersonalSyncEngine::new(config.clone(), db, svn_client, git_client, github_client);
    Ok((engine, config))
}

/// Start the sync daemon.
async fn cmd_start(config_path: &str, foreground: bool) -> Result<()> {
    let (engine, config) = build_engine(config_path).await?;
    let data_dir = &config.personal.data_dir;

    // Check if already running.
    if let Some(pid) = daemon::is_running(data_dir)? {
        anyhow::bail!("daemon is already running (PID {})", pid);
    }

    if foreground {
        info!("starting personal sync daemon in foreground mode");
        daemon::write_pid_file(&daemon::pid_file_path(data_dir))?;

        let shutdown = signals::setup_signal_handlers();
        let interval = Duration::from_secs(config.personal.poll_interval_secs);

        let result = scheduler::run_polling_loop(&engine, interval, shutdown).await;

        daemon::remove_pid_file(&daemon::pid_file_path(data_dir))?;
        result
    } else {
        eprintln!("WARNING: The --background flag does not truly daemonize the process.");
        eprintln!("         For production use, run with systemd, launchd, or nohup:");
        eprintln!("         $ nohup reposync-personal start --foreground &");
        eprintln!("         Or use the provided systemd service file.");
        eprintln!();
        info!("starting personal sync daemon (note: not truly daemonized, use systemd for production)");
        daemon::write_pid_file(&daemon::pid_file_path(data_dir))?;

        let shutdown = signals::setup_signal_handlers();
        let interval = Duration::from_secs(config.personal.poll_interval_secs);

        let result = scheduler::run_polling_loop(&engine, interval, shutdown).await;

        daemon::remove_pid_file(&daemon::pid_file_path(data_dir))?;
        result
    }
}

/// Stop the daemon.
fn cmd_stop(config_path: &str) -> Result<()> {
    let config =
        PersonalConfig::load_and_resolve(config_path).context("failed to load personal config")?;
    let data_dir = &config.personal.data_dir;

    match daemon::stop_daemon(data_dir)? {
        true => {
            info!("daemon stopped successfully");
            println!("✓ Daemon stopped gracefully");
        }
        false => {
            println!("Daemon is not running");
        }
    }
    Ok(())
}

/// Show daemon status.
fn cmd_status(config_path: &str) -> Result<()> {
    let config =
        PersonalConfig::load_and_resolve(config_path).context("failed to load personal config")?;
    let data_dir = &config.personal.data_dir;

    match daemon::is_running(data_dir)? {
        Some(pid) => println!("● Running (PID {})", pid),
        None => println!("○ Not running"),
    }
    Ok(())
}

/// Run a single sync cycle.
async fn cmd_sync(config_path: &str) -> Result<()> {
    let (engine, _config) = build_engine(config_path).await?;

    info!("running single sync cycle");
    let stats = engine.run_cycle().await?;

    println!(
        "Sync complete: SVN→Git: {} commits, Git→SVN: {} commits ({} PRs)",
        stats.svn_to_git_count, stats.git_to_svn_count, stats.prs_processed
    );
    Ok(())
}

/// Import SVN history into Git.
async fn cmd_import(config_path: &str, mode: ImportMode) -> Result<()> {
    let config =
        PersonalConfig::load_and_resolve(config_path).context("failed to load personal config")?;
    config.validate().context("invalid personal config")?;

    let data_dir = &config.personal.data_dir;
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create data directory: {}", data_dir.display()))?;

    // Initialize database.
    let db_path = data_dir.join("personal.db");
    let db = Database::new(&db_path).context("failed to initialize database")?;

    // Create SVN client.
    let svn_client = SvnClient::new(
        &config.svn.url,
        &config.svn.username,
        config.svn.password.as_deref().unwrap_or(""),
    );

    // GitHub client.
    let github_token = config.github.token.as_deref().unwrap_or("");
    let github_client = GitHubClient::new(&config.github.api_url, github_token, GitProvider::default());

    // Initialize Git repo for import.
    let git_repo_path = data_dir.join("git-repo");
    let git_client = if git_repo_path.exists() {
        GitClient::new(&git_repo_path).context("failed to open git repository")?
    } else {
        std::fs::create_dir_all(&git_repo_path).context("failed to create git repo directory")?;
        // For import, try to clone first; if repo doesn't exist yet, init locally
        let remote_url = config.github.clone_url();
        match GitClient::clone_repo(&remote_url, &git_repo_path, config.github.token.as_deref()) {
            Ok(client) => client,
            Err(_) => {
                // Repo might not exist yet (auto-create will handle it)
                GitClient::init(&git_repo_path).context("failed to init git repository")?
            }
        }
    };
    let git_client = std::sync::Arc::new(std::sync::Mutex::new(git_client));

    let commit_format = crate::commit_format::CommitFormatter::new(&config.commit_format);

    let importer = InitialImport {
        svn_client: &svn_client,
        git_client: &git_client,
        github_client: &github_client,
        db: &db,
        config: &config,
        formatter: &commit_format,
    };

    let count = importer.import(mode).await?;
    println!("✓ Import complete! {} commits created.", count);
    Ok(())
}

/// Emit one log message at each level for integration-test verification.
/// Includes a brief pause to let the non-blocking file appender flush
/// (the guard is intentionally leaked in `init_tracing`).
fn cmd_log_probe() {
    tracing::error!("LOG_PROBE error-level marker");
    tracing::warn!("LOG_PROBE warn-level marker");
    tracing::info!("LOG_PROBE info-level marker");
    tracing::debug!("LOG_PROBE debug-level marker");
    tracing::trace!("LOG_PROBE trace-level marker");
    // Allow the non-blocking writer's background thread to drain.
    std::thread::sleep(Duration::from_millis(200));
}

/// Expand `~` to the user's home directory.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}/{}", home.display(), rest);
        }
    }
    path.to_string()
}
