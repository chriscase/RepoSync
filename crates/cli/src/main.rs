//! RepoSync command-line management tool.
//!
//! Provides subcommands for inspecting sync status, managing conflicts,
//! editing identity mappings, viewing the audit log, and generating /
//! validating configuration files.
//!
//! Also provides the `personal` subcommand group for Personal Branch Mode.

mod personal;
mod status;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use reposync_core::config::AppConfig;
use reposync_core::db::Database;
use reposync_core::identity::IdentityMapper;

// ---------------------------------------------------------------------------
// CLI argument definitions
// ---------------------------------------------------------------------------

/// RepoSync command-line management tool.
#[derive(Parser, Debug)]
#[command(
    name = "reposync",
    version,
    about = "Manage and inspect a RepoSync synchronization bridge"
)]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(
        short,
        long,
        global = true,
        default_value = "/etc/reposync/config.toml"
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Show current synchronization status.
    Status,

    /// Manage sync conflicts.
    Conflicts {
        #[command(subcommand)]
        action: ConflictsAction,
    },

    /// Trigger an immediate sync cycle.
    Sync {
        #[command(subcommand)]
        action: SyncAction,
    },

    /// Manage SVN-to-Git identity mappings.
    Identity {
        #[command(subcommand)]
        action: IdentityAction,
    },

    /// Generate a default configuration file.
    Init {
        /// Output path for the generated config file.
        #[arg(short, long, default_value = "./reposync.toml")]
        output: PathBuf,
    },

    /// Validate a configuration file.
    Validate,

    /// Show recent audit log entries.
    Audit {
        /// Maximum number of entries to show.
        #[arg(short, long, default_value = "20")]
        limit: u32,
    },

    /// Personal Branch Mode — individual SVN↔Git sync.
    Personal {
        /// Path to the personal config file.
        #[arg(long, default_value = "~/.config/reposync/personal.toml")]
        personal_config: String,

        #[command(subcommand)]
        action: personal::PersonalCommands,
    },
}

#[derive(Subcommand, Debug)]
enum ConflictsAction {
    /// List all active conflicts.
    List {
        /// Filter by status: detected, resolved, deferred.
        #[arg(short, long)]
        status: Option<String>,

        /// Number of results.
        #[arg(long, default_value = "20")]
        limit: u32,
    },
    /// Show details of a specific conflict.
    Show {
        /// Conflict ID.
        id: String,
    },
    /// Resolve a conflict.
    Resolve {
        /// Conflict ID.
        id: String,

        /// Resolution: svn or git.
        #[arg(long)]
        accept: String,
    },
}

#[derive(Subcommand, Debug)]
enum SyncAction {
    /// Trigger an immediate sync cycle.
    Now,
}

#[derive(Subcommand, Debug)]
enum IdentityAction {
    /// Test mapping an SVN user to a Git identity.
    Lookup {
        /// SVN username to look up.
        svn_user: String,
    },
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> ExitCode {
    // Minimal logging for CLI
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("warn"))
        .with_target(false)
        .without_time()
        .init();

