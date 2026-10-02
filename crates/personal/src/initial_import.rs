//! Initial SVN→Git import for personal branch mode.
//!
//! Supports two modes:
//! - **Snapshot**: Export SVN HEAD as a single Git commit.
//! - **Full history**: Replay all SVN revisions as individual Git commits.

use std::sync::Arc;

use anyhow::{Context, Result};
use std::sync::Mutex;
use tracing::{debug, error, info, warn};

use reposync_core::db::Database;
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::github::GitHubClient;
use reposync_core::git::GitClient;
use reposync_core::identity::mapper::IdentityMapper;
use reposync_core::personal_config::PersonalConfig;
use reposync_core::svn::SvnClient;

use crate::commit_format::CommitFormatter;
use crate::svn_to_git::SvnToGitSync;

/// Import mode selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// Export HEAD only — one Git commit with all current files.
    Snapshot,
    /// Replay every SVN revision as an individual Git commit.
    Full,
}

/// Handles the initial import of SVN history into Git.
pub struct InitialImport<'a> {
    pub svn_client: &'a SvnClient,
    pub git_client: &'a Arc<Mutex<GitClient>>,
    pub github_client: &'a GitHubClient,
    pub db: &'a Database,
    pub config: &'a PersonalConfig,
    pub formatter: &'a CommitFormatter,
}

impl<'a> InitialImport<'a> {
    /// Run the import.
    ///
    /// Returns the number of commits created.
    pub async fn import(&self, mode: ImportMode) -> Result<u64> {
        // LFS preflight: if LFS is configured, verify git-lfs is available.
        if self.config.options.lfs_threshold > 0 {
            match reposync_core::lfs::preflight_check() {
                Ok(version) => {
                    info!(
                        version = %version,
                        lfs_threshold = self.config.options.lfs_threshold,
                        "Git LFS preflight passed"
                    );
                }
                Err(e) => {
                    anyhow::bail!(
                        "Git LFS is configured (lfs_threshold = {}) but git-lfs is not available: {}",
                        self.config.options.lfs_threshold,
                        e
                    );
                }
            }
        }

        // Ensure the GitHub repo exists (auto-create if configured)
        self.ensure_github_repo().await?;

        match mode {
            ImportMode::Snapshot => self.import_snapshot().await,
            ImportMode::Full => self.import_full().await,
        }
    }

    /// Auto-create the GitHub repo if it doesn't exist and auto_create is enabled.
    async fn ensure_github_repo(&self) -> Result<()> {
        let repo = &self.config.github.repo;

        let exists = self
            .github_client
            .repo_exists(repo)
            .await
            .context("failed to check if GitHub repo exists")?;

        if exists {
            info!(repo, "GitHub repository already exists");
            return Ok(());
        }

        if !self.config.github.auto_create {
            anyhow::bail!(
                "GitHub repository '{}' does not exist and auto_create is disabled",
                repo
            );
        }

        // Extract repo name from "owner/name" format
        let name = repo
            .split('/')
            .nth(1)
            .context("invalid repo format, expected 'owner/repo'")?;

        info!(
            repo,
            private = self.config.github.private,
            "creating GitHub repository"
        );
        self.github_client
            .create_repo(
                name,
                self.config.github.private,
                &format!(
                    "SVN mirror managed by RepoSync (source: {})",
                    self.config.svn.url
                ),
            )
            .await
            .context("failed to create GitHub repository")?;

        info!(repo, "GitHub repository created successfully");
        Ok(())
    }

