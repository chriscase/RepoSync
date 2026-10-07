//! Commit message formatting and echo suppression for personal branch mode.

use reposync_core::echo_suppression::personal_mode_marker_echo;
use reposync_core::personal_config::CommitFormatConfig;

/// Formats commit messages for both sync directions using configurable templates.
pub struct CommitFormatter {
    svn_to_git_template: String,
    git_to_svn_template: String,
}

impl CommitFormatter {
    /// Create a new formatter from config templates.
    pub fn new(config: &CommitFormatConfig) -> Self {
        Self {
            svn_to_git_template: config.svn_to_git.clone(),
            git_to_svn_template: config.git_to_svn.clone(),
        }
    }

    /// Format a commit message for SVN→Git direction.
    pub fn format_svn_to_git(
        &self,
        original_message: &str,
        svn_rev: i64,
        svn_author: &str,
        svn_date: &str,
    ) -> String {
        self.svn_to_git_template
            .replace("{original_message}", original_message.trim())
            .replace("{svn_rev}", &svn_rev.to_string())
            .replace("{svn_author}", svn_author)
            .replace("{svn_date}", svn_date)
    }

    /// Format a commit message for Git→SVN direction.
    pub fn format_git_to_svn(
        &self,
        original_message: &str,
        git_sha: &str,
        pr_number: u64,
        pr_branch: &str,
    ) -> String {
        self.git_to_svn_template
            .replace("{original_message}", original_message.trim())
            .replace("{git_sha}", git_sha)
            .replace("{pr_number}", &pr_number.to_string())
            .replace("{pr_branch}", pr_branch)
    }

    /// Check whether a commit message contains the sync marker (diagnostic only).
    #[allow(dead_code)]
    pub fn is_sync_marker(message: &str) -> bool {
        personal_mode_marker_echo(message)
    }

    /// Extract the SVN revision from a commit message trailer.
    /// Looks for `SVN-Revision: r<number>`.
    #[allow(dead_code)]
    pub fn extract_svn_rev(message: &str) -> Option<i64> {
        for line in message.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("SVN-Revision:") {
                let rest = rest.trim().trim_start_matches('r');
                if let Ok(rev) = rest.parse::<i64>() {
                    return Some(rev);
                }
            }
        }
        None
    }

    /// Extract the Git SHA from a commit message trailer.
    /// Looks for `Git-SHA: <hex>`.
    #[allow(dead_code)]
    pub fn extract_git_sha(message: &str) -> Option<String> {
        for line in message.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("Git-SHA:") {
                let sha = rest.trim();
                if !sha.is_empty() {
                    return Some(sha.to_string());
                }
            }
        }
        None
    }

    /// Extract the PR number from a commit message trailer.
    /// Looks for `PR-Number: #<number>`.
    #[allow(dead_code)]
    pub fn extract_pr_number(message: &str) -> Option<u64> {
        for line in message.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("PR-Number:") {
                let rest = rest.trim().trim_start_matches('#');
                if let Ok(num) = rest.parse::<u64>() {
                    return Some(num);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> CommitFormatConfig {
        CommitFormatConfig::default()
    }

    #[test]
    fn test_svn_to_git_format() {
        let fmt = CommitFormatter::new(&default_config());
        let result =
            fmt.format_svn_to_git("Fix bug in parser", 42, "alice", "2025-01-15T10:30:00Z");
        assert!(result.contains("Fix bug in parser"));
        assert!(result.contains("SVN-Revision: r42"));
        assert!(result.contains("SVN-Author: alice"));
        assert!(result.contains("[reposync]"));
    }

    #[test]
    fn test_git_to_svn_format() {
        let fmt = CommitFormatter::new(&default_config());
        let result =
            fmt.format_git_to_svn("Add search endpoint", "abc123def", 42, "feature/search");
        assert!(result.contains("Add search endpoint"));
        assert!(result.contains("Git-SHA: abc123def"));
        assert!(result.contains("PR-Number: #42"));
        assert!(result.contains("PR-Branch: feature/search"));
        assert!(result.contains("[reposync]"));
    }

    #[test]
    fn test_is_sync_marker() {
        assert!(CommitFormatter::is_sync_marker("Some commit [reposync]"));
        assert!(CommitFormatter::is_sync_marker(
            "Fix bug\n\nSync-Marker: [reposync]"
        ));
        assert!(!CommitFormatter::is_sync_marker("Normal commit message"));
    }

    #[test]
    fn test_extract_svn_rev() {
        let msg = "Fix bug\n\nSVN-Revision: r42\nSVN-Author: alice";
        assert_eq!(CommitFormatter::extract_svn_rev(msg), Some(42));
        assert_eq!(CommitFormatter::extract_svn_rev("no trailer"), None);
    }

    #[test]
    fn test_extract_git_sha() {
        let msg = "Fix bug\n\nGit-SHA: abc123def456\nPR-Number: #10";
        assert_eq!(
            CommitFormatter::extract_git_sha(msg),
            Some("abc123def456".to_string())
        );
        assert_eq!(CommitFormatter::extract_git_sha("no trailer"), None);
    }

    #[test]
    fn test_extract_pr_number() {
        let msg = "Fix bug\n\nPR-Number: #42";
        assert_eq!(CommitFormatter::extract_pr_number(msg), Some(42));
        assert_eq!(CommitFormatter::extract_pr_number("no trailer"), None);
    }

    #[test]
    fn test_custom_template() {
        let config = CommitFormatConfig {
            svn_to_git: "{original_message} (from SVN r{svn_rev})".into(),
            git_to_svn: "{original_message} [reposync] from {git_sha}".into(),
        };
        let fmt = CommitFormatter::new(&config);
        let result = fmt.format_svn_to_git("Hello", 10, "bob", "2025-01-01");
        assert_eq!(result, "Hello (from SVN r10)");
    }
}
