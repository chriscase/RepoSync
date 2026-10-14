//! Rich status dashboard for personal branch mode.

use anyhow::{Context, Result};

use reposync_core::db::Database;
use reposync_core::personal_config::PersonalConfig;

use super::style;

/// Display the personal branch status dashboard.
pub fn run_status(config: &PersonalConfig) -> Result<()> {
    let data_dir = &config.personal.data_dir;

    // Header
    println!();
    println!("{}", style::header("RepoSync Personal Branch"));
    println!("{}", "═".repeat(26));
    println!();

    // Daemon status
    let daemon_status = match reposync_personal::daemon::is_running(data_dir) {
        Ok(Some(pid)) => format!("{} (PID {})", style::status_running(), pid),
        _ => style::status_stopped(),
    };
    println!("  Status     {}", daemon_status);

    // Try to open DB for watermarks
    let db_path = data_dir.join("personal.db");
    if db_path.try_exists()? {
        let db = Database::open_existing_read_only(&db_path)
            .context("failed to inspect existing Personal database; preserve storage")?;
        let svn_rev = db
            .get_watermark("svn_rev")
            .context("failed to read Personal SVN checkpoint")?
            .unwrap_or_else(|| "—".to_string());
        let git_sha = db
            .get_watermark("git_sha")
            .context("failed to read Personal Git checkpoint")?
            .unwrap_or_else(|| "—".to_string());

        let git_display = if git_sha.len() > 7 {
            &git_sha[..7]
        } else {
            &git_sha
        };

        println!("  SVN        r{}", svn_rev);
        println!("  Git        {}", git_display);

        // Recent activity from audit log
        println!();
        let recent = db
            .list_audit_log(10, 0)
            .context("failed to read Personal audit history")?;
        if !recent.is_empty() {
            println!("  {}", style::header("Recent Activity"));
            println!("  {}", "─".repeat(40));
            for entry in recent.iter().take(5) {
                let action_styled = match entry.action.as_str() {
                    a if a.contains("svn_to_git") => style::svn_to_git(),
                    a if a.contains("git_to_svn") => style::git_to_svn(),
                    _ => entry.action.clone(),
                };
                println!(
                    "  {} {}",
                    style::dim(&entry.created_at[..19.min(entry.created_at.len())]),
                    action_styled
                );
            }
        }
    } else {
        println!(
            "  {}",
            style::dim("Database not initialized. Run 'reposync personal import' first.")
        );
    }

    println!();
    Ok(())
}