    let cli = Cli::parse();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {:#}", e);
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Init { output } => cmd_init(&output),
        Commands::Validate => cmd_validate(&cli.config),
        Commands::Personal {
            personal_config,
            action,
        } => personal::run_personal(action, &personal_config).await,
        _ => {
            // All other commands need the team-mode config and database
            let config = load_config(&cli.config)?;
            let db = open_database(&config)?;

            match cli.command {
                Commands::Status => cmd_status(&db),
                Commands::Conflicts { action } => cmd_conflicts(&db, action),
                Commands::Sync { action } => cmd_sync(&db, &config, action).await,
                Commands::Identity { action } => cmd_identity(&config, action),
                Commands::Audit { limit } => cmd_audit(&db, limit),
                _ => unreachable!(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

fn load_config(path: &PathBuf) -> Result<AppConfig> {
    let mut config =
        AppConfig::load_from_file(path).context("failed to load configuration file")?;
    config
        .resolve_env_vars()
        .context("failed to resolve environment variables")?;
    Ok(config)
}

fn open_database(config: &AppConfig) -> Result<Database> {
    let db_path = config.daemon.data_dir.join("reposync.db");
    let db = Database::new(&db_path).context("failed to open database")?;
    db.initialize().context("failed to initialize database")?;
    Ok(db)
}

// ---------------------------------------------------------------------------
// Subcommand implementations
// ---------------------------------------------------------------------------

fn cmd_init(output: &PathBuf) -> Result<()> {
    let default_config = r#"# RepoSync Configuration
# See documentation for all available options.

[daemon]
poll_interval_secs = 60
log_level = "info"
data_dir = "/var/lib/reposync"

[svn]
url = "https://svn.example.com/repo"
username = "svn_user"
password_env = "SVN_PASSWORD"
layout = "standard"
trunk_path = "trunk"
branches_path = "branches"
tags_path = "tags"
# webhook_secret_env = "SVN_WEBHOOK_SECRET"

[github]
api_url = "https://api.github.com"
repo = "owner/repo"
token_env = "GITHUB_TOKEN"
webhook_secret_env = "GITHUB_WEBHOOK_SECRET"
default_branch = "main"

[identity]
email_domain = "example.com"

[web]
listen = "127.0.0.1:3000"
auth_mode = "simple"
admin_password_env = "ADMIN_PASSWORD"

[notifications]
# slack_webhook_url_env = "SLACK_WEBHOOK_URL"
# email_smtp = "smtp.example.com:587"
# email_from = "reposync@example.com"
# email_recipients = ["admin@example.com"]

[sync]
mode = "direct"
auto_merge = true
sync_tags = true
"#;

    if output.exists() {
        anyhow::bail!(
            "file already exists: {}. Use a different path or remove the existing file.",
            output.display()
        );
    }

    std::fs::write(output, default_config).context("failed to write config file")?;

    println!("Default configuration written to {}", output.display());
    println!();
    println!("Next steps:");
    println!("  1. Edit the config file with your SVN and GitHub details");
    println!("  2. Set the referenced environment variables (SVN_PASSWORD, GITHUB_TOKEN, etc.)");
    println!(
        "  3. Validate with: reposync validate --config {}",
        output.display()
    );
    println!(
        "  4. Start the daemon: reposync-daemon --config {}",
        output.display()
    );

    Ok(())
}

fn cmd_validate(config_path: &PathBuf) -> Result<()> {
    println!("Validating configuration: {}", config_path.display());
    println!();

    let config = AppConfig::load_from_file(config_path).context("failed to parse configuration")?;

    // Check structure
    println!("  [OK] TOML structure is valid");

    // Resolve env vars (non-fatal warnings)
    let mut config = config;
    let _ = config.resolve_env_vars();
    println!("  [OK] Environment variable references processed");

    // Validate values
    match config.validate() {
        Ok(()) => {
            println!("  [OK] All required fields are valid");
        }
        Err(e) => {
            println!("  [FAIL] Validation error: {}", e);
            anyhow::bail!("configuration validation failed");
        }
    }

    // Summary
    println!();
    println!("Configuration summary:");
    println!("  SVN URL       : {}", config.svn.url);
    println!("  SVN user      : {}", config.svn.username);
    println!(
        "  SVN password  : {}",
        if config.svn.password.is_some() {
            "set"
        } else {
            "NOT SET"
        }
    );
    println!("  GitHub repo   : {}", config.github.repo);
    println!(
        "  GitHub token  : {}",
        if config.github.token.is_some() {
            "set"
        } else {
            "NOT SET"
        }
    );
    println!(
        "  Webhook secret: {}",
        if config.github.webhook_secret.is_some() {
            "set"
        } else {
            "not set"
        }
    );
    println!("  Web listen    : {}", config.web.listen);
    println!("  Poll interval : {}s", config.daemon.poll_interval_secs);
    println!("  Data directory: {}", config.daemon.data_dir.display());
    println!();
    println!("Configuration is valid.");

    Ok(())
}

fn cmd_status(db: &Database) -> Result<()> {
    let state = reposync_core::sync_status::resolve_unscoped_sync_state(db)
        .context("failed to read sync state")?;

    let last_sync = db
        .get_state("last_sync_at")
        .context("failed to read last sync time")?;

    let last_svn_rev = db
        .get_last_svn_revision()
        .context("failed to read last SVN revision")?;

    let last_git_hash = status::last_git_hash_display(db)?;

    let total_syncs = db
        .count_sync_records()
        .context("failed to count sync records")?;

    let active_conflicts = db
        .count_active_conflicts()
        .context("failed to count active conflicts")?;

    let total_conflicts = db
        .count_all_conflicts()
        .context("failed to count total conflicts")?;

    let total_errors = db.count_errors().context("failed to count errors")?;

    println!("RepoSync Status");
    println!("=================");
    println!();
    println!("  Sync state       : {}", state);
    println!(
        "  Last sync at     : {}",
        last_sync.as_deref().unwrap_or("never")
    );
    println!(
        "  Last SVN revision: {}",
        last_svn_rev
            .map(|r: i64| r.to_string())
            .unwrap_or_else(|| "none".to_string())
    );
    println!("  Last Git hash    : {}", last_git_hash);
    println!("  Total sync ops   : {}", total_syncs);
    println!("  Active conflicts : {}", active_conflicts);
    println!("  Total conflicts  : {}", total_conflicts);
    println!("  Total errors     : {}", total_errors);

    Ok(())
}

fn cmd_conflicts(db: &Database, action: ConflictsAction) -> Result<()> {
    match action {
        ConflictsAction::List { status, limit } => {
            let conflicts = db
                .list_conflicts(status.as_deref(), limit)
                .context("failed to list conflicts")?;

            if conflicts.is_empty() {
                println!("No conflicts found.");
                return Ok(());
            }

            println!(
                "{:<38} {:<10} {:<40} {:<10}",
                "ID", "STATUS", "FILE", "SVN REV"
            );
            println!("{}", "-".repeat(100));

            for c in &conflicts {
                let rev = c
                    .svn_rev
                    .map(|r: i64| r.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{:<38} {:<10} {:<40} {:<10}",
                    c.id,
                    c.status,
                    truncate(&c.file_path, 38),
                    rev,
                );
            }

            println!();
            println!("{} conflict(s) shown", conflicts.len());

            Ok(())
        }

        ConflictsAction::Show { id } => {
            let conflict = db
                .get_conflict(&id)
                .context("database error")?
                .ok_or_else(|| anyhow::anyhow!("conflict '{}' not found", id))?;

            println!("Conflict: {}", conflict.id);
            println!("==========={}", "=".repeat(conflict.id.len()));
            println!();
            println!("  File path    : {}", conflict.file_path);
            println!("  Type         : {}", conflict.conflict_type);
            println!("  Status       : {}", conflict.status);
            println!(
                "  SVN revision : {}",
                conflict
                    .svn_rev
                    .map(|r: i64| r.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
            println!(
                "  Git hash     : {}",
                conflict.git_sha.as_deref().unwrap_or("-")
            );
            println!("  Created at   : {}", conflict.created_at);

            if let Some(ref resolution) = conflict.resolution {
                println!("  Resolution   : {}", resolution);
                println!(
                    "  Resolved at  : {}",
                    conflict.resolved_at.as_deref().unwrap_or("-")
                );
                println!(
                    "  Resolved by  : {}",
                    conflict.resolved_by.as_deref().unwrap_or("-")
                );
            }

            if conflict.svn_content.is_some() || conflict.git_content.is_some() {
                println!();
                if let Some(ref content) = conflict.svn_content {
                    println!("SVN Content ({} bytes):", content.len());
                    println!("{}", "-".repeat(40));
                    let preview = if content.len() > 1000 {
                        format!(
                            "{}...\n[truncated, {} bytes total]",
                            &content[..1000],
                            content.len()
                        )
                    } else {
                        content.clone()
                    };
                    println!("{}", preview);
                }
                println!();
                if let Some(ref content) = conflict.git_content {
                    println!("Git Content ({} bytes):", content.len());
                    println!("{}", "-".repeat(40));
                    let preview = if content.len() > 1000 {
                        format!(
                            "{}...\n[truncated, {} bytes total]",
                            &content[..1000],
                            content.len()
                        )
                    } else {
                        content.clone()
                    };
                    println!("{}", preview);
                }
            }

            Ok(())
        }

        ConflictsAction::Resolve { id, accept } => {
            let resolution = match accept.as_str() {
                "svn" => "accept_svn",
                "git" => "accept_git",
                other => {
                    anyhow::bail!("invalid resolution '{}': use 'svn' or 'git'", other);
                }
            };

            db.resolve_conflict(&id, "resolved", resolution, "cli")
                .context("failed to resolve conflict")?;

            println!("Conflict {} resolved (accepted {})", id, accept);
            Ok(())
        }
    }
}

async fn cmd_sync(_db: &Database, config: &AppConfig, action: SyncAction) -> Result<()> {
    match action {
        SyncAction::Now => {
            println!("Triggering immediate sync cycle...");
            println!();

            let svn_client = reposync_core::svn::SvnClient::new(
                &config.svn.url,
                &config.svn.username,
                config.svn.password.as_deref().unwrap_or(""),
            );

            let git_repo_path = config.daemon.data_dir.join("git-repo");
            let git_client = reposync_core::git::GitClient::new(&git_repo_path)
                .context("failed to open Git repository")?;

            let identity = IdentityMapper::new(&config.identity)
                .context("failed to initialize identity mapper")?;

            // Open a dedicated database connection for the sync engine
            let engine_db = {
                let db_path = config.daemon.data_dir.join("reposync.db");
                Database::new(&db_path).context("failed to open engine database")?
            };

            let engine = reposync_core::sync_engine::SyncEngine::new(
                config.clone(),
                engine_db,
                svn_client,
                git_client,
                Arc::new(identity),
            );

            let spinner = indicatif::ProgressBar::new_spinner();
            spinner.set_message("Running sync cycle...");
            spinner.enable_steady_tick(std::time::Duration::from_millis(100));

            let result = engine
                .run_sync_cycle()
                .await
                .map_err(|e| anyhow::anyhow!("sync cycle failed: {}", e))?;

            spinner.finish_and_clear();
            println!("Sync cycle completed:");
            println!("  SVN -> Git : {} operations", result.svn_to_git_count);
            println!("  Git -> SVN : {} operations", result.git_to_svn_count);
            println!("  Conflicts  : {}", result.conflicts_detected);
            println!("  Auto-resolved: {}", result.conflicts_auto_resolved);
            println!("  Started at : {}", result.started_at);
            if let Some(ref completed) = result.completed_at {
                println!("  Completed  : {}", completed);
            }

            Ok(())
        }
    }
}

fn cmd_identity(config: &AppConfig, action: IdentityAction) -> Result<()> {
    let mapper =
        IdentityMapper::new(&config.identity).context("failed to initialize identity mapper")?;

    match action {
        IdentityAction::Lookup { svn_user } => {
            match mapper.svn_to_git(&svn_user) {
                Ok(identity) => {
                    println!("SVN user: {}", svn_user);
                    println!("Git name: {}", identity.name);
                    println!("Git email: {}", identity.email);
                }
                Err(e) => {
                    println!("No mapping found for SVN user '{}': {}", svn_user, e);
                }
            }
            Ok(())
        }
    }
}

fn cmd_audit(db: &Database, limit: u32) -> Result<()> {
    let entries = db
        .list_audit_log(limit, 0)
        .context("failed to list audit entries")?;

    if entries.is_empty() {
        println!("No audit log entries found.");
        return Ok(());
    }

    println!("{:<22} {:<20} DETAILS", "TIMESTAMP", "ACTION");
    println!("{}", "-".repeat(80));

    for entry in &entries {
        println!(
            "{:<22} {:<20} {}",
            entry.created_at,
            entry.action,
            truncate(entry.details.as_deref().unwrap_or(""), 50),
        );
    }

    println!();
    println!("{} entries shown", entries.len());

    Ok(())
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}
