//! Confined cleanup of one repository's RepoSync-owned working tree.
//!
//! The managed root is `{data_dir}/repos/{repo_id}`. Cleanup refuses symlink
//! roots, path traversal, and any directory that is not a direct child of the
//! canonical managed root. Nested symlinks are unlinked as links and are not
//! followed. Sibling directories and the managed root itself are left in place.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Why a local cleanup path was refused or could not be removed.
#[derive(Debug)]
pub struct OwnedPathError {
    pub message: String,
}

impl std::fmt::Display for OwnedPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OwnedPathError {}

fn path_err(message: impl Into<String>) -> OwnedPathError {
    OwnedPathError {
        message: message.into(),
    }
}

/// Repository ids become a single path component. Reject traversal and separators.
pub fn validate_repo_id(repo_id: &str) -> Result<(), OwnedPathError> {
    if repo_id.is_empty()
        || repo_id.len() > 128
        || repo_id == "."
        || repo_id == ".."
        || !repo_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(path_err(
            "repository id is not a single safe path component under the managed root",
        ));
    }
    Ok(())
}

fn strict_child(parent: &Path, child: &Path) -> bool {
    child.parent() == Some(parent) && child.starts_with(parent) && child != parent
}

fn canonical_dir(path: &Path) -> Result<PathBuf, OwnedPathError> {
    path.canonicalize()
        .map_err(|error| path_err(format!("cannot canonicalize {}: {error}", path.display())))
}

/// Delete `{data_dir}/repos/{repo_id}` when it is a real directory or file
/// owned by that managed root. A missing path is success. A symlink at the
/// managed path is an error and is not followed.
pub fn remove_owned_repo_tree(data_dir: &Path, repo_id: &str) -> Result<(), OwnedPathError> {
    validate_repo_id(repo_id)?;
    let data_canon = canonical_dir(data_dir)?;
    let repos = data_dir.join("repos");
    let target = repos.join(repo_id);
    let meta = match fs::symlink_metadata(&target) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(path_err(format!(
                "cannot inspect owned path {}: {error}",
                target.display()
            )))
        }
    };
    if meta.file_type().is_symlink() {
        return Err(path_err(format!(
            "refusing to follow symlink at managed path {}",
            target.display()
        )));
    }
    if !repos.exists() {
        return Err(path_err(
            "managed repos root disappeared while inspecting an owned path",
        ));
    }
    let repos_meta = fs::symlink_metadata(&repos).map_err(|error| {
        path_err(format!(
            "cannot inspect managed repos root {}: {error}",
            repos.display()
        ))
    })?;
    if repos_meta.file_type().is_symlink() {
        return Err(path_err(
            "refusing to follow a symlink at the managed repos root",
        ));
    }
    let repos_canon = canonical_dir(&repos)?;
    if !repos_canon.starts_with(&data_canon) {
        return Err(path_err("managed repos root is outside the data directory"));
    }
    let target_canon = canonical_dir(&target)?;
    if !strict_child(&repos_canon, &target_canon)
        || target_canon.file_name().and_then(|n| n.to_str()) != Some(repo_id)
    {
        return Err(path_err(format!(
            "owned path {} escaped the managed repos root",
            target.display()
        )));
    }
    if meta.is_dir() {
        remove_tree_nofollow(&target_canon, &repos_canon)?;
    } else {
        fs::remove_file(&target_canon).map_err(|error| {
            path_err(format!(
                "failed to remove owned file {}: {error}",
                target_canon.display()
            ))
        })?;
    }
    Ok(())
}

fn remove_tree_nofollow(dir: &Path, repos_root: &Path) -> Result<(), OwnedPathError> {
    if !dir.starts_with(repos_root) || dir == repos_root {
        return Err(path_err(format!(
            "refusing to delete {} outside the managed repos root",
            dir.display()
        )));
    }
    let entries = fs::read_dir(dir).map_err(|error| {
        path_err(format!(
            "cannot read owned directory {}: {error}",
            dir.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            path_err(format!(
                "cannot read entry under {}: {error}",
                dir.display()
            ))
        })?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)
            .map_err(|error| path_err(format!("cannot inspect {}: {error}", path.display())))?;
        if meta.file_type().is_symlink() {
            fs::remove_file(&path).map_err(|error| {
                path_err(format!(
                    "failed to unlink symlink {} without following it: {error}",
                    path.display()
                ))
            })?;
            continue;
        }
        if meta.is_dir() {
            let canon = canonical_dir(&path)?;
            if !canon.starts_with(dir) || canon == repos_root {
                return Err(path_err(format!(
                    "refusing escaped directory {}",
                    path.display()
                )));
            }
            remove_tree_nofollow(&canon, repos_root)?;
        } else {
            let canon = canonical_dir(&path)?;
            if !canon.starts_with(dir) {
                return Err(path_err(format!(
                    "refusing escaped file {}",
                    path.display()
                )));
            }
            fs::remove_file(&path).map_err(|error| {
                path_err(format!("failed to remove {}: {error}", path.display()))
            })?;
        }
    }
    fs::remove_dir(dir).map_err(|error| {
        path_err(format!(
            "failed to remove owned directory {}: {error}",
            dir.display()
        ))
    })
}

