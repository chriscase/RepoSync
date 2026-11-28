//! Initial SVN→Git import for personal branch mode.
//!
//! Supports two modes:
//! - **Snapshot**: Export SVN HEAD as a single Git commit.
//! - **Full history**: Replay all SVN revisions as individual Git commits.

use std::sync::Arc;

use anyhow::{Context, Result};
use std::sync::Mutex;
use tracing::{debug, error, info, warn};

use reposync_core::db::{ordinary_writer_windows::OrdinaryWriterWindow, Database};
use reposync_core::file_policy::FilePolicy;
use reposync_core::git::github::GitHubClient;
use reposync_core::git::GitClient;
use reposync_core::identity::mapper::IdentityMapper;
use reposync_core::personal_config::PersonalConfig;
use reposync_core::svn::SvnClient;

use crate::commit_format::CommitFormatter;
use crate::svn_to_git::SvnToGitSync;

enum AcceptedImportCommit {
    /// A new Git commit was stored.
    New(String),
    /// Qualifying files were already pointer commits and this revision did not
    /// change the tree. The string is the existing HEAD.
    Unchanged(String),
    /// A non-LFS revision produced no commit. Real LFS failures are not skipped.
    Skipped,
}

fn accept_import_commit(
    result: Result<crate::svn_to_git::PersonalRevisionCommit>,
    qualifying_paths: usize,
) -> Result<AcceptedImportCommit> {
    match result {
        Ok(crate::svn_to_git::PersonalRevisionCommit::Created(sha)) => {
            Ok(AcceptedImportCommit::New(sha))
        }
        Ok(crate::svn_to_git::PersonalRevisionCommit::Unchanged(sha)) => {
            Ok(AcceptedImportCommit::Unchanged(sha))
        }
        Err(error) if qualifying_paths == 0 => {
            debug!(error = %error, "commit failed (possibly empty revision)");
            Ok(AcceptedImportCommit::Skipped)
        }
        Err(error) => Err(error).context("qualifying files were not stored as pointer commits"),
    }
}

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
        let window = self
            .db
            .begin_ordinary_writer_window(None)
            .context("Personal initial import writer admission refused")?;
        self.import_with_writer_window(mode, window).await
    }

    /// Continue the caller's preparation window across clone and import. A
    /// fresh admission here would leave a gap or contend with the caller itself.
    pub async fn import_with_writer_window(
        &self,
        mode: ImportMode,
        window: OrdinaryWriterWindow<'_>,
    ) -> Result<u64> {
        window
            .require_engine_scope(self.db, None)
            .context("Personal initial import writer handoff refused")?;
        self.db
            .require_legacy_import_watermarks_available()
            .context("Personal initial import checkpoint admission refused")?;
        // LFS preflight: a threshold or any explicit pattern enables LFS.
        let lfs_policy = FilePolicy::from(&self.config.options);
        if lfs_policy.lfs_enabled() {
            match reposync_core::lfs::preflight_check() {
                Ok(version) => {
                    info!(
                        version = %version,
                        lfs_threshold = lfs_policy.lfs_threshold(),
                        lfs_patterns = self.config.options.lfs_patterns.len(),
                        "Git LFS preflight passed"
                    );
                }
                Err(e) => {
                    anyhow::bail!(
                        "Git LFS is enabled (threshold {}, {} pattern(s)) but git-lfs is not available: {}",
                        lfs_policy.lfs_threshold(),
                        self.config.options.lfs_patterns.len(),
                        e
                    );
                }
            }
        }

        // Ensure the GitHub repo exists (auto-create if configured)
        self.ensure_github_repo().await?;

        let count = match mode {
            ImportMode::Snapshot => self.import_snapshot(&window).await?,
            ImportMode::Full => self.import_full(&window).await?,
        };
        window
            .finish_preparation()
            .context("Personal initial import writer completion refused")?;
        Ok(count)
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
    async fn import_snapshot(
        &self,
        window: &reposync_core::db::ordinary_writer_windows::OrdinaryWriterWindow<'_>,
    ) -> Result<u64> {
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
        self.db.require_legacy_import_watermarks_available()?;
        window.mark_unjournalled_publication_possible()?;
        {
            let git_client = self.git_client.lock().unwrap();
            git_client
                .push("origin", &self.config.github.default_branch)
                .context("failed to push to GitHub")?;
        }

        reposync_core::import::confirm_personal_snapshot_import_publication(
            window,
            reposync_core::import::LegacyImportRef {
                workdir: &repo_path,
                remote: "origin",
                branch: &self.config.github.default_branch,
                token: self.config.github.token.as_deref(),
                sha: &sha,
            },
            head_rev,
            &self.config.developer.svn_username,
            &format!(
                "{} <{}>",
                self.config.developer.name, self.config.developer.email
            ),
        )
        .await
        .context("failed to checkpoint personal snapshot import")?;

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
    async fn import_full(
        &self,
        window: &reposync_core::db::ordinary_writer_windows::OrdinaryWriterWindow<'_>,
    ) -> Result<u64> {
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

            if self
                .db
                .is_personal_svn_rev_synced(rev)
                .context("failed to check personal SVN revision sync state")?
            {
                debug!(
                    rev,
                    "SVN revision already mapped in personal scope, skipping"
                );
                continue;
            }

            // Export this revision to a temp directory, then copy with policy.
            let export_dir = tempfile::tempdir()
                .with_context(|| format!("failed to create temp dir for SVN revision r{}", rev))?;

            self.svn_client
                .export("", rev, export_dir.path())
                .await
                .with_context(|| format!("failed to export SVN revision r{}", rev))?;

            // Copy with policy enforcement — propagate hard I/O errors
            // instead of silently swallowing them.
            let copy_stats = match SvnToGitSync::copy_tree_with_policy(
                export_dir.path(),
                &repo_path,
                &policy,
                self.db,
            ) {
                Ok(stats) => stats,
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

            if copy_stats.skipped > 0 {
                debug!(
                    rev,
                    skipped = copy_stats.skipped,
                    "files skipped by policy during import"
                );
            }

            // This consumer is separate from the ordinary synchronization
            // loop. A copy alone leaves paths deleted by later revisions in
            // the workdir, so reconcile and independently verify every full
            // history revision before its commit or mapping can be accepted.
            reposync_core::import::remove_stale_files(export_dir.path(), &repo_path)
                .with_context(|| format!("failed to reconcile stale paths for SVN r{rev}"))?;
            reposync_core::import::verify_export_projection(export_dir.path(), &repo_path, &policy)
                .with_context(|| {
                    format!(
                        "independent full-history projection verification failed for SVN r{rev}"
                    )
                })?;

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
            let committed = crate::svn_to_git::commit_personal_revision(
                &git_client,
                &copy_stats.lfs_paths,
                &message,
                &author_name,
                &author_email,
                &self.config.developer.name,
                &self.config.developer.email,
            );
            match accept_import_commit(committed, copy_stats.lfs_paths.len())
                .with_context(|| format!("LFS import commit failed for r{rev}"))?
            {
                AcceptedImportCommit::New(sha) => {
                    debug!(rev, sha = %sha, author = %author_name, "committed revision");
                    self.db
                        .record_personal_import_revision(
                            rev,
                            &sha,
                            &entry.author,
                            &format!("{} <{}>", author_name, author_email),
                        )
                        .with_context(|| {
                            format!(
                                "failed to record personal import revision mapping for r{}",
                                rev
                            )
                        })?;
                    count += 1;
                }
                AcceptedImportCommit::Unchanged(sha) => {
                    debug!(
                        rev,
                        sha = %sha,
                        "SVN revision did not change the Git tree"
                    );
                    self.db
                        .record_personal_import_revision(
                            rev,
                            &sha,
                            &entry.author,
                            &format!("{} <{}>", author_name, author_email),
                        )
                        .with_context(|| {
                            format!("failed to record unchanged personal import revision r{rev}")
                        })?;
                }
                AcceptedImportCommit::Skipped => {}
            }
            drop(git_client);
        }

        // Push all at once (credentials via remote URL)
        if count > 0 {
            self.db.require_legacy_import_watermarks_available()?;
            window.mark_unjournalled_publication_possible()?;
            let git_client = self.git_client.lock().unwrap();
            git_client
                .push("origin", &self.config.github.default_branch)
                .context("failed to push to GitHub")?;
            drop(git_client);
        }

        // Set watermarks (both must succeed or the import fails closed).
        if let Some(last) = log_entries.last() {
            let head_sha = {
                let git_client = self.git_client.lock().unwrap();
                git_client
                    .get_head_sha()
                    .context("failed to read Git HEAD")?
            };
            if count > 0 {
                reposync_core::import::confirm_personal_full_import_publication(
                    window,
                    reposync_core::import::LegacyImportRef {
                        workdir: &repo_path,
                        remote: "origin",
                        branch: &self.config.github.default_branch,
                        token: self.config.github.token.as_deref(),
                        sha: &head_sha,
                    },
                    last.revision,
                )
                .await?;
            } else {
                self.db
                    .finalize_personal_full_import_watermarks(last.revision, &head_sha)
                    .context("failed to finalize personal full-import watermarks")?;
            }
        }

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

#[cfg(test)]
mod tests {
    use super::{accept_import_commit, AcceptedImportCommit};
    use crate::svn_to_git::PersonalRevisionCommit;

    #[test]
    fn unchanged_lfs_import_is_recorded_and_real_failures_stay_closed() {
        let unchanged =
            accept_import_commit(Ok(PersonalRevisionCommit::Unchanged("abc".into())), 1).unwrap();
        assert!(matches!(unchanged, AcceptedImportCommit::Unchanged(sha) if sha == "abc"));

        let created =
            accept_import_commit(Ok(PersonalRevisionCommit::Created("def".into())), 1).unwrap();
        assert!(matches!(created, AcceptedImportCommit::New(sha) if sha == "def"));

        let fatal = accept_import_commit(
            Err(anyhow::anyhow!("git lfs smudge failed for model.bin")),
            1,
        );
        assert!(fatal.is_err(), "filter and object errors stay closed");

        let empty = accept_import_commit(Err(anyhow::anyhow!("nothing to commit")), 0).unwrap();
        assert!(matches!(empty, AcceptedImportCommit::Skipped));
    }

    /// Full import of an LFS asset, a property-only revision, then an ordinary
    /// file. The property revision is recorded against the existing pointer
    /// commit and does not abort the import.
    #[tokio::test]
    async fn full_import_property_only_revision_keeps_the_pointer_commit() {
        let svn_ok = std::process::Command::new("svn")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        let svnadmin_ok = std::process::Command::new("svnadmin")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !svn_ok || !svnadmin_ok {
            eprintln!("SKIP: svn/svnadmin not available");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("svn_repo");
        assert!(std::process::Command::new("svnadmin")
            .args(["create", repo_dir.to_str().unwrap()])
            .status()
            .unwrap()
            .success());
        let svn_url = format!("file://{}", repo_dir.display());
        let svn_wc = dir.path().join("svn_wc");
        assert!(std::process::Command::new("svn")
            .args([
                "checkout",
                &svn_url,
                svn_wc.to_str().unwrap(),
                "--non-interactive",
            ])
            .status()
            .unwrap()
            .success());
        let asset = svn_wc.join("asset.bin");
        std::fs::write(&asset, "Q".repeat(80)).unwrap();
        assert!(std::process::Command::new("svn")
            .args(["add", asset.to_str().unwrap()])
            .status()
            .unwrap()
            .success());
        let added = std::process::Command::new("svn")
            .args([
                "commit",
                "-m",
                "add asset",
                svn_wc.to_str().unwrap(),
                "--non-interactive",
            ])
            .output()
            .unwrap();
        assert!(
            added.status.success(),
            "svn add commit failed: {}",
            String::from_utf8_lossy(&added.stderr)
        );
        let prop = std::process::Command::new("svn")
            .args([
                "propset",
                "reposync:note",
                "marker",
                asset.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            prop.status.success(),
            "svn propset failed: {}",
            String::from_utf8_lossy(&prop.stderr)
        );
        let property_commit = std::process::Command::new("svn")
            .args([
                "commit",
                "-m",
                "property only",
                svn_wc.to_str().unwrap(),
                "--non-interactive",
            ])
            .output()
            .unwrap();
        assert!(
            property_commit.status.success(),
            "property commit failed: {}",
            String::from_utf8_lossy(&property_commit.stderr)
        );
        let note = svn_wc.join("note.txt");
        std::fs::write(&note, "after").unwrap();
        assert!(std::process::Command::new("svn")
            .args(["add", note.to_str().unwrap()])
            .status()
            .unwrap()
            .success());
        let note_commit = std::process::Command::new("svn")
            .args([
                "commit",
                "-m",
                "add note",
                svn_wc.to_str().unwrap(),
                "--non-interactive",
            ])
            .output()
            .unwrap();
        assert!(
            note_commit.status.success(),
            "note commit failed: {}",
            String::from_utf8_lossy(&note_commit.stderr)
        );

        let git_bare = dir.path().join("git_bare");
        let git_work = dir.path().join("git_work");
        git2::Repository::init_bare(&git_bare).unwrap();
        let git = reposync_core::git::GitClient::init(&git_work).unwrap();
        git.ensure_head_on_branch("main").unwrap();
        git2::Repository::open(&git_work)
            .unwrap()
            .remote("origin", git_bare.to_str().unwrap())
            .unwrap();

        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let db = reposync_core::db::Database::in_memory().unwrap();
        db.initialize().unwrap();
        let config: reposync_core::personal_config::PersonalConfig =
            serde_json::from_value(serde_json::json!({
                "personal": {"data_dir": data_dir},
                "svn": {"url": svn_url, "username": "fixture", "password_env": "UNUSED"},
                "github": {
                    "api_url": "http://127.0.0.1:1",
                    "repo": "fixture/repo",
                    "token_env": "UNUSED",
                    "default_branch": "main",
                    "auto_create": false,
                    "private": true
                },
                "developer": {
                    "name": "Fixture",
                    "email": "fixture@example.com",
                    "svn_username": "fixture"
                },
                "options": {"lfs_threshold": 40}
            }))
            .unwrap();
        let formatter = crate::commit_format::CommitFormatter::new(&config.commit_format);
        let svn = reposync_core::svn::SvnClient::new(&config.svn.url, "fixture", "");
        let github = reposync_core::git::github::GitHubClient::new(
            &config.github.api_url,
            "",
            reposync_core::config::GitProvider::GitHub,
        );
        let git = std::sync::Arc::new(std::sync::Mutex::new(git));
        let importer = super::InitialImport {
            svn_client: &svn,
            git_client: &git,
            github_client: &github,
            db: &db,
            config: &config,
            formatter: &formatter,
        };
        let window = db.begin_ordinary_writer_window(None).unwrap();
        let count = importer
            .import_full(&window)
            .await
            .expect("full import must accept the property-only revision");
        assert_eq!(count, 2, "r1 and r3 create commits; r2 does not");
        for rev in 1..=3 {
            assert!(
                db.is_personal_svn_rev_synced(rev).unwrap(),
                "r{rev} must have an import receipt"
            );
        }
        assert_eq!(db.get_watermark("svn_rev").unwrap().as_deref(), Some("3"));
        let mapped = db.list_commit_map(10).unwrap();
        let sha_for = |rev: i64| {
            mapped
                .iter()
                .find(|entry| entry.svn_rev == rev)
                .map(|entry| entry.git_sha.clone())
                .unwrap_or_else(|| panic!("missing commit map for r{rev}"))
        };
        assert_eq!(
            sha_for(1),
            sha_for(2),
            "property-only revision must reuse the existing pointer commit"
        );
        assert_ne!(sha_for(2), sha_for(3));
        let blob = std::process::Command::new("git")
            .args([
                "-C",
                git_work.to_str().unwrap(),
                "cat-file",
                "-p",
                "HEAD:asset.bin",
            ])
            .output()
            .unwrap();
        assert!(
            reposync_core::lfs::is_lfs_pointer(&blob.stdout),
            "imported asset must stay a pointer, got {}",
            String::from_utf8_lossy(&blob.stdout)
        );
        let note_blob = std::process::Command::new("git")
            .args([
                "-C",
                git_work.to_str().unwrap(),
                "cat-file",
                "-p",
                "HEAD:note.txt",
            ])
            .output()
            .unwrap();
        assert_eq!(note_blob.stdout, b"after");
        let failed = db
            .list_audit_log_by_action("lfs_resolution_failed", 10)
            .unwrap();
        assert!(
            failed.is_empty(),
            "property-only import must not fabricate an LFS failure"
        );
    }
}
