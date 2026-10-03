//! SVN-to-Git sync engine for personal branch mode.
//!
//! Polls SVN for new revisions beyond the stored watermark and replays each
//! revision as a Git commit with proper author identity and metadata trailers.
//! Echo suppression prevents re-syncing commits that originated from the
//! Git side (identified by the `[reposync]` marker in the commit message).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, info};

use reposync_core::db::git_push_operations::{
    git_push_target_fingerprint, GitPushIntent, GitPushOperation, GitPushOperationState,
};
use reposync_core::db::Database;
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::GitClient;
use reposync_core::git_push::{observed_git_ref, observed_git_tree};
use reposync_core::import::{
    copy_tree_with_policy as copy_tree_with_policy_shared,
    remove_stale_files as remove_stale_files_shared,
};
use reposync_core::personal_config::PersonalConfig;
use reposync_core::svn::SvnClient;

use crate::commit_format::CommitFormatter;

/// Watermark key used to track the last SVN revision synced to Git.
const WATERMARK_KEY: &str = "svn_rev";

/// Personal-mode repository scope for the shared SVN→Git push journal.
const PERSONAL_REPO_ID: &str = "personal";

const GIT_REMOTE: &str = "origin";

/// The SVN-to-Git sync engine for personal branch mode.
///
/// Holds references to all required collaborators: SVN client (async), Git
/// client (sync, behind `Arc<std::sync::Mutex>`), the database for
/// persistence, and the personal config for identity and template settings.
pub struct SvnToGitSync {
    svn_client: SvnClient,
    git_client: Arc<std::sync::Mutex<GitClient>>,
    db: Arc<Database>,
    config: PersonalConfig,
    formatter: CommitFormatter,
    policy: FilePolicy,
}

impl SvnToGitSync {
    /// Create a new `SvnToGitSync` instance.
    pub fn new(
        svn_client: SvnClient,
        git_client: Arc<std::sync::Mutex<GitClient>>,
        db: Arc<Database>,
        config: PersonalConfig,
    ) -> Self {
        let formatter = CommitFormatter::new(&config.commit_format);
        let policy = FilePolicy::from(&config.options);
        if policy.has_constraints() {
            info!(
                max_file_size = policy.max_file_size(),
                ignore_patterns = config.options.ignore_patterns.len(),
                lfs_enabled = policy.lfs_enabled(),
                lfs_threshold = policy.lfs_threshold(),
                "file policy active for SVN→Git sync"
            );
        }
        Self {
            svn_client,
            git_client,
            db,
            config,
            formatter,
            policy,
        }
    }

    /// Run one SVN-to-Git sync pass.
    ///
    /// Fetches new SVN revisions since the stored watermark and replays each
    /// one as a Git commit (with push). Returns the number of revisions
    /// successfully synced.
    ///
    /// Revisions are skipped if:
    /// - The commit message contains the `[reposync]` echo marker.
    /// - The revision is already recorded in the `commit_map` table.
    pub async fn sync(&self) -> Result<usize> {
        if let Some(op) = self
            .db
            .active_git_push_operation(PERSONAL_REPO_ID)
            .context("failed to read active personal svn-to-git push")?
        {
            if let Some(reason) = blocking_git_push_hold(&op) {
                anyhow::bail!(reason);
            }
        }

        // 1. Read the current watermark.
        let watermark = self
            .db
            .get_watermark(WATERMARK_KEY)
            .context("failed to read SVN watermark from database")?
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);

        info!(watermark, "starting SVN-to-Git sync pass");

        // 2. Query SVN HEAD revision.
        let svn_info = self
            .svn_client
            .info()
            .await
            .context("failed to get SVN repository info")?;
        let head_rev = svn_info.latest_rev;

        if head_rev <= watermark {
            debug!(head_rev, watermark, "SVN is up to date, nothing to sync");
            return Ok(0);
        }

        info!(
            from = watermark + 1,
            to = head_rev,
            "found new SVN revisions to sync"
        );

        // 3. Fetch the log entries for the new revision range.
        let log_entries = self
            .svn_client
            .log(watermark + 1, head_rev)
            .await
            .context("failed to fetch SVN log entries")?;

