//! Asynchronous SVN CLI client.

use std::fmt;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tracing::{debug, info, instrument, trace, warn};

use super::operand::{literal_path, literal_url, local_destination, repository_url};
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
    cancel: Option<Arc<AtomicBool>>,
    refresh_config: Option<Arc<tempfile::TempDir>>,
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
    fn svn_command(&self) -> Command {
        #[cfg(feature = "reliability-fixture")]
        if self.cancel.is_some() {
            if let (Ok(root), Ok(binary)) = (
                std::env::var("REPOSYNC_FIXTURE_ROOT"),
                std::env::var("REPOSYNC_IMPORT_SVN_BINARY"),
            ) {
                if let (Ok(root), Ok(binary)) = (
                    Path::new(&root).canonicalize(),
                    Path::new(&binary).canonicalize(),
                ) {
                    if binary.starts_with(root) {
                        // The isolated runtime's fixture tmpfs can deny direct
                        // execution. The shell reads only this sealed script;
                        // production still invokes the SVN binary directly.
                        let mut command = Command::new("sh");
                        command.arg(binary);
                        return command;
                    }
                }
            }
        }
        Command::new("svn")
    }
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
            cancel: None,
            refresh_config: None,
        };
        info!(url = %client.url, username = %client.username, "created SvnClient");
        client
    }

    /// Refresh clients never read the daemon user's SVN configuration, cached
    /// credentials or platform password stores. The private directory remains
    /// alive until the last client clone drops; ordinary clients are unchanged.
    pub(crate) fn new_isolated_refresh(
        url: &str,
        username: &str,
        password: Option<&str>,
    ) -> Result<Self, SvnError> {
        if password.is_some_and(|value| value.is_empty())
            || password.is_some() == username.is_empty()
        {
            return Err(SvnError::CommandFailed {
                exit_code: -1,
                stderr: "refresh requires a complete explicit SVN credential or anonymous access"
                    .into(),
            });
        }
        let config = tempfile::Builder::new()
            .prefix("reposync-refresh-auth-")
            .tempdir()?;
        std::fs::write(
            config.path().join("config"),
            "[auth]\npassword-stores =\nstore-passwords = no\nstore-auth-creds = no\n",
        )?;
        std::fs::write(
            config.path().join("servers"),
            "[global]\nstore-passwords = no\nstore-plaintext-passwords = no\nstore-ssl-client-cert-pp = no\nstore-ssl-client-cert-pp-plaintext = no\n",
        )?;
        let mut client = Self::new(url, username, password.unwrap_or(""));
        client.refresh_config = Some(Arc::new(config));
        Ok(client)
    }

    fn apply_refresh_configuration(&self, command: &mut Command) {
        if let Some(config) = &self.refresh_config {
            command
                .arg("--config-dir")
                .arg(config.path())
                .arg("--config-option")
                .arg("config:auth:password-stores=");
        }
    }

    fn command_stderr(&self, bytes: &[u8]) -> String {
        let stderr = String::from_utf8_lossy(bytes).to_string();
        if !self.password.is_empty() {
            stderr.replace(&self.password, "[REDACTED]")
        } else {
            stderr
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn has_refresh_isolation(&self) -> bool {
        self.refresh_config.is_some()
    }

    pub(crate) fn refresh_ancestor_client(&self, url: &str) -> Result<Self, SvnError> {
        let refused = || SvnError::LineageRefused {
            reason: "unproved_svn_ancestor_visibility".into(),
            detail: "ancestor observation requires the same isolated source authority".into(),
        };
        let source = reqwest::Url::parse(&self.url).map_err(|_| refused())?;
        let ancestor = reqwest::Url::parse(url).map_err(|_| refused())?;
        let parent = ancestor.path().trim_end_matches('/');
        if !self.has_refresh_isolation()
            || source.scheme() != ancestor.scheme()
            || source.host_str() != ancestor.host_str()
            || source.port_or_known_default() != ancestor.port_or_known_default()
            || !ancestor.username().is_empty()
            || ancestor.password().is_some()
            || ancestor.query().is_some()
            || ancestor.fragment().is_some()
            || !source
                .path()
                .strip_prefix(parent)
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
        {
            return Err(refused());
        }
        let mut client = self.clone();
        client.url = url.to_owned();
        Ok(client)
    }

    pub(crate) async fn direct_properties_at_rev(&self, revision: i64) -> Result<String, SvnError> {
        if revision < 1 {
            return Err(SvnError::RevisionNotFound(revision));
        }
        self.run_svn(&[
            "proplist",
            "--xml",
            "--verbose",
            "--depth",
            "empty",
            "-r",
            &revision.to_string(),
            "--",
            &literal_url(&self.url, Some(revision))?,
        ])
        .await
    }

    /// Import-scoped cancellation; ordinary sync clients remain unchanged.
    pub fn with_cancel_signal(mut self, signal: Arc<AtomicBool>) -> Self {
        self.cancel = Some(signal);
        self
    }

    /// Update the password at runtime (credential hot-reload from DB).
    pub fn set_password(&mut self, password: impl Into<String>) {
        self.password = password.into();
    }

    /// Test-only marker for fixture credential isolation proofs.
    #[doc(hidden)]
    pub fn fixture_password_marker(&self) -> &str {
        &self.password
    }

    /// Update the username at runtime (credential hot-reload from DB).
    pub fn set_username(&mut self, username: impl Into<String>) {
        self.username = username.into();
    }

    #[instrument(skip(self), fields(url = %self.url))]
    pub async fn info(&self) -> Result<SvnInfo, SvnError> {
        let output = self
            .run_svn(&["info", "--xml", "--", &literal_url(&self.url, None)?])
            .await?;
        parse_svn_info(&output)
    }

    /// Last changed revision of this URL. Repository HEAD is not path identity.
    pub async fn last_changed_revision(&self) -> Result<i64, SvnError> {
        let output = self
            .run_svn(&["info", "--xml", "--", &literal_url(&self.url, None)?])
            .await?;
        super::parser::parse_svn_last_changed_revision(&output)
    }

    /// `svn info` at a peg revision so identity is the selected snapshot,
    /// not whatever HEAD has become.
    #[instrument(skip(self), fields(url = %self.url, rev))]
    pub async fn info_at_rev(&self, rev: i64) -> Result<SvnInfo, SvnError> {
        if rev < 1 {
            return Err(SvnError::RevisionNotFound(rev));
        }
        let peg_url = literal_url(&self.url, Some(rev))?;
        let output = self.run_svn(&["info", "--xml", "--", &peg_url]).await?;
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
            .run_svn(&[
                "log",
                "--xml",
                "--verbose",
                "-r",
                &rev_range,
                "--",
                &literal_url(&self.url, None)?,
            ])
            .await?;
        parse_svn_log(&output)
    }

    /// A complete bounded verbose observation for checkpoint proof. Unlike
    /// ordinary display/replay parsing, any malformed entry or path refuses.
    pub(crate) async fn strict_log_range(
        &self,
        start_rev: i64,
        end_rev: i64,
    ) -> Result<Vec<SvnLogEntry>, SvnError> {
        if start_rev < 1 || end_rev < start_rev {
            return Err(SvnError::XmlParseError("invalid proof log range".into()));
        }
        let range = format!("{start_rev}:{end_rev}");
        let target = literal_url(&self.url, Some(end_rev))?;
        let output = self
            .run_svn(&["log", "--xml", "--verbose", "-r", &range, "--", &target])
            .await?;
        let entries = super::parser::parse_svn_copy_history(&output)?;
        let mut revisions = std::collections::BTreeSet::new();
        if entries.iter().any(|entry| {
            entry.revision < start_rev
                || entry.revision > end_rev
                || !revisions.insert(entry.revision)
        }) {
            return Err(SvnError::XmlParseError(
                "out-of-range or duplicate proof revision".into(),
            ));
        }
        Ok(entries)
    }

    /// Complete history of the selected path incarnation, stopping at its
    /// originating copy rather than following an older source path. No fixed
    /// revision window or limit may truncate the origin used for approval.
    pub async fn copy_history_at_rev(
        &self,
        peg_revision: i64,
    ) -> Result<Vec<SvnLogEntry>, SvnError> {
        if peg_revision < 1 {
            return Err(SvnError::RevisionNotFound(peg_revision));
        }
        let range = format!("{peg_revision}:1");
        let target = literal_url(&self.url, Some(peg_revision))?;
        let output = self
            .run_svn(&[
                "log",
                "--xml",
                "--verbose",
                "--stop-on-copy",
                "-r",
                &range,
                "--",
                &target,
            ])
            .await?;
        super::parser::parse_svn_copy_history(&output)
    }

    #[instrument(skip(self), fields(url = %self.url, rev))]
    pub async fn diff(&self, rev: i64) -> Result<Vec<SvnDiffEntry>, SvnError> {
        if rev < 1 {
            return Err(SvnError::RevisionNotFound(rev));
        }
        let rev_range = format!("{}:{}", rev - 1, rev);
        let output = self
            .run_svn(&[
                "diff",
                "--summarize",
                "--xml",
                "-r",
                &rev_range,
                "--",
                &literal_url(&self.url, Some(rev))?,
            ])
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
        let peg_url = literal_url(&self.url, Some(rev))?;
        self.run_svn(&["diff", "-r", &rev_range, "--", &peg_url])
            .await
    }

    /// Inspect file-content changes without SVN properties when deciding
    /// whether a revision has any Git-representable target delta.
    pub async fn diff_content_only(&self, rev: i64) -> Result<String, SvnError> {
        if rev < 1 {
            return Err(SvnError::RevisionNotFound(rev));
        }
        let rev_range = format!("{}:{}", rev - 1, rev);
        let peg_url = literal_url(&self.url, Some(rev))?;
        self.run_svn(&[
            "diff",
            "--ignore-properties",
            "-r",
            &rev_range,
            "--",
            &peg_url,
        ])
        .await
    }

    #[instrument(skip(self), fields(url = %self.url, rev))]
    pub async fn checkout(&self, path: &Path, rev: i64) -> Result<(), SvnError> {
        let rev_str = rev.to_string();
        let path_str = local_destination(path)?;
        self.run_svn(&[
            "checkout",
            "--ignore-externals",
            "-r",
            &rev_str,
            "--",
            &literal_url(&self.url, Some(rev))?,
            path_str,
        ])
        .await?;
        info!(path = %path.display(), rev, "svn checkout completed");
        Ok(())
    }

    /// Isolated refresh preparation never follows externals or a moving URL.
    pub async fn checkout_pinned_without_externals(
        &self,
        path: &Path,
        rev: i64,
    ) -> Result<(), SvnError> {
        if rev < 1 {
            return Err(SvnError::RevisionNotFound(rev));
        }
        let destination = path.to_str().ok_or_else(|| SvnError::WorkingCopyError {
            path: path.display().to_string(),
            detail: "refresh working directory must be UTF-8".into(),
        })?;
        self.run_svn(&[
            "checkout",
            "--ignore-externals",
            "-r",
            &rev.to_string(),
            "--",
            &literal_url(&self.url, Some(rev))?,
            destination,
        ])
        .await?;
        Ok(())
    }

    /// A fresh visibility observation always requests the complete subtree.
    pub async fn checkout_full_pinned_without_externals(
        &self,
        path: &Path,
        rev: i64,
    ) -> Result<(), SvnError> {
        if rev < 1 {
            return Err(SvnError::RevisionNotFound(rev));
        }
        let destination = path.to_str().ok_or_else(|| SvnError::WorkingCopyError {
            path: path.display().to_string(),
            detail: "refresh working directory must be UTF-8".into(),
        })?;
        self.run_svn(&[
            "checkout",
            "--ignore-externals",
            "--depth",
            "infinity",
            "-r",
            &rev.to_string(),
            "--",
            &literal_url(&self.url, Some(rev))?,
            destination,
        ])
        .await?;
        Ok(())
    }

    /// Full property values, including inherited properties, at the selected peg.
    pub async fn tree_properties_at_rev(&self, rev: i64) -> Result<String, SvnError> {
        if rev < 1 {
            return Err(SvnError::RevisionNotFound(rev));
        }
        self.run_svn(&[
            "proplist",
            "--xml",
            "--verbose",
            "--recursive",
            "--show-inherited-props",
            "-r",
            &rev.to_string(),
            "--",
            &literal_url(&self.url, Some(rev))?,
        ])
        .await
    }

    /// Checkout the SVN repository at HEAD into the given directory.
    #[instrument(skip(self), fields(url = %self.url))]
    pub async fn checkout_head(&self, path: &Path) -> Result<(), SvnError> {
        let path_str = local_destination(path)?;
        self.run_svn(&[
            "checkout",
            "--ignore-externals",
            "--",
            &literal_url(&self.url, None)?,
            path_str,
        ])
        .await?;
        info!(path = %path.display(), "svn checkout (HEAD) completed");
        Ok(())
    }

    #[instrument(skip(self, message), fields(path = %path.display()))]
    pub async fn commit(&self, path: &Path, message: &str, _author: &str) -> Result<i64, SvnError> {
        self.require_supported_working_copy(path).await?;
        let path_str = local_destination(path)?;
        let output = self
            .run_svn_in_dir_with_input(
                path,
                &[
                    "commit",
                    "--file",
                    "-",
                    "--encoding",
                    "UTF-8",
                    "--",
                    &format!("{path_str}@"),
                ],
                Some(crate::commit_message::input(message)?),
            )
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
            "--",
            prop_name,
            prop_value,
            &literal_url(&self.url, None)?,
        ])
        .await?;
        debug!(rev, prop_name, "set revision property");
        Ok(())
    }

    #[instrument(skip(self), fields(url = %self.url))]
    pub async fn list_branches(&self, branches_path: &str) -> Result<Vec<String>, SvnError> {
        let branches_url = literal_url(&repository_url(&self.url, branches_path)?, None)?;
        let output = self
            .run_svn(&["list", "--xml", "--", &branches_url])
            .await?;
        let document = roxmltree::Document::parse(&output)
            .map_err(|e| SvnError::XmlParseError(e.to_string()))?;
        let branches: Vec<String> = document
            .descendants()
            .filter(|n| n.has_tag_name("entry"))
            .map(|n| {
                n.children()
                    .find(|n| n.has_tag_name("name"))
                    .and_then(|n| n.text())
                    .map(str::to_owned)
                    .ok_or_else(|| SvnError::XmlParseError("missing SVN list name".into()))
            })
            .collect::<Result<_, _>>()?;
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
        let src_url = literal_url(&repository_url(&self.url, source_path)?, Some(source_rev))?;
        let branch = format!("{branches_path}/{name}");
        let dest_url = literal_url(&repository_url(&self.url, &branch)?, None)?;
        let rev_str = source_rev.to_string();

        // Ensure the parent directory exists (e.g., branches/dev/ for branches/dev/james-wilson).
        // svn copy fails if intermediate directories are missing.
        let parent_url = literal_url(&repository_url(&self.url, branches_path)?, None)?;
        match self
            .run_svn(&[
                "mkdir",
                "--parents",
                "-m",
                &format!("Create directory {}", branches_path),
                "--",
                &parent_url,
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
        self.run_svn(&[
            "copy", "-r", &rev_str, "-m", &message, "--", &src_url, &dest_url,
        ])
        .await?;
        info!(name, source_rev, "created branch");
        Ok(())
    }

    /// Delete an SVN branch (or directory) from the repository.
    #[instrument(skip(self), fields(url = %self.url))]
    pub async fn delete_branch(&self, branch_path: &str) -> Result<(), SvnError> {
        let branch_url = literal_url(&repository_url(&self.url, branch_path)?, None)?;
        let message = format!("Delete branch {}", branch_path);
        match self
            .run_svn(&["rm", "-m", &message, "--", &branch_url])
            .await
        {
            Ok(_) => {
                info!(branch_path, "deleted SVN branch");
                Ok(())
            }
            Err(e) => {
                let err_str = e.to_string();
                // Treat "not found" as success — branch may already be gone
                if err_str.contains("E200009")
                    || err_str.contains("non-existent")
                    || err_str.contains("E160013")
                {
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
        let src_url = repository_url(&self.url, path)?;
        self.require_supported_export_nodes(&src_url, rev).await?;
        let rev_str = rev.to_string();
        let dest_str = local_destination(dest)?;
        // Peg the path at `rev` so export stays on the pinned snapshot even
        // if the live URL is later deleted, copied, or advanced.
        let peg_url = literal_url(&src_url, Some(rev))?;
        self.run_svn(&[
            "export",
            "--force",
            "--ignore-externals",
            "-r",
            &rev_str,
            "--",
            &peg_url,
            dest_str,
        ])
        .await?;
        info!(dest = %dest.display(), rev, "svn export completed");
        Ok(())
    }

    /// Enumerate properties on one file at a pinned revision. The no-target
    /// verifier conservatively refuses any property until its mapping is
    /// explicitly defined; export alone does not include property evidence.
    pub async fn file_properties_at_rev(&self, path: &str, rev: i64) -> Result<String, SvnError> {
        let url = literal_url(&repository_url(&self.url, path)?, Some(rev))?;
        self.run_svn(&["proplist", "--xml", "-r", &rev.to_string(), "--", &url])
            .await
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
        let src_url = repository_url(&self.url, path)?;
        self.require_supported_export_nodes(&src_url, rev).await?;
        let rev_str = rev.to_string();
        let dest_str = local_destination(dest)?;
        self.run_svn(&[
            "export",
            "--force",
            "--ignore-externals",
            "-r",
            &rev_str,
            "--depth",
            depth,
            "--",
            &literal_url(&src_url, Some(rev))?,
            dest_str,
        ])
        .await?;
        debug!(dest = %dest.display(), rev, depth, "svn export (depth) completed");
        Ok(())
    }

    // -- Working copy methods (personal branch mode) -------------------------

    /// Run `svn update` on a working copy.
    #[instrument(skip(self), fields(path = %path.display()))]
    pub async fn update(&self, path: &Path) -> Result<String, SvnError> {
        let output = self
            .run_svn_in_dir(path, &["update", "--ignore-externals", "--", ".@"])
            .await?;
        info!(path = %path.display(), "svn update completed");
        Ok(output)
    }

    /// Run `svn revert` on files in a working copy.
    #[instrument(skip(self, files), fields(path = %path.display()))]
    pub async fn revert_files(&self, path: &Path, files: &[&str]) -> Result<(), SvnError> {
        if files.is_empty() {
            return Ok(());
        }
        let confined = super::working_copy::ConfinedWorkingCopy::open(path)?;
        for file in files {
            confined.inspect_operand(file, true)?;
        }
        let encoded = files
            .iter()
            .map(|p| literal_path(p))
            .collect::<Result<Vec<_>, _>>()?;
        let mut args = vec!["revert", "--"];
        args.extend(encoded.iter().map(String::as_str));
        self.run_svn_in_dir(path, &args).await?;
        debug!(count = files.len(), "svn revert completed");
        Ok(())
    }

    /// Revert every local change under a working-copy root.
    #[instrument(skip(self), fields(path = %path.display()))]
    pub async fn revert_recursive(&self, path: &Path) -> Result<(), SvnError> {
        self.run_svn_in_dir(path, &["revert", "-R", "--", ".@"])
            .await?;
        debug!(path = %path.display(), "svn revert -R completed");
        Ok(())
    }

    /// Run `svn add` on files in a working copy.
    #[instrument(skip(self, files), fields(path = %path.display()))]
    pub async fn add(&self, path: &Path, files: &[&str]) -> Result<(), SvnError> {
        if files.is_empty() {
            return Ok(());
        }
        let confined = super::working_copy::ConfinedWorkingCopy::open(path)?;
        for file in files {
            confined.inspect_operand(file, true)?;
        }
        for file in files {
            self.run_svn_in_dir(
                path,
                &[
                    "add",
                    "--force",
                    "--parents",
                    "--depth",
                    "empty",
                    "--",
                    &literal_path(file)?,
                ],
            )
            .await?;
        }
        debug!(count = files.len(), "svn add completed");
        Ok(())
    }

    /// Add files robustly: find the topmost NEW directory for each file
    /// and add it recursively with `svn add --force`. This avoids E150000
    /// parent-node errors because we always add from a versioned parent
    /// downward, letting SVN handle the entire subtree at once.
    pub async fn add_with_retry(&self, wc_path: &Path, files: &[&str]) -> Result<(), SvnError> {
        // --parents stages only ancestors; --depth empty never recursively
        // admits neighboring files outside the supplied projection.
        self.add(wc_path, files).await
    }

    /// Run `svn rm` on files in a working copy.
    #[instrument(skip(self, files), fields(path = %path.display()))]
    pub async fn rm(&self, path: &Path, files: &[&str]) -> Result<(), SvnError> {
        if files.is_empty() {
            return Ok(());
        }
        let confined = super::working_copy::ConfinedWorkingCopy::open(path)?;
        for file in files {
            confined.inspect_file(file)?;
        }
        let encoded = files
            .iter()
            .map(|p| literal_path(p))
            .collect::<Result<Vec<_>, _>>()?;
        let mut args = vec!["rm", "--force", "--"];
        args.extend(encoded.iter().map(String::as_str));
        self.run_svn_in_dir(path, &args).await?;
        debug!(count = files.len(), "svn rm completed");
        Ok(())
    }

    /// Get the working copy status (modified, added, deleted files).
    #[instrument(skip(self), fields(path = %path.display()))]
    pub async fn status(&self, path: &Path) -> Result<String, SvnError> {
        self.run_svn_in_dir(path, &["status"]).await
    }

    fn require_supported_node_properties(xml: &str) -> Result<(), SvnError> {
        let document =
            roxmltree::Document::parse(xml).map_err(|e| SvnError::XmlParseError(e.to_string()))?;
        if !document.root_element().has_tag_name("properties") {
            return Err(SvnError::XmlParseError(
                "missing SVN node property evidence".into(),
            ));
        }
        for node in document
            .descendants()
            .filter(|n| n.has_tag_name("property"))
        {
            if matches!(
                node.attribute("name"),
                Some("svn:externals" | "svn:special")
            ) {
                return Err(SvnError::WorkingCopyError {
                    path: "[SVN node]".into(),
                    detail:
                        "unsupported SVN external or special node; refusing before content mutation"
                            .into(),
                });
            }
        }
        Ok(())
    }

    async fn require_supported_export_nodes(
        &self,
        url: &str,
        revision: i64,
    ) -> Result<(), SvnError> {
        let target = literal_url(url, Some(revision))?;
        let xml = self
            .run_svn(&[
                "proplist",
                "--xml",
                "--verbose",
                "--recursive",
                "--show-inherited-props",
                "-r",
                &revision.to_string(),
                "--",
                &target,
            ])
            .await?;
        Self::require_supported_node_properties(&xml)
    }

    /// Ignoring externals is containment, not proof that omitted work is safe.
    /// Content writers must explicitly refuse these declared node semantics.
    pub async fn require_supported_working_copy(&self, path: &Path) -> Result<(), SvnError> {
        let xml = self
            .run_svn_in_dir(
                path,
                &[
                    "proplist",
                    "--xml",
                    "--verbose",
                    "--recursive",
                    "--show-inherited-props",
                    "--",
                    ".@",
                ],
            )
            .await?;
        Self::require_supported_node_properties(&xml)
    }

    /// Machine-readable paths; line-oriented display is not a write oracle.
    pub async fn status_entries(
        &self,
        path: &Path,
    ) -> Result<Vec<super::parser::SvnStatusEntry>, SvnError> {
        let xml = self
            .run_svn_in_dir(path, &["status", "--xml", "--no-ignore", "--", ".@"])
            .await?;
        super::parser::parse_svn_status_xml(&xml)
    }

    pub async fn add_directory(&self, path: &Path, directory: &str) -> Result<(), SvnError> {
        super::working_copy::ConfinedWorkingCopy::open(path)?.inspect_directory(directory)?;
        self.run_svn_in_dir(
            path,
            &[
                "add",
                "--force",
                "--depth",
                "empty",
                "--",
                &literal_path(directory)?,
            ],
        )
        .await?;
        Ok(())
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
        let base_url = if reqwest::Url::parse(file_path).is_ok() {
            let base = super::operand::checked_url(&self.url)?;
            let target = super::operand::checked_url(file_path)?;
            let prefix = format!("{}/", base.path().trim_end_matches('/'));
            if base.scheme() != target.scheme()
                || base.host_str() != target.host_str()
                || base.port_or_known_default() != target.port_or_known_default()
                || base.username() != target.username()
                || (base.path() != target.path() && !target.path().starts_with(&prefix))
            {
                return Err(SvnError::WorkingCopyError {
                    path: file_path.into(),
                    detail: "cat URL escapes configured SVN source".into(),
                });
            }
            target.to_string()
        } else {
            repository_url(&self.url, file_path)?
        };
        let peg_url = literal_url(&base_url, Some(rev))?;
        self.run_svn(&["cat", "-r", &rev.to_string(), "--", &peg_url])
            .await
    }

    // -- Internal helpers ----------------------------------------------------

    async fn run_svn(&self, args: &[&str]) -> Result<String, SvnError> {
        let mut cmd = self.svn_command();
        let (verb, rest) = args
            .split_first()
            .ok_or_else(|| SvnError::XmlParseError("missing SVN command".into()))?;
        cmd.arg(verb)
            .arg("--non-interactive")
            .arg("--no-auth-cache")
            .arg("--username")
            .arg(&self.username)
            .arg("--password")
            .arg(&self.password)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.apply_refresh_configuration(&mut cmd);
        cmd.args(rest);
        record_svn_diag(&cmd);

        debug!(cmd = ?format!("svn {}", args.join(" ")), "running svn command");
        let output = crate::process::run(cmd, SVN_COMMAND_TIMEOUT, self.cancel.as_ref())
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    SvnError::BinaryNotFound("svn".into())
                } else {
                    SvnError::IoError(e)
                }
            })?;

        if !output.status.success() {
            let stderr = self.command_stderr(&output.stderr);
            let exit_code = output.status.code().unwrap_or(-1);
            warn!(exit_code, %stderr, "svn command failed");
            return Err(SvnError::CommandFailed { exit_code, stderr });
        }
        let stdout = if args
            .iter()
            .take_while(|arg| **arg != "--")
            .any(|arg| *arg == "--xml")
        {
            String::from_utf8(output.stdout)
                .map_err(|_| SvnError::XmlParseError("SVN XML output is not UTF-8".into()))?
        } else {
            String::from_utf8_lossy(&output.stdout).to_string()
        };
        trace!(
            cmd = args[0],
            exit_code = output.status.code().unwrap_or(0),
            stdout_len = stdout.len(),
            "svn command completed"
        );
        Ok(stdout)
    }

    /// Checked compatibility entry for the supported internal WC verbs.
    /// Option, property-value and target roles are rebuilt before invocation;
    /// this is not an arbitrary command/configuration override interface.
    pub async fn run_svn_in_dir_public(
        &self,
        dir: &Path,
        args: &[&str],
    ) -> Result<String, SvnError> {
        let encoded = super::operand::working_copy_arguments(dir, args)?;
        let base = super::operand::checked_url(&self.url)?;
        let scalars = match args.first().copied() {
            Some("propset") => 2,
            Some("propdel") => 1,
            _ => 0,
        };
        let separator = encoded
            .iter()
            .position(|v| v == "--")
            .expect("encoded command has separator");
        for value in encoded.iter().skip(separator + 1 + scalars) {
            if let Ok(target) = reqwest::Url::parse(value) {
                if base.scheme() != target.scheme()
                    || base.host_str() != target.host_str()
                    || base.port_or_known_default() != target.port_or_known_default()
                    || base.username() != target.username()
                {
                    return Err(SvnError::WorkingCopyError {
                        path: value.clone(),
                        detail: "working-copy command cannot change SVN authority".into(),
                    });
                }
            }
        }
        let args: Vec<&str> = encoded.iter().map(String::as_str).collect();
        self.run_svn_in_dir(dir, &args).await
    }

    async fn run_svn_in_dir(&self, dir: &Path, args: &[&str]) -> Result<String, SvnError> {
        self.run_svn_in_dir_with_input(dir, args, None).await
    }

    async fn run_svn_in_dir_with_input(
        &self,
        dir: &Path,
        args: &[&str],
        input: Option<std::fs::File>,
    ) -> Result<String, SvnError> {
        // A working-copy root itself must not be a planted symlink. Hold the
        // verified descriptor through spawn/completion like the other owners.
        let _working_directory = super::working_copy::ConfinedWorkingCopy::open(dir)?;
        match std::fs::symlink_metadata(dir.join(".svn")) {
            Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                return Err(SvnError::WorkingCopyError {
                    path: dir.display().to_string(),
                    detail: "unsupported SVN administrative node".into(),
                })
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut cmd = self.svn_command();
        if let Some(input) = input {
            cmd.stdin(Stdio::from(input));
        }
        let (verb, rest) = args
            .split_first()
            .ok_or_else(|| SvnError::XmlParseError("missing SVN command".into()))?;
        cmd.current_dir(dir)
            .arg(verb)
            .arg("--non-interactive")
            .arg("--no-auth-cache")
            .arg("--username")
            .arg(&self.username)
            .arg("--password")
            .arg(&self.password)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.apply_refresh_configuration(&mut cmd);
        cmd.args(rest);
        record_svn_diag(&cmd);

        debug!(cmd = ?format!("svn {} (in {})", args.join(" "), dir.display()), "running svn command in dir");
        let output = crate::process::run(cmd, SVN_COMMAND_TIMEOUT, self.cancel.as_ref())
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    SvnError::BinaryNotFound("svn".into())
                } else {
                    SvnError::IoError(e)
                }
            })?;

        if !output.status.success() {
            let stderr = self.command_stderr(&output.stderr);
            let exit_code = output.status.code().unwrap_or(-1);
            warn!(exit_code, %stderr, "svn command failed");
            return Err(SvnError::CommandFailed { exit_code, stderr });
        }
        // Combine stdout and stderr — some SVN operations (especially commit)
        // output the "Committed revision N" line to stderr, not stdout.
        let stdout = if args
            .iter()
            .take_while(|arg| **arg != "--")
            .any(|arg| *arg == "--xml")
        {
            String::from_utf8(output.stdout)
                .map_err(|_| SvnError::XmlParseError("SVN XML output is not UTF-8".into()))?
        } else {
            String::from_utf8_lossy(&output.stdout).to_string()
        };
        let stderr = self.command_stderr(&output.stderr);
        if stdout.is_empty() && !stderr.is_empty() {
            debug!(stderr = %stderr, "svn command produced no stdout, using stderr");
            Ok(format!("{}\n{}", stdout, stderr))
        } else {
            Ok(stdout)
        }
    }
}

/// When `REPOSYNC_SVN_DIAG_LOG` is set, append the program and argv.
/// Password values are replaced. The command result is unchanged.
fn record_svn_diag(cmd: &Command) {
    let Ok(path) = std::env::var("REPOSYNC_SVN_DIAG_LOG") else {
        return;
    };
    let cmd = cmd.as_std();
    let records_commit = cmd.get_args().next().is_some_and(|arg| arg == "commit");
    if !records_commit {
        return;
    }
    let mut redacted = Vec::new();
    let mut hide_value = false;
    for arg in cmd.get_args() {
        let arg = arg.to_string_lossy();
        if hide_value {
            redacted.push("[REDACTED]".to_string());
            hide_value = false;
            continue;
        }
        if arg == "--password" {
            redacted.push("--password".to_string());
            hide_value = true;
            continue;
        }
        redacted.push(arg.into_owned());
    }
    let line = format!(
        "program={} argv={redacted:?}\n",
        cmd.get_program().to_string_lossy()
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = file.write_all(line.as_bytes());
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
            let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
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

#[cfg(test)]
#[path = "literal_tests.rs"]
mod literal_tests;
