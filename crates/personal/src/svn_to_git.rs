//! SVN-to-Git sync engine for personal branch mode.
//!
//! Polls SVN for new revisions beyond the stored watermark and replays each
//! revision as a Git commit with proper author identity and metadata trailers.
//! Echo suppression prevents re-syncing commits that originated from the
//! Git side (identified by the `[reposync]` marker in the commit message).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use reposync_core::db::Database;
use reposync_core::file_policy::{FilePolicy, FilePolicyDecision};
use reposync_core::git::GitClient;
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
            let git_client = self.git_client.lock().unwrap();
            let repo_path = git_client.repo_path().to_path_buf();
            drop(git_client); // Release lock before blocking I/O.

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

    /// Recursively copy all files from `src` into `dst`, overwriting existing
    /// files. Directories that exist in `dst` are preserved; new directories
    /// are created. Hidden files and directories (starting with `.`) in the
    /// destination root are skipped to avoid clobbering `.git/`.
    ///
    /// This is the policy-unaware version, retained for tests.
    #[cfg(test)]
    fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
        let noop_policy = FilePolicy::new(0, vec![]);
        Self::copy_tree_policy_inner(src, dst, dst, src, true, &noop_policy, &mut 0)
    }

    /// Recursively copy files from `src` into `dst`, enforcing `FilePolicy`.
    /// Returns the number of files skipped by policy.
    pub fn copy_tree_with_policy(
        src: &Path,
        dst: &Path,
        policy: &FilePolicy,
        db: &Database,
    ) -> Result<usize> {
        let mut skipped = 0;
        Self::copy_tree_policy_inner(src, dst, dst, src, true, policy, &mut skipped)?;

        // Audit skipped count if any.
        if skipped > 0 {
            let _ = db.insert_audit_log(
                "file_policy_skip",
                Some("svn_to_git"),
                None,
                None,
                None,
                Some(&format!(
                    "Skipped {} files by policy during SVN→Git copy",
                    skipped
                )),
                true,
            );
        }

        Ok(skipped)
    }

    fn copy_tree_policy_inner(
        src: &Path,
        dst: &Path,
        dst_root: &Path,
        export_root: &Path,
        is_root: bool,
        policy: &FilePolicy,
        skipped: &mut usize,
    ) -> Result<()> {
        let entries = std::fs::read_dir(src)
            .with_context(|| format!("failed to read directory: {}", src.display()))?;

        for entry in entries {
            let entry = entry?;
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();

            // At the root level of the destination, skip dotfiles/dotdirs to
            // avoid overwriting `.git/` and similar metadata.
            if is_root && name_str.starts_with('.') {
                debug!(name = %name_str, "skipping dotfile/dotdir in export root");
                continue;
            }

            let src_path = entry.path();
            let dst_path = dst.join(&file_name);

            if src_path.is_dir() {
                // Check if the entire directory matches an ignore pattern.
                let rel = src_path
                    .strip_prefix(export_root)
                    .unwrap_or(&src_path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let dir_pattern_path = format!("{}/", rel);
                // Quick check: does the dir itself match? Some patterns like
                // "build/**" should still recurse into `build/` to find matches.
                // We check individual files below.
                if !dst_path.exists() {
                    std::fs::create_dir_all(&dst_path).with_context(|| {
                        format!("failed to create directory: {}", dst_path.display())
                    })?;
                }
                let _ = dir_pattern_path; // Used for potential future directory-level skipping.
                Self::copy_tree_policy_inner(
                    &src_path,
                    &dst_path,
                    dst_root,
                    export_root,
                    false,
                    policy,
                    skipped,
                )?;
            } else {
                // Compute relative path for policy evaluation.
                let rel = src_path
                    .strip_prefix(export_root)
                    .unwrap_or(&src_path)
                    .to_string_lossy()
                    .replace('\\', "/");

                let decision = policy.evaluate_path(export_root, &rel);
                match &decision {
                    FilePolicyDecision::Allow => {
                        std::fs::copy(&src_path, &dst_path).with_context(|| {
                            format!(
                                "failed to copy {} -> {}",
                                src_path.display(),
                                dst_path.display()
                            )
                        })?;
                    }
                    FilePolicyDecision::LfsTrack { size, threshold } => {
                        // Copy the actual file content to the Git working tree.
                        // With `.gitattributes` set up, `git add` will run the
                        // LFS clean filter and store the blob in LFS storage,
                        // replacing the file content with a pointer in the index.
                        std::fs::copy(&src_path, &dst_path).with_context(|| {
                            format!(
                                "failed to copy {} -> {}",
                                src_path.display(),
                                dst_path.display()
                            )
                        })?;

                        // Ensure `.gitattributes` has the appropriate LFS tracking pattern.
                        // Use dst_root (the Git repo root) for .gitattributes placement.
                        let pattern = reposync_core::lfs::pattern_for_path(&rel);
                        if let Err(e) = reposync_core::lfs::ensure_lfs_tracked(dst_root, &pattern)
                        {
                            warn!(
                                path = rel.as_str(),
                                pattern = pattern.as_str(),
                                error = %e,
                                "failed to update .gitattributes for LFS tracking"
                            );
                        } else {
                            info!(
                                path = rel.as_str(),
                                size,
                                threshold,
                                pattern = pattern.as_str(),
                                "LFS: file copied and .gitattributes updated"
                            );
                        }
                    }
                    FilePolicyDecision::Ignored { pattern } => {
                        warn!(
                            path = rel.as_str(),
                            pattern = pattern.as_str(),
                            "file ignored by policy — not copied to Git"
                        );
                        *skipped += 1;
                    }
                    FilePolicyDecision::Oversize { size, limit } => {
                        warn!(
                            path = rel.as_str(),
                            size, limit, "file exceeds max_file_size — not copied to Git"
                        );
                        *skipped += 1;
                    }
                }
            }
        }

        Ok(())
    }

    /// Remove files and directories from `dst` (Git working tree) that do not
    /// exist in `src` (SVN export). Hidden entries (starting with `.`) at the
    /// root level are always skipped so `.git/` is never touched.
    fn remove_stale_files(src: &Path, dst: &Path) -> Result<()> {
        Self::remove_stale_inner(src, dst, true)
    }

    fn remove_stale_inner(src: &Path, dst: &Path, is_root: bool) -> Result<()> {
        let entries = match std::fs::read_dir(dst) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("failed to read directory: {}", dst.display()));
            }
        };

        for entry in entries {
            let entry = entry?;
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();

            // Skip dotfiles/dotdirs at root to protect .git/.
            if is_root && name_str.starts_with('.') {
                continue;
            }

            let src_path = src.join(&file_name);
            let dst_path = entry.path();

            if dst_path.is_dir() {
                if src_path.is_dir() {
                    // Both exist — recurse.
                    Self::remove_stale_inner(&src_path, &dst_path, false)?;
                } else {
                    // Directory exists in Git but not in SVN export — remove it.
                    std::fs::remove_dir_all(&dst_path).with_context(|| {
                        format!("failed to remove stale directory: {}", dst_path.display())
                    })?;
                    debug!(path = %dst_path.display(), "removed stale directory");
                }
            } else if !src_path.exists() {
                // File exists in Git but not in SVN export — remove it.
                std::fs::remove_file(&dst_path).with_context(|| {
                    format!("failed to remove stale file: {}", dst_path.display())
                })?;
                debug!(path = %dst_path.display(), "removed stale file");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_copy_tree_skips_dotfiles_at_root() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        // Create a dotdir and a normal file in src.
        std::fs::create_dir(src.path().join(".svn")).unwrap();
        std::fs::write(src.path().join(".svn/entries"), "data").unwrap();
        std::fs::write(src.path().join("hello.txt"), "world").unwrap();
        std::fs::create_dir(src.path().join("subdir")).unwrap();
        std::fs::write(src.path().join("subdir/.hidden"), "secret").unwrap();
        std::fs::write(src.path().join("subdir/visible.txt"), "content").unwrap();

        // Create a .git dir in dst that should not be touched.
        std::fs::create_dir(dst.path().join(".git")).unwrap();
        std::fs::write(dst.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();

        SvnToGitSync::copy_tree(src.path(), dst.path()).unwrap();

        // .git should be untouched.
        assert_eq!(
            std::fs::read_to_string(dst.path().join(".git/HEAD")).unwrap(),
            "ref: refs/heads/main"
        );
        // .svn should NOT have been copied.
        assert!(!dst.path().join(".svn").exists());

        // Normal file should be present.
        assert_eq!(
            std::fs::read_to_string(dst.path().join("hello.txt")).unwrap(),
            "world"
        );

        // Nested dotfiles SHOULD be copied (only root-level dots are skipped).
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