        let mut synced_count: usize = 0;

        for entry in &log_entries {
            let rev = entry.revision;

            // 4a. Echo suppression: skip commits that contain our sync marker.
            if CommitFormatter::is_sync_marker(&entry.message) {
                debug!(rev, "skipping echo SVN revision (sync marker detected)");
                self.advance_watermark(rev)?;
                continue;
            }

            // 4b. Idempotency: skip if already recorded in the commit map.
            let already_synced = self
                .db
                .is_svn_rev_synced(rev)
                .context("failed to check commit_map for SVN revision")?;
            if already_synced {
                debug!(rev, "skipping already-synced SVN revision");
                self.advance_watermark(rev)?;
                continue;
            }

            // 5. Export the SVN revision to a temporary directory.
            let export_dir = tempfile::tempdir()
                .context("failed to create temporary directory for SVN export")?;

            self.svn_client
                .export("", rev, export_dir.path())
                .await
                .with_context(|| format!("failed to export SVN revision r{}", rev))?;

            // 6. Copy exported files into the Git working tree (with policy).
            let repo_path = {
                let git_client = self.git_client.lock().unwrap();
                git_client.repo_path().to_path_buf()
            }; // Release lock before blocking I/O.

            let skipped =
                Self::copy_tree_with_policy(export_dir.path(), &repo_path, &self.policy, &self.db)
                    .with_context(|| format!("failed to copy exported files for r{}", rev))?;

            if skipped > 0 {
                info!(rev, skipped, "SVN→Git: files skipped by policy during copy");
            }

            // 6b. Remove files from the Git tree that are no longer in the SVN export.
            Self::remove_stale_files(export_dir.path(), &repo_path)
                .with_context(|| format!("failed to remove stale files for r{}", rev))?;

            // 7. Format the commit message with metadata trailers.
            let commit_message =
                self.formatter
                    .format_svn_to_git(&entry.message, rev, &entry.author, &entry.date);

            // 8. Read the remote tip, then stage and commit locally.
            let branch = self.config.github.default_branch.clone();
            let (pre_push_sha, pre_push_tree) = {
                let git_client = self.git_client.lock().unwrap();
                let pre_push_sha = git_client
                    .ls_remote_ref(GIT_REMOTE, &branch)
                    .with_context(|| format!("failed to read remote {GIT_REMOTE}/{branch}"))?
                    .unwrap_or_default();
                let pre_push_tree = if pre_push_sha.is_empty() {
                    None
                } else {
                    Some(
                        git_client
                            .commit_parent_and_tree(&pre_push_sha)
                            .with_context(|| {
                                format!("failed to read remote tree for {}", pre_push_sha)
                            })?
                            .1,
                    )
                };
                (pre_push_sha, pre_push_tree)
            };

            let git_author = format!(
                "{} <{}>",
                self.config.developer.name, self.config.developer.email
            );
            let git_sha = {
                let author_name = self.config.developer.name.clone();
                let author_email = self.config.developer.email.clone();
                let committer_name = author_name.clone();
                let committer_email = author_email.clone();
                let msg = commit_message.clone();
                let gc = self.git_client.clone();

                tokio::task::spawn_blocking(move || {
                    let git_client = gc.lock().unwrap();
                    git_client.commit(
                        &msg,
                        &author_name,
                        &author_email,
                        &committer_name,
                        &committer_email,
                    )
                })
                .await
                .context("commit task panicked")?
                .with_context(|| format!("failed to create Git commit for SVN r{}", rev))?
            };

            let sha_str = git_sha.to_string();
            info!(rev, sha = %sha_str, "committed SVN revision as Git commit");

            let (intended_parent, intended_tree) = {
                let git_client = self.git_client.lock().unwrap();
                git_client
                    .commit_parent_and_tree(&sha_str)
                    .with_context(|| format!("failed to read local tree for {}", sha_str))?
            };
            let target_fingerprint =
                git_push_target_fingerprint(PERSONAL_REPO_ID, GIT_REMOTE, &branch);
            let request_id = format!("svn-r{}", rev);
            let push_op = self
                .db
                .begin_svn_to_git_push(GitPushIntent {
                    repo_id: PERSONAL_REPO_ID,
                    initiator_id: "personal_worker",
                    request_id: &request_id,
                    target_fingerprint: &target_fingerprint,
                    source_svn_rev: rev,
                    source_svn_author: &entry.author,
                    source_svn_message: &entry.message,
                    pre_push_git_remote: GIT_REMOTE,
                    pre_push_git_branch: &branch,
                    pre_push_git_sha: &pre_push_sha,
                    pre_push_git_tree: pre_push_tree.as_deref(),
                    intended_local_git_sha: &sha_str,
                    intended_local_git_parent: intended_parent.as_deref(),
                    intended_local_git_tree: &intended_tree,
                })
                .with_context(|| format!("failed to record svn-to-git push intent for r{}", rev))?;

            #[cfg(debug_assertions)]
            if git_push_fixture_flag(
                "REPOSYNC_GIT_PUSH_CRASH_BEFORE",
                PERSONAL_REPO_ID,
                &repo_path,
            ) {
                let _ = self.db.hold_svn_to_git_reconciliation(
                    PERSONAL_REPO_ID,
                    &push_op.id,
                    "intent recorded; planned Git push was not issued",
                );
                anyhow::bail!(
                    "reconciliation_required: intent recorded before personal git push for r{}",
                    rev
                );
            }

            // 9. Push to origin after durable intent is recorded.
            let gc = self.git_client.clone();
            let branch_for_push = branch.clone();
            let push_result = tokio::task::spawn_blocking(move || {
                let git_client = gc.lock().unwrap();
                git_client.push(GIT_REMOTE, &branch_for_push)
            })
            .await
            .context("push task panicked")?;
            if let Err(push_err) = push_result {
                let _ = self.db.hold_svn_to_git_reconciliation(
                    PERSONAL_REPO_ID,
                    &push_op.id,
                    &format!("git push failed after intent was recorded: {push_err}"),
                );
                return Err(push_err).context(format!(
                    "failed to push Git commit for SVN r{}; held for reconcile",
                    rev
                ));
            }

            info!(rev, sha = %sha_str, "pushed to origin");

            #[cfg(debug_assertions)]
            if git_push_fixture_flag("REPOSYNC_GIT_PUSH_LOST_REPLY", PERSONAL_REPO_ID, &repo_path) {
                let _ = self.db.hold_svn_to_git_reconciliation(
                    PERSONAL_REPO_ID,
                    &push_op.id,
                    "Git accepted the push but the reply was lost before local checkpoint",
                );
                anyhow::bail!(
                    "reconciliation_required: lost push reply for personal svn-to-git r{}",
                    rev
                );
            }

            let (observed_sha, observed_tree) = {
                let git_client = self.git_client.lock().unwrap();
                let observed_sha = observed_git_ref(&git_client, GIT_REMOTE, &branch)
                    .with_context(|| format!("failed to observe remote {GIT_REMOTE}/{branch}"))?;
                let observed_tree =
                    observed_git_tree(&git_client, &observed_sha).with_context(|| {
                        format!("failed to read observed tree for {}", observed_sha)
                    })?;
                (observed_sha, observed_tree)
            };
            #[cfg(debug_assertions)]
            let observed_tree = if git_push_fixture_flag(
                "REPOSYNC_GIT_PUSH_OBSERVED_TREE_MISMATCH",
                PERSONAL_REPO_ID,
                &repo_path,
            ) {
                "ffffffffffffffffffffffffffffffffffffffff".to_string()
            } else {
                observed_tree
            };
            if observed_sha != sha_str {
                let detail = format!(
                    "remote ref {branch} is {observed_sha} but intended local commit was {sha_str}"
                );
                let _ =
                    self.db
                        .hold_svn_to_git_reconciliation(PERSONAL_REPO_ID, &push_op.id, &detail);
                anyhow::bail!("reconciliation_required: {detail}");
            }
            if observed_tree != intended_tree {
                let detail = format!(
                    "remote commit tree {observed_tree} does not match intended local tree {intended_tree}"
                );
                let _ =
                    self.db
                        .hold_svn_to_git_reconciliation(PERSONAL_REPO_ID, &push_op.id, &detail);
                anyhow::bail!("reconciliation_required: {detail}");
            }

            match self.db.confirm_personal_svn_to_git_push(
                PERSONAL_REPO_ID,
                &push_op.id,
                &observed_sha,
                &observed_tree,
                WATERMARK_KEY,
                &git_author,
            ) {
                Ok(confirmed) => {
                    info!(
                        rev,
                        operation_id = %confirmed.id,
                        sha = %observed_sha,
                        "personal svn-to-git push verified and checkpointed"
                    );
                }
                Err(error) => {
                    let _ = self.db.hold_svn_to_git_reconciliation(
                        PERSONAL_REPO_ID,
                        &push_op.id,
                        &format!(
                            "Git push verified but the local checkpoint write failed: {error}"
                        ),
                    );
                    return Err(error).with_context(|| {
                        format!(
                            "failed to checkpoint personal svn-to-git push for r{}; held for reconcile",
                            rev
                        )
                    });
                }
            }

            // 11. Audit log entry.
            let _ = self.db.insert_audit_log(
                "svn_to_git_sync",
                Some("svn_to_git"),
                Some(rev),
                Some(&sha_str),
                Some(&entry.author),
                Some(&format!(
                    "synced SVN r{} as Git {}",
                    rev,
                    &sha_str[..8.min(sha_str.len())]
                )),
                true,
            );

            synced_count += 1;
        }

