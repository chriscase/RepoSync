//! Interactive init wizard for Personal Branch Mode.
//!
//! Walks the user through configuring a personal sync setup and writes
//! the resulting TOML configuration file.

use std::path::Path;

use anyhow::{Context, Result};
use console::Style;
use dialoguer::{Confirm, Input, Select};

use super::style;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the interactive init wizard and write the config to `output_path`.
pub async fn run_init(output_path: &str) -> Result<()> {
    let path = Path::new(output_path);

    // Guard against overwriting an existing file.
    if path.exists() {
        let overwrite = Confirm::new()
            .with_prompt(format!("{} already exists. Overwrite?", path.display()))
            .default(false)
            .interact()
            .context("failed to read confirmation")?;

        if !overwrite {
            println!(
                "{}",
                style::warn("Init cancelled. Existing file was not modified.")
            );
            return Ok(());
        }
    }

    // Print a welcome banner.
    let accent = Style::new().cyan().bold();
    println!();
    println!(
        "{}",
        accent.apply_to("=== RepoSync Personal Branch Mode — Setup Wizard ===")
    );
    println!();
    println!("This wizard will guide you through creating a personal sync configuration.");
    println!("The resulting TOML file can be used with `reposync personal start`.");
    println!();

    // -----------------------------------------------------------------
    // 1. SVN settings
    // -----------------------------------------------------------------
    println!("{}", style::header("1/5  SVN Repository"));
    println!();

    let svn_url: String = Input::new()
        .with_prompt("SVN repository URL (e.g. https://svn.example.com/repos/project/trunk)")
        .interact_text()
        .context("failed to read SVN URL")?;

    let svn_username: String = Input::new()
        .with_prompt("SVN username")
        .interact_text()
        .context("failed to read SVN username")?;

    let svn_password_env: String = Input::new()
        .with_prompt("Environment variable that holds the SVN password")
        .default("REPOSYNC_SVN_PASSWORD".into())
        .interact_text()
        .context("failed to read SVN password env var name")?;

    println!();

    // -----------------------------------------------------------------
    // 2. GitHub settings
    // -----------------------------------------------------------------
    println!("{}", style::header("2/5  GitHub Repository"));
    println!();

    let github_api_url: String = Input::new()
        .with_prompt("GitHub API URL")
        .default("https://api.github.com".into())
        .interact_text()
        .context("failed to read GitHub API URL")?;

    let github_repo: String = Input::new()
        .with_prompt("GitHub repository (owner/repo format)")
        .validate_with(|input: &String| -> Result<(), String> {
            if input.contains('/') && input.split('/').count() == 2 {
                Ok(())
            } else {
                Err("Must be in owner/repo format (e.g. jdoe/my-project)".into())
            }
        })
        .interact_text()
        .context("failed to read GitHub repo")?;

    let github_token_env: String = Input::new()
        .with_prompt("Environment variable that holds the GitHub token")
        .default("REPOSYNC_GITHUB_TOKEN".into())
        .interact_text()
        .context("failed to read GitHub token env var name")?;

    let github_private = Confirm::new()
        .with_prompt("Should the GitHub repo be private (if auto-created)?")
        .default(true)
        .interact()
        .context("failed to read private preference")?;

    println!();

    // -----------------------------------------------------------------
    // 3. Developer identity
    // -----------------------------------------------------------------
    println!("{}", style::header("3/5  Developer Identity"));
    println!();

    let dev_name: String = Input::new()
        .with_prompt("Git author name")
        .interact_text()
        .context("failed to read developer name")?;

    let dev_email: String = Input::new()
        .with_prompt("Git author email")
        .interact_text()
        .context("failed to read developer email")?;

    let dev_svn_username: String = Input::new()
        .with_prompt("SVN username (for echo suppression / author attribution)")
        .default(svn_username.clone())
        .interact_text()
        .context("failed to read developer SVN username")?;

    println!();

    // -----------------------------------------------------------------
    // 4. Sync settings
    // -----------------------------------------------------------------
    println!("{}", style::header("4/5  Sync Settings"));
    println!();

    let poll_interval_options = &["30 seconds", "60 seconds", "120 seconds", "Custom"];

    let poll_choice = Select::new()
        .with_prompt("Poll interval (how often to check for changes)")
        .items(poll_interval_options)
        .default(0)
        .interact()
        .context("failed to read poll interval selection")?;

    let poll_interval_secs: u64 = match poll_choice {
        0 => 30,
        1 => 60,
        2 => 120,
        3 => {
            let custom: u64 = Input::new()
                .with_prompt("Custom poll interval in seconds (minimum 10)")
                .validate_with(|input: &u64| -> Result<(), String> {
                    if *input >= 10 {
                        Ok(())
                    } else {
                        Err("Poll interval must be at least 10 seconds".into())
                    }
                })
                .interact_text()
                .context("failed to read custom poll interval")?;
            custom
        }
        _ => 30,
    };

    let import_mode_options = &[
        "snapshot  — import only the latest SVN revision (fast, no history)",
        "full      — import all SVN history as individual commits (slow, complete history)",
    ];

    let import_choice = Select::new()
        .with_prompt("Initial import mode")
        .items(import_mode_options)
        .default(0)
        .interact()
        .context("failed to read import mode selection")?;

    let import_mode = match import_choice {
        0 => "snapshot",
        1 => "full",
        _ => "snapshot",
    };

    println!();

    // -----------------------------------------------------------------
    // 5. Summary and confirmation
    // -----------------------------------------------------------------
    println!("{}", style::header("5/5  Summary"));
    println!();

    let label = Style::new().bold();
    let value_style = Style::new().cyan();

    println!("  {}:", label.apply_to("SVN"));
    println!("    URL            : {}", value_style.apply_to(&svn_url));
    println!(
        "    Username       : {}",
        value_style.apply_to(&svn_username)
    );
    println!(
        "    Password env   : {}",
        value_style.apply_to(&svn_password_env)
    );
    println!();
    println!("  {}:", label.apply_to("GitHub"));
    println!(
        "    API URL        : {}",
        value_style.apply_to(&github_api_url)
    );
    println!(
        "    Repository     : {}",
        value_style.apply_to(&github_repo)
    );
    println!(
        "    Token env      : {}",
        value_style.apply_to(&github_token_env)
    );
    println!(
        "    Private        : {}",
        value_style.apply_to(if github_private { "yes" } else { "no" })
    );
    println!();
    println!("  {}:", label.apply_to("Developer"));
    println!("    Name           : {}", value_style.apply_to(&dev_name));
    println!("    Email          : {}", value_style.apply_to(&dev_email));
    println!(
        "    SVN username   : {}",
        value_style.apply_to(&dev_svn_username)
    );
    println!();
    println!("  {}:", label.apply_to("Sync"));
    println!(
        "    Poll interval  : {}",
        value_style.apply_to(format!("{}s", poll_interval_secs))
    );
    println!("    Import mode    : {}", value_style.apply_to(import_mode));
    println!();
    println!(
        "  Config will be written to: {}",
        Style::new().yellow().apply_to(output_path)
    );
    println!();

    let confirmed = Confirm::new()
        .with_prompt("Write this configuration?")
        .default(true)
        .interact()
        .context("failed to read confirmation")?;

    if !confirmed {
        println!("{}", style::warn("Init cancelled. No file was written."));
        return Ok(());
    }

    // -----------------------------------------------------------------
    // Generate TOML with comments
    // -----------------------------------------------------------------
    let toml_content = format!(
        r##"# RepoSync Personal Branch Mode Configuration
# Generated by `reposync personal init`
# Documentation: https://github.com/chriscase/RepoSync/blob/main/docs/personal-branch/configuration.md

[personal]
# How often (in seconds) to poll SVN and GitHub for changes.
poll_interval_secs = {poll_interval_secs}

# Minimum log level: trace, debug, info, warn, error.
log_level = "info"

# Directory for persistent data (database, working copies).
# Defaults to a platform-appropriate location if omitted.
# data_dir = "~/.local/share/reposync"

[svn]
# SVN repository URL — typically the trunk URL.
url = "{svn_url}"

# SVN credentials.
username = "{svn_username}"
password_env = "{svn_password_env}"

[github]
# GitHub API base URL (change for GitHub Enterprise).
api_url = "{github_api_url}"

# Target repository in owner/repo format.
repo = "{github_repo}"

# Environment variable holding a GitHub personal access token.
token_env = "{github_token_env}"

# Default branch name for the mirror repository.
default_branch = "main"

# Automatically create the GitHub repo if it does not exist.
auto_create = true

# Whether an auto-created repository should be private.
private = {github_private}

[developer]
# Your Git identity — used as commit author/committer.
name = "{dev_name}"
email = "{dev_email}"

# Your SVN username — used for echo suppression so your own SVN
# commits are not mirrored back as duplicate Git commits.
svn_username = "{dev_svn_username}"

[commit_format]
# Templates for rewriting commit messages during sync.
# Available placeholders for svn_to_git:
#   {{original_message}}, {{svn_rev}}, {{svn_author}}, {{svn_date}}
# Available placeholders for git_to_svn:
#   {{original_message}}, {{git_sha}}, {{pr_number}}, {{pr_branch}}
#
# Uncomment to customise (sensible defaults are built in):
# svn_to_git = "{{original_message}}\n\nSynced-From: svn\nSVN-Revision: r{{svn_rev}}"
# git_to_svn = "{{original_message}}\n\n[reposync] Git-SHA: {{git_sha}}"

[options]
# Normalize CRLF to LF during sync.
normalize_line_endings = true

# Preserve the executable bit from SVN svn:executable.
sync_executable_bit = true

# Skip files larger than this size in bytes. 0 = no limit.
max_file_size = 0

# Glob patterns for files/directories to ignore during sync.
ignore_patterns = []

# Whether to sync SVN externals (metadata only).
sync_externals = false

# Whether to sync direct pushes to main (not just merged PRs).
sync_direct_pushes = false

# Automatically merge conflicts when a clean 3-way merge is possible.
auto_merge = true

# --- Initial Import ---
# Import mode: "snapshot" (latest revision only) or "full" (entire history).
# import_mode = "{import_mode}"
"##,
        poll_interval_secs = poll_interval_secs,
        svn_url = escape_toml_string(&svn_url),
        svn_username = escape_toml_string(&svn_username),
        svn_password_env = escape_toml_string(&svn_password_env),
        github_api_url = escape_toml_string(&github_api_url),
        github_repo = escape_toml_string(&github_repo),
        github_token_env = escape_toml_string(&github_token_env),
        github_private = github_private,
        dev_name = escape_toml_string(&dev_name),
        dev_email = escape_toml_string(&dev_email),
        dev_svn_username = escape_toml_string(&dev_svn_username),
        import_mode = import_mode,
    );

    // Ensure parent directories exist.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
    }

    std::fs::write(path, &toml_content)
        .with_context(|| format!("failed to write configuration to {}", path.display()))?;

    println!();
    println!(
        "{}",
        style::success(&format!("Configuration written to {}", output_path))
    );
    println!();
    println!("{}", style::header("Next steps:"));
    println!();
    println!("  1. Set the environment variables referenced above:");
    println!("       export {}=\"your-svn-password\"", svn_password_env);
    println!(
        "       export {}=\"ghp_your-github-token\"",
        github_token_env
    );
    println!();
    println!("  2. Validate the config:");
    println!(
        "       reposync personal validate --config {}",
        output_path
    );
    println!();
    println!("  3. Start syncing:");
    println!("       reposync personal start --config {}", output_path);
    println!();

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Escape a string for safe inclusion inside a TOML double-quoted value.
///
/// Handles backslashes, double quotes, and common control characters.
fn escape_toml_string(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_toml_string_plain() {
        assert_eq!(escape_toml_string("hello"), "hello");
    }

    #[test]
    fn test_escape_toml_string_with_quotes() {
        assert_eq!(escape_toml_string(r#"say "hi""#), r#"say \"hi\""#);
    }

    #[test]
    fn test_escape_toml_string_with_backslash() {
        assert_eq!(escape_toml_string(r"C:\Users"), r"C:\\Users");
    }

    #[test]
    fn test_escape_toml_string_with_control_chars() {
        assert_eq!(escape_toml_string("a\nb\tc"), r"a\nb\tc");
    }
}
