//! Local Git repository operations via `git2`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use git2::{
    build::RepoBuilder, BranchType, Cred, CredentialType, FetchOptions, IndexAddOption, Oid,
    RemoteCallbacks, Repository, Signature,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, instrument, warn};

use crate::errors::GitError;

/// Legacy plaintext sidecar from earlier builds (must not be used for auth).
const LEGACY_CLEAN_HTTP_TOKEN_REL: &str = "reposync/clean-http-token";

fn purge_legacy_clean_http_token_sidecar(repo: &Repository) {
    let path = repo.path().join(LEGACY_CLEAN_HTTP_TOKEN_REL);
    if path.is_file() {
        let _ = std::fs::remove_file(path);
    }
}

/// High-level Git client wrapping a `git2::Repository`.
pub struct GitClient {
    repo: Repository,
    repo_path: PathBuf,
    /// HTTP(S) token for git CLI / libgit2 callbacks (never written into `.git/config`).
    http_auth_token: RefCell<Option<String>>,
}

/// Information about a single Git commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommitInfo {
    pub sha: String,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub committer_name: String,
    pub committer_email: String,
}

/// One path changed in a Git commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitChangedPath {
    pub action: String,
    pub path: String,
    /// Source path when `action` is `R` (rename/move).
    pub rename_from: Option<String>,
}

/// Result of capped pending-commit selection on the P→R frontier.
#[derive(Debug, Clone)]
pub struct PendingCommitSelection {
    /// Oldest-first replay batch, at most the requested cap.
    pub commits: Vec<GitCommitInfo>,
    /// Total pending commits on the frontier (may exceed `commits.len()`).
    pub total: usize,
    /// True when another replay batch is required after this one.
    pub has_more: bool,
    /// Checkout target for this batch: last replay commit, or `tip_sha` when empty/done.
    pub batch_tip: String,
}

impl GitClient {
    /// Open an existing Git repository at `repo_path`.
    pub fn new<P: AsRef<Path>>(repo_path: P) -> Result<Self, GitError> {
        let path = repo_path.as_ref();
        info!(path = %path.display(), "opening git repository");
        let repo = Repository::open(path)
            .map_err(|_| GitError::RepositoryNotFound(path.display().to_string()))?;
        purge_legacy_clean_http_token_sidecar(&repo);
        Ok(Self {
            repo,
            repo_path: path.to_path_buf(),
            http_auth_token: RefCell::new(None),
        })
    }

    /// Initialize a new empty Git repository at `repo_path`.
    pub fn init<P: AsRef<Path>>(repo_path: P) -> Result<Self, GitError> {
        let path = repo_path.as_ref();
        info!(path = %path.display(), "initializing new git repository");
        let repo = Repository::init(path)?;
        Ok(Self {
            repo,
            repo_path: path.to_path_buf(),
            http_auth_token: RefCell::new(None),
        })
    }

