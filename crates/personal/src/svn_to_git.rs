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

use reposync_core::db::Database;
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::GitClient;
use reposync_core::import::{
    copy_tree_with_policy as copy_tree_with_policy_shared,
    remove_stale_files as remove_stale_files_shared,
};
use reposync_core::personal_config::PersonalConfig;
use reposync_core::svn::SvnClient;

use crate::commit_format::CommitFormatter;

/// Watermark key used to track the last SVN revision synced to Git.
const WATERMARK_KEY: &str = "svn_rev";

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

            // 8. Stage all changes and commit using the developer's identity.
            //
            // The GitClient uses git2 (synchronous). We wrap the call in
            // spawn_blocking so we don't block the async runtime.
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

            // 9. Push to origin. Credentials come from the remote URL
            // (set up at clone time / by ensure_remote_credentials).
            let branch = self.config.github.default_branch.clone();
            let gc = self.git_client.clone();

            tokio::task::spawn_blocking(move || {
                let git_client = gc.lock().unwrap();
                git_client.push("origin", &branch)
            })
            .await
            .context("push task panicked")?
            .with_context(|| format!("failed to push Git commit for SVN r{}", rev))?;

            info!(rev, sha = %sha_str, "pushed to origin");

            // 10. Record the mapping and advance the watermark.
            self.db
                .insert_commit_map(
                    rev,
                    &sha_str,
                    "svn_to_git",
                    &entry.author,
                    &format!(
                        "{} <{}>",
                        self.config.developer.name, self.config.developer.email
                    ),
                )
                .with_context(|| format!("failed to insert commit_map for SVN r{}", rev))?;

            self.advance_watermark(rev)?;

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
    /// Delegates to the shared core helper (reserved VCS metadata is preserved).
    fn remove_stale_files(src: &Path, dst: &Path) -> Result<()> {
        remove_stale_files_shared(src, dst)
    }
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

        SvnToGitSync::remove_stale_files(src.path(), dst.path()).unwrap();

        assert!(dst.path().join("keep.txt").exists());
        assert!(!dst.path().join("stale.txt").exists());
        // .git must be preserved (root dotdir).
        assert!(dst.path().join(".git/HEAD").exists());
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
}
