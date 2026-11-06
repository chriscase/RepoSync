//! Personal branch mode CLI commands.
//!
//! Provides subcommands for managing a personal SVN↔Git sync bridge:
//! init, import, start, stop, status, log, pr-log, doctor, conflicts.

pub mod conflicts;
pub mod daemon_ctl;
pub mod doctor;
pub mod import;
pub mod init;
pub mod log;
pub mod pr_log;
pub mod status;
pub mod style;

use anyhow::{Context, Result};
use clap::Subcommand;

use reposync_core::personal_config::PersonalConfig;

/// Personal branch mode subcommands.
#[derive(Subcommand, Debug)]
pub enum PersonalCommands {
    /// Interactive setup wizard — creates a personal config file.
    Init {
        /// Output path for the config file.
        #[arg(short, long, default_value = "~/.config/reposync/personal.toml")]
        output: String,
    },

    /// Import SVN history into Git.
    Import {
        /// Import only a snapshot of HEAD (one commit).
        #[arg(long, conflicts_with = "full")]
        snapshot: bool,

        /// Import full SVN history (one commit per revision).
        #[arg(long, conflicts_with = "snapshot")]
        full: bool,
    },

    /// Start the sync daemon.
    Start {
        /// Run in the foreground with live output.
        #[arg(long)]
        foreground: bool,
    },

    /// Stop the running daemon.
    Stop,

    /// Show sync status dashboard.
    Status,

    /// Show sync history log.
    Log {
        /// Maximum number of entries.
        #[arg(short, long, default_value = "20")]
        limit: u32,
    },

    /// Show PR sync history.
    PrLog {
        /// Maximum number of entries.
        #[arg(short, long, default_value = "20")]
        limit: u32,
    },

    /// Run health checks on the setup.
    Doctor,

    /// Manage sync conflicts.
    Conflicts {
        #[command(subcommand)]
        action: ConflictAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConflictAction {
    /// List active conflicts.
    List,
    /// Resolve a conflict.
    Resolve {
        /// Conflict ID.
        id: String,
        /// Accept 'svn' or 'git' version.
        #[arg(long)]
        accept: String,
    },
}

/// Run a personal subcommand.
pub async fn run_personal(cmd: PersonalCommands, config_path: &str) -> Result<()> {
    match cmd {
        PersonalCommands::Init { output } => init::run_init(&output).await,

        PersonalCommands::Import { snapshot, full: _ } => {
            let config = load_config(config_path)?;
            let mode = if snapshot { "snapshot" } else { "full" };
            import::run_import(&config, mode).await
        }

        PersonalCommands::Start { foreground } => {
            let config = load_config(config_path)?;
            daemon_ctl::run_start(&config, foreground).await
        }

        PersonalCommands::Stop => {
            let config = load_config(config_path)?;
            daemon_ctl::run_stop(&config)
        }

        PersonalCommands::Status => {
            let config = load_config(config_path)?;
            status::run_status(&config)
        }

        PersonalCommands::Log { limit } => {
            let config = load_config(config_path)?;
            log::run_log(&config, limit)
        }

        PersonalCommands::PrLog { limit } => {
            let config = load_config(config_path)?;
            pr_log::run_pr_log(&config, limit)
        }

        PersonalCommands::Doctor => {
            let config = load_config(config_path)?;
            doctor::run_doctor(&config)
        }

        PersonalCommands::Conflicts { action } => {
            let config = load_config(config_path)?;
            match action {
                ConflictAction::List => conflicts::run_list(&config),
                ConflictAction::Resolve { id, accept } => {
                    conflicts::run_resolve(&config, &id, &accept)
                }
            }
        }
    }
}

/// Load and validate the personal config.
fn load_config(config_path: &str) -> Result<PersonalConfig> {
    let resolved = expand_tilde(config_path);
    let config =
        PersonalConfig::load_and_resolve(&resolved).context("failed to load personal config")?;
    config.validate().context("invalid personal config")?;
    Ok(config)
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
