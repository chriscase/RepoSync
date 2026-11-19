//! PR monitor for personal branch mode.
//!
//! Polls GitHub for recently merged pull requests, detects the merge strategy
//! used for each one, and returns unprocessed PRs for the sync engine to
//! replay into SVN.

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use reposync_core::db::Database;
use reposync_core::git::github::{GitHubClient, GitHubCommit};
use reposync_core::models::MergeStrategy;
use reposync_core::personal_config::PersonalConfig;

// ---------------------------------------------------------------------------
// MergedPr — output type
// ---------------------------------------------------------------------------

/// A merged pull request that has not yet been synced to SVN.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct MergedPr {
    /// GitHub PR number.
    pub pr_number: u64,
    /// PR title.
    pub title: String,
    /// Source branch name.
    pub branch: String,
    /// SHA of the merge commit on the target branch.
    pub merge_sha: String,
    /// Detected merge strategy (merge, squash, rebase).
    pub merge_strategy: MergeStrategy,
    /// Individual commits from the PR (before merging).
    pub commits: Vec<GitHubCommit>,
}

// ---------------------------------------------------------------------------
// PrMonitor
// ---------------------------------------------------------------------------

/// Monitors GitHub for merged pull requests that need to be synced to SVN.
pub struct PrMonitor<'a> {
    github_client: &'a GitHubClient,
    db: &'a Database,
    repo: String,
    base_branch: String,
}

impl<'a> PrMonitor<'a> {
    /// Create a new PR monitor from the personal config.
    pub fn new(github_client: &'a GitHubClient, db: &'a Database, config: &PersonalConfig) -> Self {
        Self {
            github_client,
            db,
            repo: config.github.repo.clone(),
            base_branch: config.github.default_branch.clone(),
        }
    }

    /// Check GitHub for recently merged PRs that have not yet been processed.
    ///
    /// Returns a list of [`MergedPr`] structs ready for the sync engine to
    /// replay into SVN. PRs whose `merge_commit_sha` has already been
    /// recorded in the database are silently skipped.
    pub async fn check_for_merged_prs(&self) -> Result<Vec<MergedPr>> {
        // Determine the "since" timestamp so we only look at recently merged PRs.
        let since = self
            .db
            .get_last_pr_sync_time()
            .context("failed to read last PR sync timestamp from database")?;

        info!(
            repo = %self.repo,
            base = %self.base_branch,
            since = ?since,
            "checking for merged pull requests"
        );

        // Fetch merged PRs from GitHub.
        let pull_requests = self
            .github_client
            .get_merged_pull_requests(&self.repo, &self.base_branch, since.as_deref())
            .await
            .context("failed to fetch merged pull requests from GitHub")?;

        debug!(count = pull_requests.len(), "fetched merged pull requests");

        let mut result = Vec::new();

        for pr in pull_requests {
            // Every merged PR must have a merge commit SHA.
            let merge_sha = match &pr.merge_commit_sha {
                Some(sha) => sha.clone(),
                None => {
                    warn!(
                        pr_number = pr.number,
                        title = %pr.title,
                        "merged PR has no merge_commit_sha, skipping"
                    );
                    continue;
                }
            };

            // Skip PRs that have already been processed.
            let already_synced = self
                .db
                .is_personal_pr_synced(&merge_sha)
                .context("failed to check PR sync status in database")?;

            if already_synced {
                debug!(
                    pr_number = pr.number,
                    merge_sha = %merge_sha,
                    "PR already synced, skipping"
                );
                continue;
            }

            // Detect the merge strategy by inspecting the merge commit's parents.
            let merge_strategy = self
                .detect_merge_strategy(&merge_sha, pr.number)
                .await
                .context("failed to detect merge strategy")?;

            // Fetch the individual commits from the PR.
            let commits = self
                .github_client
                .get_pr_commits(&self.repo, pr.number)
                .await
                .context("failed to fetch PR commits")?;

            info!(
                pr_number = pr.number,
                title = %pr.title,
                branch = %pr.head.ref_name,
                merge_sha = %merge_sha,
                strategy = %merge_strategy,
                commit_count = commits.len(),
                "detected unprocessed merged PR"
            );

            result.push(MergedPr {
                pr_number: pr.number,
                title: pr.title,
                branch: pr.head.ref_name,
                merge_sha,
                merge_strategy,
                commits,
            });
        }

        info!(count = result.len(), "found unprocessed merged PRs");
        Ok(result)
    }

    /// Detect the merge strategy used for a PR by inspecting the merge commit.
    ///
    /// - 2 parents  -> standard merge commit
    /// - 1 parent, 1 PR commit  -> squash merge
    /// - 1 parent, N PR commits -> rebase merge
    async fn detect_merge_strategy(
        &self,
        merge_sha: &str,
        pr_number: u64,
    ) -> Result<MergeStrategy> {
        let commit_detail = self
            .github_client
            .get_commit(&self.repo, merge_sha)
            .await
            .context("failed to fetch merge commit details")?;

        let parent_count = commit_detail.parents.len();

        let strategy = if parent_count == 2 {
            MergeStrategy::Merge
        } else if parent_count == 1 {
            // Squash and rebase both produce single-parent commits.
            // Distinguish by the number of commits in the PR.
            let pr_commits = self
                .github_client
                .get_pr_commits(&self.repo, pr_number)
                .await
                .context("failed to fetch PR commits for strategy detection")?;

            if pr_commits.len() <= 1 {
                MergeStrategy::Squash
            } else {
                MergeStrategy::Rebase
            }
        } else {
            warn!(
                merge_sha,
                parent_count, "unexpected parent count on merge commit"
            );
            MergeStrategy::Unknown
        };

        debug!(
            merge_sha,
            parent_count,
            strategy = %strategy,
            "detected merge strategy"
        );

        Ok(strategy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merged_pr_struct() {
        let pr = MergedPr {
            pr_number: 42,
            title: "Add search endpoint".to_string(),
            branch: "feature/search".to_string(),
            merge_sha: "abc123def456".to_string(),
            merge_strategy: MergeStrategy::Squash,
            commits: vec![],
        };
        assert_eq!(pr.pr_number, 42);
        assert_eq!(pr.merge_strategy, MergeStrategy::Squash);
        assert_eq!(pr.branch, "feature/search");
    }

    #[test]
    fn test_merge_strategy_display() {
        assert_eq!(MergeStrategy::Merge.to_string(), "merge");
        assert_eq!(MergeStrategy::Squash.to_string(), "squash");
        assert_eq!(MergeStrategy::Rebase.to_string(), "rebase");
        assert_eq!(MergeStrategy::Unknown.to_string(), "unknown");
    }
}