    /// Clone a remote repository to `path`.
    ///
    /// HTTP(S) tokens are supplied via libgit2 credential callbacks; the stored
    /// `remote.origin.url` remains the clean URL without embedded credentials.
    #[instrument(skip(token), fields(url = %url, path = %path.display()))]
    pub fn clone_repo(url: &str, path: &Path, token: Option<&str>) -> Result<Self, GitError> {
        info!("cloning git repository");
        let clean_url = Self::strip_http_credentials(url).unwrap_or_else(|| url.to_string());
        let mut builder = RepoBuilder::new();
        if let Some(tok) =
            token.filter(|_| clean_url.starts_with("https://") || clean_url.starts_with("http://"))
        {
            let tok = tok.to_string();
            let mut callbacks = RemoteCallbacks::new();
            callbacks.credentials(move |_url, _username_from_url, allowed| {
                if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
                    Cred::userpass_plaintext("x-access-token", &tok)
                } else {
                    Err(git2::Error::from_str("unsupported git credential type"))
                }
            });
            let mut fetch_options = FetchOptions::new();
            fetch_options.remote_callbacks(callbacks);
            builder.fetch_options(fetch_options);
        }
        let repo = builder.clone(&clean_url, path)?;
        info!("clone completed");
        let client = Self {
            repo,
            repo_path: path.to_path_buf(),
            http_auth_token: RefCell::new(token.map(str::to_string)),
        };
        client.migrate_remote_url_clean("origin")?;
        if let Some(tok) =
            token.filter(|_| clean_url.starts_with("https://") || clean_url.starts_with("http://"))
        {
            super::startup_handoff::remember_startup_http_auth(path, tok);
        }
        Ok(client)
    }

    fn migrate_remote_url_clean(&self, remote_name: &str) -> Result<(), GitError> {
        let remote = self.repo.find_remote(remote_name)?;
        let Some(url) = remote.url() else {
            return Ok(());
        };
        if let Some(clean_url) = Self::strip_http_credentials(url) {
            if url != clean_url {
                info!("stripping embedded credentials from remote URL");
                self.repo.remote_set_url(remote_name, &clean_url)?;
            }
        }
        Ok(())
    }

    fn resolve_http_token(&self, explicit: Option<&str>) -> Option<String> {
        explicit
            .map(str::to_string)
            .or_else(|| self.http_auth_token.borrow().clone())
    }

    fn apply_cli_http_auth(&self, cmd: &mut std::process::Command, token: Option<&str>) {
        super::subprocess_auth::apply_git_http_auth_optional(
            cmd,
            self.resolve_http_token(token).as_deref(),
        );
    }

    /// Ensure the local HEAD points at `refs/heads/<branch>`. Useful after
    /// cloning an empty remote where libgit2 may set HEAD to a default
    /// branch name that doesn't match the configured `git_branch`.
    pub fn ensure_head_on_branch(&self, branch: &str) -> Result<(), GitError> {
        let target_ref = format!("refs/heads/{}", branch);
        // If HEAD is already pointing at the right symbolic ref, do nothing.
        if let Ok(head) = self.repo.head() {
            if head.name() == Some(target_ref.as_str()) {
                return Ok(());
            }
        }
        // Set HEAD as a symbolic reference (it's fine if the target doesn't
        // exist yet — that's what "unborn" HEAD means, and the first commit
        // will materialize it).
        self.repo.reference_symbolic(
            "HEAD",
            &target_ref,
            true,
            "reposync: align HEAD with configured branch",
        )?;
        info!(branch, "aligned HEAD to refs/heads/{}", branch);
        Ok(())
    }

    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    /// HTTP(S) token held for Git CLI subprocess auth (not embedded in remote URLs on clean paths).
    pub fn stored_http_auth_token(&self) -> Option<String> {
        self.http_auth_token.borrow().clone()
    }

    /// Get the current HEAD commit SHA.
    pub fn head_sha(&self) -> Result<String, GitError> {
        let head = self.repo.head().map_err(GitError::from)?;
        let oid = head.peel_to_commit().map_err(GitError::from)?.id();
        Ok(oid.to_string())
    }

    /// Hard-reset HEAD to a specific commit SHA.
    /// Used to roll back failed pushes so bad commits don't accumulate.
    pub fn reset_hard(&self, sha: &str) -> Result<(), GitError> {
        let oid = git2::Oid::from_str(sha).map_err(GitError::Git2Error)?;
        let commit = self.repo.find_commit(oid).map_err(GitError::from)?;
        self.repo
            .reset(commit.as_object(), git2::ResetType::Hard, None)
            .map_err(GitError::from)?;
        info!(sha, "git reset --hard completed");
        Ok(())
    }

    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    fn strip_http_credentials(url: &str) -> Option<String> {
        let (scheme, rest) = url
            .strip_prefix("https://")
            .map(|r| ("https://", r))
            .or_else(|| url.strip_prefix("http://").map(|r| ("http://", r)))?;

        let hostpath = if let Some(at_pos) = rest.find('@') {
            let slash_pos = rest.find('/').unwrap_or(rest.len());
            if at_pos < slash_pos {
                &rest[at_pos + 1..]
            } else {
                rest
            }
        } else {
            rest
        };

        Some(format!("{scheme}{hostpath}"))
    }

    /// Keep the remote URL clean and store HTTP(S) tokens for CLI/callback auth.
    ///
    /// Strips legacy credentialed URLs. Absence of `token` means "no new token",
    /// not revoke — use [`Self::clear_remote_credentials`].
    pub fn ensure_remote_credentials(
        &self,
        remote_name: &str,
        token: Option<&str>,
    ) -> Result<(), GitError> {
        self.migrate_remote_url_clean(remote_name)?;
        if let Some(tok) = token {
            *self.http_auth_token.borrow_mut() = Some(tok.to_string());
            if let Some(workdir) = self.repo.workdir() {
                super::startup_handoff::remember_startup_http_auth(workdir, tok);
            } else {
                super::startup_handoff::remember_startup_http_auth(&self.repo_path, tok);
            }
        }
        Ok(())
    }

    /// Embed HTTP(S) credentials in the remote URL (scheduler/sync reload path).
    pub fn ensure_remote_credentials_embedded(
        &self,
        remote_name: &str,
        token: Option<&str>,
    ) -> Result<(), GitError> {
        let Some(tok) = token else {
            return Ok(());
        };
        let remote = self.repo.find_remote(remote_name)?;
        let Some(url) = remote.url() else {
            return Ok(());
        };
        let Some(clean_url) = Self::strip_http_credentials(url) else {
            return Ok(());
        };
        let new_url = if let Some(hostpath) = clean_url.strip_prefix("https://") {
            format!("https://x-access-token:{tok}@{hostpath}")
        } else if let Some(hostpath) = clean_url.strip_prefix("http://") {
            format!("http://x-access-token:{tok}@{hostpath}")
        } else {
            return Ok(());
        };
        if url != new_url {
            info!("updating remote URL to embed fresh credentials");
            self.repo.remote_set_url(remote_name, &new_url)?;
        }
        *self.http_auth_token.borrow_mut() = Some(tok.to_string());
        Ok(())
    }

    /// Store HTTP(S) auth in memory only (no remote mutation).
    pub fn set_in_memory_http_auth(&self, token: &str) {
        *self.http_auth_token.borrow_mut() = Some(token.to_string());
    }

    /// Drop in-memory HTTP auth and startup handoff without changing remotes.
    pub fn clear_in_memory_http_auth(&self, _remote_name: &str) -> Result<(), GitError> {
        self.http_auth_token.borrow_mut().take();
        let workdir = self
            .repo
            .workdir()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.repo_path.clone());
        super::startup_handoff::forget_startup_http_auth(&workdir);
        purge_legacy_clean_http_token_sidecar(&self.repo);
        Ok(())
    }

    /// Clear in-memory HTTP auth and strip legacy credentialed remote URLs.
    pub fn clear_http_auth_memory(&self, remote_name: &str) -> Result<(), GitError> {
        self.clear_in_memory_http_auth(remote_name)?;
        self.migrate_remote_url_clean(remote_name)?;
        Ok(())
    }

    /// Remove embedded HTTP(S) credentials from a remote URL.
    pub fn clear_remote_credentials(&self, remote_name: &str) -> Result<(), GitError> {
        self.clear_http_auth_memory(remote_name)
    }

    /// Fetch from a named remote with a 5-minute timeout.
    #[instrument(skip(self, token))]
    pub fn fetch(&self, remote_name: &str, token: Option<&str>) -> Result<(), GitError> {
        info!(remote = remote_name, "fetching via git CLI");
        let repo_path = self.repo.workdir().unwrap_or_else(|| self.repo.path());

        // Use git CLI for fetch — libgit2's HTTP client fails with 403 on
        // GitHub Enterprise in some configurations, and doesn't support LFS
        // filter smudge on fetched refs. HTTP(S) auth uses GIT_CONFIG env only.
        let mut cmd = std::process::Command::new("git");
        cmd.args(["fetch", remote_name, "--prune", "--quiet"])
            .current_dir(repo_path);
        self.apply_cli_http_auth(&mut cmd, token);
        let output = cmd.output().map_err(GitError::IoError)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GitError::Git2Error(git2::Error::from_str(&format!(
                "git fetch failed: {}",
                stderr.trim()
            ))));
        }
        debug!("fetch completed");
        Ok(())
    }

    /// Fetch and fast-forward merge.
    ///
    /// Gracefully handles two empty-repo scenarios:
    /// 1. Remote has no branches yet (brand-new empty Git repo) — just
    ///    fetches and returns; nothing to merge.
    /// 2. Local HEAD doesn't point to a branch yet (fresh clone of empty
    ///    repo) — set HEAD to the fetched ref so subsequent commits land
    ///    on the right branch.
    #[instrument(skip(self, token))]
    pub fn pull(
        &self,
        remote_name: &str,
        branch: &str,
        token: Option<&str>,
    ) -> Result<(), GitError> {
        self.fetch(remote_name, token)?;

        // Use git CLI for the reset/checkout step instead of libgit2.
        // libgit2's checkout_head does not support Git LFS smudge filters,
        // causing "failed to read file into stream" for any LFS-tracked file.
        let repo_path = self.repo.workdir().unwrap_or_else(|| self.repo.path());
        let remote_ref = format!("{}/{}", remote_name, branch);

        let output = std::process::Command::new("git")
            .args(["reset", "--hard", &remote_ref])
            .current_dir(repo_path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(GitError::IoError)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // If the remote ref doesn't exist yet (empty remote), that's OK
            if stderr.contains("unknown revision") || stderr.contains("ambiguous argument") {
                info!(
                    remote = remote_name,
                    branch, "remote has no '{}' branch yet; treating as empty remote", branch
                );
                return Ok(());
            }
            return Err(GitError::Git2Error(git2::Error::from_str(&format!(
                "git reset --hard {} failed: {}",
                remote_ref,
                stderr.trim()
            ))));
        }
        info!("pull completed");
        Ok(())
    }

    /// Stage all changes and create a commit.
    #[instrument(skip(self, message))]
    pub fn commit(
        &self,
        message: &str,
        author_name: &str,
        author_email: &str,
        committer_name: &str,
        committer_email: &str,
    ) -> Result<Oid, GitError> {
        // Remove stale index.lock if present
        if let Some(workdir) = self.repo.workdir() {
            let lock_path = workdir.join(".git/index.lock");
            if lock_path.exists() {
                warn!(path = %lock_path.display(), "removing stale index.lock before commit");
                let _ = std::fs::remove_file(&lock_path);
            }
        }
        let mut index = self.repo.index()?;
        index.add_all(["*"].iter(), IndexAddOption::DEFAULT, None)?;
        index.write()?;
        let tree_oid = index.write_tree()?;
        let tree = self.repo.find_tree(tree_oid)?;
        let author = Signature::now(author_name, author_email)?;
        let committer = Signature::now(committer_name, committer_email)?;
        let parent_commit = match self.repo.head() {
            Ok(head) => Some(head.peel_to_commit()?),
            Err(_) => None,
        };
        let parents: Vec<&git2::Commit> = parent_commit.iter().collect();
        let oid = self
            .repo
            .commit(Some("HEAD"), &author, &committer, message, &tree, &parents)?;
        info!(sha = %oid, "created commit");
        Ok(oid)
    }

    /// Stage all changes and create a commit using the `git` CLI.
    ///
    /// This is required when LFS-tracked files are present because `git2`
    /// (libgit2) does not support Git LFS filters.  The CLI `git add` invokes
    /// the LFS clean filter which replaces large files with pointer files,
    /// whereas `git2::Index::add_all()` adds the raw file content as a blob.
    ///
    /// Returns the commit SHA on success.
    #[instrument(skip(self, message))]
    pub fn commit_via_cli(
        &self,
        message: &str,
        author_name: &str,
        author_email: &str,
        committer_name: &str,
        committer_email: &str,
    ) -> Result<Oid, GitError> {
        let repo_path = self.repo.workdir().unwrap_or_else(|| self.repo.path());

        info!(
            repo_path = %repo_path.display(),
            "committing via git CLI (LFS-aware)"
        );

        // Remove stale index.lock if present — a previous git operation
        // may have crashed or been killed, leaving the lock behind.
        let lock_path = repo_path.join(".git/index.lock");
        if lock_path.exists() {
            warn!(
                path = %lock_path.display(),
                "removing stale index.lock before git add"
            );
            let _ = std::fs::remove_file(&lock_path);
        }

        // Stage all changes using git add, which invokes LFS clean filters.
        let add_output = std::process::Command::new("git")
            .args(["add", "--all"])
            .current_dir(repo_path)
            .output()
            .map_err(|e| {
                error!(error = %e, "failed to spawn git add");
                GitError::IoError(e)
            })?;

        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr);
            error!(stderr = %stderr, "git add --all failed");
            return Err(GitError::Git2Error(git2::Error::from_str(&format!(
                "git add --all failed: {}",
                stderr.trim()
            ))));
        }

        // Check if there's anything to commit (git diff --cached --quiet).
        let diff_output = std::process::Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .current_dir(repo_path)
            .output()
            .map_err(GitError::IoError)?;

        if diff_output.status.success() {
            // Exit code 0 means no staged changes — nothing to commit.
            return Err(GitError::Git2Error(git2::Error::from_str(
                "nothing to commit (working tree clean)",
            )));
        }

        // Build the commit via git CLI with explicit author/committer.
        let author_str = format!("{} <{}>", author_name, author_email);
        let commit_output = std::process::Command::new("git")
            .args(["commit", "-m", message, "--author", &author_str])
            .current_dir(repo_path)
            .env("GIT_COMMITTER_NAME", committer_name)
            .env("GIT_COMMITTER_EMAIL", committer_email)
            .output()
            .map_err(|e| {
                error!(error = %e, "failed to spawn git commit");
                GitError::IoError(e)
            })?;

        if !commit_output.status.success() {
            let stderr = String::from_utf8_lossy(&commit_output.stderr);
            error!(stderr = %stderr, "git commit failed");
            return Err(GitError::Git2Error(git2::Error::from_str(&format!(
                "git commit failed: {}",
                stderr.trim()
            ))));
        }

        // Read the resulting commit SHA from rev-parse HEAD.
        let rev_output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo_path)
            .output()
            .map_err(GitError::IoError)?;

        let sha_str = String::from_utf8_lossy(&rev_output.stdout)
            .trim()
            .to_string();
        let oid = Oid::from_str(&sha_str).map_err(|e| {
            error!(sha = %sha_str, error = %e, "failed to parse commit SHA");
            e
        })?;

        // Reload the git2 index so subsequent operations see the new state.
        // This is needed because we bypassed git2 for the commit.
        if let Ok(mut index) = self.repo.index() {
            let _ = index.read(false);
        }

        info!(sha = %oid, "created commit via CLI (LFS-aware)");
        Ok(oid)
    }

    /// Push a local branch to a remote.
    ///
    /// HTTP(S) authentication uses the token stored by
    /// [`Self::ensure_remote_credentials`] (never URL userinfo).
    #[instrument(skip(self))]
    pub fn push(&self, remote_name: &str, branch: &str) -> Result<(), GitError> {
        self.push_impl(remote_name, branch, false)
    }

    /// Force-push a local branch to a remote (overwrites remote history).
    ///
    /// See [`Self::push`] for authentication notes.
    #[instrument(skip(self))]
    pub fn push_force(&self, remote_name: &str, branch: &str) -> Result<(), GitError> {
        self.push_impl(remote_name, branch, true)
    }

    /// Get the repo working directory path (for use with async push).
    pub fn repo_workdir(&self) -> std::path::PathBuf {
        self.repo
            .workdir()
            .unwrap_or_else(|| self.repo.path())
            .to_path_buf()
    }

    fn push_impl(&self, remote_name: &str, branch: &str, force: bool) -> Result<(), GitError> {
        let start = std::time::Instant::now();
        info!(
            remote = remote_name,
            branch, force, "pushing via git CLI (LFS-compatible)"
        );

        let repo_path = self.repo.workdir().unwrap_or_else(|| self.repo.path());

        debug!(
            repo_path = %repo_path.display(),
            force,
            "spawning git push subprocess"
        );

        let mut args = vec!["push", "--progress"];
        if force {
            args.push("--force");
        }
        args.push(remote_name);
        args.push(branch);

        let mut cmd = std::process::Command::new("git");
        cmd.args(&args).current_dir(repo_path);
        self.apply_cli_http_auth(&mut cmd, None);
        let output = cmd.output().map_err(|e| {
            error!(error = %e, "failed to spawn git push process");
            GitError::IoError(e)
        })?;

        let elapsed = start.elapsed();

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            error!(
                remote = remote_name,
                branch,
                exit_code = ?output.status.code(),
                elapsed_secs = elapsed.as_secs_f64(),
                stderr = %stderr,
                stdout = %stdout,
                "git push failed"
            );
            return Err(GitError::PushRejected {
                branch: branch.to_string(),
                detail: format!(
                    "git push failed (exit {:?}, {:.1}s): {}",
                    output.status.code(),
                    elapsed.as_secs_f64(),
                    stderr.trim()
                ),
            });
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            debug!(stderr = %stderr, "git push stderr (informational)");
        }
        info!(
            elapsed_secs = elapsed.as_secs_f64(),
            "push completed successfully"
        );
        Ok(())
    }

    /// Return the SHA of HEAD.
    pub fn get_head_sha(&self) -> Result<String, GitError> {
        let head = self.repo.head()?;
        let commit = head.peel_to_commit()?;
        Ok(commit.id().to_string())
    }

    /// Read one exact remote branch tip via `git ls-remote --exit-code`.
    #[instrument(skip(self))]
    pub fn ls_remote_ref(&self, remote: &str, branch: &str) -> Result<Option<String>, GitError> {
        let repo_path = self.repo.workdir().unwrap_or_else(|| self.repo.path());
        let ref_name = format!("refs/heads/{branch}");
        let mut cmd = std::process::Command::new("git");
        cmd.args(["ls-remote", "--exit-code", remote, &ref_name])
            .current_dir(repo_path);
        self.apply_cli_http_auth(&mut cmd, None);
        let output = cmd.output().map_err(GitError::IoError)?;
        if output.status.code() == Some(2) {
            return Ok(None);
        }
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GitError::RefNotFound(format!(
                "git ls-remote {remote} {ref_name} failed: {}",
                stderr.trim()
            )));
        }
        let sha = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        if sha.is_empty() {
            Ok(None)
        } else {
            Ok(Some(sha))
        }
    }

    /// Pending commits on the ancestry frontier from `since_sha` to `tip_sha`.
    ///
    /// Uses hide/push (`since..tip`), not a visited-order stop at `since_sha`.
    /// Qualified merge DAGs and linear backlogs both return the oldest-first
    /// replay batch with `has_more` set when the frontier exceeds the cap.
    pub fn pending_commits_between(
        &self,
        since_sha: &str,
        tip_sha: &str,
        max_commits: Option<usize>,
    ) -> Result<PendingCommitSelection, GitError> {
        #[cfg(debug_assertions)]
        if std::env::var("REPOSYNC_TEST_FETCH_PENDING_FAULT")
            .ok()
            .as_deref()
            == self.repo_path.to_str()
        {
            return Err(GitError::UnsupportedHistory {
                reason: crate::pending_frontier::REASON_MERGE_DAG.into(),
                detail: crate::pending_frontier::DETAIL_MERGE_DAG.into(),
            });
        }
        let cap = max_commits.unwrap_or(crate::pending_frontier::DEFAULT_PENDING_COMMIT_CAP);
        let batch =
            crate::pending_frontier::select_pending_batch(&self.repo, since_sha, tip_sha, cap)?;
        if batch.has_more {
            debug!(
                total = batch.total,
                batch = batch.commits.len(),
                cap,
                "pending Git history continues in later sync cycles"
            );
        }
        let mut commits = Vec::with_capacity(batch.commits.len());
        for oid in batch.commits {
            let commit = self.repo.find_commit(oid)?;
            commits.push(git_commit_info(&commit));
        }
        let batch_tip = if batch.has_more {
            let Some(last) = commits.last() else {
                return Err(GitError::UnsupportedHistory {
                    reason: crate::pending_frontier::REASON_BACKLOG.into(),
                    detail: format!(
                        "{}: empty continuation batch",
                        crate::pending_frontier::DETAIL_BACKLOG
                    ),
                });
            };
            last.sha.clone()
        } else {
            commits
                .last()
                .map(|commit| commit.sha.clone())
                .unwrap_or_else(|| tip_sha.to_string())
        };
        debug!(
            count = commits.len(),
            total = batch.total,
            has_more = batch.has_more,
            batch_tip = %batch_tip,
            "collected pending commits"
        );
        Ok(PendingCommitSelection {
            commits,
            total: batch.total,
            has_more: batch.has_more,
            batch_tip,
        })
    }

    /// Next merge-DAG continuation batch after prior handled commits.
    pub fn pending_commits_continuation_batch(
        &self,
        since_sha: &str,
        tip_sha: &str,
        handled_shas: &[String],
        max_commits: Option<usize>,
    ) -> Result<PendingCommitSelection, GitError> {
        let cap = max_commits.unwrap_or(crate::pending_frontier::DEFAULT_PENDING_COMMIT_CAP);
        let handled: std::collections::HashSet<Oid> = handled_shas
            .iter()
            .map(|sha| Oid::from_str(sha).map_err(GitError::Git2Error))
            .collect::<Result<_, _>>()?;
        let batch = crate::pending_frontier::select_continuation_batch(
            &self.repo, since_sha, tip_sha, &handled, cap,
        )?;
        let mut commits = Vec::with_capacity(batch.commits.len());
        for oid in batch.commits {
            let commit = self.repo.find_commit(oid)?;
            commits.push(git_commit_info(&commit));
        }
        let batch_tip = if batch.has_more {
            let Some(last) = commits.last() else {
                return Err(GitError::UnsupportedHistory {
                    reason: crate::pending_frontier::REASON_BACKLOG.into(),
                    detail: format!(
                        "{}: empty continuation batch",
                        crate::pending_frontier::DETAIL_BACKLOG
                    ),
                });
            };
            last.sha.clone()
        } else {
            commits
                .last()
                .map(|commit| commit.sha.clone())
                .unwrap_or_else(|| tip_sha.to_string())
        };
        Ok(PendingCommitSelection {
            commits,
            total: batch.total,
            has_more: batch.has_more,
            batch_tip,
        })
    }

    /// All pending commits on the P→R frontier for conflict coverage.
    ///
    /// Callers that replay in batches use this to detect overlapping paths
    /// across the full admitted range, not only the current batch.
    pub fn pending_commits_for_conflict_coverage(
        &self,
        since_sha: &str,
        tip_sha: &str,
        max_commits: Option<usize>,
    ) -> Result<Vec<GitCommitInfo>, GitError> {
        let cap = max_commits.unwrap_or(crate::pending_frontier::DEFAULT_PENDING_COMMIT_CAP);
        let batch =
            crate::pending_frontier::select_pending_batch(&self.repo, since_sha, tip_sha, cap)?;
        if !batch.has_more {
            let mut commits = Vec::with_capacity(batch.commits.len());
            for oid in batch.commits {
                let commit = self.repo.find_commit(oid)?;
                commits.push(git_commit_info(&commit));
            }
            return Ok(commits);
        }
        let full = crate::pending_frontier::select_pending_batch(
            &self.repo,
            since_sha,
            tip_sha,
            batch.total,
        )?;
        let mut commits = Vec::with_capacity(full.commits.len());
        for oid in full.commits {
            let commit = self.repo.find_commit(oid)?;
            commits.push(git_commit_info(&commit));
        }
        Ok(commits)
    }

    /// Pending commits from `since_sha` to HEAD using the ancestry frontier.
    ///
    /// Returns an empty vec if HEAD is unborn (empty repo). A missing
    /// `since_sha` is not treated as start-from-zero.
    pub fn get_commits_since(
        &self,
        since_sha: Option<&str>,
        max_commits: Option<usize>,
    ) -> Result<Vec<GitCommitInfo>, GitError> {
        if self.repo.head().is_err() {
            return Ok(Vec::new());
        }
        let Some(since_sha) = since_sha else {
            return Err(GitError::UnsupportedHistory {
                reason: "missing_checkpoint".into(),
                detail: "pending Git selection requires a handled checkpoint".into(),
            });
        };
        let tip = self.head_sha()?;
        Ok(self
            .pending_commits_between(since_sha, &tip, max_commits)?
            .commits)
    }

    /// Create a new branch pointing at `from_sha`.
    #[instrument(skip(self))]
    pub fn create_branch(&self, name: &str, from_sha: &str) -> Result<(), GitError> {
        let oid = Oid::from_str(from_sha)?;
        let commit = self.repo.find_commit(oid)?;
        self.repo.branch(name, &commit, false)?;
        info!(name, from_sha, "created branch");
        Ok(())
    }

    /// Delete a local branch.
    #[instrument(skip(self))]
    pub fn delete_branch(&self, name: &str) -> Result<(), GitError> {
        let mut branch = self.repo.find_branch(name, BranchType::Local)?;
        branch.delete()?;
        info!(name, "deleted branch");
        Ok(())
    }

    /// List all local branch names.
    pub fn list_branches(&self) -> Result<Vec<String>, GitError> {
        let branches = self.repo.branches(Some(BranchType::Local))?;
        let mut names = Vec::new();
        for branch_result in branches {
            let (branch, _) = branch_result?;
            if let Some(name) = branch.name()? {
                names.push(name.to_string());
            }
        }
        Ok(names)
    }

    // -- Personal Branch Mode methods -----------------------------------------

    /// Check if `ancestor_sha` is an ancestor of `descendant_sha`.
    ///
    /// Returns `true` if the history from `descendant` contains `ancestor`,
    /// indicating no force push / history rewrite occurred.
    pub fn is_ancestor(&self, ancestor_sha: &str, descendant_sha: &str) -> Result<bool, GitError> {
        let ancestor_oid = Oid::from_str(ancestor_sha)?;
        let descendant_oid = Oid::from_str(descendant_sha)?;
        match self.repo.graph_descendant_of(descendant_oid, ancestor_oid) {
            Ok(is_descendant) => Ok(is_descendant),
            Err(_) => Ok(false),
        }
    }

    /// Checkout an existing local branch.
    #[instrument(skip(self))]
    pub fn checkout_branch(&self, name: &str) -> Result<(), GitError> {
        let refname = format!("refs/heads/{}", name);
        self.repo.set_head(&refname)?;
        self.repo
            .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;
        info!(name, "checked out branch");
        Ok(())
    }

    /// Reset HEAD to a specific commit SHA.
    #[instrument(skip(self))]
    pub fn reset_to(&self, sha: &str) -> Result<(), GitError> {
        let oid = Oid::from_str(sha)?;
        let commit = self.repo.find_commit(oid)?;
        let obj = commit.as_object();
        self.repo.reset(obj, git2::ResetType::Hard, None)?;
        info!(sha, "reset HEAD");
        Ok(())
    }

    /// Author display name for a commit SHA.
    pub fn commit_author_name(&self, sha: &str) -> Result<String, GitError> {
        let oid = Oid::from_str(sha)?;
        let commit = self.repo.find_commit(oid)?;
        let name = commit.author().name().unwrap_or("").to_string();
        Ok(name)
    }

    /// Get the number of parents a commit has (useful for merge detection).
    pub fn get_parent_count(&self, sha: &str) -> Result<usize, GitError> {
        let oid = Oid::from_str(sha)?;
        let commit = self.repo.find_commit(oid)?;
        Ok(commit.parent_count())
    }

    /// First parent SHA (if any) and the commit tree object ID.
    pub fn commit_parent_and_tree(&self, sha: &str) -> Result<(Option<String>, String), GitError> {
        let oid = Oid::from_str(sha)?;
        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?.id().to_string();
        let parent = if commit.parent_count() > 0 {
            Some(commit.parent_id(0)?.to_string())
        } else {
            None
        };
        Ok((parent, tree))
    }

    /// Get the list of changed files for a specific commit.
    ///
    /// Actions are `A` (added), `M` (modified), `D` (deleted), or `R`
    /// (rename/move). Renames carry the old path in `rename_from`.
    pub fn get_changed_files(&self, sha: &str) -> Result<Vec<GitChangedPath>, GitError> {
        let oid = Oid::from_str(sha)?;
        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?;

        let parent_tree = if commit.parent_count() > 0 {
            Some(commit.parent(0)?.tree()?)
        } else {
            None
        };

        let mut diff = self
            .repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
        let mut find_opts = git2::DiffFindOptions::new();
        find_opts.renames(true);
        diff.find_similar(Some(&mut find_opts))?;

        let mut changes = Vec::new();
        diff.foreach(
            &mut |delta, _progress| {
                let (action, path, rename_from) = match delta.status() {
                    git2::Delta::Added | git2::Delta::Untracked => {
                        ("A", delta.new_file().path(), None)
                    }
                    git2::Delta::Deleted => ("D", delta.old_file().path(), None),
                    git2::Delta::Modified => ("M", delta.new_file().path(), None),
                    git2::Delta::Renamed => ("R", delta.new_file().path(), delta.old_file().path()),
                    _ => (
                        "M",
                        delta.new_file().path().or_else(|| delta.old_file().path()),
                        None,
                    ),
                };
                let path = path
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                if path.is_empty() {
                    return true;
                }
                let rename_from = rename_from.map(|p| p.to_string_lossy().to_string());
                changes.push(GitChangedPath {
                    action: action.to_string(),
                    path,
                    rename_from,
                });
                true
            },
            None,
            None,
            None,
        )?;

        debug!(sha, count = changes.len(), "got changed files for commit");
        Ok(changes)
    }

    /// Tree-entry modes on both sides of a changed path. A missing side is
    /// an add or delete, not evidence that its surviving side is a regular file.
    pub fn changed_entry_modes(
        &self,
        sha: &str,
        path: &str,
    ) -> Result<(Option<i32>, Option<i32>), GitError> {
        let commit = self.repo.find_commit(Oid::from_str(sha)?)?;
        let current = commit
            .tree()?
            .get_path(std::path::Path::new(path))
            .ok()
            .map(|entry| entry.filemode());
        let previous = if commit.parent_count() > 0 {
            commit
                .parent(0)?
                .tree()?
                .get_path(std::path::Path::new(path))
                .ok()
                .map(|entry| entry.filemode())
        } else {
            None
        };
        Ok((previous, current))
    }

    /// Get the content of a file at a specific commit.
    ///
    /// Returns `None` if the file does not exist in that commit's tree.
    pub fn get_file_content_at_commit(
        &self,
        sha: &str,
        file_path: &str,
    ) -> Result<Option<Vec<u8>>, GitError> {
        let oid = Oid::from_str(sha)?;
        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?;

        match tree.get_path(std::path::Path::new(file_path)) {
            Ok(entry) => {
                let obj = entry.to_object(&self.repo)?;
                let blob = obj
                    .as_blob()
                    .ok_or_else(|| GitError::RefNotFound(file_path.to_string()))?;
                Ok(Some(blob.content().to_vec()))
            }
            Err(_) => Ok(None),
        }
    }

    /// Apply a unified diff to the working tree.
    #[instrument(skip(self, diff_content))]
    pub async fn apply_diff(&self, diff_content: &str) -> Result<(), GitError> {
        use std::process::Stdio;
        use tokio::process::Command;
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.repo_path)
            .args(["apply", "--3way", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(GitError::IoError)?;
        if let Some(ref mut stdin) = child.stdin {
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(diff_content.as_bytes())
                .await
                .map_err(GitError::IoError)?;
        }
        let output = child.wait_with_output().await.map_err(GitError::IoError)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            warn!(%stderr, "git apply failed");
            return Err(GitError::ApplyFailed(stderr));
        }
        info!("diff applied successfully");
        Ok(())
    }
}