/// Pinned, explicit removal. Native observations are producers of receipts;
/// HTTP clients never supply a serialized plan or a successful-effect flag.
pub mod reviewed {
    use super::*;
    use crate::db::managed_remove::{
        DeletionPhase, DeletionReceipt, ManagedRemoveOperation, ManagedRemoveRemoteOutcome,
        ManagedRemoveState, RemovalChoices, ReviewedGitDeletion, ReviewedRemovalPlan,
        ReviewedSvnDeletion, REVIEWED_REMOVAL_POLICY,
    };
    use crate::db::Database;
    use crate::models::Repository;
    use crate::svn::SvnClient;
    use anyhow::{bail, ensure, Context, Result};
    use std::collections::BTreeSet;
    use std::time::Duration;
    use tokio::process::Command;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Fault {
        AfterGitIntent,
        AfterGitEffect,
        AfterSvnEffect,
        AfterGitReceipt,
        AfterSvnReceipt,
        BeforeCleanup,
    }
    #[cfg(debug_assertions)]
    static FAULTS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Fault>>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    #[cfg(debug_assertions)]
    pub fn set_fault(repo: &str, fault: Fault) {
        FAULTS.lock().unwrap().insert(repo.into(), fault);
    }
    fn fault(repo: &str, point: Fault) -> Result<()> {
        #[cfg(debug_assertions)]
        {
            let mut faults = FAULTS.lock().unwrap();
            if faults.get(repo) == Some(&point) {
                faults.remove(repo);
                bail!("controlled managed-removal interruption at {point:?}");
            }
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = (repo, point);
        }
        Ok(())
    }

    /// Non-deserializable capability, constructed only after actual native
    /// observation. A copied JSON receipt is not a producer capability.
    pub(crate) struct VerifiedDeletion {
        operation: String,
        pin: String,
        side: &'static str,
        receipt: DeletionReceipt,
    }
    impl VerifiedDeletion {
        pub(crate) fn into_parts(self) -> (String, String, &'static str, DeletionReceipt) {
            (self.operation, self.pin, self.side, self.receipt)
        }
    }

    fn credential(db: &Database, id: &str, prefix: &str) -> Result<Option<String>> {
        let mut next = Some(id.to_string());
        let mut seen = BTreeSet::new();
        while let Some(id) = next {
            ensure!(seen.insert(id.clone()), "cyclic credential ancestry");
            if let Some(value) = db.get_state(&format!("{prefix}_{id}"))? {
                ensure!(
                    !value.is_empty(),
                    "explicit credential revocation blocks removal preflight"
                );
                return Ok(Some(value));
            }
            next = db
                .get_repository(&id)?
                .context("missing credential ancestor")?
                .parent_id;
        }
        Ok(db.get_state(prefix)?.filter(|s| !s.is_empty()))
    }

    fn svn_client(db: &Database, repo: &Repository, url: &str) -> Result<SvnClient> {
        let password = credential(db, &repo.id, "secret_svn_password")?;
        // Local synthetic repositories have no authentication realm. Do not
        // reinterpret missing credentials as anonymous on a network endpoint.
        let username = if url.starts_with("file:") && password.is_none() {
            ""
        } else {
            &repo.svn_username
        };
        SvnClient::new_isolated_refresh(url, username, password.as_deref())
            .map_err(|_| anyhow::anyhow!("isolated SVN credentials/configuration unavailable"))
    }

    fn git_url(repo: &Repository) -> Result<String> {
        let raw =
            crate::git::remote_url::derive_git_remote_url(&repo.git_api_url, None, &repo.git_repo);
        let url = reqwest::Url::parse(&raw).context("invalid Git endpoint")?;
        ensure!(
            matches!(url.scheme(), "http" | "https" | "file")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "unsupported or credential-bearing Git endpoint"
        );
        if url.scheme() == "file" {
            ensure!(url.host_str().is_none(), "nonlocal file endpoint refused");
        }
        Ok(url.to_string())
    }

