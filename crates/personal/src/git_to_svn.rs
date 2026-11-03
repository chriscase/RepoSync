//! Git-to-SVN sync engine for Personal Branch Mode.
//!
//! Replays merged pull request commits from a GitHub repository back into an
//! SVN working copy. Each PR's commits are applied in order and committed to
//! SVN with metadata trailers (Git SHA, PR number, branch) for traceability
//! and echo suppression.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, error, info, instrument, warn};

use reposync_core::db::Database;
use reposync_core::file_policy::{FilePolicy, FilePolicyDecision};
use reposync_core::git::github::{GitHubClient, GitHubCommit, PullRequest};
use reposync_core::git::GitClient;
use reposync_core::personal_config::PersonalConfig;
use reposync_core::svn::SvnClient;

use crate::commit_format::CommitFormatter;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of a single Git-to-SVN sync cycle.
#[derive(Debug, Clone, Default)]
pub struct GitToSvnResult {
    /// Total number of commits replayed to SVN.
    pub commits_synced: u64,
    /// Number of PRs fully processed.
    pub prs_synced: u64,
    /// Number of PRs skipped (already synced).
    pub prs_skipped: u64,
    /// Number of PRs that failed to sync.
    pub prs_failed: u64,
}

/// Syncs merged PR commits from Git back to SVN.
pub struct GitToSvnSync {
    svn: SvnClient,
    github: GitHubClient,
    db: Arc<Database>,
    formatter: CommitFormatter,
    policy: FilePolicy,
    svn_wc_path: PathBuf,
    git_repo_path: PathBuf,
    github_repo: String,
    default_branch: String,
    svn_author: String,
    svn_url: String,
}

impl GitToSvnSync {
    /// Create a new `GitToSvnSync` from resolved configuration.
    ///
    /// `svn_wc_path` is the path to the local SVN working copy.
    /// `git_repo_path` is the path to the local Git repository clone.
    pub fn new(
        svn: SvnClient,
        github: GitHubClient,
        db: Arc<Database>,
        config: &PersonalConfig,
        svn_wc_path: PathBuf,
        git_repo_path: PathBuf,
    ) -> Self {
        let formatter = CommitFormatter::new(&config.commit_format);
        let policy = FilePolicy::from(&config.options);
        if policy.has_constraints() {
            info!(
                max_file_size = policy.max_file_size(),
                ignore_patterns = config.options.ignore_patterns.len(),
                "file policy active for Git→SVN sync"
            );
        }
        Self {
            svn,
            github,
            db,
            formatter,
            policy,
            svn_wc_path,
            git_repo_path,
            github_repo: config.github.repo.clone(),
            default_branch: config.github.default_branch.clone(),
            svn_author: config.developer.svn_username.clone(),
            svn_url: config.svn.url.clone(),
        }
    }

