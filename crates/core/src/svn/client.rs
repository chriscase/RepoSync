//! Asynchronous SVN CLI client.

use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tracing::{debug, info, instrument, warn};

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
        self.run_svn(&["diff", "-r", &rev_range, &self.url]).await
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
        let src_url = format!("{}/{}", self.url, source_path);
        let dest_url = format!("{}/{}/{}", self.url, branches_path, name);
        let rev_str = source_rev.to_string();
        let message = format!(
            "Create branch {} from {} at r{}",
            name, source_path, source_rev
        );
        self.run_svn(&["copy", "-r", &rev_str, &src_url, &dest_url, "-m", &message])
            .await?;
        info!(name, source_rev, "created branch");
        Ok(())
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

    // -- Working copy methods (personal branch mode) -------------------------

    /// Run `svn update` on a working copy.
    #[instrument(skip(self), fields(path = %path.display()))]
    pub async fn update(&self, path: &Path) -> Result<String, SvnError> {
        let output = self.run_svn_in_dir(path, &["update"]).await?;
        info!(path = %path.display(), "svn update completed");
        Ok(output)
    }

    /// Run `svn add` on files in a working copy.
    #[instrument(skip(self, files), fields(path = %path.display()))]
    pub async fn add(&self, path: &Path, files: &[&str]) -> Result<(), SvnError> {
        if files.is_empty() {
            return Ok(());
        }
        let mut args = vec!["add", "--force"];
        args.extend(files);
        self.run_svn_in_dir(path, &args).await?;
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
    #[instrument(skip(self), fields(file_path = %file_path, rev))]
    pub async fn cat(&self, file_path: &str, rev: i64) -> Result<String, SvnError> {
        let rev_str = rev.to_string();
        let url = if file_path.starts_with("http://") || file_path.starts_with("https://") {
            file_path.to_string()
        } else {
            format!("{}/{}", self.url, file_path)
        };
        self.run_svn(&["cat", "-r", &rev_str, &url]).await
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
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
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
