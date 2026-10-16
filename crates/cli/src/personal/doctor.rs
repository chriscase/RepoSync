//! Health check for personal branch mode.

use anyhow::Result;

use reposync_core::db::Database;
use reposync_core::personal_config::PersonalConfig;

use super::style;

/// Run a health check on the personal branch setup.
pub fn run_doctor(config: &PersonalConfig) -> Result<()> {
    println!();
    println!("{}", style::header("RepoSync Doctor"));
    println!("{}", "═".repeat(17));
    println!();

    let mut issues = Vec::new();

    // 1. Configuration
    match config.validate() {
        Ok(()) => println!("  {}", style::success("Configuration     Valid")),
        Err(e) => {
            println!("  {}", style::error(&format!("Configuration     {}", e)));
            issues.push("Fix configuration errors".to_string());
        }
    }

    // 2. Data directory
    let data_dir = &config.personal.data_dir;
    if data_dir.exists() {
        println!(
            "  {}",
            style::success(&format!("Data Directory    {}", data_dir.display()))
        );
    } else {
        println!(
            "  {}",
            style::error(&format!(
                "Data Directory    {} (missing)",
                data_dir.display()
            ))
        );
        issues.push(format!(
            "Create data directory: mkdir -p {}",
            data_dir.display()
        ));
    }

    // 3. Database
    let db_path = data_dir.join("personal.db");
    if db_path.exists() {
        match Database::open_existing_read_only(&db_path) {
            Ok(db) => {
                let schema_ok = db.get_watermark("svn_rev").is_ok();
                if schema_ok {
                    println!("  {}", style::success("Database          OK"));
                } else {
                    println!("  {}", style::error("Database          Schema outdated"));
                    issues.push("Re-run import to update database schema".to_string());
                }
            }
            Err(e) => {
                println!("  {}", style::error(&format!("Database          {}", e)));
                issues.push("Database is corrupted. Delete and re-import.".to_string());
            }
        }
    } else {
        println!("  {}", style::warn("Database          Not initialized"));
        issues.push("Run 'reposync personal import' to initialize".to_string());
    }

    // 4. Git repository
    let git_path = data_dir.join("git-repo");
    if git_path.exists() && git_path.join(".git").exists() {
        println!(
            "  {}",
            style::success(&format!("Git Repository    {}", git_path.display()))
        );
    } else if git_path.exists() {
        println!("  {}", style::error("Git Repository    Not a git repo"));
        issues.push("Git repo directory exists but is not initialized".to_string());
    } else {
        println!("  {}", style::warn("Git Repository    Not cloned"));
        issues.push("Run 'reposync personal import' to set up".to_string());
    }

    // 5. SVN working copy
    let svn_wc = data_dir.join("svn-wc");
    if svn_wc.exists() {
        println!(
            "  {}",
            style::success(&format!("SVN Working Copy  {}", svn_wc.display()))
        );
    } else {
        println!(
            "  {}",
            style::dim("  ○ SVN Working Copy  Not created (created on first Git→SVN sync)")
        );
    }

    // 6. Daemon status
    match reposync_personal::daemon::is_running(data_dir) {
        Ok(Some(pid)) => println!(
            "  {}",
            style::success(&format!("Daemon            Running (PID {})", pid))
        ),
        Ok(None) => {
            println!("  {}", style::warn("Daemon            Not running"));
            issues.push("Start daemon with: reposync personal start".to_string());
        }
        Err(e) => {
            println!(
                "  {}",
                style::error(&format!("Daemon            Error: {}", e))
            );
        }
    }

    // 7. Watermark consistency
    let db_path = data_dir.join("personal.db");
    if let Ok(db) = Database::open_existing_read_only(&db_path) {
        let svn_wm = db.get_watermark("svn_rev");
        let git_wm = db.get_watermark("git_sha");

        match (svn_wm, git_wm) {
            (Ok(Some(svn)), Ok(Some(git))) => {
                let git_short = if git.len() > 7 { &git[..7] } else { &git };
                println!(
                    "  {}",
                    style::success(&format!(
                        "Watermarks        SVN: r{}, Git: {}",
                        svn, git_short
                    ))
                );
            }
            (Ok(None), Ok(None)) => {
                println!(
                    "  {}",
                    style::dim("  ○ Watermarks        Not set (run import first)")
                );
            }
            (Err(error), _) | (_, Err(error)) => {
                println!(
                    "  {}",
                    style::error(&format!("Watermarks        Read failed: {}", error))
                );
                issues.push("Preserve storage and diagnose the failed watermark read.".to_string());
            }
            _ => {
                println!("  {}", style::warn("Watermarks        Inconsistent"));
                issues.push("Watermarks are out of sync. Run import to reset.".to_string());
            }
        }
    }

    // Summary
    println!();
    if issues.is_empty() {
        println!(
            "  {} All checks passed!",
            console::style("✓").green().bold()
        );
    } else {
        println!(
            "  {} {} issue(s) found:",
            issues.len(),
            console::style("!").yellow().bold()
        );
        for (i, issue) in issues.iter().enumerate() {
            println!("    {}. {}", i + 1, issue);
        }
    }
    println!();

    Ok(())
}
