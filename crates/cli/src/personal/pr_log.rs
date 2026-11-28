//! PR sync history for personal branch mode.

use anyhow::{Context, Result};
use comfy_table::{presets::UTF8_FULL, Cell, ContentArrangement, Table};

use reposync_core::db::Database;
use reposync_core::personal_config::PersonalConfig;

use super::style;

/// Display PR sync history.
pub fn run_pr_log(config: &PersonalConfig, limit: u32) -> Result<()> {
    let data_dir = &config.personal.data_dir;
    let db_path = data_dir.join("personal.db");
    let db = Database::open_existing_read_only(&db_path).context("failed to open database")?;

    let entries = db
        .list_pr_syncs(limit)
        .context("failed to list PR sync entries")?;

    if entries.is_empty() {
        println!("No PR sync history found.");
        return Ok(());
    }

    println!();
    println!("{}", style::header("PR Sync History"));
    println!();

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec![
        "PR #", "Branch", "Strategy", "Commits", "SVN Revs", "Status",
    ]);

    for entry in &entries {
        let svn_range = match (entry.svn_rev_start, entry.svn_rev_end) {
            (Some(start), Some(end)) if start == end => format!("r{}", start),
            (Some(start), Some(end)) => format!("r{}-r{}", start, end),
            _ => "—".to_string(),
        };

        let status_cell = match entry.status.as_str() {
            "completed" => Cell::new("✓ synced").fg(comfy_table::Color::Green),
            "failed" => Cell::new("✗ failed").fg(comfy_table::Color::Red),
            "pending" => Cell::new("⧗ pending").fg(comfy_table::Color::Yellow),
            _ => Cell::new(&entry.status),
        };

        table.add_row(vec![
            Cell::new(format!("#{}", entry.pr_number)),
            Cell::new(&entry.pr_branch),
            Cell::new(&entry.merge_strategy),
            Cell::new(entry.commit_count),
            Cell::new(&svn_range),
            status_cell,
        ]);
    }

    println!("{}", table);
    println!();

    Ok(())
}