    /// Ensure the SVN working copy directory exists and is properly checked out.
    ///
    /// If `svn_wc_path` does not exist or does not contain a `.svn` directory,
    /// performs an `svn checkout` of the configured SVN URL at HEAD.
    #[instrument(skip(self), fields(svn_wc = %self.svn_wc_path.display(), svn_url = %self.svn_url))]
    async fn ensure_svn_working_copy(&self) -> Result<()> {
        let svn_dir = self.svn_wc_path.join(".svn");
        if svn_dir.is_dir() {
            debug!(
                "SVN working copy already exists at {}",
                self.svn_wc_path.display()
            );
            return Ok(());
        }

        info!(
            "SVN working copy not found at {}, running svn checkout",
            self.svn_wc_path.display()
        );

        // Ensure the parent directory exists so `svn checkout` can create the WC dir.
        if let Some(parent) = self.svn_wc_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create parent directory for SVN working copy: {}",
                    parent.display()
                )
            })?;
        }

        self.svn
            .checkout_head(&self.svn_wc_path)
            .await
            .with_context(|| {
                format!(
                    "failed to checkout SVN repository {} into {}",
                    self.svn_url,
                    self.svn_wc_path.display()
                )
            })?;

        info!(
            "SVN working copy initialized at {}",
            self.svn_wc_path.display()
        );
        Ok(())
    }

    /// Run a full Git-to-SVN sync cycle.
    ///
    /// 1. Ensure the SVN working copy is checked out.
    /// 2. Fetch recently merged PRs from GitHub.
    /// 3. Skip any PRs whose merge SHA is already recorded in `pr_sync_log`.
    /// 4. For each unsynced PR, replay its commits into the SVN working copy.
    /// 5. Record results in `pr_sync_log` and `commit_map`.
    ///
    /// Returns a summary of what was synced.
    #[instrument(skip(self), fields(repo = %self.github_repo))]
    pub async fn sync(&self) -> Result<GitToSvnResult> {
        info!("starting git-to-svn sync cycle");
        let mut result = GitToSvnResult::default();

        // Ensure SVN working copy exists before doing anything else.
        self.ensure_svn_working_copy()
            .await
            .context("failed to ensure SVN working copy")?;

        // Determine the "since" timestamp from the last completed PR sync.
        let since = self
            .db
            .get_last_pr_sync_time()
            .context("failed to query last PR sync time")?;
        let since_ref = since.as_deref();

        // Fetch recently merged PRs targeting the default branch.
        let merged_prs = self
            .github
            .get_merged_pull_requests(&self.github_repo, &self.default_branch, since_ref)
            .await
            .context("failed to fetch merged pull requests")?;

        info!(count = merged_prs.len(), "found merged pull requests");

        if merged_prs.is_empty() {
            debug!("no new merged PRs to sync");
            return Ok(result);
        }

        // Process each merged PR (oldest first for correct ordering).
        let mut prs_ordered: Vec<PullRequest> = merged_prs;
        prs_ordered.sort_by(|a, b| {
            let a_time = a.merged_at.as_deref().unwrap_or("");
            let b_time = b.merged_at.as_deref().unwrap_or("");
            a_time.cmp(b_time)
        });

        for pr in &prs_ordered {
            let merge_sha = match &pr.merge_commit_sha {
                Some(sha) => sha.clone(),
                None => {
                    warn!(
                        pr_number = pr.number,
                        "PR has no merge_commit_sha, skipping"
                    );
                    continue;
                }
            };

            // Check if this PR merge has already been processed.
            let already_synced = self
                .db
                .is_pr_synced(&merge_sha)
                .context("failed to check pr_sync_log")?;
            if already_synced {
                debug!(pr_number = pr.number, merge_sha = %merge_sha, "PR already synced, skipping");
                result.prs_skipped += 1;
                continue;
            }

            match self.sync_pr(pr, &merge_sha).await {
                Ok(commit_count) => {
                    result.commits_synced += commit_count;
                    result.prs_synced += 1;
                    info!(
                        pr_number = pr.number,
                        commits = commit_count,
                        "PR synced to SVN"
                    );
                }
                Err(e) => {
                    error!(pr_number = pr.number, error = %e, "failed to sync PR to SVN");
                    result.prs_failed += 1;
                }
            }
        }

        info!(
            commits = result.commits_synced,
            prs = result.prs_synced,
            skipped = result.prs_skipped,
            failed = result.prs_failed,
            "git-to-svn sync cycle complete"
        );

        Ok(result)
    }

    /// Sync a single merged PR's commits to SVN.
    ///
    /// Returns the number of commits successfully replayed.
    #[instrument(skip(self, pr), fields(pr_number = pr.number, merge_sha = %merge_sha))]
    async fn sync_pr(&self, pr: &PullRequest, merge_sha: &str) -> Result<u64> {
        let pr_branch = &pr.head.ref_name;

        // Fetch the commits belonging to this PR.
        let commits = self
            .github
            .get_pr_commits(&self.github_repo, pr.number)
            .await
            .context("failed to fetch PR commits")?;

        if commits.is_empty() {
            warn!(pr_number = pr.number, "PR has no commits, skipping");
            return Ok(0);
        }

        // Detect merge strategy for metadata.
        let merge_strategy = self.detect_merge_strategy(pr, &commits).await;

        // Record PR sync as pending.
        let sync_id = self
            .db
            .insert_pr_sync(
                pr.number as i64,
                &pr.title,
                pr_branch,
                merge_sha,
                &merge_strategy,
                commits.len() as i64,
            )
            .context("failed to insert pr_sync_log entry")?;

        // Filter out echo commits (ones we created during SVN-to-Git sync).
        let commits_to_replay: Vec<&GitHubCommit> = commits
            .iter()
            .filter(|c| !CommitFormatter::is_sync_marker(&c.commit.message))
            .collect();

        if commits_to_replay.is_empty() {
            info!(
                pr_number = pr.number,
                "all PR commits are echo commits, marking as synced"
            );
            self.db
                .complete_pr_sync(sync_id, 0, 0)
                .context("failed to complete pr_sync_log entry")?;
            return Ok(0);
        }

        let mut synced_count: u64 = 0;
        let mut first_svn_rev: Option<i64> = None;
        let mut last_svn_rev: Option<i64> = None;

        for commit in &commits_to_replay {
            match self.replay_commit(commit, pr.number, pr_branch).await {
                Ok(svn_rev) => {
                    if first_svn_rev.is_none() {
                        first_svn_rev = Some(svn_rev);
                    }
                    last_svn_rev = Some(svn_rev);
                    synced_count += 1;

                    // Record the commit mapping.
                    let git_author = commit.commit.author.name.as_str();
                    if let Err(e) = self.db.insert_commit_map(
                        svn_rev,
                        &commit.sha,
                        "git_to_svn",
                        &self.svn_author,
                        git_author,
                    ) {
                        warn!(
                            svn_rev,
                            git_sha = %commit.sha,
                            error = %e,
                            "failed to record commit mapping (continuing)"
                        );
                    }

                    // Audit log entry.
                    if let Err(e) = self.db.insert_audit_log(
                        "git_to_svn_commit",
                        Some("git_to_svn"),
                        Some(svn_rev),
                        Some(&commit.sha),
                        Some(&self.svn_author),
                        Some(&format!(
                            "PR #{}: replayed commit {} as r{}",
                            pr.number,
                            &commit.sha[..8.min(commit.sha.len())],
                            svn_rev
                        )),
                        true,
                    ) {
                        warn!(error = %e, "failed to insert audit log entry (continuing)");
                    }
                }
                Err(e) => {
                    error!(
                        git_sha = %commit.sha,
                        error = %e,
                        "failed to replay commit to SVN"
                    );

                    // Mark PR sync as failed.
                    let _ = self.db.fail_pr_sync(sync_id, &format!("{:#}", e));

                    // Audit failure.
                    let _ = self.db.insert_audit_log(
                        "git_to_svn_error",
                        Some("git_to_svn"),
                        None,
                        Some(&commit.sha),
                        Some(&self.svn_author),
                        Some(&format!(
                            "PR #{}: failed to replay commit {}: {}",
                            pr.number,
                            &commit.sha[..8.min(commit.sha.len())],
                            e
                        )),
                        false,
                    );

                    return Err(e);
                }
            }
        }

        // Mark PR sync as completed.
        let svn_start = first_svn_rev.unwrap_or(0);
        let svn_end = last_svn_rev.unwrap_or(0);
        self.db
            .complete_pr_sync(sync_id, svn_start, svn_end)
            .context("failed to complete pr_sync_log entry")?;

        Ok(synced_count)
    }

    /// Replay a single Git commit into the SVN working copy.
    ///
    /// Steps:
    /// 1. `svn update` the working copy to HEAD.
    /// 2. Copy changed files from the Git repo into the SVN working copy.
    /// 3. Detect added/deleted files and run `svn add` / `svn rm`.
    /// 4. `svn commit` with formatted message including metadata trailers.
    ///
    /// Returns the new SVN revision number.
    #[instrument(skip(self, commit), fields(git_sha = %commit.sha))]
    async fn replay_commit(
        &self,
        commit: &GitHubCommit,
        pr_number: u64,
        pr_branch: &str,
    ) -> Result<i64> {
        // 1. Update SVN working copy to latest.
        self.svn
            .update(&self.svn_wc_path)
            .await
            .context("svn update failed")?;

        // 2. Copy files from Git repo to SVN working copy.
        self.apply_git_changes_to_svn(commit)
            .await
            .context("failed to apply git changes to SVN working copy")?;

        // 3. Detect status changes and stage them.
        let status_output = self
            .svn
            .status(&self.svn_wc_path)
            .await
            .context("svn status failed")?;

        let (added, deleted) = parse_svn_status(&status_output);

        if !added.is_empty() {
            let refs: Vec<&str> = added.iter().map(|s| s.as_str()).collect();
            self.svn
                .add(&self.svn_wc_path, &refs)
                .await
                .context("svn add failed")?;
            debug!(count = added.len(), "staged new files for svn add");
        }

        if !deleted.is_empty() {
            let refs: Vec<&str> = deleted.iter().map(|s| s.as_str()).collect();
            self.svn
                .rm(&self.svn_wc_path, &refs)
                .await
                .context("svn rm failed")?;
            debug!(count = deleted.len(), "staged deleted files for svn rm");
        }

        // If there are no changes, the commit is a no-op (e.g., merge-only commits).
        if status_output.trim().is_empty() && added.is_empty() && deleted.is_empty() {
            warn!(
                git_sha = %commit.sha,
                "no file changes detected, performing empty-diff commit for traceability"
            );
        }

        // 4. Format commit message with trailers and commit.
        let formatted_message = self.formatter.format_git_to_svn(
            &commit.commit.message,
            &commit.sha,
            pr_number,
            pr_branch,
        );

        let svn_rev = self
            .svn
            .commit(&self.svn_wc_path, &formatted_message, &self.svn_author)
            .await
            .context("svn commit failed")?;

        info!(
            svn_rev,
            git_sha = %commit.sha,
            "replayed git commit to SVN"
        );

        Ok(svn_rev)
    }

    /// Apply only the specific files changed in this commit to the SVN working
    /// copy. Uses git2 to read the commit's diff and extract per-file content
    /// at the commit SHA, avoiding full-tree copies that could leak unrelated
    /// workspace state or collapse multi-commit PRs.
    ///
    /// Public for integration testing (drives the real LFS-pointer-skip and
    /// policy-evaluation code paths without needing GitHub API calls).
    pub async fn apply_git_changes_to_svn(&self, commit: &GitHubCommit) -> Result<()> {
        let git_client = GitClient::new(&self.git_repo_path)
            .context("failed to open local git repo for commit diff")?;

        // Get the list of files changed in this specific commit.
        let changed_files = git_client
            .get_changed_files(&commit.sha)
            .context("failed to get changed files for commit")?;

        if changed_files.is_empty() {
            debug!(git_sha = %commit.sha, "commit has no file changes");
            return Ok(());
        }

        for (action, file_path) in &changed_files {
            let dst = self.svn_wc_path.join(file_path);

            match action.as_str() {
                "D" => {
                    // File was deleted in this commit: remove it from SVN WC
                    // so `svn status` picks it up as missing.
                    if dst.exists() {
                        std::fs::remove_file(&dst).with_context(|| {
                            format!("failed to remove deleted file: {}", dst.display())
                        })?;
                    }
                }
                _ => {
                    // File was added or modified: read its content at this
                    // specific commit SHA and write to the SVN WC.
                    if let Some(content) = git_client
                        .get_file_content_at_commit(&commit.sha, file_path)
                        .with_context(|| {
                            format!(
                                "failed to read file '{}' at commit {}",
                                file_path, &commit.sha
                            )
                        })?
                    {
                        // Evaluate file against policy before writing.
                        let decision = self.policy.evaluate(file_path, content.len() as u64);
                        match &decision {
                            FilePolicyDecision::Allow => {
                                // Check if this is an LFS pointer that needs resolution.
                                let write_content = if reposync_core::lfs::is_lfs_pointer(
                                    &content,
                                ) {
                                    // The file in Git is an LFS pointer — resolve
                                    // it to the actual blob content before writing
                                    // to SVN (SVN doesn't understand LFS pointers).
                                    match reposync_core::lfs::resolve_lfs_pointer(
                                        &self.git_repo_path,
                                        &content,
                                    ) {
                                        Ok(resolved) => {
                                            info!(
                                                path = file_path,
                                                pointer_size = content.len(),
                                                resolved_size = resolved.len(),
                                                "Git→SVN: resolved LFS pointer to actual content"
                                            );
                                            resolved
                                        }
                                        Err(e) => {
                                            // DO NOT write raw pointer text to SVN —
                                            // that would corrupt the SVN copy. Skip
                                            // this file safely and audit the skip.
                                            error!(
                                                path = file_path,
                                                error = %e,
                                                "Git→SVN: LFS pointer resolution failed — skipping file to prevent pointer text in SVN"
                                            );
                                            let _ = self.db.insert_audit_log(
                                                "lfs_resolution_failed",
                                                Some("git_to_svn"),
                                                None,
                                                Some(&commit.sha),
                                                None,
                                                Some(&format!(
                                                    "Skipped '{}': LFS pointer could not be resolved ({})",
                                                    file_path, e
                                                )),
                                                false,
                                            );
                                            continue;
                                        }
                                    }
                                } else {
                                    content.clone()
                                };

                                if let Some(parent) = dst.parent() {
                                    if !parent.exists() {
                                        std::fs::create_dir_all(parent).with_context(|| {
                                            format!(
                                                "failed to create directory: {}",
                                                parent.display()
                                            )
                                        })?;
                                    }
                                }
                                std::fs::write(&dst, &write_content).with_context(|| {
                                    format!("failed to write file: {}", dst.display())
                                })?;
                            }
                            FilePolicyDecision::LfsTrack { .. } => {
                                // File exceeds LFS threshold — same LFS pointer
                                // resolution logic applies.
                                let write_content = if reposync_core::lfs::is_lfs_pointer(
                                    &content,
                                ) {
                                    match reposync_core::lfs::resolve_lfs_pointer(
                                        &self.git_repo_path,
                                        &content,
                                    ) {
                                        Ok(resolved) => {
                                            info!(
                                                path = file_path,
                                                pointer_size = content.len(),
                                                resolved_size = resolved.len(),
                                                "Git→SVN: resolved LFS pointer (LfsTrack)"
                                            );
                                            resolved
                                        }
                                        Err(e) => {
                                            // DO NOT write raw pointer text to SVN —
                                            // skip safely and audit.
                                            error!(
                                                path = file_path,
                                                error = %e,
                                                "Git→SVN: LFS pointer resolution failed (LfsTrack) — skipping file"
                                            );
                                            let _ = self.db.insert_audit_log(
                                                "lfs_resolution_failed",
                                                Some("git_to_svn"),
                                                None,
                                                Some(&commit.sha),
                                                None,
                                                Some(&format!(
                                                    "Skipped '{}': LFS pointer could not be resolved [LfsTrack] ({})",
                                                    file_path, e
                                                )),
                                                false,
                                            );
                                            continue;
                                        }
                                    }
                                } else {
                                    content.clone()
                                };

                                if let Some(parent) = dst.parent() {
                                    if !parent.exists() {
                                        std::fs::create_dir_all(parent).with_context(|| {
                                            format!(
                                                "failed to create directory: {}",
                                                parent.display()
                                            )
                                        })?;
                                    }
                                }
                                std::fs::write(&dst, &write_content).with_context(|| {
                                    format!("failed to write file: {}", dst.display())
                                })?;
                            }
                            FilePolicyDecision::Ignored { pattern } => {
                                warn!(
                                    path = file_path,
                                    pattern = pattern.as_str(),
                                    git_sha = %commit.sha,
                                    "Git→SVN: file ignored by policy — not replayed"
                                );
                                let _ = self.db.insert_audit_log(
                                    "file_policy_skip",
                                    Some("git_to_svn"),
                                    None,
                                    Some(&commit.sha),
                                    None,
                                    Some(&format!(
                                        "Skipped '{}' (matches '{}')",
                                        file_path, pattern
                                    )),
                                    true,
                                );
                                continue;
                            }
                            FilePolicyDecision::Oversize { size, limit } => {
                                warn!(
                                    path = file_path,
                                    size,
                                    limit,
                                    git_sha = %commit.sha,
                                    "Git→SVN: file exceeds max_file_size — not replayed"
                                );
                                let _ = self.db.insert_audit_log(
                                    "file_policy_skip",
                                    Some("git_to_svn"),
                                    None,
                                    Some(&commit.sha),
                                    None,
                                    Some(&format!(
                                        "Skipped '{}' ({} bytes > {} limit)",
                                        file_path, size, limit
                                    )),
                                    true,
                                );
                                continue;
                            }
                        }
                    }
                }
            }
        }

        debug!(
            git_sha = %commit.sha,
            file_count = changed_files.len(),
            "applied commit-specific changes to SVN working copy"
        );

        Ok(())
    }

    /// Detect the merge strategy used for a PR by inspecting the merge commit.
    async fn detect_merge_strategy(&self, pr: &PullRequest, commits: &[GitHubCommit]) -> String {
        let merge_sha = match &pr.merge_commit_sha {
            Some(sha) => sha,
            None => return "unknown".to_string(),
        };

        // Try to get the merge commit details to check parent count.
        match self.github.get_commit(&self.github_repo, merge_sha).await {
            Ok(detail) => {
                let parent_count = detail.parents.len();
                if parent_count >= 2 {
                    "merge".to_string()
                } else if commits.len() == 1 {
                    "squash".to_string()
                } else {
                    "rebase".to_string()
                }
            }
            Err(e) => {
                warn!(error = %e, "could not detect merge strategy, defaulting to unknown");
                "unknown".to_string()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// File-level helpers (retained for tests)
// ---------------------------------------------------------------------------

/// Recursively copy files from `src` to `dst`, skipping `.git` and `.svn`
/// directories. Existing files are overwritten.
#[cfg(test)]
fn copy_tree(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    let entries = std::fs::read_dir(src)
        .with_context(|| format!("failed to read directory: {}", src.display()))?;

    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Skip VCS metadata directories.
        if name == ".git" || name == ".svn" {
            continue;
        }

        let src_path = entry.path();
        let dst_path = dst.join(&file_name);

        if src_path.is_dir() {
            if !dst_path.exists() {
                std::fs::create_dir_all(&dst_path).with_context(|| {
                    format!("failed to create directory: {}", dst_path.display())
                })?;
            }
            copy_tree(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "failed to copy {} -> {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        }
    }

    Ok(())
}

/// Remove files from `dst` that no longer exist in `src`, skipping `.git`
/// and `.svn` directories. Empty directories are also removed.
#[cfg(test)]
fn remove_stale_files(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    let entries = match std::fs::read_dir(dst) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read directory: {}", dst.display()));
        }
    };

    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Never touch VCS metadata directories.
        if name == ".git" || name == ".svn" {
            continue;
        }

        let src_path = src.join(&file_name);
        let dst_path = entry.path();

        if dst_path.is_dir() {
            if !src_path.exists() {
                // Entire directory removed in git -- leave it for `svn rm` to handle.
                // We just mark it by ensuring the files don't exist, and `svn status`
                // will pick up the missing items.
                continue;
            }
            remove_stale_files(&src_path, &dst_path)?;
        } else if !src_path.exists() {
            // File exists in SVN WC but not in Git repo -- remove it so
            // `svn status` reports it as missing (which we convert to `svn rm`).
            std::fs::remove_file(&dst_path)
                .with_context(|| format!("failed to remove stale file: {}", dst_path.display()))?;
        }
    }

    Ok(())
}

/// Parse `svn status` output to identify unversioned (?) and missing (!) files.
///
/// Returns `(added, deleted)` where:
/// - `added` contains paths of unversioned files to `svn add`.
/// - `deleted` contains paths of missing files to `svn rm`.
fn parse_svn_status(output: &str) -> (Vec<String>, Vec<String>) {
    let mut added = Vec::new();
    let mut deleted = Vec::new();

    for line in output.lines() {
        let line = line.trim_end();
        if line.len() < 2 {
            continue;
        }

        let status_char = line.chars().next().unwrap_or(' ');
        // The file path starts at column 8 in standard `svn status` output,
        // but we handle both formats by trimming leading whitespace after the
        // status character.
        let path = line[1..].trim_start();
        if path.is_empty() {
            continue;
        }

        match status_char {
            '?' => added.push(path.to_string()),
            '!' => deleted.push(path.to_string()),
            _ => {}
        }
    }

    (added, deleted)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_svn_status_added_and_deleted() {
        let output = "\
?       src/new_file.rs
M       src/modified.rs
!       src/removed.rs
?       docs/readme.md
A       src/already_added.rs
!       old/legacy.txt
";
        let (added, deleted) = parse_svn_status(output);
        assert_eq!(added, vec!["src/new_file.rs", "docs/readme.md"]);
        assert_eq!(deleted, vec!["src/removed.rs", "old/legacy.txt"]);
    }

    #[test]
    fn test_parse_svn_status_empty() {
        let (added, deleted) = parse_svn_status("");
        assert!(added.is_empty());
        assert!(deleted.is_empty());
    }

    #[test]
    fn test_parse_svn_status_no_unversioned() {
        let output = "\
M       src/lib.rs
M       Cargo.toml
";
        let (added, deleted) = parse_svn_status(output);
        assert!(added.is_empty());
        assert!(deleted.is_empty());
    }

    #[test]
    fn test_copy_tree_basic() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        // Create source structure.
        std::fs::write(src.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub/b.txt"), "world").unwrap();
        std::fs::create_dir(src.path().join(".git")).unwrap();
        std::fs::write(src.path().join(".git/config"), "secret").unwrap();

        copy_tree(src.path(), dst.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.path().join("a.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join("sub/b.txt")).unwrap(),
            "world"
        );
        // .git should NOT be copied.
        assert!(!dst.path().join(".git").exists());
    }

    #[test]
    fn test_remove_stale_files() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        // Setup: dst has files that src does not.
        std::fs::write(dst.path().join("keep.txt"), "keep").unwrap();
        std::fs::write(dst.path().join("stale.txt"), "remove me").unwrap();
        std::fs::create_dir(dst.path().join(".svn")).unwrap();
        std::fs::write(dst.path().join(".svn/entries"), "svn data").unwrap();

        // src only has keep.txt.
        std::fs::write(src.path().join("keep.txt"), "keep").unwrap();

        remove_stale_files(src.path(), dst.path()).unwrap();

        assert!(dst.path().join("keep.txt").exists());
        assert!(!dst.path().join("stale.txt").exists());
        // .svn must be preserved.
        assert!(dst.path().join(".svn/entries").exists());
    }

    #[test]
    fn test_git_to_svn_result_default() {
        let result = GitToSvnResult::default();
        assert_eq!(result.commits_synced, 0);
        assert_eq!(result.prs_synced, 0);
        assert_eq!(result.prs_skipped, 0);
        assert_eq!(result.prs_failed, 0);
    }
}
