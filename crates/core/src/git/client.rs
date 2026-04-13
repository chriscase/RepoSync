//! Local Git repository operations via `git2`.

use std::path::{Path, PathBuf};

use git2::{
    BranchType, Cred, FetchOptions, IndexAddOption, Oid, RemoteCallbacks, Repository,
    Signature,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, instrument, warn};

use crate::errors::GitError;

/// High-level Git client wrapping a `git2::Repository`.
pub struct GitClient {
    repo: Repository,
    repo_path: PathBuf,
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

impl GitClient {
    /// Open an existing Git repository at `repo_path`.
    pub fn new<P: AsRef<Path>>(repo_path: P) -> Result<Self, GitError> {
        let path = repo_path.as_ref();
        info!(path = %path.display(), "opening git repository");
        let repo = Repository::open(path)
            .map_err(|_| GitError::RepositoryNotFound(path.display().to_string()))?;
        Ok(Self {
            repo,
            repo_path: path.to_path_buf(),
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
        })
    }

    /// Clone a remote repository to `path`.
    ///
    /// If a token is provided and the URL is HTTP(S), the token is embedded
    /// directly in the URL (`x-access-token:<token>@host`) for maximum
    /// compatibility with servers like Gitea that don't work well with
    /// libgit2's credential callback.
    #[instrument(skip(token), fields(url = %url, path = %path.display()))]
    pub fn clone_repo(url: &str, path: &Path, token: Option<&str>) -> Result<Self, GitError> {
        info!("cloning git repository");
        let clone_url = match token {
            Some(tok) if url.starts_with("https://") => {
                let rest = url.strip_prefix("https://").unwrap();
                format!("https://x-access-token:{}@{}", tok, rest)
            }
            Some(tok) if url.starts_with("http://") => {
                let rest = url.strip_prefix("http://").unwrap();
                format!("http://x-access-token:{}@{}", tok, rest)
            }
            _ => url.to_string(),
        };
        let repo = Repository::clone(&clone_url, path)?;
        info!("clone completed");
        Ok(Self {
            repo,
            repo_path: path.to_path_buf(),
        })
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
        self.repo
            .reference_symbolic("HEAD", &target_ref, true, "reposync: align HEAD with configured branch")?;
        info!(branch, "aligned HEAD to refs/heads/{}", branch);
        Ok(())
    }

    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    /// Get the current HEAD commit SHA.
    pub fn head_sha(&self) -> Result<String, GitError> {
        let head = self.repo.head().map_err(GitError::from)?;
        let oid = head
            .peel_to_commit()
            .map_err(GitError::from)?
            .id();
        Ok(oid.to_string())
    }

    /// Hard-reset HEAD to a specific commit SHA.
    /// Used to roll back failed pushes so bad commits don't accumulate.
    pub fn reset_hard(&self, sha: &str) -> Result<(), GitError> {
        let oid = git2::Oid::from_str(sha)
            .map_err(|e| GitError::Git2Error(e))?;
        let commit = self.repo.find_commit(oid)
            .map_err(GitError::from)?;
        self.repo
            .reset(commit.as_object(), git2::ResetType::Hard, None)
            .map_err(GitError::from)?;
        info!(sha, "git reset --hard completed");
        Ok(())
    }

    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    /// Ensure the origin remote URL contains embedded credentials for HTTP(S) remotes.
    ///
    /// libgit2's credential callback doesn't work reliably with all Git servers
    /// (e.g. Gitea). Embedding `x-access-token:<token>` in the URL is the most
    /// portable approach and mirrors what CI/CD systems do.
    pub fn ensure_remote_credentials(
        &self,
        remote_name: &str,
        token: Option<&str>,
    ) -> Result<(), GitError> {
        let Some(tok) = token else { return Ok(()) };
        let remote = self.repo.find_remote(remote_name)?;
        let Some(url) = remote.url() else {
            return Ok(());
        };
        // Only modify http(s) URLs that don't already have credentials.
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Ok(());
        }
        if url.contains('@') {
            // Already has credentials embedded — leave it alone.
            return Ok(());
        }
        // Insert x-access-token:<tok>@ after the scheme.
        let new_url = if let Some(rest) = url.strip_prefix("https://") {
            format!("https://x-access-token:{}@{}", tok, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            format!("http://x-access-token:{}@{}", tok, rest)
        } else {
            return Ok(());
        };
        info!("updating remote URL to embed credentials");
        self.repo
            .remote_set_url(remote_name, &new_url)?;
        Ok(())
    }