    /// Snapshot import: pin HEAD once, export that revision, commit, push.
    ///
    /// Reuses the shared core snapshot engine (`resolve_snapshot_pin` +
    /// `materialize_snapshot` + projected-content verify). Team onboarding
    /// uses the same functions; do not add a second snapshot implementation.
    async fn import_snapshot(&self) -> Result<u64> {
        info!("starting snapshot import");

        let pin = reposync_core::snapshot::resolve_snapshot_pin(
            self.svn_client,
            reposync_core::snapshot::SnapshotRevision::Head,
        )
        .await
        .context("failed to pin SVN snapshot revision")?;
        let head_rev = pin.operative_rev;
        info!(
            head_rev,
            uuid = %pin.svn_uuid,
            url = %pin.canonical_url,
            "SVN snapshot pinned"
        );

        // Build file policy from config.
        let policy = FilePolicy::from(&self.config.options);
        if policy.has_constraints() {
            info!(
                max_file_size = policy.max_file_size(),
                ignore_patterns = self.config.options.ignore_patterns.len(),
                lfs_enabled = policy.lfs_enabled(),
                lfs_threshold = policy.lfs_threshold(),
                "file policy active for snapshot import"
            );
        }

        let repo_path = {
            let git_client = self.git_client.lock().unwrap();
            git_client.repo_path().to_path_buf()
        };

        let stats = reposync_core::snapshot::materialize_snapshot(
            self.svn_client,
            &repo_path,
            &policy,
            self.db,
            &pin,
        )
        .await
        .context("failed to materialize pinned SVN snapshot")?;

        if stats.skipped > 0 {
            info!(
                skipped = stats.skipped,
                "snapshot import: files skipped by policy"
            );
        }

        reposync_core::snapshot::verify_projected_snapshot(
            self.svn_client,
            &repo_path,
            &policy,
            self.db,
            &pin,
        )
        .await
        .context("snapshot working tree does not match pinned projection")?;

        if policy.lfs_enabled() {
            reposync_core::lfs::install_lfs_hooks(&repo_path).map_err(|e| {
                anyhow::anyhow!("Git LFS is configured but git lfs install --local failed: {e}")
            })?;
        }

        // Commit
        let message = self.formatter.format_svn_to_git(
            &format!(
                "Initial import from SVN (snapshot at r{})\n\n{}",
                head_rev,
                pin.history_boundary()
            ),
            head_rev,
            &self.config.developer.svn_username,
            &chrono::Utc::now().to_rfc3339(),
        );

        let sha = {
            let git_client = self.git_client.lock().unwrap();
            let oid = if policy.lfs_enabled() {
                git_client.commit_via_cli(
                    &message,
                    &self.config.developer.name,
                    &self.config.developer.email,
                    &self.config.developer.name,
                    &self.config.developer.email,
                )
            } else {
                git_client.commit(
                    &message,
                    &self.config.developer.name,
                    &self.config.developer.email,
                    &self.config.developer.name,
                    &self.config.developer.email,
                )
            }
            .context("failed to create initial commit")?;
            oid.to_string()
        };
        info!(sha = %sha, rev = head_rev, "created snapshot commit");

        let projected =
            reposync_core::snapshot::project_snapshot_tree(self.svn_client, &policy, self.db, &pin)
                .await
                .context("failed to re-project snapshot for commit verification")?;
        reposync_core::snapshot::verify_commit_matches_projection(
            projected.path(),
            &repo_path,
            &sha,
            &policy,
        )
        .context("snapshot Git commit does not match pinned SVN projection")?;

        // Push (credentials via remote URL)
        {
            let git_client = self.git_client.lock().unwrap();
            git_client
                .push("origin", &self.config.github.default_branch)
                .context("failed to push to GitHub")?;
        }

        // Record in database
        self.db
            .insert_commit_map(
                head_rev,
                &sha,
                "svn_to_git",
                &self.config.developer.svn_username,
                &format!(
                    "{} <{}>",
                    self.config.developer.name, self.config.developer.email
                ),
            )
            .context("failed to record in commit_map")?;

        self.db
            .set_watermark("svn_rev", &head_rev.to_string())
            .context("failed to set SVN watermark")?;

        self.db
            .set_watermark("git_sha", &sha)
            .context("failed to set Git watermark")?;

        self.db
            .insert_audit_log(
                "import_snapshot",
                Some("svn_to_git"),
                Some(head_rev),
                Some(&sha),
                Some(&self.config.developer.svn_username),
                Some(&format!("Snapshot import from SVN r{}", head_rev)),
                true,
            )
            .ok();

        info!("snapshot import completed successfully");
        Ok(1)
    }