    async fn git_command(
        db: &Database,
        repo: &Repository,
        args: &[&str],
        expected_config: Option<&str>,
    ) -> Result<std::process::Output> {
        let work = tempfile::Builder::new()
            .prefix("reposync-remove-git-")
            .tempdir()?;
        let mut init = Command::new("git");
        init.current_dir(work.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_TEMPLATE_DIR")
            .args(["init", "--bare", "--quiet", "--template=", "."]);
        let initialized = crate::process::run(init, Duration::from_secs(15), None).await?;
        ensure!(
            initialized.status.success(),
            "private Git command repository could not be created"
        );
        let mut command = Command::new("git");
        command
            .current_dir(work.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_SSH_COMMAND")
            .env_remove("GIT_ASKPASS")
            .args([
                "-c",
                "credential.helper=",
                "-c",
                "core.askPass=",
                "-c",
                "protocol.ext.allow=never",
            ])
            .args(args);
        let token = credential(db, &repo.id, "secret_git_token")?;
        if let Some(expected) = expected_config {
            ensure!(
                db.reviewed_removal_configuration(&repo.id)? == expected,
                "credential or dependency generation changed before Git launch"
            );
        }
        crate::git::subprocess_auth::apply_git_http_auth_tokio_optional(
            &mut command,
            token.as_deref(),
        );
        let mut output=crate::process::run(command,Duration::from_secs(60),None).await
            .map_err(|error|anyhow::anyhow!("Git removal command did not return collected output (kind {:?}, cleanup unconfirmed: {}): {}",error.kind(),crate::process::cleanup_unconfirmed(&error),crate::errors::redact_vcs_error_detail(&error.to_string())))?;
        if let Some(token) = token {
            output.stderr = String::from_utf8_lossy(&output.stderr)
                .replace(&token, "[redacted]")
                .into_bytes();
        }
        Ok(output)
    }

    async fn git_tip(
        db: &Database,
        repo: &Repository,
        target: &ReviewedGitDeletion,
    ) -> Result<Option<String>> {
        let output = git_command(
            db,
            repo,
            &[
                "ls-remote",
                "--exit-code",
                "--refs",
                "--",
                &target.remote_url,
                &target.reference,
            ],
            None,
        )
        .await?;
        if output.status.code() == Some(2) && output.stdout.is_empty() {
            return Ok(None);
        }
        ensure!(
            output.status.success(),
            "Git target observation failed; absence is not established"
        );
        let text = std::str::from_utf8(&output.stdout)?;
        let rows: Vec<_> = text.lines().collect();
        ensure!(rows.len() == 1, "ambiguous Git target observation");
        let (sha, reference) = rows[0]
            .split_once('\t')
            .context("malformed Git target observation")?;
        ensure!(
            reference == target.reference
                && sha.len() == 40
                && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "Git target observation identity mismatch"
        );
        Ok(Some(sha.into()))
    }

    async fn svn_present(client: &SvnClient) -> Result<bool> {
        match client.info().await {
            Ok(_) => Ok(true),
            Err(error) => {
                let detail = error.to_string();
                if detail.contains("E160013")
                    || (detail.contains("W170000") && detail.contains("E200009"))
                {
                    Ok(false)
                } else {
                    bail!("SVN target observation failed; absence is not established")
                }
            }
        }
    }

    fn canonical_root(data: &Path) -> Result<String> {
        ensure!(
            !fs::symlink_metadata(data)?.file_type().is_symlink(),
            "managed data root is a symlink"
        );
        let root = data.canonicalize()?;
        let repos = root.join("repos");
        if repos.exists() {
            ensure!(
                !fs::symlink_metadata(&repos)?.file_type().is_symlink(),
                "managed repositories root is a symlink"
            );
        }
        root.to_str()
            .map(String::from)
            .context("managed root is not UTF-8")
    }

    fn preflight_owned_target(data: &Path, id: &str) -> Result<()> {
        validate_repo_id(id)?;
        let root = PathBuf::from(canonical_root(data)?);
        let repos = root.join("repos");
        match fs::symlink_metadata(&repos) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
            Ok(meta) => ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "managed repositories root is not a real owned directory"
            ),
        }
        let target = repos.join(id);
        match fs::symlink_metadata(&target) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
            Ok(meta) => {
                ensure!(
                    !meta.file_type().is_symlink(),
                    "managed target is a symlink; removal refused before new remote effects"
                );
                ensure!(
                    strict_child(&repos, &target.canonicalize()?),
                    "managed target escaped its exact owned path"
                );
                Ok(())
            }
        }
    }

    /// Read-only fresh target capture. Neither a missing tool nor unavailable
    /// remote can be represented as an absent branch.
    pub async fn capture(
        db: &Database,
        data: &Path,
        id: &str,
        actor: &str,
        choices: RemovalChoices,
    ) -> Result<ReviewedRemovalPlan> {
        validate_repo_id(id)?;
        let repo = db.get_repository(id)?.context("repository not found")?;
        ensure!(db.managed_removal(id)?.is_none_or(|op|op.state.is_terminal_success()),"an existing removal already owns this registration; inspect and continue its retained operation instead of approving different work");
        let root = canonical_root(data)?;
        preflight_owned_target(data, id)?;
        let config = db.reviewed_removal_configuration(id)?;
        ensure!(
            db.list_child_repositories(id)?.is_empty(),
            "child registrations must be handled first; no cascade is performed"
        );
        if choices.delete_git || choices.delete_svn {
            ensure!(
                repo.parent_id.is_some(),
                "root removal does not delete remote branches"
            );
        }
        let scope = crate::db::repo_scope_identity::repository_scope_uuid(&db.conn(), id)?;
        let parent_scope = repo
            .parent_id
            .as_ref()
            .map(|p| crate::db::repo_scope_identity::repository_scope_uuid(&db.conn(), p))
            .transpose()?;
        let mut git = None;
        let mut svn = None;
        if choices.delete_git {
            ensure!(
                !repo.git_branch.is_empty(),
                "empty Git branch cannot be deleted"
            );
            let mut target = ReviewedGitDeletion {
                remote_url: git_url(&repo)?,
                reference: format!("refs/heads/{}", repo.git_branch),
                expected_sha: None,
            };
            ensure!(
                git2::Reference::is_valid_name(&target.reference),
                "invalid Git branch"
            );
            for other in db.list_repositories()? {
                if other.id != repo.id
                    && git_url(&other).ok().as_ref() == Some(&target.remote_url)
                    && other.git_branch == repo.git_branch
                {
                    bail!("Git branch is shared by another registration");
                }
            }
            target.expected_sha = git_tip(db, &repo, &target).await?;
            git = Some(target);
        }
        if choices.delete_svn {
            ensure!(
                !repo.svn_branch.is_empty(),
                "empty SVN branch cannot be deleted"
            );
            let mut tool = Command::new("svnmucc");
            tool.arg("--version");
            let available=crate::process::run(tool,Duration::from_secs(15),None).await.context("svnmucc is required for a revision-bound SVN deletion; no installation was attempted")?;
            ensure!(
                available.status.success(),
                "svnmucc readiness failed; no removal operation was claimed"
            );
            let url = crate::svn::operand::configured_branch_url(&repo.svn_url, &repo.svn_branch)?;
            for other in db.list_repositories()? {
                if other.id == repo.id {
                    continue;
                }
                let other_url =
                    crate::svn::operand::configured_branch_url(&other.svn_url, &other.svn_branch)?;
                ensure!(
                    other_url != url && !other_url.starts_with(&(url.clone() + "/")),
                    "SVN deletion would remove another registration's target"
                );
            }
            let info = svn_client(db, &repo, &repo.svn_url)?
                .info()
                .await
                .map_err(|_| anyhow::anyhow!("SVN root observation failed"))?;
            let target = svn_client(db, &repo, &url)?;
            let present = svn_present(&target).await?;
            let identity = crate::path_projection::checked_svn_path_identity(&info.root_url, &url)
                .context("SVN target is outside the observed root")?;
            // Revision is a repository-wide baseline used by svnmucc's commit
            // editor, not a check-then-unconditional URL deletion.
            svn = Some(ReviewedSvnDeletion {
                root_url: info.root_url,
                uuid: info.uuid,
                url,
                branch_path: identity.branch_path,
                base_revision: info.latest_rev,
                expected_present: present,
            });
        }
        ensure!(
            config == db.reviewed_removal_configuration(id)?,
            "configuration changed during review"
        );
        let mut plan = ReviewedRemovalPlan {
            version: 2,
            policy: REVIEWED_REMOVAL_POLICY.into(),
            actor: actor.into(),
            repo_id: id.into(),
            scope_uuid: scope,
            parent_id: repo.parent_id,
            parent_scope_uuid: parent_scope,
            configuration_digest: config,
            managed_root: root,
            choices,
            git,
            svn,
            approval_digest: String::new(),
        };
        plan.approval_digest = plan.digest()?;
        plan.validate()?;
        Ok(plan)
    }

    /// Recapture on explicit approval; a client supplies only the opaque pin.
    pub async fn begin(
        db: &Database,
        data: &Path,
        id: &str,
        actor: &str,
        choices: RemovalChoices,
        pin: &str,
        request: &str,
    ) -> Result<ManagedRemoveOperation> {
        ensure!(!pin.is_empty(), "reviewed removal pin is required");
        if let Some(op) = db.managed_removal(id)? {
            if op.version == 2 && op.initiator_id == actor && op.request_id == request {
                ensure!(
                    op.target_fingerprint == pin,
                    "request identity already belongs to another review"
                );
                ensure!(
                    op.reviewed
                        .as_ref()
                        .is_some_and(|p| p.plan.choices == choices),
                    "request identity has different deletion choices"
                );
                if op.state == ManagedRemoveState::Completed {
                    ensure!(db.get_repository(id)?.is_none(),"this completed operation predates restoration; a fresh review and request identity are required");
                }
                return Ok(op);
            }
        }
        let plan = capture(db, data, id, actor, choices).await?;
        ensure!(
            plan.approval_digest == pin,
            "stale removal review; inspect the current targets again"
        );
        Ok(db.claim_reviewed_removal(plan, request)?)
    }

    fn native_proof(
        op: &ManagedRemoveOperation,
        side: &'static str,
        mut receipt: DeletionReceipt,
        revision: Option<i64>,
        absent_before: bool,
    ) -> Result<VerifiedDeletion> {
        receipt.phase = if absent_before {
            DeletionPhase::VerifiedAlreadyAbsent
        } else {
            DeletionPhase::VerifiedDeleted
        };
        receipt.confirmed_svn_revision = revision;
        receipt.observed_git_sha = None;
        receipt.evidence_digest = Some(crate::db::managed_remove::deletion_evidence_digest(
            &op.id,
            &op.target_fingerprint,
            side,
            &receipt,
        )?);
        Ok(VerifiedDeletion {
            operation: op.id.clone(),
            pin: op.target_fingerprint.clone(),
            side,
            receipt,
        })
    }

    fn receipt<'a>(op: &'a ManagedRemoveOperation, side: &str) -> &'a DeletionReceipt {
        let p = op.reviewed.as_ref().expect("validated reviewed operation");
        if side == "git" {
            &p.git
        } else {
            &p.svn
        }
    }
    fn set_receipt(op: &mut ManagedRemoveOperation, side: &str, value: DeletionReceipt) {
        let p = op.reviewed.as_mut().expect("validated reviewed operation");
        let label = match value.phase {
            DeletionPhase::Prepared => "pending",
            DeletionPhase::FailedBeforeEffect => "failed",
            _ => "reconciliation_required",
        };
        if side == "git" {
            p.git = value;
            op.remote_git = label.into();
        } else {
            p.svn = value;
            op.remote_svn = label.into();
        }
    }

    fn hold(
        db: &Database,
        op: &ManagedRemoveOperation,
        side: &str,
        detail: &str,
    ) -> Result<ManagedRemoveOperation> {
        let mut r = receipt(op, side).clone();
        // A repeated unknown operation is immutable. It cannot append an
        // uncharged error or turn a previously uncertain launch into success.
        if r.phase == DeletionPhase::ReconciliationRequired {
            return Ok(op.clone());
        }
        ensure!(
            r.errors.len() < 8,
            "retained error capacity exhausted; operation remains held"
        );
        r.phase = DeletionPhase::ReconciliationRequired;
        let detail: String = detail
            .chars()
            .scan(0usize, |n, c| {
                *n += c.len_utf8();
                Some((*n, c))
            })
            .take_while(|(n, _)| *n <= 8192)
            .map(|(_, c)| c)
            .collect();
        r.errors.push(detail.clone());
        let mut next = op.clone();
        set_receipt(&mut next, side, r);
        next.state = ManagedRemoveState::ReconciliationRequired;
        next.outcome_detail = Some(detail);
        Ok(db.transition_reviewed_removal(op, next)?)
    }

    fn local_failure(
        db: &Database,
        op: &ManagedRemoveOperation,
        detail: String,
    ) -> Result<ManagedRemoveOperation> {
        let mut next = op.clone();
        let p = next.reviewed.as_mut().context("reviewed payload missing")?;
        let detail: String = detail
            .chars()
            .scan(0usize, |n, c| {
                *n += c.len_utf8();
                Some((*n, c))
            })
            .take_while(|(n, _)| *n <= 8192)
            .map(|(_, c)| c)
            .collect();
        if p.cleanup_errors.last() == Some(&detail) && op.state == ManagedRemoveState::Failed {
            return Ok(op.clone());
        }
        ensure!(
            p.cleanup_errors.len() < 8,
            "retained cleanup error capacity exhausted; no further cleanup admitted"
        );
        p.cleanup_errors.push(detail.clone());
        next.state = ManagedRemoveState::Failed;
        next.outcome_detail = Some(detail);
        Ok(db.transition_reviewed_removal(op, next)?)
    }

    async fn svn_delete(
        db: &Database,
        repo: &Repository,
        target: &ReviewedSvnDeletion,
        marker: &str,
        expected_config: &str,
    ) -> Result<()> {
        let config = tempfile::Builder::new()
            .prefix("reposync-remove-svn-")
            .tempdir()?;
        fs::write(
            config.path().join("config"),
            "[auth]\npassword-stores =\nstore-auth-creds = no\n",
        )?;
        let password = credential(db, &repo.id, "secret_svn_password")?;
        let mut command = Command::new("svnmucc");
        command
            .args(["--non-interactive", "--no-auth-cache", "--config-dir"])
            .arg(config.path())
            .args(["-r", &target.base_revision.to_string(), "-m", marker]);
        if !repo.svn_username.is_empty() {
            command.args(["--username", &repo.svn_username]);
        }
        if password.is_some() {
            command.arg("--password-from-stdin");
        }
        command.args(["rm", &target.url]);
        // Secret staging is an owned anonymous regular file, owner-only before
        // the first password byte. Keep it through the supervised completion.
        let mut input = tempfile::tempfile()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            input.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        {
            use std::io::{Seek, Write};
            if let Some(password) = password.as_deref() {
                writeln!(input, "{password}")?;
            }
            input.rewind()?;
        }
        command.stdin(std::process::Stdio::from(input));
        ensure!(
            db.reviewed_removal_configuration(&repo.id)? == expected_config,
            "credential or dependency generation changed before SVN launch"
        );
        let output=crate::process::run(command,Duration::from_secs(60),None).await
            .map_err(|error|anyhow::anyhow!("SVN deletion did not return collected output (kind {:?}, OS code {:?}, cleanup unconfirmed {}): {}; do not repeat the intent",error.kind(),error.raw_os_error(),crate::process::cleanup_unconfirmed(&error),crate::errors::redact_vcs_error_detail(&error.to_string())))?;
        let mut detail = String::from_utf8_lossy(&output.stderr).into_owned();
        if let Some(password) = password {
            detail = detail.replace(&password, "[redacted]");
        }
        ensure!(
            output.status.success(),
            "SVN pinned deletion refused or failed ({}): {}",
            output.status,
            crate::errors::redact_vcs_error_detail(&detail)
        );
        Ok(())
    }

    async fn svn_deletion_revision(
        db: &Database,
        repo: &Repository,
        target: &ReviewedSvnDeletion,
        marker: &str,
    ) -> Result<Option<i64>> {
        let client = svn_client(db, repo, &target.root_url)?;
        let info = client
            .info()
            .await
            .map_err(|_| anyhow::anyhow!("SVN recovery root unavailable"))?;
        ensure!(
            info.uuid == target.uuid
                && info.root_url == target.root_url
                && info.latest_rev >= target.base_revision,
            "SVN recovery identity changed"
        );
        ensure!(
            info.latest_rev - target.base_revision <= 10000,
            "SVN recovery history exceeds bounded observation; hold for explicit reconciliation"
        );
        if info.latest_rev == target.base_revision {
            return Ok(None);
        }
        let entries = client
            .strict_log_range(target.base_revision + 1, info.latest_rev)
            .await?;
        let identity =
            crate::path_projection::checked_svn_path_identity(&target.root_url, &target.url)
                .context("SVN deletion target is outside its repository root")?;
        let literal = format!("/{}", identity.branch_path);
        let matches: Vec<_> = entries.iter().filter(|e| e.message == marker).collect();
        ensure!(
            matches.len() <= 1,
            "duplicate removal markers; recovery is ambiguous"
        );
        match matches.first() {
            Some(e) => {
                ensure!(
                    e.changed_paths.len() == 1
                        && e.changed_paths[0].action == "D"
                        && e.changed_paths[0].path == literal,
                    "removal marker has foreign effects"
                );
                Ok(Some(e.revision))
            }
            None => Ok(None),
        }
    }

    /// Continues one durable identity. A consumed intent is inspected rather
    /// than replayed; unknown collection/failure stays held with its evidence.
    pub async fn run(
        db: &Database,
        data: &Path,
        id: &str,
        actor: &str,
        operation: &str,
    ) -> Result<ManagedRemoveOperation> {
        let _busy =
            crate::busy::try_acquire(id).context("repository is busy; operation remains queued")?;
        let mut op = db
            .managed_removal_operation(id, operation)?
            .context("removal operation not found")?;
        ensure!(
            op.version == 2 && op.initiator_id == actor,
            "removal operation belongs to another actor or protocol"
        );
        if op.state == ManagedRemoveState::Completed {
            return Ok(op);
        }
        let plan = op
            .reviewed
            .as_ref()
            .context("missing reviewed plan")?
            .plan
            .clone();
        ensure!(
            canonical_root(data)? == plan.managed_root,
            "managed data root changed"
        );
        if op.reviewed.as_ref().is_some_and(|p| {
            p.git.phase == DeletionPhase::ReconciliationRequired
                || p.svn.phase == DeletionPhase::ReconciliationRequired
        }) {
            return Ok(op);
        }
        if let Err(error) = preflight_owned_target(data, id) {
            return local_failure(
                db,
                &op,
                format!("owned-path preflight refused: {error}; no new effects were launched"),
            );
        }
        op = match db.admit_reviewed_removal_work(&op, false) {
            Ok(op) => op,
            Err(error) => {
                return local_failure(
                    db,
                    &op,
                    format!(
                        "removal admission refused: {}; no new effects were launched",
                        crate::errors::redact_vcs_error_detail(&error.to_string())
                    ),
                )
            }
        };
        if op.state != ManagedRemoveState::Running {
            return Ok(op);
        }
        let repo = db
            .get_repository(id)?
            .context("registration missing before removal")?;
        for side in ["git", "svn"] {
            let mut r = receipt(&op, side).clone();
            if r.phase.is_verified() {
                continue;
            }
            if r.phase == DeletionPhase::ReconciliationRequired {
                return Ok(op);
            }
            let was_intent = r.phase == DeletionPhase::IntentRecorded;
            // Precharge the retained error slot before starting any I/O.
            ensure!(r.errors.len() < 8, "retained error capacity exhausted");
            if side == "git" {
                let target = plan.git.as_ref().context("missing Git deletion target")?;
                let observed = match git_tip(db, &repo, target).await {
                    Ok(t) => t,
                    Err(_) => {
                        return hold(
                            db,
                            &op,
                            side,
                            "Git target cannot be observed; local cleanup remains held",
                        )
                    }
                };
                if was_intent {
                    // Git ref deletion has no durable operation marker. An
                    // absent ref cannot prove which request removed it.
                    return hold(db,&op,side,"Git deletion intent has no persisted result; no blind retry or attribution from absence is permitted");
                }
                if observed != target.expected_sha {
                    return hold(
                        db,
                        &op,
                        side,
                        "Git target changed since review; no deletion was launched",
                    );
                }
                if observed.is_none() {
                    let proof = native_proof(&op, "git", r, None, true)?;
                    op = db.record_reviewed_deletion(&op, proof)?;
                    continue;
                }
                r.phase = DeletionPhase::IntentRecorded;
                let mut next = op.clone();
                set_receipt(&mut next, side, r.clone());
                op = db.transition_reviewed_removal(&op, next)?;
                fault(id, Fault::AfterGitIntent)?;
                let lease = format!(
                    "--force-with-lease={}:{}",
                    target.reference,
                    target.expected_sha.as_deref().unwrap()
                );
                let deletion = format!(":{}", target.reference);
                // Exact old-tip lease is server-enforced. A check followed by
                // provider REST DELETE would not provide this CAS guarantee.
                let result = git_command(
                    db,
                    &repo,
                    &[
                        "push",
                        "--porcelain",
                        &lease,
                        "--",
                        &target.remote_url,
                        &deletion,
                    ],
                    Some(&plan.configuration_digest),
                )
                .await;
                match result {
                    Ok(output) if output.status.success()=>{
                        match git_tip(db,&repo,target).await {Ok(None)=>{fault(id,Fault::AfterGitEffect)?;let proof=native_proof(&op,"git",r,None,false)?;op=db.record_reviewed_deletion(&op,proof)?;},_=>return hold(db,&op,side,"Git deletion returned success but its target absence is not verified")}
                    },
                    Ok(output)=>return hold(db,&op,side,&format!("Git pinned deletion failed ({}): {}; no automatic retry is allowed",output.status,crate::errors::redact_vcs_error_detail(&String::from_utf8_lossy(&output.stderr)))),
                    Err(error)=>return hold(db,&op,side,&format!("Git pinned deletion result is uncertain: {error}; no automatic retry is allowed"))
                }
            } else {
                let target = plan.svn.as_ref().context("missing SVN deletion target")?;
                let client=match svn_client(db,&repo,&target.url) {Ok(client)=>client,Err(_)=>return hold(db,&op,side,"isolated SVN credentials or configuration are unavailable; no new deletion was launched")};
                if was_intent {
                    match svn_deletion_revision(db,&repo,target,&r.operation_marker).await {
                        Ok(Some(revision))=>return hold(db,&op,side,&format!("SVN history contains the intended deletion marker at r{revision}, but the predecessor has no persisted collected-result receipt; inspection does not release unknown work or authorize cleanup")),
                        _=>return hold(db,&op,side,"SVN deletion intent cannot be reconciled to a collected exact result; it was not repeated")
                    }
                }
                let root_client = match svn_client(db, &repo, &target.root_url) {
                    Ok(client) => client,
                    Err(_) => {
                        return hold(
                            db,
                            &op,
                            side,
                            "isolated SVN root credentials or configuration are unavailable",
                        )
                    }
                };
                let root = match root_client.info().await {
                    Ok(info) => info,
                    Err(_) => {
                        return hold(
                            db,
                            &op,
                            side,
                            "SVN root cannot be observed; no new deletion was launched",
                        )
                    }
                };
                let present = match svn_present(&client).await {
                    Ok(present) => present,
                    Err(_) => {
                        return hold(
                            db,
                            &op,
                            side,
                            "SVN target cannot be observed; no new deletion was launched",
                        )
                    }
                };
                if root.uuid != target.uuid
                    || root.latest_rev != target.base_revision
                    || present != target.expected_present
                {
                    return hold(
                        db,
                        &op,
                        side,
                        "SVN target or baseline changed since review; no deletion was launched",
                    );
                }
                if !present {
                    let proof = native_proof(&op, "svn", r, None, true)?;
                    op = db.record_reviewed_deletion(&op, proof)?;
                    continue;
                }
                r.phase = DeletionPhase::IntentRecorded;
                let mut next = op.clone();
                set_receipt(&mut next, side, r.clone());
                op = db.transition_reviewed_removal(&op, next)?;
                if let Err(error) = svn_delete(
                    db,
                    &repo,
                    target,
                    &r.operation_marker,
                    &plan.configuration_digest,
                )
                .await
                {
                    return hold(db,&op,side,&format!("SVN pinned deletion failed or collection is uncertain: {error}; inspect the retained intent before any retry"));
                }
                match svn_deletion_revision(db,&repo,target,&r.operation_marker).await {
                    Ok(Some(revision)) if matches!(svn_present(&client).await,Ok(false))=>{fault(id,Fault::AfterSvnEffect)?;let proof=native_proof(&op,"svn",r,Some(revision),false)?;op=db.record_reviewed_deletion(&op,proof)?;},
                    _=>return hold(db,&op,side,"SVN deletion returned without complete exact-target evidence; cleanup remains held")
                }
            }
            fault(
                id,
                if side == "git" {
                    Fault::AfterGitReceipt
                } else {
                    Fault::AfterSvnReceipt
                },
            )?;
        }
        op = match db.admit_reviewed_removal_work(&op, true) {
            Ok(op) => op,
            Err(error) => {
                return local_failure(
                    db,
                    &op,
                    format!(
                        "cleanup admission refused: {}; verified remote receipts remain retained",
                        crate::errors::redact_vcs_error_detail(&error.to_string())
                    ),
                )
            }
        };
        if op.state != ManagedRemoveState::Running {
            return Ok(op);
        }
        ensure!(
            op.reviewed
                .as_ref()
                .is_some_and(|p| p.cleanup_errors.len() < 8),
            "retained cleanup error capacity exhausted; cleanup remains held"
        );
        fault(id, Fault::BeforeCleanup)?;
        if let Err(error) = remove_owned_repo_tree(data, id) {
            return local_failure(db,&op,format!("owned local cleanup failed: {error}; verified remote receipts and registration remain retained"));
        }
        Ok(db.complete_managed_remove(
            id,
            &op.id,
            &ManagedRemoveRemoteOutcome {
                remote_git: op.remote_git.clone(),
                remote_svn: op.remote_svn.clone(),
            },
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_r02_path_confinement_blocks_symlink_and_sibling_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let repos = data.join("repos");
        fs::create_dir_all(repos.join("sibling")).unwrap();
        fs::write(repos.join("sibling").join("keep.txt"), "sibling\n").unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "do-not-delete\n").unwrap();

        let owned = repos.join("repo-1");
        fs::create_dir_all(owned.join("git-repo")).unwrap();
        fs::write(owned.join("git-repo").join("owned.txt"), "owned\n").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), owned.join("escape")).unwrap();

        remove_owned_repo_tree(&data, "repo-1").unwrap();
        assert!(!owned.exists());
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "do-not-delete\n"
        );
        assert_eq!(
            fs::read_to_string(repos.join("sibling").join("keep.txt")).unwrap(),
            "sibling\n"
        );
        assert!(repos.is_dir());

        fs::create_dir_all(&owned).unwrap();
        std::os::unix::fs::symlink(&outside, repos.join("linked")).unwrap();
        let error = remove_owned_repo_tree(&data, "linked").unwrap_err();
        assert!(error.message.contains("symlink"), "{error}");
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "do-not-delete\n"
        );
        assert!(repos
            .join("linked")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());

        for id in ["../outside", "repo/child", "", ".", "..", "repo 1"] {
            let error = remove_owned_repo_tree(&data, id).unwrap_err();
            assert!(
                error.message.contains("safe path component"),
                "{id}: {error}"
            );
        }
        assert_eq!(
            fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "do-not-delete\n"
        );
        assert!(owned.is_dir());
    }
}
