//! Git-to-SVN sync engine for Personal Branch Mode.
//!
//! Replays merged pull request commits from a GitHub repository back into an
//! SVN working copy. Each PR's commits are applied in order and committed to
//! SVN with metadata trailers (Git SHA, PR number, branch) for traceability
//! and echo suppression.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, error, info, instrument, warn};

use reposync_core::db::git_push_operations::{GitPushOperation, GitPushOperationState};
use reposync_core::db::personal_scope::PERSONAL_SCOPE_KEY;
use reposync_core::db::svn_commit_operations::{
    svn_commit_target_fingerprint, SvnCommitIntent, SvnCommitOperation, SvnCommitOperationState,
};
use reposync_core::db::Database;
use reposync_core::echo_suppression::{
    classify_incoming_git_commit_personal, EchoDisposition, TeamEchoContext,
};
use reposync_core::file_policy::{FilePolicy, FilePolicyDecision};
use reposync_core::git::github::{GitHubClient, GitHubCommit, PullRequest};
use reposync_core::git::GitClient;
use reposync_core::history_inspect::inspect_personal_history;
use reposync_core::path_projection::{
    project_git_to_svn_changeset, svn_path_identity, GitToSvnInputChange,
    ProjectedGitToSvnChangeset,
};
use reposync_core::personal_config::PersonalConfig;
use reposync_core::svn::SvnClient;
use reposync_core::svn_commit::{
    hash_regular_file_tree, intended_paths_from_contents, observed_svn_tree_at_revision,
};

use crate::commit_format::CommitFormatter;

/// Personal-mode repository scope for the shared Git→SVN commit journal.
const PERSONAL_REPO_ID: &str = PERSONAL_SCOPE_KEY;

