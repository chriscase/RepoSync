//! Git-to-SVN sync engine for Personal Branch Mode.
//!
//! Replays merged pull request commits from a GitHub repository back into an
//! SVN working copy. Each PR's commits are applied in order and committed to
//! SVN with metadata trailers (Git SHA, PR number, branch) for traceability
//! and echo suppression.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use reposync_core::errors::SvnError;

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
use reposync_core::history_inspect::inspect_personal_history_with_http_auth;
use reposync_core::path_projection::{
    project_git_to_svn_changeset, svn_path_identity, GitToSvnInputChange,
    ProjectedGitToSvnChangeset,
};
use reposync_core::personal_config::PersonalConfig;
use reposync_core::svn::SvnClient;
use reposync_core::svn_commit::{
    append_durable_git_to_svn_identity, hash_regular_file_tree, inspect_git_to_svn_commit,
    intended_paths_from_contents, observed_svn_tree_at_revision, SvnCommitInspect,
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
    http_auth_token: Option<String>,
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
            http_auth_token: config.github.token.clone(),
        }
    }

    fn personal_history_http_auth(&self) -> Option<String> {
        let config_token = self
            .http_auth_token
            .as_deref()
            .filter(|value| !value.is_empty());
        reposync_core::git::resolve_git_http_auth_token_for_workdir(
            &self.db,
            PERSONAL_REPO_ID,
            config_token,
            &self.git_repo_path,
        )
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
        let http_auth_token = self.personal_history_http_auth();
        inspect_personal_history_with_http_auth(
            &self.db,
            &self.git_repo_path,
            &self.default_branch,
            PERSONAL_REPO_ID,
            http_auth_token.as_deref(),
            self.http_auth_token.as_deref(),
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
            if op.state == SvnCommitOperationState::ReconciliationRequired {
                match self
                    .recover_held_personal_git_to_svn_reconciliation(&op, None)
                    .await
                {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        if let Some(reason) = blocking_svn_commit_hold(&op) {
                            anyhow::bail!(reason);
                        }
                    }
                    Err(error) => return Err(error),
                }
            } else if let Some(reason) = blocking_svn_commit_hold(&op) {
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
                    // Ordering matters: once a PR aborts mid-apply, later PRs in this
                    // pass must not run until the held SHA retries successfully.
                    warn!(
                        pr_number = pr.number,
                        "stopping git-to-svn batch after apply abort"
                    );
                    break;
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
                    if err_text.contains("reconciliation_required") {
                        // durable hold already recorded; leave pr_sync_log pending
                    } else {
                        // Abandon the pending row so the next sync pass retries this SHA.
                        let _ = self.db.abandon_pending_pr_sync(sync_id);
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
            if op.state == SvnCommitOperationState::ReconciliationRequired
                && op.source_git_sha == commit.sha
            {
                match self
                    .recover_held_personal_git_to_svn_reconciliation(
                        &op,
                        Some(commit.commit.author.name.as_str()),
                    )
                    .await
                {
                    Ok(Some(svn_rev)) => return Ok(svn_rev),
                    Ok(None) if op.resume_authorized => {}
                    Ok(None) => {
                        if let Some(reason) = blocking_svn_commit_hold(&op) {
                            anyhow::bail!(reason);
                        }
                    }
                    Err(error) => return Err(error),
                }
            } else if let Some(reason) = blocking_svn_commit_hold(&op) {
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

        let mut apply_ownership = ApplyAbortOwnership::default();
        let replay_result: Result<i64> = async {
            let info = self
                .svn
                .info()
                .await
                .context("failed to read SVN info before personal git-to-svn write")?;
            let pre_write_svn_tree = hash_regular_file_tree(&self.svn_wc_path)
                .context("failed to hash pre-write SVN working copy")?;

            // 2. Copy files from Git repo to SVN working copy.
            self.apply_git_changes_to_svn_inner(commit, &mut apply_ownership)
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
            let formatted_message = append_durable_git_to_svn_identity(
                &self.formatter.format_git_to_svn(
                    &commit.commit.message,
                    &commit.sha,
                    pr_number,
                    pr_branch,
                ),
                &commit.sha,
                &op.id,
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

            // SVN accepted the commit; abort restore must never delete the committed tree.
            apply_ownership.disarm();

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
                let detail = format!(
                    "SVN revision {svn_rev} tree does not match the intended Git-to-SVN tree"
                );
                let _ = self
                    .db
                    .hold_git_to_svn_reconciliation(PERSONAL_REPO_ID, &op.id, &detail);
                anyhow::bail!("reconciliation_required: {detail}");
            }

            #[cfg(debug_assertions)]
            if svn_commit_fixture_flag(
                "REPOSYNC_SVN_COMMIT_CONFIRM_FAIL",
                PERSONAL_REPO_ID,
                &self.svn_wc_path,
            ) {
                let detail =
                    "SVN accepted the commit but the local checkpoint write failed: simulated confirm failure";
                let _ = self
                    .db
                    .hold_git_to_svn_reconciliation(PERSONAL_REPO_ID, &op.id, detail);
                return Err(anyhow::anyhow!(detail)).with_context(|| {
                    format!(
                        "failed to checkpoint personal git-to-svn commit for {}; held for reconcile",
                        commit.sha
                    )
                });
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
        .await;

        match replay_result {
            Ok(rev) => Ok(rev),
            Err(error) => {
                if apply_ownership.should_restore_after_abort() {
                    self.restore_svn_working_copy_after_abort(&apply_ownership)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to restore SVN working copy after git-to-svn apply abort for {}",
                                commit.sha
                            )
                        })?;
                }
                Err(error)
            }
        }
    }

    /// Apply only the specific files changed in this commit to the SVN working
    /// copy. Uses git2 to read the commit's diff and extract per-file content
    /// at the commit SHA, avoiding full-tree copies that could leak unrelated
    /// workspace state or collapse multi-commit PRs.
    ///
    /// Public for integration testing (drives the real LFS-pointer-skip and
    /// policy-evaluation code paths without needing GitHub API calls).
    #[allow(dead_code)]
    pub async fn apply_git_changes_to_svn(&self, commit: &GitHubCommit) -> Result<()> {
        let mut ownership = ApplyAbortOwnership::default();
        match self
            .apply_git_changes_to_svn_inner(commit, &mut ownership)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => {
                self.restore_svn_working_copy_after_abort(&ownership)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to restore SVN working copy after git-to-svn apply abort for {}",
                            commit.sha
                        )
                    })?;
                Err(error)
            }
        }
    }

    async fn apply_git_changes_to_svn_inner(
        &self,
        commit: &GitHubCommit,
        ownership: &mut ApplyAbortOwnership,
    ) -> Result<()> {
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
            validate_apply_rel_path(file_path)?;

            match action.as_str() {
                "D" => {
                    // File was deleted in this commit: remove it from SVN WC
                    // so `svn status` picks it up as missing.
                    refuse_apply_path_symlink_ancestors(&self.svn_wc_path, file_path)?;
                    let dst = confined_apply_path(&self.svn_wc_path, file_path)?;
                    if dst.exists() {
                        std::fs::remove_file(&dst).with_context(|| {
                            format!("failed to remove deleted file: {}", dst.display())
                        })?;
                    }
                    ownership.touched_paths.push(file_path.clone());
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

                                refuse_apply_path_symlink_ancestors(&self.svn_wc_path, file_path)?;
                                let dst = confined_apply_path(&self.svn_wc_path, file_path)?;
                                let file_preexisted = std::fs::symlink_metadata(&dst).is_ok();
                                ensure_apply_parent_dirs(&self.svn_wc_path, file_path, ownership)?;
                                std::fs::write(&dst, &write_content).with_context(|| {
                                    format!("failed to write file: {}", dst.display())
                                })?;
                                ownership.touched_paths.push(file_path.clone());
                                if !file_preexisted {
                                    ownership.new_files.push(file_path.clone());
                                }
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

                                refuse_apply_path_symlink_ancestors(&self.svn_wc_path, file_path)?;
                                let dst = confined_apply_path(&self.svn_wc_path, file_path)?;
                                let file_preexisted = std::fs::symlink_metadata(&dst).is_ok();
                                ensure_apply_parent_dirs(&self.svn_wc_path, file_path, ownership)?;
                                std::fs::write(&dst, &write_content).with_context(|| {
                                    format!("failed to write file: {}", dst.display())
                                })?;
                                ownership.touched_paths.push(file_path.clone());
                                if !file_preexisted {
                                    ownership.new_files.push(file_path.clone());
                                }
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

    /// Restore only the paths touched by this apply after a mid-apply abort.
    async fn restore_svn_working_copy_after_abort(
        &self,
        ownership: &ApplyAbortOwnership,
    ) -> Result<()> {
        restore_apply_abort_on_wc(&self.svn, &self.svn_wc_path, ownership, None).await
    }

    /// Restore the working copy after a partial apply (integration tests).
    #[allow(dead_code)]
    #[doc(hidden)]
    pub async fn restore_working_copy_after_apply_abort(
        &self,
        ownership: &ApplyAbortOwnership,
    ) -> Result<()> {
        self.restore_svn_working_copy_after_abort(ownership).await
    }

    /// Restore hook for integration tests that must drive `svn revert` targets directly.
    #[allow(dead_code)]
    #[doc(hidden)]
    pub async fn restore_apply_abort_reverting_paths_for_test(
        &self,
        ownership: &ApplyAbortOwnership,
        revert_paths: &[&str],
    ) -> Result<()> {
        restore_apply_abort_on_wc(&self.svn, &self.svn_wc_path, ownership, Some(revert_paths)).await
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

impl GitToSvnSync {
    /// Finalize a proven held personal Git→SVN journal (SVN inspect before author).
    pub async fn promote_held_git_to_svn_reconciliation_if_proven(&self) -> Result<Option<i64>> {
        let Some(op) = self
            .db
            .active_personal_svn_commit_operation()
            .context("failed to read active personal git-to-svn commit")?
        else {
            return Ok(None);
        };
        if op.state != SvnCommitOperationState::ReconciliationRequired {
            return Ok(None);
        }
        self.recover_held_personal_git_to_svn_reconciliation(&op, None)
            .await
    }

    async fn recover_held_personal_git_to_svn_reconciliation(
        &self,
        op: &SvnCommitOperation,
        git_author_override: Option<&str>,
    ) -> Result<Option<i64>> {
        if op.state != SvnCommitOperationState::ReconciliationRequired {
            return Ok(None);
        }
        self.finalize_held_personal_git_to_svn_if_proven(op, git_author_override)
            .await
    }

    async fn resolve_git_author_for_held_commit(
        &self,
        op: &SvnCommitOperation,
        git_author_override: Option<&str>,
    ) -> Result<String> {
        if let Some(name) = git_author_override {
            return Ok(name.to_string());
        }
        if let Ok(git_client) = GitClient::new(&self.git_repo_path) {
            if let Ok(name) = git_client.commit_author_name(&op.source_git_sha) {
                if !name.is_empty() {
                    return Ok(name);
                }
            }
        }
        let detail = self
            .github
            .get_commit(&self.github_repo, &op.source_git_sha)
            .await
            .context("failed to resolve git author for held personal git-to-svn commit")?;
        let name = detail.commit.author.name.trim();
        if name.is_empty() {
            anyhow::bail!(
                "reconciliation_required: GitHub author name is empty for held commit {}",
                op.source_git_sha
            );
        }
        Ok(name.to_string())
    }

    async fn finalize_held_personal_git_to_svn_if_proven(
        &self,
        op: &SvnCommitOperation,
        git_author_override: Option<&str>,
    ) -> Result<Option<i64>> {
        if op.state != SvnCommitOperationState::ReconciliationRequired {
            return Ok(None);
        }
        let inspect = inspect_git_to_svn_commit(&self.svn, op).await;
        match inspect {
            SvnCommitInspect::UniqueMatch {
                svn_rev, svn_tree, ..
            } => {
                let git_author = self
                    .resolve_git_author_for_held_commit(op, git_author_override)
                    .await?;
                self.db
                    .finalize_personal_verified_git_to_svn_commit(
                        PERSONAL_REPO_ID,
                        &op.id,
                        svn_rev,
                        &svn_tree,
                        &git_author,
                    )
                    .with_context(|| {
                        format!(
                            "failed to finalize held personal git-to-svn commit for {}",
                            op.source_git_sha
                        )
                    })?;
                info!(
                    svn_rev,
                    operation_id = %op.id,
                    git_sha = %op.source_git_sha,
                    "held personal git-to-svn commit verified and checkpointed"
                );
                Ok(Some(svn_rev))
            }
            SvnCommitInspect::AbsentUnchanged => Ok(None),
            SvnCommitInspect::Conflict { reason } => anyhow::bail!(
                "reconciliation_required: cannot prove held SVN revision is ours: {reason}"
            ),
            SvnCommitInspect::Unavailable { reason } => {
                anyhow::bail!("reconciliation_required: {reason}")
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
    std::env::var(&scoped).is_ok()
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

/// Column index where `svn status` paths begin (after seven status columns and a separator).
const SVN_STATUS_PATH_OFFSET: usize = 8;

fn svn_status_path(line: &str) -> Option<&str> {
    let line = line.trim_end();
    if line.len() <= SVN_STATUS_PATH_OFFSET {
        return None;
    }
    let path = line[SVN_STATUS_PATH_OFFSET..].trim_start();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn is_path_prefix_ancestor(ancestor: &str, descendant: &str) -> bool {
    if ancestor.is_empty() {
        return false;
    }
    if descendant == ancestor {
        return true;
    }
    if descendant.len() <= ancestor.len() {
        return false;
    }
    descendant.starts_with(ancestor) && descendant.as_bytes().get(ancestor.len()) == Some(&b'/')
}

/// True when an `svn status` path indicates dirt on a scoped apply path we own.
fn apply_abort_status_in_scope(status_path: &str, scope_path: &str) -> bool {
    status_path == scope_path || is_path_prefix_ancestor(scope_path, status_path)
}

/// Integration-test helper for abort-restore cleanliness assertions.
#[allow(dead_code)]
#[doc(hidden)]
pub fn personal_apply_abort_paths_clean_for_test(status_output: &str, scope: &[&str]) -> bool {
    let scope = scope
        .iter()
        .map(|path| (*path).to_string())
        .collect::<Vec<_>>();
    are_apply_paths_clean(status_output, &scope)
}

/// True when none of the touched apply paths appear in `svn status` output.
fn are_apply_paths_clean(status_output: &str, scope: &[String]) -> bool {
    if scope.is_empty() {
        return true;
    }
    for line in status_output.lines() {
        let Some(path) = svn_status_path(line) else {
            continue;
        };
        if scope
            .iter()
            .any(|scoped| apply_abort_status_in_scope(path, scoped))
        {
            return false;
        }
    }
    true
}

/// Paths recorded while applying one commit, used to restore the WC after abort.
#[derive(Debug, Clone)]
pub struct ApplyAbortOwnership {
    /// Paths passed to `svn revert` (added, modified, or deleted by this apply).
    pub touched_paths: Vec<String>,
    /// Files this apply created on disk (pre-existing files are reverted only).
    pub new_files: Vec<String>,
    /// Directories this apply created via `create_dir_all`, shallow to deep.
    pub created_dirs: Vec<String>,
    /// When false, SVN commit succeeded and abort restore must not run.
    restore_armed: bool,
}

impl Default for ApplyAbortOwnership {
    fn default() -> Self {
        Self {
            touched_paths: Vec::new(),
            new_files: Vec::new(),
            created_dirs: Vec::new(),
            restore_armed: true,
        }
    }
}

impl ApplyAbortOwnership {
    /// Build ownership records for integration tests and abort-restore helpers.
    #[allow(dead_code)]
    pub fn with_recorded_paths(
        touched_paths: Vec<String>,
        new_files: Vec<String>,
        created_dirs: Vec<String>,
    ) -> Self {
        Self {
            touched_paths,
            new_files,
            created_dirs,
            restore_armed: true,
        }
    }

    fn is_empty(&self) -> bool {
        self.touched_paths.is_empty() && self.new_files.is_empty() && self.created_dirs.is_empty()
    }

    /// Disarm abort restore after SVN accepts a commit.
    fn disarm(&mut self) {
        self.restore_armed = false;
    }

    fn should_restore_after_abort(&self) -> bool {
        self.restore_armed && !self.is_empty()
    }
}

fn apply_abort_cleanliness_scope(ownership: &ApplyAbortOwnership) -> Vec<String> {
    let mut scope = ownership.touched_paths.clone();
    scope.extend(ownership.created_dirs.iter().cloned());
    scope
}

/// Refuse apply when any existing path component under the WC root is a symlink.
fn refuse_apply_path_symlink_ancestors(wc_root: &Path, rel: &str) -> Result<()> {
    validate_apply_rel_path(rel)?;
    let wc_meta = std::fs::symlink_metadata(wc_root).with_context(|| {
        format!(
            "failed to read SVN working copy root metadata: {}",
            wc_root.display()
        )
    })?;
    if wc_meta.file_type().is_symlink() {
        anyhow::bail!(
            "SVN working copy root is a symlink; refusing apply for '{}'",
            rel
        );
    }

    let mut built = wc_root.to_path_buf();
    for component in Path::new(rel).components() {
        if let Component::Normal(name) = component {
            built = built.join(name);
            let meta = match std::fs::symlink_metadata(&built) {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to inspect apply path component: {}",
                            built.display()
                        )
                    });
                }
            };
            if meta.file_type().is_symlink() {
                anyhow::bail!(
                    "refusing apply through symlink at '{}' (requested '{}')",
                    built.display(),
                    rel
                );
            }
        }
    }
    Ok(())
}

fn confined_apply_path(wc_root: &Path, rel: &str) -> Result<PathBuf> {
    refuse_apply_path_symlink_ancestors(wc_root, rel)?;
    Ok(wc_root.join(rel))
}

fn validate_apply_rel_path(path: &str) -> Result<()> {
    if path.is_empty() {
        anyhow::bail!("apply path must not be empty");
    }
    let path_obj = Path::new(path);
    if path_obj.is_absolute() {
        anyhow::bail!("apply path must be relative: {path}");
    }
    for component in path_obj.components() {
        match component {
            Component::ParentDir => {
                anyhow::bail!("apply path must not contain '..': {path}");
            }
            Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("apply path must be relative: {path}");
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_apply_ownership_paths(ownership: &ApplyAbortOwnership) -> Result<()> {
    for path in ownership
        .touched_paths
        .iter()
        .chain(&ownership.new_files)
        .chain(&ownership.created_dirs)
    {
        validate_apply_rel_path(path)?;
    }
    Ok(())
}

fn rel_path_under_wc(wc_root: &Path, dir: &Path) -> Result<String> {
    let rel = dir
        .strip_prefix(wc_root)
        .with_context(|| format!("path escapes SVN working copy: {}", dir.display()))?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

fn record_apply_parent_dirs_if_missing(
    wc_root: &Path,
    file_rel: &str,
    ownership: &mut ApplyAbortOwnership,
) -> Result<()> {
    let file_path = wc_root.join(file_rel);
    let mut parents = Vec::new();
    let mut current = file_path.parent();
    while let Some(dir) = current {
        if dir == wc_root {
            break;
        }
        parents.push(dir.to_path_buf());
        current = dir.parent();
    }
    parents.reverse();
    for dir in parents {
        if std::fs::symlink_metadata(&dir).is_err() {
            let rel = rel_path_under_wc(wc_root, &dir)?;
            if !ownership.created_dirs.contains(&rel) {
                ownership.created_dirs.push(rel);
            }
        }
    }
    Ok(())
}

fn ensure_apply_parent_dirs(
    wc_root: &Path,
    file_rel: &str,
    ownership: &mut ApplyAbortOwnership,
) -> Result<()> {
    record_apply_parent_dirs_if_missing(wc_root, file_rel, ownership)?;
    let file_path = wc_root.join(file_rel);
    if let Some(parent) = file_path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory: {}", parent.display()))?;
        }
    }
    Ok(())
}

fn svn_stderr_error_code(stderr: &str) -> Option<&str> {
    for line in stderr.lines() {
        let rest = line.strip_prefix("svn: ")?.trim();
        let code = rest.split(':').next()?.trim();
        if code.len() > 1
            && code.starts_with('E')
            && code[1..].chars().all(|ch| ch.is_ascii_digit())
        {
            return Some(code);
        }
    }
    None
}

fn svn_revert_error_is_missing_node(error: &SvnError) -> bool {
    match error {
        SvnError::CommandFailed { stderr, .. } => svn_stderr_error_code(stderr) == Some("E155010"),
        _ => false,
    }
}

async fn restore_apply_abort_on_wc(
    svn: &SvnClient,
    wc_root: &Path,
    ownership: &ApplyAbortOwnership,
    revert_paths_override: Option<&[&str]>,
) -> Result<()> {
    if ownership.is_empty() {
        return Ok(());
    }
    validate_apply_ownership_paths(ownership)?;

    let revert_paths: Vec<&str> = match revert_paths_override {
        Some(paths) => paths.to_vec(),
        None => ownership
            .touched_paths
            .iter()
            .map(|path| path.as_str())
            .collect(),
    };
    for path in revert_paths {
        match svn.revert_files(wc_root, &[path]).await {
            Ok(()) => {}
            Err(error) if svn_revert_error_is_missing_node(&error) => {
                debug!(
                    error = %error,
                    path,
                    "svn revert on touched apply path failed with E155010; continuing with recorded cleanup"
                );
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("svn revert on touched apply path '{path}' failed"));
            }
        }
    }

    remove_apply_created_paths(wc_root, ownership)
        .context("failed to remove apply-created files and directories")?;

    let final_status = svn
        .status(wc_root)
        .await
        .context("svn status failed while verifying restored working copy")?;
    let scope = apply_abort_cleanliness_scope(ownership);
    if !are_apply_paths_clean(&final_status, &scope) {
        anyhow::bail!(
            "SVN working copy still dirty on touched apply paths after abort restore: {}",
            final_status.trim()
        );
    }

    debug!(
        wc = %wc_root.display(),
        touched = ownership.touched_paths.len(),
        "restored SVN working copy after git-to-svn apply abort"
    );
    Ok(())
}

fn remove_apply_created_paths(wc_root: &Path, ownership: &ApplyAbortOwnership) -> Result<()> {
    for rel in &ownership.new_files {
        validate_apply_rel_path(rel)?;
        let full = wc_root.join(rel);
        if full
            .symlink_metadata()
            .map(|meta| meta.is_file())
            .unwrap_or(false)
        {
            std::fs::remove_file(&full).with_context(|| {
                format!("failed to remove apply-created file: {}", full.display())
            })?;
        }
    }

    for rel in ownership.created_dirs.iter().rev() {
        validate_apply_rel_path(rel)?;
        let full = wc_root.join(rel);
        if !full.is_dir() {
            continue;
        }
        match std::fs::remove_dir(&full) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                return Err(error).with_context(|| {
                    format!(
                        "apply-created directory is not empty after cleanup: {}",
                        full.display()
                    )
                });
            }
            Err(error) if error.raw_os_error() == Some(39) => {
                return Err(error).with_context(|| {
                    format!(
                        "apply-created directory is not empty after cleanup: {}",
                        full.display()
                    )
                });
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to remove apply-created directory: {}",
                        full.display()
                    )
                });
            }
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
        let Some(path) = svn_status_path(line) else {
            continue;
        };

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
    fn test_are_apply_paths_clean() {
        let status = "\
M       src/modified.rs
?       docs/unrelated.md
";
        assert!(are_apply_paths_clean(status, &["other.txt".to_string()]));
        assert!(!are_apply_paths_clean(
            status,
            &["src/modified.rs".to_string()]
        ));
        assert!(!are_apply_paths_clean(
            "?       src/newdir\n",
            &["src/newdir".to_string()]
        ));
        assert!(are_apply_paths_clean(
            "?       scratch\n",
            &["scratch/note.txt".to_string()]
        ));
        assert!(!are_apply_paths_clean(
            "MM       src/modified.rs\n",
            &["src/modified.rs".to_string()]
        ));
    }

    #[test]
    fn test_remove_apply_created_paths_preserves_preexisting_unversioned_dir() {
        let wc = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wc.path().join("scratch")).unwrap();
        std::fs::write(wc.path().join("scratch/other.txt"), "keep").unwrap();
        std::fs::write(wc.path().join("scratch/note.txt"), "new").unwrap();

        let ownership = ApplyAbortOwnership {
            touched_paths: vec!["scratch/note.txt".to_string()],
            new_files: vec!["scratch/note.txt".to_string()],
            created_dirs: vec![],
            ..Default::default()
        };
        remove_apply_created_paths(wc.path(), &ownership).unwrap();

        assert!(!wc.path().join("scratch/note.txt").exists());
        assert!(wc.path().join("scratch/other.txt").exists());
        assert!(wc.path().join("scratch").is_dir());
    }

    #[test]
    fn test_remove_apply_created_paths_removes_recorded_empty_dir() {
        let wc = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wc.path().join("src/newdir")).unwrap();
        std::fs::write(wc.path().join("src/newdir/file.txt"), "new").unwrap();

        let ownership = ApplyAbortOwnership {
            touched_paths: vec!["src/newdir/file.txt".to_string()],
            new_files: vec!["src/newdir/file.txt".to_string()],
            created_dirs: vec!["src/newdir".to_string()],
            ..Default::default()
        };
        std::fs::remove_file(wc.path().join("src/newdir/file.txt")).unwrap();
        remove_apply_created_paths(wc.path(), &ownership).unwrap();

        assert!(!wc.path().join("src/newdir").exists());
    }

    #[test]
    fn test_remove_apply_created_paths_skips_versioned_parent_dirs() {
        let wc = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wc.path().join("keep/emptydir")).unwrap();
        std::fs::write(wc.path().join("keep/emptydir/file.txt"), "new").unwrap();

        let ownership = ApplyAbortOwnership {
            touched_paths: vec!["keep/emptydir/file.txt".to_string()],
            new_files: vec!["keep/emptydir/file.txt".to_string()],
            created_dirs: vec![],
            ..Default::default()
        };
        remove_apply_created_paths(wc.path(), &ownership).unwrap();

        assert!(!wc.path().join("keep/emptydir/file.txt").exists());
        assert!(wc.path().join("keep/emptydir").is_dir());
        assert!(wc.path().join("keep").is_dir());
    }

    #[test]
    fn test_remove_apply_created_paths_fails_on_nonempty_ignored_dir() {
        let wc = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(wc.path().join("pkg/.libs")).unwrap();
        std::fs::write(wc.path().join("pkg/.libs/stale.o"), "obj").unwrap();
        std::fs::write(wc.path().join("pkg/file.txt"), "new").unwrap();

        let ownership = ApplyAbortOwnership {
            touched_paths: vec!["pkg/file.txt".to_string()],
            new_files: vec!["pkg/file.txt".to_string()],
            created_dirs: vec!["pkg".to_string()],
            ..Default::default()
        };
        std::fs::remove_file(wc.path().join("pkg/file.txt")).unwrap();
        let err = remove_apply_created_paths(wc.path(), &ownership)
            .expect_err("non-empty created directory must fail restore cleanup");
        assert!(
            format!("{err:#}").contains("not empty"),
            "unexpected error: {err:#}"
        );
        assert!(wc.path().join("pkg/.libs").is_dir());
    }

    #[test]
    fn test_apply_abort_ownership_disarm_skips_restore() {
        let mut ownership = ApplyAbortOwnership::default();
        ownership.touched_paths.push("a.txt".to_string());
        assert!(ownership.should_restore_after_abort());
        ownership.disarm();
        assert!(!ownership.should_restore_after_abort());
    }

    #[test]
    fn test_refuse_apply_path_symlink_ancestors() {
        let wc = tempfile::tempdir().unwrap();
        std::fs::write(wc.path().join("seed.txt"), "seed").unwrap();
        std::os::unix::fs::symlink("seed.txt", wc.path().join("link")).unwrap();
        assert!(refuse_apply_path_symlink_ancestors(wc.path(), "link/keep.txt").is_err());
        assert!(refuse_apply_path_symlink_ancestors(wc.path(), "seed.txt").is_ok());
    }

    #[test]
    fn test_validate_apply_rel_path_rejects_escape() {
        assert!(validate_apply_rel_path("scratch/note.txt").is_ok());
        assert!(validate_apply_rel_path("../outside/file.txt").is_err());
        assert!(validate_apply_rel_path("/etc/hostname").is_err());
    }

    #[test]
    fn test_svn_revert_error_code_classification() {
        let e155010 = SvnError::CommandFailed {
            exit_code: 1,
            stderr: "svn: E155010: The node 'wc' was not found.\n".into(),
        };
        let e155007 = SvnError::CommandFailed {
            exit_code: 1,
            stderr: "svn: E155007: '/etc/hostname' is not a working copy\n".into(),
        };
        assert!(svn_revert_error_is_missing_node(&e155010));
        assert!(!svn_revert_error_is_missing_node(&e155007));
        assert_eq!(
            svn_stderr_error_code("svn: E155007: '/etc/hostname' is not a working copy\n"),
            Some("E155007")
        );
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