fn git_commit_info(commit: &git2::Commit<'_>) -> GitCommitInfo {
    GitCommitInfo {
        sha: commit.id().to_string(),
        message: commit.message().unwrap_or("").to_string(),
        author_name: commit.author().name().unwrap_or("").to_string(),
        author_email: commit.author().email().unwrap_or("").to_string(),
        author_time: commit.author().when().seconds(),
        committer_name: commit.committer().name().unwrap_or("").to_string(),
        committer_email: commit.committer().email().unwrap_or("").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn open_repo_purges_legacy_clean_http_token_sidecar() {
        let tmp = TempDir::new().unwrap();
        let work = tmp.path().join("work");
        std::process::Command::new("git")
            .args(["init", work.to_str().unwrap()])
            .status()
            .unwrap();
        let sidecar = work.join(".git").join("reposync").join("clean-http-token");
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(&sidecar, "legacy-plaintext-token").unwrap();
        assert!(sidecar.is_file());
        GitClient::new(&work).unwrap();
        assert!(!sidecar.is_file());
    }

    #[test]
    fn test_init_and_commit() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let client = GitClient::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello world").unwrap();
        let oid = client
            .commit(
                "initial commit",
                "Test",
                "test@test.com",
                "Test",
                "test@test.com",
            )
            .unwrap();
        assert!(!oid.is_zero());
        assert_eq!(client.get_head_sha().unwrap(), oid.to_string());
    }

    #[test]
    fn test_create_and_delete_branch() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let client = GitClient::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("f.txt"), "c").unwrap();
        let oid = client
            .commit("init", "T", "t@t.com", "T", "t@t.com")
            .unwrap();
        client.create_branch("feature", &oid.to_string()).unwrap();
        assert!(client
            .list_branches()
            .unwrap()
            .contains(&"feature".to_string()));
        client.delete_branch("feature").unwrap();
        assert!(!client
            .list_branches()
            .unwrap()
            .contains(&"feature".to_string()));
    }

    #[test]
    fn test_repo_not_found() {
        assert!(matches!(
            GitClient::new("/nonexistent"),
            Err(GitError::RepositoryNotFound(_))
        ));
    }

    #[test]
    fn test_is_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let client = GitClient::new(dir.path()).unwrap();

        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let oid1 = client
            .commit("first", "T", "t@t.com", "T", "t@t.com")
            .unwrap();

        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        let oid2 = client
            .commit("second", "T", "t@t.com", "T", "t@t.com")
            .unwrap();

        // oid1 is ancestor of oid2
        assert!(client
            .is_ancestor(&oid1.to_string(), &oid2.to_string())
            .unwrap());
        // oid2 is NOT ancestor of oid1
        assert!(!client
            .is_ancestor(&oid2.to_string(), &oid1.to_string())
            .unwrap());
    }

    #[test]
    fn test_checkout_branch_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let client = GitClient::new(dir.path()).unwrap();

        std::fs::write(dir.path().join("f.txt"), "v1").unwrap();
        let oid1 = client
            .commit("init", "T", "t@t.com", "T", "t@t.com")
            .unwrap();

        client.create_branch("dev", &oid1.to_string()).unwrap();
        client.checkout_branch("dev").unwrap();

        std::fs::write(dir.path().join("f.txt"), "v2").unwrap();
        let oid2 = client
            .commit("update", "T", "t@t.com", "T", "t@t.com")
            .unwrap();

        assert_eq!(client.get_head_sha().unwrap(), oid2.to_string());

        // Reset back to oid1
        client.reset_to(&oid1.to_string()).unwrap();
        assert_eq!(client.get_head_sha().unwrap(), oid1.to_string());
    }

    #[test]
    fn test_get_parent_count() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let client = GitClient::new(dir.path()).unwrap();

        std::fs::write(dir.path().join("f.txt"), "c").unwrap();
        let oid = client
            .commit("init", "T", "t@t.com", "T", "t@t.com")
            .unwrap();

        // First commit has 0 parents
        assert_eq!(client.get_parent_count(&oid.to_string()).unwrap(), 0);

        std::fs::write(dir.path().join("g.txt"), "d").unwrap();
        let oid2 = client
            .commit("second", "T", "t@t.com", "T", "t@t.com")
            .unwrap();

        // Second commit has 1 parent
        assert_eq!(client.get_parent_count(&oid2.to_string()).unwrap(), 1);
    }

    #[test]
    fn pending_commits_between_linear_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let client = GitClient::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let a = client
            .commit("A", "T", "t@t.com", "T", "t@t.com")
            .unwrap()
            .to_string();
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        let b = client
            .commit("B", "T", "t@t.com", "T", "t@t.com")
            .unwrap()
            .to_string();
        std::fs::write(dir.path().join("c.txt"), "c").unwrap();
        let c = client
            .commit("C", "T", "t@t.com", "T", "t@t.com")
            .unwrap()
            .to_string();
        let pending = client.pending_commits_between(&a, &c, None).unwrap();
        assert_eq!(
            pending
                .commits
                .iter()
                .map(|info| info.sha.as_str())
                .collect::<Vec<_>>(),
            vec![b.as_str(), c.as_str()]
        );
        assert!(!pending.has_more);
        assert_eq!(pending.batch_tip, c);
        assert!(client
            .pending_commits_between(&c, &c, None)
            .unwrap()
            .commits
            .is_empty());
    }

    #[test]
    fn test_get_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let client = GitClient::new(dir.path()).unwrap();

        // First commit: add a.txt
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let oid1 = client
            .commit("add a", "T", "t@t.com", "T", "t@t.com")
            .unwrap();

        let files1 = client.get_changed_files(&oid1.to_string()).unwrap();
        assert_eq!(files1.len(), 1);
        assert_eq!(files1[0].action, "A");
        assert_eq!(files1[0].path, "a.txt");
        assert!(files1[0].rename_from.is_none());

        // Second commit: modify a.txt, add b.txt
        std::fs::write(dir.path().join("a.txt"), "modified").unwrap();
        std::fs::write(dir.path().join("b.txt"), "new file").unwrap();
        let oid2 = client
            .commit("modify a, add b", "T", "t@t.com", "T", "t@t.com")
            .unwrap();

        let files2 = client.get_changed_files(&oid2.to_string()).unwrap();
        assert_eq!(files2.len(), 2);
        let paths: Vec<&str> = files2.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"a.txt"));
        assert!(paths.contains(&"b.txt"));
    }

    #[test]
    fn test_get_changed_files_rename() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let client = GitClient::new(dir.path()).unwrap();

        std::fs::write(dir.path().join("old.txt"), "payload").unwrap();
        client
            .commit("add old", "T", "t@t.com", "T", "t@t.com")
            .unwrap();
        for args in [
            ["config", "user.email", "t@t.com"],
            ["config", "user.name", "T"],
            ["mv", "old.txt", "new.txt"],
            ["commit", "-m", "rename old to new"],
        ] {
            std::process::Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .status()
                .unwrap();
        }
        let oid2 = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let sha = String::from_utf8(oid2.stdout).unwrap().trim().to_string();

        let files = client.get_changed_files(&sha).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].action, "R");
        assert_eq!(files[0].path, "new.txt");
        assert_eq!(files[0].rename_from.as_deref(), Some("old.txt"));
    }

    #[test]
    fn test_get_file_content_at_commit() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        let client = GitClient::new(dir.path()).unwrap();

        std::fs::write(dir.path().join("data.txt"), "version 1").unwrap();
        let oid1 = client.commit("v1", "T", "t@t.com", "T", "t@t.com").unwrap();

        std::fs::write(dir.path().join("data.txt"), "version 2").unwrap();
        let oid2 = client.commit("v2", "T", "t@t.com", "T", "t@t.com").unwrap();

        // Read content at commit 1
        let content1 = client
            .get_file_content_at_commit(&oid1.to_string(), "data.txt")
            .unwrap()
            .unwrap();
        assert_eq!(String::from_utf8(content1).unwrap(), "version 1");

        // Read content at commit 2
        let content2 = client
            .get_file_content_at_commit(&oid2.to_string(), "data.txt")
            .unwrap()
            .unwrap();
        assert_eq!(String::from_utf8(content2).unwrap(), "version 2");

        // Read non-existent file
        let missing = client
            .get_file_content_at_commit(&oid1.to_string(), "nonexistent.txt")
            .unwrap();
        assert!(missing.is_none());
    }
}
