//! Asynchronous SVN CLI client.

use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tracing::{debug, info, instrument, trace, warn};

use super::parser::{
    parse_svn_diff_summarize, parse_svn_info, parse_svn_log, SvnDiffEntry, SvnInfo, SvnLogEntry,
};
use crate::errors::SvnError;

/// Default timeout for SVN commands (5 minutes).
const SVN_COMMAND_TIMEOUT: Duration = Duration::from_secs(300);

/// Asynchronous client for interacting with an SVN repository via the CLI.
#[derive(Clone)]
pub struct SvnClient {
    url: String,
    username: String,
    password: String,
}

// Custom Debug implementation that redacts the password field.
impl fmt::Debug for SvnClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SvnClient")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl SvnClient {
    /// Create a new SVN client targeting `url` with the given credentials.
    pub fn new(
        url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        let client = Self {
            url: url.into(),
            username: username.into(),
            password: password.into(),
        };
        info!(url = %client.url, username = %client.username, "created SvnClient");
        client
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Update the password at runtime (credential hot-reload from DB).
    pub fn set_password(&mut self, password: impl Into<String>) {
        self.password = password.into();
    }

    /// Update the username at runtime (credential hot-reload from DB).
    pub fn set_username(&mut self, username: impl Into<String>) {
        self.username = username.into();
    }

    #[instrument(skip(self), fields(url = %self.url))]
    pub async fn info(&self) -> Result<SvnInfo, SvnError> {
        let output = self.run_svn(&["info", "--xml", &self.url]).await?;
        parse_svn_info(&output)
    }

    #[instrument(skip(self), fields(url = %self.url))]
    pub async fn log(&self, start_rev: i64, end_rev: i64) -> Result<Vec<SvnLogEntry>, SvnError> {
        let end_str = if end_rev < 0 {
            "HEAD".to_string()
        } else {
            end_rev.to_string()
        };
        let rev_range = format!("{}:{}", start_rev, end_str);
        let output = self
            .run_svn(&["log", "--xml", "--verbose", "-r", &rev_range, &self.url])
            .await?;
        parse_svn_log(&output)
    }

    #[instrument(skip(self), fields(url = %self.url, rev))]
    pub async fn diff(&self, rev: i64) -> Result<Vec<SvnDiffEntry>, SvnError> {
        if rev < 1 {
            return Err(SvnError::RevisionNotFound(rev));
        }
        let rev_range = format!("{}:{}", rev - 1, rev);
        let output = self
            .run_svn(&["diff", "--summarize", "--xml", "-r", &rev_range, &self.url])
            .await?;
        parse_svn_diff_summarize(&output)
    }

    #[instrument(skip(self), fields(url = %self.url, rev))]
    pub async fn diff_full(&self, rev: i64) -> Result<String, SvnError> {
        if rev < 1 {
            return Err(SvnError::RevisionNotFound(rev));
        }
        let rev_range = format!("{}:{}", rev - 1, rev);
        // Peg at rev so the URL is resolved as it existed at that
        // revision — important for branch pairs whose branch path was
        // deleted in later revisions.
        let peg_url = format!("{}@{}", self.url, rev);
        self.run_svn(&["diff", "-r", &rev_range, &peg_url]).await
    }

    #[instrument(skip(self), fields(url = %self.url, rev))]
    pub async fn checkout(&self, path: &Path, rev: i64) -> Result<(), SvnError> {
        let rev_str = rev.to_string();
        let path_str = path.to_string_lossy().to_string();
        self.run_svn(&["checkout", "-r", &rev_str, &self.url, &path_str])
            .await?;
        info!(path = %path.display(), rev, "svn checkout completed");
        Ok(())
    }

    /// Checkout the SVN repository at HEAD into the given directory.
    #[instrument(skip(self), fields(url = %self.url))]
    pub async fn checkout_head(&self, path: &Path) -> Result<(), SvnError> {
        let path_str = path.to_string_lossy().to_string();
        self.run_svn(&["checkout", &self.url, &path_str]).await?;
        info!(path = %path.display(), "svn checkout (HEAD) completed");
        Ok(())
    }

    #[instrument(skip(self, message), fields(path = %path.display()))]
    pub async fn commit(&self, path: &Path, message: &str, _author: &str) -> Result<i64, SvnError> {
        let path_str = path.to_string_lossy().to_string();
        let output = self
            .run_svn_in_dir(path, &["commit", "-m", message, &path_str])
            .await?;

        // SVN returns exit 0 with empty output when there's nothing to commit.
        // This is not an error — it just means the working copy is clean.
        if output.trim().is_empty() {
            info!("svn commit: nothing to commit (empty output)");
            return Err(SvnError::NothingToCommit);
        }

        let rev = parse_committed_revision(&output).ok_or_else(|| {
            warn!(raw_output = %output, "failed to parse committed revision from svn commit output");
            SvnError::CommandFailed {
                exit_code: 0,
                stderr: format!("could not parse committed revision from: {}", output),
            }
        })?;
        info!(rev, "svn commit succeeded");
        Ok(rev)
    }

    #[instrument(skip(self, prop_value), fields(url = %self.url, rev, prop_name))]
    pub async fn set_rev_prop(
        &self,
        rev: i64,
        prop_name: &str,
        prop_value: &str,
    ) -> Result<(), SvnError> {
        let rev_str = rev.to_string();
        self.run_svn(&[
            "propset",
            "--revprop",
            "-r",
            &rev_str,
            prop_name,
            prop_value,
            &self.url,
        ])
        .await?;
        debug!(rev, prop_name, "set revision property");
        Ok(())
    }

    #[instrument(skip(self), fields(url = %self.url))]
    pub async fn list_branches(&self, branches_path: &str) -> Result<Vec<String>, SvnError> {
        let branches_url = format!("{}/{}", self.url, branches_path);
        let output = self.run_svn(&["list", &branches_url]).await?;
        let branches: Vec<String> = output
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.trim_end_matches('/').to_string())
            .collect();
        debug!(count = branches.len(), "listed branches");
        Ok(branches)
    }

    #[instrument(skip(self), fields(url = %self.url))]
    pub async fn create_branch(
        &self,
        name: &str,
        source_path: &str,
        branches_path: &str,
        source_rev: i64,
    ) -> Result<(), SvnError> {
        // Guard against path traversal in branch names
        if name.contains("..") || branches_path.contains("..") || source_path.contains("..") {
            return Err(SvnError::CommandFailed {
                exit_code: 1,
                stderr: "path traversal ('..') not allowed in branch names".to_string(),
            });
        }

        let src_url = format!("{}/{}", self.url, source_path);
        let dest_url = format!("{}/{}/{}", self.url, branches_path, name);
        let rev_str = source_rev.to_string();

        // Ensure the parent directory exists (e.g., branches/dev/ for branches/dev/james-wilson).
        // svn copy fails if intermediate directories are missing.
        let parent_url = format!("{}/{}", self.url, branches_path);
        match self
            .run_svn(&[
                "mkdir",
                "--parents",
                &parent_url,
                "-m",
                &format!("Create directory {}", branches_path),
            ])
            .await
        {
            Ok(_) => {
                info!(branches_path, "created parent directory for branch");
            }
            Err(e) => {
                let err_str = e.to_string();
                // Ignore "already exists" — the directory is already there
                if err_str.contains("already exists") || err_str.contains("E160020") {
                    debug!(branches_path, "parent directory already exists");
                } else {
                    return Err(e);
                }
            }
        }

        let message = format!(
            "Create branch {} from {} at r{}",
            name, source_path, source_rev
        );
        self.run_svn(&["copy", "-r", &rev_str, &src_url, &dest_url, "-m", &message])
            .await?;
        info!(name, source_rev, "created branch");
        Ok(())
    }

    /// Delete an SVN branch (or directory) from the repository.
    #[instrument(skip(self), fields(url = %self.url))]
    pub async fn delete_branch(
        &self,
        branch_path: &str,
    ) -> Result<(), SvnError> {
        if branch_path.contains("..") {
            return Err(SvnError::CommandFailed {
                exit_code: 1,
                stderr: "path traversal ('..') not allowed in branch path".to_string(),
            });
        }
        let branch_url = format!("{}/{}", self.url, branch_path);
        let message = format!("Delete branch {}", branch_path);
        match self.run_svn(&["rm", &branch_url, "-m", &message]).await {
            Ok(_) => {
                info!(branch_path, "deleted SVN branch");
                Ok(())
            }
            Err(e) => {
                let err_str = e.to_string();
                // Treat "not found" as success — branch may already be gone
                if err_str.contains("E200009") || err_str.contains("non-existent") || err_str.contains("E160013") {
                    info!(branch_path, "SVN branch already deleted or not found");
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    #[instrument(skip(self), fields(url = %self.url, rev))]
    pub async fn export(&self, path: &str, rev: i64, dest: &Path) -> Result<(), SvnError> {
        let src_url = if path.is_empty() {
            self.url.clone()
        } else {
            format!("{}/{}", self.url, path)
        };
        let rev_str = rev.to_string();
        let dest_str = dest.to_string_lossy().to_string();
        self.run_svn(&["export", "--force", "-r", &rev_str, &src_url, &dest_str])
            .await?;
        info!(dest = %dest.display(), rev, "svn export completed");
        Ok(())
    }

    /// Export at a given depth (e.g. "immediates" for top-level only).
    #[instrument(skip(self), fields(url = %self.url, rev, depth))]
    pub async fn export_depth(
        &self,
        path: &str,
        rev: i64,
        dest: &Path,
        depth: &str,
    ) -> Result<(), SvnError> {
        let src_url = if path.is_empty() {
            self.url.clone()
        } else {
            format!("{}/{}", self.url, path)
        };
        let rev_str = rev.to_string();
        let dest_str = dest.to_string_lossy().to_string();
        self.run_svn(&[
            "export", "--force", "-r", &rev_str, "--depth", depth, &src_url, &dest_str,
        ])
        .await?;
        debug!(dest = %dest.display(), rev, depth, "svn export (depth) completed");
        Ok(())
    }

    // -- Working copy methods (personal branch mode) -------------------------

    /// Run `svn update` on a working copy.
    #[instrument(skip(self), fields(path = %path.display()))]
    pub async fn update(&self, path: &Path) -> Result<String, SvnError> {
        let output = self.run_svn_in_dir(path, &["update"]).await?;
        info!(path = %path.display(), "svn update completed");
        Ok(output)
    }

    /// Run `svn revert` on files in a working copy.
    #[instrument(skip(self, files), fields(path = %path.display()))]
    pub async fn revert_files(&self, path: &Path, files: &[&str]) -> Result<(), SvnError> {
        if files.is_empty() {
            return Ok(());
        }
        let mut args = vec!["revert"];
        args.extend_from_slice(files);
        self.run_svn_in_dir(path, &args).await?;
        debug!(count = files.len(), "svn revert completed");
        Ok(())
    }

    /// Run `svn add` on files in a working copy.
    #[instrument(skip(self, files), fields(path = %path.display()))]
    pub async fn add(&self, path: &Path, files: &[&str]) -> Result<(), SvnError> {
        if files.is_empty() {
            return Ok(());
        }
        for file in files {
            self.run_svn_in_dir(path, &["add", "--force", "--parents", file]).await?;
        }
        debug!(count = files.len(), "svn add completed");
        Ok(())
    }

    /// Add files robustly: find the topmost NEW directory for each file
    /// and add it recursively with `svn add --force`. This avoids E150000
    /// parent-node errors because we always add from a versioned parent
    /// downward, letting SVN handle the entire subtree at once.
    pub async fn add_with_retry(&self, wc_path: &Path, files: &[&str]) -> Result<(), SvnError> {
        if files.is_empty() {
            return Ok(());
        }

        // Find topmost new directories that need adding.
        // A "new" directory is one that exists on disk but whose parent
        // IS versioned in SVN (i.e., the parent is part of the checkout).
        // We detect this by checking `svn info` on each ancestor.
        let mut top_dirs_added: std::collections::HashSet<String> = std::collections::HashSet::new();

        for file in files {
            let file_path = std::path::Path::new(file);

            // Walk up from the file's parent to find the topmost unversioned dir
            let mut topmost_new: Option<String> = None;
            let mut cur = file_path.parent();
            while let Some(p) = cur {
                if p.as_os_str().is_empty() { break; }
                let dir_str = p.to_string_lossy().to_string();
                // Check if this directory is versioned by running svn info
                let info_result = self.run_svn_in_dir(
                    wc_path, &["info", &dir_str]
                ).await;
                if info_result.is_err() {
                    // Not versioned — this might be the topmost new dir
                    topmost_new = Some(dir_str);
                } else {
                    // Versioned — stop walking up
                    break;
                }
                cur = p.parent();
            }

            if let Some(ref top_dir) = topmost_new {
                if !top_dirs_added.contains(top_dir) {
                    debug!(dir = %top_dir, "adding new directory tree to SVN");
                    // Add the topmost new directory — SVN will recursively
                    // add everything inside it since --force doesn't skip
                    // unversioned contents.
                    self.run_svn_in_dir(
                        wc_path, &["add", "--force", top_dir]
                    ).await?;
                    top_dirs_added.insert(top_dir.clone());
                }
                // File is already added by the recursive dir add
            } else {
                // Parent is versioned, just add the file directly
                self.run_svn_in_dir(
                    wc_path, &["add", "--force", file]
                ).await?;
            }
        }
        debug!(count = files.len(), "svn add completed");
        Ok(())
    }

    /// Run `svn rm` on files in a working copy.
    #[instrument(skip(self, files), fields(path = %path.display()))]
    pub async fn rm(&self, path: &Path, files: &[&str]) -> Result<(), SvnError> {
        if files.is_empty() {
            return Ok(());
        }
        let mut args = vec!["rm", "--force"];
        args.extend(files);
        self.run_svn_in_dir(path, &args).await?;
        debug!(count = files.len(), "svn rm completed");
        Ok(())
    }

    /// Get the working copy status (modified, added, deleted files).
    #[instrument(skip(self), fields(path = %path.display()))]
    pub async fn status(&self, path: &Path) -> Result<String, SvnError> {
        self.run_svn_in_dir(path, &["status"]).await
    }

    /// Get the content of a file at a specific revision.
    ///
    /// Uses peg-revision syntax (`url@N`) so the path is resolved *in
    /// revision N*, not in HEAD. Without the peg, `svn cat -r N url`
    /// asks the server "find `url` as of HEAD, then give me its content
    /// at rev N" — which fails for files that were renamed or deleted
    /// in any later revision. With the peg, the server treats `url` as
    /// a path that existed at rev N, which is what we actually want.
    #[instrument(skip(self), fields(file_path = %file_path, rev))]
    pub async fn cat(&self, file_path: &str, rev: i64) -> Result<String, SvnError> {
        let rev_str = rev.to_string();
        let base_url = if file_path.starts_with("http://") || file_path.starts_with("https://") {
            file_path.to_string()
        } else {
            format!("{}/{}", self.url, file_path)
        };
        // Append the peg revision. SVN URLs can contain '@' in path
        // segments, but the peg must be the last '@'. Escape any
        // existing '@' by doubling: svn accepts `path@@REV` to mean
        // "path named `path@` at peg REV" when the path has a literal
        // '@'. Simpler: append `@REV` — if the path has no '@' this is
        // unambiguous, and we don't expect SVN paths with '@' in them.
        let peg_url = format!("{}@{}", base_url, rev_str);
        self.run_svn(&["cat", &peg_url]).await
    }

    // -- Internal helpers ----------------------------------------------------

    async fn run_svn(&self, args: &[&str]) -> Result<String, SvnError> {
        let mut cmd = Command::new("svn");
        cmd.args(args)
            .arg("--non-interactive")
            .arg("--no-auth-cache")
            .arg("--username")
            .arg(&self.username)
            .arg("--password")
            .arg(&self.password)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        debug!(cmd = ?format!("svn {}", args.join(" ")), "running svn command");
        let output = tokio::time::timeout(SVN_COMMAND_TIMEOUT, cmd.output())
            .await
            .map_err(|_| {
                SvnError::NetworkError(format!(
                    "svn command timed out after {}s: svn {}",
                    SVN_COMMAND_TIMEOUT.as_secs(),
                    args.join(" ")
                ))
            })?
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    SvnError::BinaryNotFound("svn".into())
                } else {
                    SvnError::IoError(e)
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);
            warn!(exit_code, %stderr, "svn command failed");
            return Err(SvnError::CommandFailed { exit_code, stderr });
        }
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        trace!(
            cmd = args[0],
            exit_code = output.status.code().unwrap_or(0),
            stdout_len = stdout.len(),
            "svn command completed"
        );
        Ok(stdout)
    }

    /// Public wrapper for running SVN commands in a working copy directory.
    pub async fn run_svn_in_dir_public(&self, dir: &Path, args: &[&str]) -> Result<String, SvnError> {
        self.run_svn_in_dir(dir, args).await
    }

    async fn run_svn_in_dir(&self, dir: &Path, args: &[&str]) -> Result<String, SvnError> {
        let mut cmd = Command::new("svn");
        cmd.current_dir(dir)
            .args(args)
            .arg("--non-interactive")
            .arg("--no-auth-cache")
            .arg("--username")
            .arg(&self.username)
            .arg("--password")
            .arg(&self.password)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        debug!(cmd = ?format!("svn {} (in {})", args.join(" "), dir.display()), "running svn command in dir");
        let output = tokio::time::timeout(SVN_COMMAND_TIMEOUT, cmd.output())
            .await
            .map_err(|_| {
                SvnError::NetworkError(format!(
                    "svn command timed out after {}s: svn {} (in {})",
                    SVN_COMMAND_TIMEOUT.as_secs(),
                    args.join(" "),
                    dir.display()
                ))
            })?
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    SvnError::BinaryNotFound("svn".into())
                } else {
                    SvnError::IoError(e)
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);
            warn!(exit_code, %stderr, "svn command failed");
            return Err(SvnError::CommandFailed { exit_code, stderr });
        }
        // Combine stdout and stderr — some SVN operations (especially commit)
        // output the "Committed revision N" line to stderr, not stdout.
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if stdout.is_empty() && !stderr.is_empty() {
            debug!(stderr = %stderr, "svn command produced no stdout, using stderr");
            Ok(format!("{}\n{}", stdout, stderr))
        } else {
            Ok(stdout)
        }
    }
}

fn parse_committed_revision(output: &str) -> Option<i64> {
    // Primary: match "Committed revision NNN" (case-insensitive)
    for line in output.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();
        if lower.contains("committed revision") || lower.contains("committedrevision") {
            if let Some(pos) = lower.find("revision") {
                let after = &trimmed[pos + 8..]; // "revision" = 8 chars
                let num_str: String = after
                    .chars()
                    .skip_while(|c| !c.is_ascii_digit())
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(rev) = num_str.parse::<i64>() {
                    return Some(rev);
                }
            }
        }
    }
    // Fallback: any line containing "revision N" pattern
    for line in output.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();
        if let Some(pos) = lower.find("revision ") {
            let after = &trimmed[pos + 9..];
            let num_str: String = after
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !num_str.is_empty() {
                if let Ok(rev) = num_str.parse::<i64>() {
                    return Some(rev);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_committed_revision() {
        assert_eq!(
            parse_committed_revision("Committed revision 42.\n"),
            Some(42)
        );
        assert_eq!(parse_committed_revision("No output"), None);
    }

    #[test]
    fn test_client_construction() {
        let client = SvnClient::new("https://svn.example.com/repo", "user", "pass");
        assert_eq!(client.url(), "https://svn.example.com/repo");
    }
}
