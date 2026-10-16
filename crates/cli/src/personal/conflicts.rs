//! Conflict management for personal branch mode.

use anyhow::{Context, Result};
use comfy_table::{presets::UTF8_FULL, Cell, ContentArrangement, Table};

use reposync_core::db::Database;
use reposync_core::personal_config::PersonalConfig;

use super::style;

/// List active conflicts.
pub fn run_list(config: &PersonalConfig) -> Result<()> {
    let data_dir = &config.personal.data_dir;
    let db_path = data_dir.join("personal.db");
    let db = Database::open_existing_read_only(&db_path).context("failed to open database")?;

    let conflicts = db
        .list_conflicts(Some("detected"), 50)
        .context("failed to list conflicts")?;

    if conflicts.is_empty() {
        println!();
        println!("{}", style::success("No active conflicts"));
        println!();
        return Ok(());
    }

    println!();
    println!(
        "{}",
        style::header(&format!("Active Conflicts ({})", conflicts.len()))
    );
    println!();

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec!["ID", "File", "Type", "SVN Rev", "Created"]);

    for c in &conflicts {
        let rev = c
            .svn_rev
            .map(|r| format!("r{}", r))
            .unwrap_or_else(|| "—".to_string());

        let id_short = if c.id.len() > 8 { &c.id[..8] } else { &c.id };

        table.add_row(vec![
            Cell::new(id_short),
            Cell::new(&c.file_path),
            Cell::new(&c.conflict_type),
            Cell::new(&rev),
            Cell::new(&c.created_at[..10.min(c.created_at.len())]),
        ]);
    }

    println!("{}", table);
    println!();

    Ok(())
}

/// Resolve a conflict.
pub fn run_resolve(config: &PersonalConfig, id: &str, accept: &str) -> Result<()> {
    let data_dir = &config.personal.data_dir;
    let db = Database::open_personal_writer(data_dir)
        .context("Personal conflict startup writer admission refused")?;

    let resolution = match accept {
        "svn" => "accept_svn",
        "git" => "accept_git",
        other => anyhow::bail!("invalid resolution '{}': use 'svn' or 'git'", other),
    };

    db.resolve_conflict(id, "resolved", resolution, "cli")
        .context("failed to resolve conflict")?;

    println!(
        "{}",
        style::success(&format!("Conflict {} resolved (accepted {})", id, accept))
    );
    Ok(())
}
