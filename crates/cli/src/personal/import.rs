//! SVN import command with progress visualization.

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};

use reposync_core::db::Database;
use reposync_core::config::GitProvider;
use reposync_core::git::github::GitHubClient;
use reposync_core::git::GitClient;
use reposync_core::personal_config::PersonalConfig;
use reposync_core::svn::SvnClient;

use super::style;

/// Run the import command.
pub async fn run_import(config: &PersonalConfig, mode: &str) -> Result<()> {
    let data_dir = &config.personal.data_dir;

    println!();
    println!("Importing SVN history to GitHub...");
    println!();

    // Ensure data directory
    std::fs::create_dir_all(data_dir).context("failed to create data directory")?;

    // Initialize database
    let db_path = data_dir.join("personal.db");
    let db = Database::new(&db_path).context("failed to initialize database")?;

    // SVN client
    let svn_client = SvnClient::new(
        &config.svn.url,
        &config.svn.username,
        config.svn.password.as_deref().unwrap_or(""),
    );

    // GitHub client
    let github_token = config.github.token.as_deref().unwrap_or("");
    let github_client = GitHubClient::new(&config.github.api_url, github_token, GitProvider::default());

    // Git repository
    let git_repo_path = data_dir.join("git-repo");
    let git_client = if git_repo_path.exists() {
        GitClient::new(&git_repo_path).context("failed to open git repository")?
    } else {
        std::fs::create_dir_all(&git_repo_path).context("failed to create git repo directory")?;
        let remote_url = config.github.clone_url();
        match GitClient::clone_repo(&remote_url, &git_repo_path, config.github.token.as_deref()) {
            Ok(client) => client,
            Err(_) => GitClient::init(&git_repo_path).context("failed to init git repository")?,
        }
    };

    let git_client = std::sync::Arc::new(std::sync::Mutex::new(git_client));
    let formatter = reposync_personal::commit_format::CommitFormatter::new(&config.commit_format);

    let import_mode = match mode {
        "snapshot" => reposync_personal::initial_import::ImportMode::Snapshot,
        _ => reposync_personal::initial_import::ImportMode::Full,
    };

    let importer = reposync_personal::initial_import::InitialImport {
        svn_client: &svn_client,
        git_client: &git_client,
        github_client: &github_client,
        db: &db,
        config,
        formatter: &formatter,
    };

    let spinner = ProgressBar::new_spinner();
    // Template is a compile-time constant so with_template cannot fail here,
    // but we use expect() to be explicit about the invariant.
    spinner.set_style(
        ProgressStyle::with_template("{spinner:.blue} {msg}")
            .expect("static progress template is always valid")
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    spinner.set_message(format!("Importing ({} mode)...", mode));
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));

    let count = importer.import(import_mode).await?;

    spinner.finish_and_clear();

    println!("{}", style::success("Import complete!"));
    println!("  Commits: {}", count);
    println!("  Repository: https://github.com/{}", config.github.repo);
    println!();
    println!("Next: Run 'reposync personal start' to begin syncing");
    println!();

    Ok(())
}