    /// Full history import: replay every SVN revision as a Git commit.
    async fn import_full(&self) -> Result<u64> {
        info!("starting full history import");

        // Build identity mapper if configured — allows preserving original SVN authors.
        let identity_mapper = match &self.config.identity {
            Some(identity_config) => match IdentityMapper::new(identity_config) {
                Ok(mapper) => {
                    info!("identity mapper enabled — original SVN authors will be preserved");
                    Some(mapper)
                }
                Err(e) => {
                    warn!(error = %e, "failed to initialize identity mapper, falling back to developer identity");
                    None
                }
            },
            None => {
                info!("no identity mapping configured — all commits will use developer identity");
                None
            }
        };

        // Build file policy from config.
        let policy = FilePolicy::from(&self.config.options);
        if policy.has_constraints() {
            info!(
                max_file_size = policy.max_file_size(),
                ignore_patterns = self.config.options.ignore_patterns.len(),
                lfs_enabled = policy.lfs_enabled(),
                lfs_threshold = policy.lfs_threshold(),
                "file policy active for full history import"
            );
        }

        // Get SVN HEAD info
        let svn_info = self
            .svn_client
            .info()
            .await
            .context("failed to get SVN info")?;
        let head_rev = svn_info.latest_rev;
        info!(head_rev, "SVN HEAD revision — will import all revisions");

        let mut count = 0u64;

        // Iterate through all revisions
        let log_entries = self
            .svn_client
            .log(1, head_rev)
            .await
            .context("failed to get SVN log")?;

        let repo_path = {
            let git_client_guard = self.git_client.lock().unwrap();
            git_client_guard.repo_path().to_path_buf()
        };

        for entry in &log_entries {
            let rev = entry.revision;

            // Export this revision to a temp directory, then copy with policy.
            let export_dir = match tempfile::tempdir() {
                Ok(d) => d,
                Err(e) => {
                    warn!(rev, error = %e, "failed to create temp dir, skipping revision");
                    continue;
                }
            };

            if let Err(e) = self.svn_client.export("", rev, export_dir.path()).await {
                warn!(rev, error = %e, "failed to export revision, skipping");
                continue;
            }

            // Copy with policy enforcement — propagate hard I/O errors
            // instead of silently swallowing them.
            let skipped = match SvnToGitSync::copy_tree_with_policy(
                export_dir.path(),
                &repo_path,
                &policy,
                self.db,
            ) {
                Ok(s) => s,
                Err(e) => {
                    error!(rev, error = %e, "policy copy failed for revision");
                    let _ = self.db.insert_audit_log(
                        "import_copy_failed",
                        Some("svn_to_git"),
                        Some(rev),
                        None,
                        Some(&self.config.developer.svn_username),
                        Some(&format!("copy_tree_with_policy failed at r{}: {}", rev, e)),
                        false,
                    );
                    anyhow::bail!("copy_tree_with_policy failed for revision r{}: {}", rev, e);
                }
            };

            if skipped > 0 {
                debug!(rev, skipped, "files skipped by policy during import");
            }

            let message =
                self.formatter
                    .format_svn_to_git(&entry.message, rev, &entry.author, &entry.date);

            // Resolve the Git identity for this commit's author.
            let (author_name, author_email) = match &identity_mapper {
                Some(mapper) => match mapper.svn_to_git(&entry.author) {
                    Ok(identity) => {
                        debug!(rev, svn_author = %entry.author, git_name = %identity.name, git_email = %identity.email, "mapped SVN author to Git identity");
                        (identity.name, identity.email)
                    }
                    Err(e) => {
                        debug!(rev, svn_author = %entry.author, error = %e, "identity mapping failed, using developer identity");
                        (
                            self.config.developer.name.clone(),
                            self.config.developer.email.clone(),
                        )
                    }
                },
                None => (
                    self.config.developer.name.clone(),
                    self.config.developer.email.clone(),
                ),
            };

            let git_client = self.git_client.lock().unwrap();
            match git_client.commit(
                &message,
                &author_name,
                &author_email,
                &self.config.developer.name,
                &self.config.developer.email,
            ) {
                Ok(oid) => {
                    let sha = oid.to_string();
                    debug!(rev, sha = %sha, author = %author_name, "committed revision");

                    self.db
                        .insert_commit_map(
                            rev,
                            &sha,
                            "svn_to_git",
                            &entry.author,
                            &format!("{} <{}>", author_name, author_email),
                        )
                        .ok();

                    count += 1;
                }
                Err(e) => {
                    // Empty commits (no file changes) are expected for property-only revisions
                    debug!(rev, error = %e, "commit failed (possibly empty revision)");
                }
            }
            drop(git_client);
        }

        // Push all at once (credentials via remote URL)
        if count > 0 {
            let git_client = self.git_client.lock().unwrap();
            git_client
                .push("origin", &self.config.github.default_branch)
                .context("failed to push to GitHub")?;
            drop(git_client);
        }

        // Set watermarks
        if let Some(last) = log_entries.last() {
            self.db
                .set_watermark("svn_rev", &last.revision.to_string())
                .ok();
        }

        let git_client = self.git_client.lock().unwrap();
        if let Ok(sha) = git_client.get_head_sha() {
            self.db.set_watermark("git_sha", &sha).ok();
        }
        drop(git_client);

        self.db
            .insert_audit_log(
                "import_full",
                Some("svn_to_git"),
                Some(head_rev),
                None,
                Some(&self.config.developer.svn_username),
                Some(&format!(
                    "Full history import: {} commits from {} revisions",
                    count,
                    log_entries.len()
                )),
                true,
            )
            .ok();

        info!(
            count,
            revisions = log_entries.len(),
            "full import completed"
        );
        Ok(count)
    }
}