/// Personal-mode no-target receipt projection (empty ruleset).
const PERSONAL_NO_TARGET_PROJECTION: &str = "{}";

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
    fn ensure_personal_history_admitted(&self) -> Result<()> {
        inspect_personal_history(
            &self.db,
            &self.git_repo_path,
            &self.default_branch,
            PERSONAL_REPO_ID,
        )
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!(e))
    }

    #[instrument(skip(self), fields(repo = %self.github_repo))]
    pub async fn sync(&self) -> Result<GitToSvnResult> {
        if let Some(op) = self
            .db
            .active_personal_svn_commit_operation()
            .context("failed to read active personal git-to-svn commit")?
        {
            if let Some(reason) = blocking_svn_commit_hold(&op) {
                anyhow::bail!(reason);
            }
        }
        if let Some(op) = self
            .db
            .active_personal_git_push_operation()
            .context("failed to read active personal svn-to-git push")?
        {
            if let Some(reason) = blocking_git_push_hold(&op) {
                anyhow::bail!(reason);
            }
        }

        self.ensure_personal_history_admitted()
            .context("personal Git history inspection blocked git-to-svn sync")?;

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
                .is_personal_pr_synced(&merge_sha)
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

    /// Integration-test entry point for [`sync_pr`] without the cycle-wide journal gate.
    #[doc(hidden)]
    #[allow(dead_code)]
    pub async fn sync_pr_for_test(&self, pr: &PullRequest, merge_sha: &str) -> Result<u64> {
        self.sync_pr(pr, merge_sha).await
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

        let echo_ctx = TeamEchoContext {
            db: self.db.as_ref(),
            repo_id: PERSONAL_REPO_ID,
            no_target_projection: PERSONAL_NO_TARGET_PROJECTION,
        };

        let mut synced_count: u64 = 0;
        let mut first_svn_rev: Option<i64> = None;
        let mut last_svn_rev: Option<i64> = None;
        let mut replayed_any = false;

        for commit in &commits {
            match classify_incoming_git_commit_personal(
                &echo_ctx,
                &commit.sha,
                &commit.commit.message,
            )
            .map_err(|e| anyhow::anyhow!(e))?
            {
                Ok(EchoDisposition::SkipEcho) => {
                    debug!(
                        git_sha = %commit.sha,
                        "skipping echo Git commit (repo-scoped receipt)"
                    );
                    continue;
                }
                Ok(EchoDisposition::DeferPendingJournal) => {
                    let _ = self.db.abandon_pending_pr_sync(sync_id);
                    anyhow::bail!(
                        "pending personal svn-to-git journal for {}; deferring git-to-svn replay",
                        commit.sha
                    );
                }
                Ok(EchoDisposition::ApplyGenuine | EchoDisposition::ApplyGenuineWithMarkerHint) => {
                }
                Err(sync_err) => return Err(sync_err).context("git commit classification failed"),
            }

            if self
                .db
                .is_personal_git_sha_synced(&commit.sha)
                .context("failed to check personal git-to-svn sync evidence for Git SHA")?
            {
                debug!(git_sha = %commit.sha, "skipping already-synced Git commit");
                continue;
            }

            replayed_any = true;
            match self.replay_commit(commit, pr.number, pr_branch).await {
                Ok(svn_rev) => {
                    if first_svn_rev.is_none() {
                        first_svn_rev = Some(svn_rev);
                    }
                    last_svn_rev = Some(svn_rev);
                    synced_count += 1;

                    // Audit log entry. Mapping is written only in journal confirm.
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

                    let err_text = format!("{:#}", e);
                    if !err_text.contains("reconciliation_required") {
                        let _ = self.db.fail_pr_sync(sync_id, &err_text);
                    }

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

        if !replayed_any && synced_count == 0 {
            info!(
                pr_number = pr.number,
                "all PR commits are echo or already synced, marking as synced"
            );
            self.db
                .complete_pr_sync(sync_id, 0, 0)
                .context("failed to complete pr_sync_log entry")?;
            return Ok(0);
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
    /// 4. Record durable Git→SVN intent, then `svn commit`.
    /// 5. Confirm only after the observed SVN revision/tree matches.
    ///
    /// Returns the new SVN revision number.
    ///
    /// Public for integration testing of the crash-safe journal path.
    #[instrument(skip(self, commit), fields(git_sha = %commit.sha))]
    pub async fn replay_commit(
        &self,
        commit: &GitHubCommit,
        pr_number: u64,
        pr_branch: &str,
    ) -> Result<i64> {
        if let Some(op) = self
            .db
            .active_personal_svn_commit_operation()
            .context("failed to read active personal git-to-svn commit")?
        {
            if let Some(reason) = blocking_svn_commit_hold(&op) {
                anyhow::bail!(reason);
            }
        }
        if let Some(op) = self
            .db
            .active_personal_git_push_operation()
            .context("failed to read active personal svn-to-git push")?
        {
            if let Some(reason) = blocking_git_push_hold(&op) {
                anyhow::bail!(reason);
            }
        }

        self.ensure_personal_history_admitted()
            .context("personal Git history inspection blocked git-to-svn replay")?;

        // 1. Update SVN working copy to latest.
        self.svn
            .update(&self.svn_wc_path)
            .await
            .context("svn update failed")?;

        let info = self
            .svn
            .info()
            .await
            .context("failed to read SVN info before personal git-to-svn write")?;
        let pre_write_svn_tree = hash_regular_file_tree(&self.svn_wc_path)
            .context("failed to hash pre-write SVN working copy")?;

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

        let git_client = GitClient::new(&self.git_repo_path)
            .context("failed to open local git repo for git-to-svn intent")?;
        let (parent, tree) = git_client
            .commit_parent_and_tree(&commit.sha)
            .context("failed to read source Git parent and tree")?;
        let projected = self
            .projected_changes_for_commit(&git_client, &commit.sha)
            .context("failed to project Git changes for commit journal")?;
        let file_contents = projected.into_file_contents();
        let intended_changed_paths = intended_paths_from_contents(&file_contents);
        let intended_svn_tree = hash_regular_file_tree(&self.svn_wc_path)
            .context("failed to hash intended SVN working copy")?;
        let fingerprint = svn_commit_target_fingerprint(
            PERSONAL_REPO_ID,
            &info.uuid,
            self.svn.url(),
            &info.url,
            "{}",
        );
        let identity = svn_path_identity(&info.root_url, &info.url);
        let request_id = format!("git-{}", commit.sha);
        let git_author = commit.commit.author.name.as_str();
        let op = self
            .db
            .begin_git_to_svn_commit(SvnCommitIntent {
                repo_id: PERSONAL_REPO_ID,
                initiator_id: "personal_worker",
                request_id: &request_id,
                target_fingerprint: &fingerprint,
                source_git_sha: &commit.sha,
                source_git_parent: parent.as_deref(),
                source_git_tree: &tree,
                target_svn_uuid: &info.uuid,
                target_svn_path: &info.url,
                target_svn_root_url: &identity.root_url,
                target_svn_branch_path: &identity.branch_path,
                pre_write_svn_rev: info.latest_rev,
                pre_write_svn_tree: &pre_write_svn_tree,
                projection: "{}",
                intended_changed_paths,
                intended_svn_tree: &intended_svn_tree,
                author: &self.svn_author,
                source_message: &commit.commit.message,
            })
            .with_context(|| {
                format!(
                    "failed to record git-to-svn commit intent for {}",
                    commit.sha
                )
            })?;

        #[cfg(debug_assertions)]
        if svn_commit_fixture_flag(
            "REPOSYNC_SVN_COMMIT_CRASH_BEFORE",
            PERSONAL_REPO_ID,
            &self.svn_wc_path,
        ) {
            let _ = self.db.hold_git_to_svn_reconciliation(
                PERSONAL_REPO_ID,
                &op.id,
                "intent recorded; planned SVN write was not issued",
            );
            anyhow::bail!(
                "reconciliation_required: intent recorded before personal svn commit for {}",
                commit.sha
            );
        }

        // 4. Format commit message with trailers and commit after durable intent.
        let formatted_message = self.formatter.format_git_to_svn(
            &commit.commit.message,
            &commit.sha,
            pr_number,
            pr_branch,
        );

        let svn_rev = match self
            .svn
            .commit(&self.svn_wc_path, &formatted_message, &self.svn_author)
            .await
        {
            Ok(rev) => rev,
            Err(e) => {
                let _ = self.db.hold_git_to_svn_reconciliation(
                    PERSONAL_REPO_ID,
                    &op.id,
                    &format!("svn commit failed after intent was recorded: {e}"),
                );
                return Err(e).context(format!(
                    "svn commit failed after intent was recorded for {}; held for reconcile",
                    commit.sha
                ));
            }
        };

        info!(
            svn_rev,
            git_sha = %commit.sha,
            "replayed git commit to SVN"
        );

        #[cfg(debug_assertions)]
        if svn_commit_fixture_flag(
            "REPOSYNC_SVN_COMMIT_LOST_REPLY",
            PERSONAL_REPO_ID,
            &self.svn_wc_path,
        ) {
            let _ = self.db.hold_git_to_svn_reconciliation(
                PERSONAL_REPO_ID,
                &op.id,
                "SVN accepted the commit but the reply was lost before local checkpoint",
            );
            anyhow::bail!(
                "reconciliation_required: lost commit reply for personal git-to-svn {}",
                commit.sha
            );
        }

        let observed_svn_tree = match observed_svn_tree_at_revision(&self.svn, svn_rev).await {
            Ok(tree) => tree,
            Err(error) => {
                let detail = format!(
                    "SVN accepted the commit but the observed tree could not be re-read: {error}"
                );
                let _ = self
                    .db
                    .hold_git_to_svn_reconciliation(PERSONAL_REPO_ID, &op.id, &detail);
                anyhow::bail!("reconciliation_required: {detail}");
            }
        };
        #[cfg(debug_assertions)]
        let observed_svn_tree = if svn_commit_fixture_flag(
            "REPOSYNC_SVN_COMMIT_OBSERVED_TREE_MISMATCH",
            PERSONAL_REPO_ID,
            &self.svn_wc_path,
        ) {
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string()
        } else {
            observed_svn_tree
        };
        if observed_svn_tree != intended_svn_tree {
            let detail =
                format!("SVN revision {svn_rev} tree does not match the intended Git-to-SVN tree");
            let _ = self
                .db
                .hold_git_to_svn_reconciliation(PERSONAL_REPO_ID, &op.id, &detail);
            anyhow::bail!("reconciliation_required: {detail}");
        }

        match self.db.confirm_personal_git_to_svn_commit(
            PERSONAL_REPO_ID,
            &op.id,
            svn_rev,
            &observed_svn_tree,
            git_author,
        ) {
            Ok(confirmed) => {
                info!(
                    svn_rev,
                    operation_id = %confirmed.id,
                    git_sha = %commit.sha,
                    "personal git-to-svn commit verified and checkpointed"
                );
            }
            Err(error) => {
                let _ = self.db.hold_git_to_svn_reconciliation(
                    PERSONAL_REPO_ID,
                    &op.id,
                    &format!(
                        "SVN accepted the commit but the local checkpoint write failed: {error}"
                    ),
                );
                return Err(error).with_context(|| {
                    format!(
                        "failed to checkpoint personal git-to-svn commit for {}; held for reconcile",
                        commit.sha
                    )
                });
            }
        }

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

        let projected = self
            .projected_changes_for_commit(&git_client, &commit.sha)
            .context("failed to project Git changes for SVN apply")?;

        if projected.is_empty() {
            debug!(git_sha = %commit.sha, "commit has no file changes");
            return Ok(());
        }

        let file_changes = projected.into_file_contents();
        for (action, file_path, content) in &file_changes {
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
                    // File was added or modified: write projected content to the SVN WC.
                    if let Some(content) = content {
                        // Evaluate file against policy before writing.
                        let decision = self.policy.evaluate(file_path, content.len() as u64);
                        match &decision {
                            FilePolicyDecision::Allow => {
                                // Check if this is an LFS pointer that needs resolution.
                                let write_content = if reposync_core::lfs::is_lfs_pointer(content) {
                                    // The file in Git is an LFS pointer — resolve
                                    // it to the actual blob content before writing
                                    // to SVN (SVN doesn't understand LFS pointers).
                                    match reposync_core::lfs::resolve_lfs_pointer(
                                        &self.git_repo_path,
                                        content,
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
                                            error!(
                                                path = file_path,
                                                error = %e,
                                                "Git→SVN: LFS pointer resolution failed — holding apply"
                                            );
                                            let _ = self.db.insert_audit_log(
                                                "lfs_resolution_failed",
                                                Some("git_to_svn"),
                                                None,
                                                Some(&commit.sha),
                                                None,
                                                Some(&format!(
                                                    "Held '{}': LFS pointer could not be resolved ({})",
                                                    file_path, e
                                                )),
                                                false,
                                            );
                                            anyhow::bail!(
                                                "LFS pointer for '{}' could not be resolved: {}",
                                                file_path,
                                                e
                                            );
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
                                let write_content = if reposync_core::lfs::is_lfs_pointer(content) {
                                    match reposync_core::lfs::resolve_lfs_pointer(
                                        &self.git_repo_path,
                                        content,
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
                                            error!(
                                                path = file_path,
                                                error = %e,
                                                "Git→SVN: LFS pointer resolution failed (LfsTrack) — holding apply"
                                            );
                                            let _ = self.db.insert_audit_log(
                                                "lfs_resolution_failed",
                                                Some("git_to_svn"),
                                                None,
                                                Some(&commit.sha),
                                                None,
                                                Some(&format!(
                                                    "Held '{}': LFS pointer could not be resolved [LfsTrack] ({})",
                                                    file_path, e
                                                )),
                                                false,
                                            );
                                            anyhow::bail!(
                                                "LFS pointer for '{}' could not be resolved [LfsTrack]: {}",
                                                file_path,
                                                e
                                            );
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
            file_count = file_changes.len(),
            "applied commit-specific changes to SVN working copy"
        );

        Ok(())
    }

    /// Build the projected Git→SVN changeset for one commit.
    ///
    /// Personal mode has no allow/block path rules (#59); empty rules mean the
    /// whole tree is in scope. Renames are split to per-endpoint D/A before apply.
    fn projected_changes_for_commit(
        &self,
        git_client: &GitClient,
        commit_sha: &str,
    ) -> Result<ProjectedGitToSvnChangeset> {
        let changed_files = git_client
            .get_changed_files(commit_sha)
            .context("failed to get changed files for commit")?;
        let mut inputs = Vec::new();
        for change in &changed_files {
            let content = if change.action != "D" {
                git_client
                    .get_file_content_at_commit(commit_sha, &change.path)
                    .with_context(|| {
                        format!("failed to read '{}' at {}", change.path, commit_sha)
                    })?
            } else {
                None
            };
            inputs.push(GitToSvnInputChange {
                action: change.action.clone(),
                path: change.path.clone(),
                content,
                rename_from: change.rename_from.clone(),
            });
        }
        const NO_RULES: &[String] = &[];
        project_git_to_svn_changeset(inputs, NO_RULES, NO_RULES).map_err(|err| anyhow::anyhow!(err))
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

fn blocking_git_push_hold(op: &GitPushOperation) -> Option<String> {
    if op.state == GitPushOperationState::ReconciliationRequired && !op.resume_authorized {
        return Some(format!(
            "reconciliation_required: personal svn-to-git push held ({})",
            op.outcome_detail
                .as_deref()
                .unwrap_or("inspect the exact Git ref before retrying")
        ));
    }
    if !op.state.is_terminal() {
        return Some(
            "repository has an active personal svn-to-git push; wait for the current operation"
                .into(),
        );
    }
    None
}

fn blocking_svn_commit_hold(op: &SvnCommitOperation) -> Option<String> {
    if op.state == SvnCommitOperationState::ReconciliationRequired && !op.resume_authorized {
        return Some(format!(
            "reconciliation_required: personal git-to-svn commit held ({})",
            op.outcome_detail
                .as_deref()
                .unwrap_or("inspect the exact SVN revision before retrying")
        ));
    }
    if !op.state.is_terminal() {
        return Some(
            "repository has an active personal git-to-svn commit; wait for the current operation"
                .into(),
        );
    }
    None
}

/// Personal mode journals every checkout under the collision-proof scope key.
/// Legacy installations used `repo_id = "personal"`; reads still honor that id.
/// Tests
/// isolate debug fixtures with the SVN working-copy path so a lost-reply hold
/// cannot trip parallel personal tests in the same process.
fn svn_commit_fixture_scope(svn_wc_path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    svn_wc_path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Env key for a personal Git→SVN debug fixture, unique to one SVN working copy.
///
/// Public for integration tests. The personal binary compiles this module
/// privately, so the helper looks unused there.
#[allow(dead_code)]
pub fn personal_svn_commit_fixture_env_key(var: &str, svn_wc_path: &Path) -> String {
    format!(
        "{}__{}__{}",
        var,
        PERSONAL_REPO_ID,
        svn_commit_fixture_scope(svn_wc_path)
    )
}

#[cfg(debug_assertions)]
fn svn_commit_fixture_flag(var: &str, repo_id: &str, svn_wc_path: &Path) -> bool {
    let scoped = format!(
        "{}__{}__{}",
        var,
        repo_id,
        svn_commit_fixture_scope(svn_wc_path)
    );
    if std::env::var(&scoped).is_ok() {
        return true;
    }
    std::env::var(var).ok().as_deref() == Some(repo_id)
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

    #[test]
    fn personal_svn_commit_fixture_key_differs_per_working_copy() {
        let a = personal_svn_commit_fixture_env_key(
            "REPOSYNC_SVN_COMMIT_LOST_REPLY",
            Path::new("/tmp/personal-svn-a"),
        );
        let b = personal_svn_commit_fixture_env_key(
            "REPOSYNC_SVN_COMMIT_LOST_REPLY",
            Path::new("/tmp/personal-svn-b"),
        );
        assert_ne!(a, b);
        assert!(a.starts_with("REPOSYNC_SVN_COMMIT_LOST_REPLY__"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn svn_commit_fixture_flag_ignores_shared_personal_env_key() {
        let leaked = "REPOSYNC_SVN_COMMIT_LOST_REPLY__personal";
        std::env::set_var(leaked, "1");
        let fired = svn_commit_fixture_flag(
            "REPOSYNC_SVN_COMMIT_LOST_REPLY",
            PERSONAL_REPO_ID,
            Path::new("/tmp/other-svn-wc"),
        );
        std::env::remove_var(leaked);
        assert!(
            !fired,
            "unscoped personal fixture key must not hold unrelated working copies"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn svn_commit_fixture_flag_honors_working_copy_scoped_key() {
        let path = Path::new("/tmp/lost-reply-svn-wc");
        let key = personal_svn_commit_fixture_env_key("REPOSYNC_SVN_COMMIT_LOST_REPLY", path);
        std::env::set_var(&key, "1");
        let fired =
            svn_commit_fixture_flag("REPOSYNC_SVN_COMMIT_LOST_REPLY", PERSONAL_REPO_ID, path);
        let other = svn_commit_fixture_flag(
            "REPOSYNC_SVN_COMMIT_LOST_REPLY",
            PERSONAL_REPO_ID,
            Path::new("/tmp/other-svn-wc"),
        );
        std::env::remove_var(&key);
        assert!(fired, "path-scoped fixture must fire for that working copy");
        assert!(!other, "path-scoped fixture must not fire for other copies");
    }
}