        info!(synced_count, "SVN-to-Git sync pass complete");
        Ok(synced_count)
    }

    /// Advance the SVN watermark to the given revision.
    fn advance_watermark(&self, rev: i64) -> Result<()> {
        self.db
            .set_watermark(WATERMARK_KEY, &rev.to_string())
            .with_context(|| format!("failed to advance SVN watermark to r{}", rev))
    }

    /// Policy-unaware wrapper over the shared core copier (tests).
    #[cfg(test)]
    fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
        let noop_policy = FilePolicy::new(0, vec![]);
        let db = Database::in_memory()?;
        db.initialize()?;
        copy_tree_with_policy_shared(src, dst, &noop_policy, &db)?;
        Ok(())
    }

    /// Recursively copy files from `src` into `dst`, enforcing `FilePolicy`.
    ///
    /// Delegates to the shared core writer used by full import and team snapshot.
    /// Returns the number of files skipped by policy.
    pub fn copy_tree_with_policy(
        src: &Path,
        dst: &Path,
        policy: &FilePolicy,
        db: &Database,
    ) -> Result<usize> {
        let stats = copy_tree_with_policy_shared(src, dst, policy, db)?;
        Ok(stats.skipped)
    }

    /// Remove files and directories from `dst` that do not exist in `src`.
    /// Delegates to the shared core helper. Reserved VCS metadata is preserved.
    /// Root `.gitattributes` is reconciled to engine-recorded LFS patterns when
    /// the export omits it.
    fn remove_stale_files(src: &Path, dst: &Path) -> Result<()> {
        remove_stale_files_shared(src, dst)
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

/// Personal mode journals every checkout under `repo_id = "personal"`. Team
/// tests isolate debug fixtures with unique repo ids; personal tests isolate
/// with the Git work-tree suffix so a lost-reply hold cannot trip parallel
/// LFS / happy-path syncs in the same process.
fn git_push_fixture_scope(git_repo_path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    git_repo_path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Env key for a personal SVN→Git debug fixture, unique to one Git work tree.
///
/// Public for integration tests. The personal binary compiles this module
/// privately, so the helper looks unused there.
#[allow(dead_code)]
pub fn personal_git_push_fixture_env_key(var: &str, git_repo_path: &Path) -> String {
    format!(
        "{}__{}__{}",
        var,
        PERSONAL_REPO_ID,
        git_push_fixture_scope(git_repo_path)
    )
}

#[cfg(debug_assertions)]
fn git_push_fixture_flag(var: &str, repo_id: &str, git_repo_path: &Path) -> bool {
    let scoped = format!(
        "{}__{}__{}",
        var,
        repo_id,
        git_push_fixture_scope(git_repo_path)
    );
    if std::env::var(&scoped).is_ok() {
        return true;
    }
    std::env::var(var).ok().as_deref() == Some(repo_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_copy_tree_skips_reserved_vcs_and_preserves_ordinary_dotfiles() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        std::fs::create_dir(src.path().join(".svn")).unwrap();
        std::fs::write(src.path().join(".svn/entries"), "data").unwrap();
        std::fs::write(src.path().join(".gitignore"), "*.tmp\n").unwrap();
        std::fs::write(src.path().join(".editorconfig"), "root = true\n").unwrap();
        std::fs::create_dir_all(src.path().join(".github/workflows")).unwrap();
        std::fs::write(src.path().join(".github/workflows/ci.yml"), "name: ci\n").unwrap();
        std::fs::write(src.path().join("hello.txt"), "world").unwrap();
        std::fs::create_dir(src.path().join("subdir")).unwrap();
        std::fs::write(src.path().join("subdir/.hidden"), "secret").unwrap();
        std::fs::write(src.path().join("subdir/visible.txt"), "content").unwrap();

        std::fs::create_dir(dst.path().join(".git")).unwrap();
        std::fs::write(dst.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();

        SvnToGitSync::copy_tree(src.path(), dst.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.path().join(".git/HEAD")).unwrap(),
            "ref: refs/heads/main"
        );
        assert!(!dst.path().join(".svn").exists());
        assert_eq!(
            std::fs::read_to_string(dst.path().join(".gitignore")).unwrap(),
            "*.tmp\n"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join(".editorconfig")).unwrap(),
            "root = true\n"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join(".github/workflows/ci.yml")).unwrap(),
            "name: ci\n"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join("hello.txt")).unwrap(),
            "world"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join("subdir/.hidden")).unwrap(),
            "secret"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join("subdir/visible.txt")).unwrap(),
            "content"
        );
    }

    #[test]
    fn test_copy_tree_creates_missing_dirs() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        std::fs::create_dir_all(src.path().join("a/b/c")).unwrap();
        std::fs::write(src.path().join("a/b/c/deep.txt"), "deep").unwrap();

        SvnToGitSync::copy_tree(src.path(), dst.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.path().join("a/b/c/deep.txt")).unwrap(),
            "deep"
        );
    }

    #[test]
    fn test_copy_tree_overwrites_existing_files() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        std::fs::write(src.path().join("file.txt"), "new content").unwrap();
        std::fs::write(dst.path().join("file.txt"), "old content").unwrap();

        SvnToGitSync::copy_tree(src.path(), dst.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.path().join("file.txt")).unwrap(),
            "new content"
        );
    }

    #[test]
    fn test_remove_stale_files_basic() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        // src has one file; dst has two files plus .git/.
        std::fs::write(src.path().join("keep.txt"), "keep").unwrap();

        std::fs::write(dst.path().join("keep.txt"), "keep").unwrap();
        std::fs::write(dst.path().join("stale.txt"), "remove me").unwrap();
        std::fs::create_dir(dst.path().join(".git")).unwrap();
        std::fs::write(dst.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        assert!(reposync_core::lfs::ensure_lfs_tracked(dst.path(), "*.bin").unwrap());

        SvnToGitSync::remove_stale_files(src.path(), dst.path()).unwrap();

        assert!(dst.path().join("keep.txt").exists());
        assert!(!dst.path().join("stale.txt").exists());
        // .git must be preserved (root dotdir).
        assert!(dst.path().join(".git/HEAD").exists());
        let gitattr = std::fs::read_to_string(dst.path().join(".gitattributes")).unwrap();
        assert_eq!(
            gitattr, "*.bin filter=lfs diff=lfs merge=lfs -text\n",
            "engine-recorded LFS .gitattributes must survive stale-remove"
        );
    }

    #[test]
    fn test_remove_stale_files_drops_planted_gitattributes() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        std::fs::write(src.path().join("keep.txt"), "keep").unwrap();
        std::fs::create_dir(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub/a.txt"), "a").unwrap();

        std::fs::write(dst.path().join("keep.txt"), "keep").unwrap();
        std::fs::create_dir(dst.path().join(".git")).unwrap();
        std::fs::write(dst.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        std::fs::write(dst.path().join(".gitattributes"), "* filter=evil\n").unwrap();
        std::fs::create_dir(dst.path().join("sub")).unwrap();
        std::fs::write(dst.path().join("sub/a.txt"), "a").unwrap();
        std::fs::write(dst.path().join("sub/.gitattributes"), "* filter=evil\n").unwrap();

        SvnToGitSync::remove_stale_files(src.path(), dst.path()).unwrap();

        assert!(dst.path().join("keep.txt").exists());
        assert!(dst.path().join("sub/a.txt").exists());
        assert!(dst.path().join(".git/HEAD").exists());
        assert!(
            !dst.path().join(".gitattributes").exists(),
            "planted root .gitattributes must not survive when the export omits it"
        );
        assert!(
            !dst.path().join("sub/.gitattributes").exists(),
            "nested planted .gitattributes must not be name-protected"
        );
    }

    #[test]
    fn test_remove_stale_files_nested_dirs() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        // src has subdir/a.txt; dst has subdir/a.txt and subdir/b.txt and old_dir/.
        std::fs::create_dir(src.path().join("subdir")).unwrap();
        std::fs::write(src.path().join("subdir/a.txt"), "a").unwrap();

        std::fs::create_dir(dst.path().join("subdir")).unwrap();
        std::fs::write(dst.path().join("subdir/a.txt"), "a").unwrap();
        std::fs::write(dst.path().join("subdir/b.txt"), "b").unwrap();
        std::fs::create_dir(dst.path().join("old_dir")).unwrap();
        std::fs::write(dst.path().join("old_dir/old.txt"), "old").unwrap();

        SvnToGitSync::remove_stale_files(src.path(), dst.path()).unwrap();

        assert!(dst.path().join("subdir/a.txt").exists());
        assert!(!dst.path().join("subdir/b.txt").exists());
        assert!(!dst.path().join("old_dir").exists());
    }

    #[test]
    fn test_watermark_key_constant() {
        assert_eq!(WATERMARK_KEY, "svn_rev");
    }

    #[test]
    fn personal_git_push_fixture_key_differs_per_work_tree() {
        let a = personal_git_push_fixture_env_key(
            "REPOSYNC_GIT_PUSH_LOST_REPLY",
            Path::new("/tmp/personal-a"),
        );
        let b = personal_git_push_fixture_env_key(
            "REPOSYNC_GIT_PUSH_LOST_REPLY",
            Path::new("/tmp/personal-b"),
        );
        assert_ne!(a, b);
        assert!(a.starts_with("REPOSYNC_GIT_PUSH_LOST_REPLY__personal__"));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn git_push_fixture_flag_ignores_shared_personal_env_key() {
        let leaked = "REPOSYNC_GIT_PUSH_LOST_REPLY__personal";
        std::env::set_var(leaked, "1");
        let fired = git_push_fixture_flag(
            "REPOSYNC_GIT_PUSH_LOST_REPLY",
            PERSONAL_REPO_ID,
            Path::new("/tmp/lfs-work-tree"),
        );
        std::env::remove_var(leaked);
        assert!(
            !fired,
            "unscoped personal fixture key must not hold unrelated work trees"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn git_push_fixture_flag_honors_work_tree_scoped_key() {
        let path = Path::new("/tmp/lost-reply-work-tree");
        let key = personal_git_push_fixture_env_key("REPOSYNC_GIT_PUSH_LOST_REPLY", path);
        std::env::set_var(&key, "1");
        let fired = git_push_fixture_flag("REPOSYNC_GIT_PUSH_LOST_REPLY", PERSONAL_REPO_ID, path);
        let other = git_push_fixture_flag(
            "REPOSYNC_GIT_PUSH_LOST_REPLY",
            PERSONAL_REPO_ID,
            Path::new("/tmp/other-work-tree"),
        );
        std::env::remove_var(&key);
        assert!(fired, "path-scoped fixture must fire for that work tree");
        assert!(!other, "path-scoped fixture must not fire for other trees");
    }
}
