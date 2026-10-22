//! Formatted sync history log for personal branch mode.

use anyhow::{Context, Result};

use reposync_core::db::Database;
use reposync_core::personal_config::PersonalConfig;

use super::style;

/// Display formatted sync history.
pub fn run_log(config: &PersonalConfig, limit: u32) -> Result<()> {
    let data_dir = &config.personal.data_dir;
    let db_path = data_dir.join("personal.db");
    let db = Database::open_existing_read_only(&db_path).context("failed to open database")?;

    let entries = db
        .list_audit_log(limit, 0)
        .context("failed to list audit entries")?;

    if entries.is_empty() {
        println!("No sync history found.");
        return Ok(());
    }

    println!();
    println!(
        "{}",
        style::header(&format!("Sync History (last {})", limit))
    );
    println!();

    for entry in &entries {
        let direction = if entry.action.contains("svn_to_git") {
            style::svn_to_git()
        } else if entry.action.contains("git_to_svn") {
            style::git_to_svn()
        } else {
            entry.action.clone()
        };

        let timestamp = &entry.created_at[..19.min(entry.created_at.len())];
        let details = entry.details.as_deref().unwrap_or("");

        println!("  {}  {}  {}", style::dim(timestamp), direction, details);
    }

    println!();
    Ok(())
}