    /// Fetch from a named remote with a 5-minute timeout.
    #[instrument(skip(self, token))]
    pub fn fetch(&self, remote_name: &str, token: Option<&str>) -> Result<(), GitError> {
        const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
        info!(remote = remote_name, "fetching");
        let mut remote = self.repo.find_remote(remote_name)?;
        let mut callbacks = RemoteCallbacks::new();
        if let Some(tok) = token {
            let tok = tok.to_string();
            callbacks.credentials(move |_url, _username, _allowed| {
                Cred::userpass_plaintext("x-access-token", &tok)
            });
        }
        // Abort the transfer if it exceeds the timeout.
        let fetch_start = std::time::Instant::now();
        let fetch_start_for_cb = fetch_start;
        callbacks.transfer_progress(move |_stats| {
            fetch_start_for_cb.elapsed() < FETCH_TIMEOUT
        });
        let mut fetch_opts = FetchOptions::new();
        fetch_opts.remote_callbacks(callbacks);
        remote.fetch(&[] as &[&str], Some(&mut fetch_opts), None)
            .map_err(|e| {
                if fetch_start.elapsed() >= FETCH_TIMEOUT {
                    GitError::ApplyFailed(format!("git fetch timed out after {}s", FETCH_TIMEOUT.as_secs()))
                } else {
                    e.into()
                }
            })?;
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
                    branch,
                    "remote has no '{}' branch yet; treating as empty remote",
                    branch
                );
                return Ok(());
            }
            return Err(GitError::Git2Error(git2::Error::from_str(
                &format!("git reset --hard {} failed: {}", remote_ref, stderr.trim())
            )));
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

        let sha_str = String::from_utf8_lossy(&rev_output.stdout).trim().to_string();
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
    /// Authentication is always driven by the credentials embedded in the
    /// remote URL via [`Self::ensure_remote_credentials`]. Callers must
    /// ensure the remote URL has fresh credentials before invoking push.
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

    fn push_impl(
        &self,
        remote_name: &str,
        branch: &str,
        force: bool,
    ) -> Result<(), GitError> {
        let start = std::time::Instant::now();
        info!(remote = remote_name, branch, force, "pushing via git CLI (LFS-compatible)");

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

        let output = std::process::Command::new("git")
            .args(&args)
            .current_dir(repo_path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| {
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

    /// Walk commits from HEAD backwards until we reach `since_sha`.
    ///
    /// Returns an empty vec if HEAD is unborn (empty repo).
    pub fn get_commits_since(
        &self,
        since_sha: Option<&str>,
        max_commits: Option<usize>,
    ) -> Result<Vec<GitCommitInfo>, GitError> {
        let cap = max_commits.unwrap_or(1000);
        // If HEAD doesn't exist (empty repo), return empty list.
        if self.repo.head().is_err() {
            return Ok(Vec::new());
        }
        let mut revwalk = self.repo.revwalk()?;
        revwalk.push_head()?;
        revwalk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        let since_oid = since_sha.map(Oid::from_str).transpose()?;
        let mut commits = Vec::new();
        for oid_result in revwalk {
            let oid = oid_result?;
            if Some(oid) == since_oid {
                break;
            }
            let commit = self.repo.find_commit(oid)?;
            commits.push(GitCommitInfo {
                sha: oid.to_string(),
                message: commit.message().unwrap_or("").to_string(),
                author_name: commit.author().name().unwrap_or("").to_string(),
                author_email: commit.author().email().unwrap_or("").to_string(),
                author_time: commit.author().when().seconds(),
                committer_name: commit.committer().name().unwrap_or("").to_string(),
                committer_email: commit.committer().email().unwrap_or("").to_string(),
            });
            if commits.len() >= cap {
                info!(cap, "reached commit limit for get_commits_since");
                break;
            }
        }
        debug!(count = commits.len(), "collected commits");
        Ok(commits)
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

    /// Get the number of parents a commit has (useful for merge detection).
    pub fn get_parent_count(&self, sha: &str) -> Result<usize, GitError> {
        let oid = Oid::from_str(sha)?;
        let commit = self.repo.find_commit(oid)?;
        Ok(commit.parent_count())
    }

    /// Get the list of changed files for a specific commit.
    ///
    /// Returns a vec of `(action, path)` tuples where action is "A" (added),
    /// "M" (modified), or "D" (deleted).
    pub fn get_changed_files(&self, sha: &str) -> Result<Vec<(String, String)>, GitError> {
        let oid = Oid::from_str(sha)?;
        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?;

        let parent_tree = if commit.parent_count() > 0 {
            Some(commit.parent(0)?.tree()?)
        } else {
            None
        };

        let diff = self
            .repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;

        let mut changes = Vec::new();
        diff.foreach(
            &mut |delta, _progress| {
                let action = match delta.status() {
                    git2::Delta::Added | git2::Delta::Untracked => "A",
                    git2::Delta::Deleted => "D",
                    git2::Delta::Modified => "M",
                    git2::Delta::Renamed => "M",
                    _ => "M",
                };
                let path = delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                if !path.is_empty() {
                    changes.push((action.to_string(), path));
                }
                true
            },
            None,
            None,
            None,
        )?;

        debug!(sha, count = changes.len(), "got changed files for commit");
        Ok(changes)
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(files1[0].0, "A"); // Added
        assert_eq!(files1[0].1, "a.txt");

        // Second commit: modify a.txt, add b.txt
        std::fs::write(dir.path().join("a.txt"), "modified").unwrap();
        std::fs::write(dir.path().join("b.txt"), "new file").unwrap();
        let oid2 = client
            .commit("modify a, add b", "T", "t@t.com", "T", "t@t.com")
            .unwrap();

        let files2 = client.get_changed_files(&oid2.to_string()).unwrap();
        assert_eq!(files2.len(), 2);
        let paths: Vec<&str> = files2.iter().map(|(_, p)| p.as_str()).collect();
        assert!(paths.contains(&"a.txt"));
        assert!(paths.contains(&"b.txt"));
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
