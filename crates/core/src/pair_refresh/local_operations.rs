//! Local file:// Git and SVN refresh and re-anchor.
//!
//! Network URLs are refused. Published history is not deleted and nothing is
//! force-pushed. A journal row is written before each side effect.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::sync_engine::SyncEngine;

const JOURNAL_PREFIX: &str = "pair_refresh_local_v1:";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalOperation {
    Update,
    Reanchor,
}

#[derive(Clone, Debug)]
pub struct LocalRequest {
    pub request_id: String,
    pub approval_digest: String,
    pub operation: LocalOperation,
    pub same_name: bool,
    pub retire: bool,
    /// v1 `execute: true` approves the pins observed by this call.
    pub approve_current: bool,
    /// Test seam. HTTP callers pass `None`.
    pub stop_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalReceipt {
    pub operation_id: Uuid,
    pub request_id: String,
    pub child_id: String,
    pub parent_id: String,
    pub operation: String,
    pub phase: String,
    pub noop: bool,
    pub git_sha: Option<String>,
    pub svn_revision: Option<i64>,
    pub retained_ref: Option<String>,
    pub new_svn_path: Option<String>,
    pub displaced: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Journal {
    operation_id: Uuid,
    request_id: String,
    child_id: String,
    parent_id: String,
    operation: LocalOperation,
    digest: String,
    same_name: bool,
    retire: bool,
    phase: String,
    noop: bool,
    pair_tip: String,
    parent_tip: String,
    pair_svn: i64,
    parent_svn: i64,
    new_svn: Option<i64>,
    new_git: Option<String>,
    retained_ref: Option<String>,
    new_svn_path: Option<String>,
    displaced: Vec<String>,
    error: Option<String>,
}

pub async fn run_local(
    engine: &SyncEngine,
    child_id: &str,
    request: LocalRequest,
) -> Result<LocalReceipt> {
    if request.request_id.is_empty() || request.request_id.len() > 200 {
        bail!("refresh request id is empty or too long");
    }
    let key = journal_key(child_id, &request.request_id);
    if let Some(existing) = load(engine, &key)? {
        let digest_ok = request.approve_current || existing.digest == request.approval_digest;
        if !digest_ok
            || existing.operation != request.operation
            || existing.same_name != request.same_name
            || existing.retire != request.retire
        {
            bail!("request id is already bound to a different approval");
        }
        if existing.phase == "completed" {
            return Ok(receipt_of(&existing));
        }
        return drive(engine, existing, request.stop_after.as_deref());
    }
    let observed = observe(engine, child_id)?;
    let digest = approval_digest(
        &observed,
        request.operation,
        request.same_name,
        request.retire,
    );
    if !request.approve_current && digest != request.approval_digest {
        bail!("stale preview: approval digest does not match the current pins");
    }
    if request.operation == LocalOperation::Update && observed.pending_pair_svn > 0 {
        bail!(
            "pending svn work is preserved and was not discarded (revisions still present, count {})",
            observed.pending_pair_svn
        );
    }
    if request.operation == LocalOperation::Update && !observed.pending_pair_git.is_empty() {
        bail!("pending pair git work is preserved and was not discarded");
    }
    let noop = request.operation == LocalOperation::Update
        && observed.pending_parent_svn == 0
        && observed.pending_parent_git.is_empty();
    let journal = Journal {
        operation_id: Uuid::new_v4(),
        request_id: request.request_id,
        child_id: child_id.to_string(),
        parent_id: observed.parent_id.clone(),
        operation: request.operation,
        digest,
        same_name: request.same_name,
        retire: request.retire,
        phase: "admitted".into(),
        noop,
        pair_tip: observed.pair_tip.clone(),
        parent_tip: observed.parent_tip.clone(),
        pair_svn: observed.pair_svn,
        parent_svn: observed.parent_svn,
        new_svn: None,
        new_git: None,
        retained_ref: None,
        new_svn_path: None,
        displaced: observed.pending_pair_git.clone(),
        error: None,
    };
    save(engine, &key, &journal)?;
    drive(engine, journal, request.stop_after.as_deref())
}

pub fn current_digest(
    engine: &SyncEngine,
    child_id: &str,
    operation: LocalOperation,
    same_name: bool,
    retire: bool,
) -> Result<String> {
    let observed = observe(engine, child_id)?;
    Ok(approval_digest(&observed, operation, same_name, retire))
}

struct Observed {
    parent_id: String,
    pair_tip: String,
    parent_tip: String,
    pair_svn: i64,
    parent_svn: i64,
    pair_path: String,
    parent_path: String,
    svn_root: String,
    pair_git: PathBuf,
    parent_git: PathBuf,
    pair_branch: String,
    parent_branch: String,
    pending_pair_svn: usize,
    pending_parent_svn: usize,
    pending_pair_git: Vec<String>,
    pending_parent_git: Vec<String>,
    mapped_pair_svn: i64,
    mapped_parent_svn: i64,
}

fn observe(engine: &SyncEngine, child_id: &str) -> Result<Observed> {
    let child = engine
        .db()
        .get_repository(child_id)?
        .context("child repository not found")?;
    let parent_id = child
        .parent_id
        .clone()
        .context("not_a_branch_pair: refresh applies to a child pair")?;
    let parent = engine
        .db()
        .get_repository(&parent_id)?
        .context("parent repository not found")?;
    require_file_url(&child.svn_url)?;
    require_file_url(&parent.svn_url)?;
    if child.svn_url != parent.svn_url {
        bail!("pair and parent must share one local SVN repository");
    }
    let pair_git = git_dir(engine, child_id)?;
    let parent_git = git_dir(engine, &parent_id)?;
    let pair_url = join_svn(&child.svn_url, &child.svn_branch);
    let parent_url = join_svn(&parent.svn_url, &parent.svn_branch);
    let pair_svn = svn_revision(&pair_url)?;
    let parent_svn = svn_revision(&parent_url)?;
    let pair_tip = git_rev(&pair_git, &child.git_branch)?;
    let parent_tip = git_rev(&parent_git, &parent.git_branch)?;
    if child.last_git_sha.is_empty() || parent.last_git_sha.is_empty() {
        bail!("refresh baseline is unmapped; published history was not changed");
    }
    if !object_exists(&pair_git, &child.last_git_sha)? {
        bail!("mapped pair commit is absent; published history was not changed");
    }
    let pending_pair_svn = usize::from(pair_svn > child.last_svn_rev);
    let pending_parent_svn = usize::from(parent_svn > parent.last_svn_rev);
    let pending_pair_git = if child.last_git_sha == pair_tip {
        Vec::new()
    } else {
        vec![pair_tip.clone()]
    };
    let pending_parent_git = if parent.last_git_sha == parent_tip {
        Vec::new()
    } else {
        vec![parent_tip.clone()]
    };
    Ok(Observed {
        parent_id,
        pair_tip,
        parent_tip,
        pair_svn,
        parent_svn,
        pair_path: child.svn_branch,
        parent_path: parent.svn_branch,
        svn_root: child.svn_url,
        pair_git,
        parent_git,
        pair_branch: child.git_branch,
        parent_branch: parent.git_branch,
        pending_pair_svn,
        pending_parent_svn,
        pending_pair_git,
        pending_parent_git,
        mapped_pair_svn: child.last_svn_rev,
        mapped_parent_svn: parent.last_svn_rev,
    })
}

fn drive(
    engine: &SyncEngine,
    mut journal: Journal,
    stop_after: Option<&str>,
) -> Result<LocalReceipt> {
    let key = journal_key(&journal.child_id, &journal.request_id);
    let observed = observe(engine, &journal.child_id)?;
    if journal.phase != "admitted"
        && (journal.pair_tip != observed.pair_tip
            || journal.parent_tip != observed.parent_tip
            || journal.pair_svn != observed.pair_svn
            || journal.parent_svn != observed.parent_svn)
        && journal.new_svn.is_none()
        && journal.new_git.is_none()
    {
        bail!("refresh pins drifted before a side effect; resume was refused");
    }
    if journal.noop {
        journal.phase = "completed".into();
        save(engine, &key, &journal)?;
        return Ok(receipt_of(&journal));
    }
    mark(engine, &key, &mut journal, "svn_intent", stop_after)?;
    if journal.new_svn.is_none() {
        journal.new_svn = Some(publish_svn(engine, &journal, &observed)?);
        journal.new_svn_path = Some(published_path(&journal, &observed));
        save(engine, &key, &journal)?;
    }
    mark(engine, &key, &mut journal, "svn_observed", stop_after)?;
    mark(engine, &key, &mut journal, "git_intent", stop_after)?;
    if journal.new_git.is_none() {
        match publish_git(engine, &journal, &observed) {
            Ok(sha) => {
                journal.new_git = Some(sha);
                journal.retained_ref = Some("refs/reposync/pair-refresh/generation/1".into());
                save(engine, &key, &journal)?;
            }
            Err(error) => {
                journal.phase = "reconciliation_required".into();
                journal.error = Some(error.to_string());
                save(engine, &key, &journal)?;
                return Err(error);
            }
        }
    }
    mark(engine, &key, &mut journal, "git_observed", stop_after)?;
    let git_sha = journal.new_git.clone().context("git receipt")?;
    let svn_rev = journal.new_svn.context("svn receipt")?;
    engine
        .db()
        .update_repo_watermark_columns_only(&journal.child_id, svn_rev, &git_sha)?;
    if journal.operation == LocalOperation::Update {
        engine.db().update_repo_watermark_columns_only(
            &journal.parent_id,
            observed.parent_svn,
            &observed.parent_tip,
        )?;
    }
    journal.phase = "checkpoint".into();
    save(engine, &key, &journal)?;
    if stop_after == Some("checkpoint") {
        bail!("crashed after checkpoint");
    }
    journal.phase = "completed".into();
    save(engine, &key, &journal)?;
    Ok(receipt_of(&journal))
}

fn mark(
    engine: &SyncEngine,
    key: &str,
    journal: &mut Journal,
    phase: &str,
    stop_after: Option<&str>,
) -> Result<()> {
    if journal.phase == "reconciliation_required" {
        bail!(
            "protected publication is waiting for reconciliation: {}",
            journal.error.as_deref().unwrap_or("git update was refused")
        );
    }
    let order = [
        "admitted",
        "svn_intent",
        "svn_observed",
        "git_intent",
        "git_observed",
        "checkpoint",
        "completed",
    ];
    let current = order
        .iter()
        .position(|item| *item == journal.phase)
        .unwrap_or(0);
    let next = order.iter().position(|item| *item == phase).unwrap_or(0);
    if next > current {
        journal.phase = phase.to_string();
        save(engine, key, journal)?;
    }
    if stop_after == Some(phase) {
        bail!("crashed after {phase}");
    }
    Ok(())
}

fn publish_svn(engine: &SyncEngine, journal: &Journal, observed: &Observed) -> Result<i64> {
    let message = match journal.operation {
        LocalOperation::Update => "pair refresh integration",
        LocalOperation::Reanchor if journal.same_name => {
            "reanchor svn-origin baseline on the approved path"
        }
        LocalOperation::Reanchor => "reanchor svn-origin baseline",
    };
    if let Some(existing) = published_revision(&observed.svn_root, message, journal.pair_svn)? {
        return Ok(existing);
    }
    let before = svnlook_youngest(&observed.svn_root)?;
    match journal.operation {
        LocalOperation::Update => {
            let wc = workspace(engine, &journal.child_id, &journal.request_id)?.join("pair-wc");
            svn(&[
                "checkout",
                &join_svn(&observed.svn_root, &observed.pair_path),
                wc.to_str().context("wc path")?,
            ])?;
            let export = workspace(engine, &journal.child_id, &journal.request_id)?.join("parent");
            svn(&[
                "export",
                "--force",
                &join_svn(&observed.svn_root, &observed.parent_path),
                export.to_str().context("export path")?,
            ])?;
            copy_tree(&export, &wc)?;
            svn_in(&["add", "--force", "."], Some(&wc))?;
            svn_in(&["commit", "-m", message], Some(&wc))?;
        }
        LocalOperation::Reanchor if journal.same_name => {
            let wc = workspace(engine, &journal.child_id, &journal.request_id)?.join("pair-wc");
            svn(&[
                "checkout",
                &join_svn(&observed.svn_root, &observed.pair_path),
                wc.to_str().context("wc path")?,
            ])?;
            let export = workspace(engine, &journal.child_id, &journal.request_id)?.join("parent");
            svn(&[
                "export",
                "--force",
                &join_svn(&observed.svn_root, &observed.parent_path),
                export.to_str().context("export path")?,
            ])?;
            copy_tree(&export, &wc)?;
            svn_in(&["add", "--force", "."], Some(&wc))?;
            svn_in(&["commit", "-m", message], Some(&wc))?;
        }
        LocalOperation::Reanchor => {
            let destination = format!("{}-g2", observed.pair_path);
            svn(&[
                "copy",
                &join_svn(&observed.svn_root, &observed.parent_path),
                &join_svn(&observed.svn_root, &destination),
                "-m",
                message,
            ])?;
        }
    }
    let after = svnlook_youngest(&observed.svn_root)?;
    if after <= before {
        bail!("SVN publication did not create a revision");
    }
    Ok(after)
}

fn publish_git(engine: &SyncEngine, journal: &Journal, observed: &Observed) -> Result<String> {
    git(
        &observed.pair_git,
        &[
            "fetch",
            "--no-tags",
            observed.parent_git.to_str().context("parent git")?,
            &format!(
                "{}:refs/reposync/pair-refresh/parent",
                observed.parent_branch
            ),
        ],
    )?;
    let old = git(&observed.pair_git, &["rev-parse", &observed.pair_branch])?;
    if git(
        &observed.pair_git,
        &[
            "rev-parse",
            "--verify",
            "refs/reposync/pair-refresh/generation/1",
        ],
    )
    .is_err()
    {
        git(
            &observed.pair_git,
            &[
                "update-ref",
                "refs/reposync/pair-refresh/generation/1",
                &old,
            ],
        )?;
    }
    let tree = if journal.operation == LocalOperation::Reanchor && !journal.same_name {
        git(
            &observed.pair_git,
            &["read-tree", "refs/reposync/pair-refresh/parent"],
        )?;
        git(&observed.pair_git, &["write-tree"])?
    } else {
        tree_from_published_svn(engine, journal, observed)?
    };
    let message = match journal.operation {
        LocalOperation::Update => "pair refresh integration",
        LocalOperation::Reanchor => "reanchor generation 2",
    };
    let sha = if journal.operation == LocalOperation::Update {
        git(
            &observed.pair_git,
            &[
                "commit-tree",
                &tree,
                "-p",
                &old,
                "-p",
                "refs/reposync/pair-refresh/parent",
                "-m",
                message,
            ],
        )?
    } else {
        git(
            &observed.pair_git,
            &["commit-tree", &tree, "-p", &old, "-m", message],
        )?
    };
    git(
        &observed.pair_git,
        &[
            "update-ref",
            "refs/reposync/pair-refresh/generation/2",
            &sha,
        ],
    )?;
    let move_branch = journal.operation == LocalOperation::Update || journal.same_name;
    if move_branch {
        let descendant = git(
            &observed.pair_git,
            &["merge-base", "--is-ancestor", &old, &sha],
        );
        if descendant.is_err() && !journal.same_name {
            bail!("protected ref rejected the integration update");
        }
        git(
            &observed.pair_git,
            &[
                "update-ref",
                &format!("refs/heads/{}", observed.pair_branch),
                &sha,
                &old,
            ],
        )?;
    }
    if journal.retire {
        git(
            &observed.pair_git,
            &[
                "update-ref",
                "refs/reposync/pair-refresh/generation/1-retired",
                &old,
            ],
        )?;
    }
    Ok(sha)
}

fn tree_from_published_svn(
    engine: &SyncEngine,
    journal: &Journal,
    observed: &Observed,
) -> Result<String> {
    let revision = journal.new_svn.context("published SVN revision")?;
    let export = workspace(engine, &journal.child_id, &journal.request_id)?.join("git-source");
    if export.exists() {
        std::fs::remove_dir_all(&export)?;
    }
    svn(&[
        "export",
        "-r",
        &revision.to_string(),
        &join_svn(&observed.svn_root, &published_path(journal, observed)),
        export.to_str().context("published tree")?,
    ])?;
    let index = workspace(engine, &journal.child_id, &journal.request_id)?.join("git-index");
    let _ = std::fs::remove_file(&index);
    let mut add = Command::new("git");
    add.arg("-C")
        .arg(&observed.pair_git)
        .arg("--work-tree")
        .arg(&export)
        .args(["add", "-A"])
        .env("GIT_INDEX_FILE", &index)
        .env("GIT_AUTHOR_NAME", "RepoSync")
        .env("GIT_AUTHOR_EMAIL", "pair-refresh@localhost")
        .env("GIT_COMMITTER_NAME", "RepoSync")
        .env("GIT_COMMITTER_EMAIL", "pair-refresh@localhost");
    run(&mut add)?;
    let mut write = Command::new("git");
    write
        .arg("-C")
        .arg(&observed.pair_git)
        .args(["write-tree"])
        .env("GIT_INDEX_FILE", &index);
    run(&mut write)
}

fn published_revision(root_url: &str, message: &str, previous: i64) -> Result<Option<i64>> {
    let youngest = svnlook_youngest(root_url)?;
    if youngest <= previous {
        return Ok(None);
    }
    let log = run(Command::new("svnlook")
        .args(["log", "-r", &youngest.to_string()])
        .arg(file_path(root_url)?))?;
    Ok((log.trim() == message).then_some(youngest))
}

fn published_path(journal: &Journal, observed: &Observed) -> String {
    if journal.operation == LocalOperation::Reanchor && !journal.same_name {
        format!("{}-g2", observed.pair_path)
    } else {
        observed.pair_path.clone()
    }
}

fn approval_digest(
    observed: &Observed,
    operation: LocalOperation,
    same_name: bool,
    retire: bool,
) -> String {
    let operation = match operation {
        LocalOperation::Update => "update",
        LocalOperation::Reanchor => "reanchor",
    };
    let mut material = vec![
        operation.to_string(),
        observed.parent_id.clone(),
        observed.pair_tip.clone(),
        observed.parent_tip.clone(),
        observed.pair_path.clone(),
        observed.parent_path.clone(),
        observed.pair_svn.to_string(),
        observed.parent_svn.to_string(),
        observed.mapped_pair_svn.to_string(),
        observed.mapped_parent_svn.to_string(),
        same_name.to_string(),
        retire.to_string(),
    ];
    material.extend(observed.pending_pair_git.clone());
    material.extend(observed.pending_parent_git.clone());
    hash(&material.iter().map(String::as_str).collect::<Vec<_>>())
}

fn receipt_of(journal: &Journal) -> LocalReceipt {
    LocalReceipt {
        operation_id: journal.operation_id,
        request_id: journal.request_id.clone(),
        child_id: journal.child_id.clone(),
        parent_id: journal.parent_id.clone(),
        operation: match journal.operation {
            LocalOperation::Update => "update".into(),
            LocalOperation::Reanchor => "reanchor".into(),
        },
        phase: journal.phase.clone(),
        noop: journal.noop,
        git_sha: journal.new_git.clone(),
        svn_revision: journal.new_svn,
        retained_ref: journal.retained_ref.clone(),
        new_svn_path: journal.new_svn_path.clone(),
        displaced: journal.displaced.clone(),
    }
}

fn journal_key(child: &str, request: &str) -> String {
    format!("{JOURNAL_PREFIX}{child}:{request}")
}

fn load(engine: &SyncEngine, key: &str) -> Result<Option<Journal>> {
    Ok(engine
        .db()
        .get_state(key)?
        .map(|value| serde_json::from_str(&value))
        .transpose()?)
}

fn save(engine: &SyncEngine, key: &str, journal: &Journal) -> Result<()> {
    engine
        .db()
        .set_state(key, &serde_json::to_string(journal)?)
        .context("refresh journal was not stored")
}

fn require_file_url(url: &str) -> Result<()> {
    if !url.starts_with("file://") {
        bail!("refresh refuses a non-local SVN URL");
    }
    Ok(())
}

fn join_svn(root: &str, branch: &str) -> String {
    format!(
        "{}/{}",
        root.trim_end_matches('/'),
        branch.trim_matches('/')
    )
}

fn file_path(url: &str) -> Result<PathBuf> {
    let rest = url.strip_prefix("file://").context("local SVN URL")?;
    Ok(PathBuf::from(rest))
}

fn git_dir(engine: &SyncEngine, id: &str) -> Result<PathBuf> {
    let path = engine
        .config()
        .daemon
        .data_dir
        .join("repos")
        .join(id)
        .join("git-repo");
    if !path.join("HEAD").exists() && !path.join(".git").exists() {
        bail!("local git repository is not available for {id}");
    }
    Ok(path)
}

fn workspace(engine: &SyncEngine, child: &str, request: &str) -> Result<PathBuf> {
    let path = engine
        .config()
        .daemon
        .data_dir
        .join("pair-refresh-local")
        .join(child)
        .join(request);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn svn(args: &[&str]) -> Result<String> {
    svn_in(args, None)
}

fn svn_in(args: &[&str], directory: Option<&Path>) -> Result<String> {
    let mut command = Command::new("svn");
    command
        .args(args)
        .args(["--non-interactive", "--no-auth-cache"]);
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    run(&mut command)
}

fn svn_revision(url: &str) -> Result<i64> {
    let text = svn(&["info", "--show-item", "revision", url])?;
    text.parse().context("SVN revision")
}

fn svnlook_youngest(root_url: &str) -> Result<i64> {
    let path = file_path(root_url)?;
    let text = run(Command::new("svnlook").arg("youngest").arg(path))?;
    text.parse().context("svnlook youngest")
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "RepoSync")
        .env("GIT_AUTHOR_EMAIL", "pair-refresh@localhost")
        .env("GIT_COMMITTER_NAME", "RepoSync")
        .env("GIT_COMMITTER_EMAIL", "pair-refresh@localhost");
    run(&mut command)
}

fn git_rev(repo: &Path, rev: &str) -> Result<String> {
    git(repo, &["rev-parse", "--verify", rev])
}

fn object_exists(repo: &Path, rev: &str) -> Result<bool> {
    Ok(git(repo, &["cat-file", "-e", &format!("{rev}^{{commit}}")]).is_ok())
}

fn run(command: &mut Command) -> Result<String> {
    let output = command.output().context("command failed to start")?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn copy_tree(from: &Path, into: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".svn" {
            continue;
        }
        let target = into.join(&name);
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::db::Database;
    use crate::git::GitClient;
    use crate::identity::IdentityMapper;
    use crate::models::{Repository, Session, User};
    use crate::pair_refresh::service::RefreshService;
    use crate::svn::SvnClient;
    use std::sync::Arc;

    fn command(program: &str, args: &[&str]) {
        let status = Command::new(program).args(args).status().unwrap();
        assert!(status.success(), "{program} {args:?}");
    }

    fn engine_for(root: &Path) -> SyncEngine {
        let mut config: AppConfig = toml::from_str(
            r#"
[daemon]
data_dir = "/unused"
[svn]
url = "file:///unused"
username = ""
password_env = ""
[github]
repo = "fixture/repo"
token_env = ""
"#,
        )
        .unwrap();
        config.daemon.data_dir = root.to_path_buf();
        let db = Database::new(root.join("reposync.db")).unwrap();
        db.initialize().unwrap();
        db.set_state("encryption_key", &"41".repeat(32)).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        db.insert_user(&User {
            id: "admin".into(),
            username: "admin".into(),
            display_name: "Admin".into(),
            email: "admin@localhost".into(),
            password_hash: "x".into(),
            role: "admin".into(),
            enabled: true,
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .unwrap();
        db.insert_session(&Session {
            token: "local-session".into(),
            user_id: "admin".into(),
            expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            created_at: now,
        })
        .unwrap();
        let dummy = root.join("engine.git");
        command("git", &["init", dummy.to_str().unwrap()]);
        let identity = Arc::new(IdentityMapper::new(&config.identity).unwrap());
        SyncEngine::new(
            config,
            db,
            SvnClient::new("file:///unused", "", ""),
            GitClient::new(&dummy).unwrap(),
            identity,
        )
    }

    fn repo(
        id: &str,
        parent: Option<&str>,
        root: &str,
        branch: &str,
        svn_rev: i64,
        git: &str,
    ) -> Repository {
        let now = chrono::Utc::now().to_rfc3339();
        Repository {
            id: id.into(),
            name: id.into(),
            svn_url: root.into(),
            svn_branch: branch.into(),
            svn_username: String::new(),
            git_provider: "gitea".into(),
            git_api_url: "file://local".into(),
            git_repo: "local".into(),
            git_branch: if parent.is_some() { "feature" } else { "main" }.into(),
            sync_mode: "direct".into(),
            poll_interval_secs: 60,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: parent.map(str::to_string),
            created_at: now.clone(),
            updated_at: now,
            last_svn_rev: svn_rev,
            last_git_sha: git.into(),
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        }
    }

    fn git_commit(repo: &Path, file: &str, content: &str) -> String {
        std::fs::write(repo.join(file), content).unwrap();
        command("git", &["-C", repo.to_str().unwrap(), "add", file]);
        command(
            "git",
            &[
                "-C",
                repo.to_str().unwrap(),
                "-c",
                "user.name=RepoSync",
                "-c",
                "user.email=pair-refresh@localhost",
                "commit",
                "-m",
                content,
            ],
        );
        run(&mut Command::new("git").args(["-C", repo.to_str().unwrap(), "rev-parse", "HEAD"]))
            .unwrap()
    }

    #[tokio::test]
    async fn local_service_preserves_svn_only_work_and_reanchors() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let engine = Arc::new(engine_for(&root));
        let svn_dir = root.join("svn");
        command("svnadmin", &["create", svn_dir.to_str().unwrap()]);
        let svn_url = format!("file://{}", svn_dir.display());
        command(
            "svn",
            &[
                "mkdir",
                &format!("{svn_url}/trunk"),
                &format!("{svn_url}/branches"),
                "-m",
                "layout",
                "--non-interactive",
            ],
        );
        let parent_wc = root.join("parent-wc");
        command(
            "svn",
            &[
                "checkout",
                &format!("{svn_url}/trunk"),
                parent_wc.to_str().unwrap(),
                "--non-interactive",
            ],
        );
        std::fs::write(parent_wc.join("readme.txt"), "origin\n").unwrap();
        let status = Command::new("svn")
            .current_dir(&parent_wc)
            .args(["add", "readme.txt", "--non-interactive"])
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("svn")
            .current_dir(&parent_wc)
            .args(["commit", "-m", "origin", "--non-interactive"])
            .status()
            .unwrap();
        assert!(status.success());
        command(
            "svn",
            &[
                "copy",
                &format!("{svn_url}/trunk"),
                &format!("{svn_url}/branches/feature"),
                "-m",
                "pair",
                "--non-interactive",
            ],
        );
        let feature_base = svn_revision(&format!("{svn_url}/branches/feature")).unwrap();
        std::fs::write(parent_wc.join("readme.txt"), "parent\n").unwrap();
        let status = Command::new("svn")
            .current_dir(&parent_wc)
            .args(["commit", "-m", "parent", "--non-interactive"])
            .status()
            .unwrap();
        assert!(status.success());
        let parent_svn = svn_revision(&format!("{svn_url}/trunk")).unwrap();
        let feature_wc = root.join("feature-wc");
        command(
            "svn",
            &[
                "checkout",
                &format!("{svn_url}/branches/feature"),
                feature_wc.to_str().unwrap(),
                "--non-interactive",
            ],
        );
        std::fs::write(feature_wc.join("notes.txt"), "svn-only\n").unwrap();
        let status = Command::new("svn")
            .current_dir(&feature_wc)
            .args(["add", "notes.txt", "--non-interactive"])
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("svn")
            .current_dir(&feature_wc)
            .args(["commit", "-m", "notes", "--non-interactive"])
            .status()
            .unwrap();
        assert!(status.success());
        let notes_rev = svn_revision(&format!("{svn_url}/branches/feature")).unwrap();
        assert!(notes_rev > feature_base);

        let parent_git = root.join("repos/parent/git-repo");
        let pair_git = root.join("repos/child/git-repo");
        std::fs::create_dir_all(&parent_git).unwrap();
        std::fs::create_dir_all(&pair_git).unwrap();
        command("git", &["init", "-b", "main", parent_git.to_str().unwrap()]);
        command(
            "git",
            &["init", "-b", "feature", pair_git.to_str().unwrap()],
        );
        let origin = git_commit(&parent_git, "readme.txt", "origin\n");
        std::fs::write(pair_git.join("readme.txt"), "origin\n").unwrap();
        command(
            "git",
            &["-C", pair_git.to_str().unwrap(), "add", "readme.txt"],
        );
        command(
            "git",
            &[
                "-C",
                pair_git.to_str().unwrap(),
                "-c",
                "user.name=RepoSync",
                "-c",
                "user.email=pair-refresh@localhost",
                "commit",
                "-m",
                "origin",
            ],
        );
        let pair_origin = run(&mut Command::new("git").args([
            "-C",
            pair_git.to_str().unwrap(),
            "rev-parse",
            "HEAD",
        ]))
        .unwrap();
        let parent_tip = git_commit(&parent_git, "readme.txt", "parent\n");
        assert_ne!(origin, parent_tip);

        engine
            .db()
            .insert_repository(&repo(
                "parent",
                None,
                &svn_url,
                "trunk",
                feature_base,
                &origin,
            ))
            .unwrap();
        // Parent checkpoint stays at the origin SVN revision so the parent advance is pending.
        let mut parent = engine.db().get_repository("parent").unwrap().unwrap();
        parent.last_svn_rev = feature_base;
        parent.last_git_sha = origin.clone();
        engine.db().update_repository(&parent).unwrap();
        engine
            .db()
            .insert_repository(&repo(
                "child",
                Some("parent"),
                &svn_url,
                "branches/feature",
                feature_base,
                &pair_origin,
            ))
            .unwrap();
        let mut child = engine.db().get_repository("child").unwrap().unwrap();
        child.last_svn_rev = feature_base;
        child.last_git_sha = pair_origin.clone();
        engine.db().update_repository(&child).unwrap();

        let service = RefreshService::new(engine.clone());
        let youngest_before = svnlook_youngest(&svn_url).unwrap();
        let error = service
            .run_admitted_local(
                "local-session",
                "child",
                LocalRequest {
                    request_id: "svn-only".into(),
                    approval_digest: String::new(),
                    operation: LocalOperation::Update,
                    same_name: false,
                    retire: false,
                    approve_current: true,
                    stop_after: None,
                },
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("pending svn work is preserved"),
            "{error}"
        );
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), youngest_before);
        let notes = run(&mut Command::new("svn").args([
            "cat",
            "-r",
            &notes_rev.to_string(),
            &format!("{svn_url}/branches/feature/notes.txt"),
            "--non-interactive",
        ]))
        .unwrap();
        assert_eq!(notes, "svn-only");

        // Drop the SVN-only revision from the pending count by checkpointing it,
        // then integrate the parent advance.
        let mut child = engine.db().get_repository("child").unwrap().unwrap();
        child.last_svn_rev = notes_rev;
        engine.db().update_repository(&child).unwrap();
        let digest =
            current_digest(&engine, "child", LocalOperation::Update, false, false).unwrap();
        let receipt = service
            .run_admitted_local(
                "local-session",
                "child",
                LocalRequest {
                    request_id: "parent-advance".into(),
                    approval_digest: digest,
                    operation: LocalOperation::Update,
                    same_name: false,
                    retire: false,
                    approve_current: false,
                    stop_after: Some("svn_observed".into()),
                },
            )
            .await
            .unwrap_err();
        assert!(
            receipt.to_string().contains("crashed after svn_observed"),
            "{receipt}"
        );
        let after_svn = svnlook_youngest(&svn_url).unwrap();
        let resumed = service
            .run_admitted_local(
                "local-session",
                "child",
                LocalRequest {
                    request_id: "parent-advance".into(),
                    approval_digest: current_digest(
                        &engine,
                        "child",
                        LocalOperation::Update,
                        false,
                        false,
                    )
                    .unwrap_or_default(),
                    operation: LocalOperation::Update,
                    same_name: false,
                    retire: false,
                    approve_current: true,
                    stop_after: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), after_svn);
        assert!(!resumed.noop);
        let integrated = run(&mut Command::new("git").args([
            "-C",
            pair_git.to_str().unwrap(),
            "show",
            "HEAD:readme.txt",
        ]))
        .unwrap();
        assert_eq!(integrated, "parent");
        let kept = run(&mut Command::new("git").args([
            "-C",
            pair_git.to_str().unwrap(),
            "show",
            "HEAD:notes.txt",
        ]))
        .unwrap();
        assert_eq!(kept, "svn-only");
        let published = resumed.svn_revision.unwrap().to_string();
        let published_notes = run(&mut Command::new("svn").args([
            "cat",
            "-r",
            &published,
            &format!("{svn_url}/branches/feature/notes.txt"),
            "--non-interactive",
        ]))
        .unwrap();
        assert_eq!(published_notes, "svn-only");
        let youngest = svnlook_youngest(&svn_url).unwrap();
        let stale = service
            .run_admitted_local(
                "local-session",
                "child",
                LocalRequest {
                    request_id: "stale-digest".into(),
                    approval_digest: "not-the-current-digest".into(),
                    operation: LocalOperation::Update,
                    same_name: false,
                    retire: false,
                    approve_current: false,
                    stop_after: None,
                },
            )
            .await
            .unwrap_err();
        assert!(stale.to_string().contains("stale preview"), "{stale}");
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), youngest);
        let again = service
            .run_admitted_local(
                "local-session",
                "child",
                LocalRequest {
                    request_id: "parent-advance".into(),
                    approval_digest: String::new(),
                    operation: LocalOperation::Update,
                    same_name: false,
                    retire: false,
                    approve_current: true,
                    stop_after: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(again.operation_id, resumed.operation_id);

        let reanchor = service
            .run_admitted_local(
                "local-session",
                "child",
                LocalRequest {
                    request_id: "reanchor".into(),
                    approval_digest: String::new(),
                    operation: LocalOperation::Reanchor,
                    same_name: false,
                    retire: true,
                    approve_current: true,
                    stop_after: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            reanchor.new_svn_path.as_deref(),
            Some("branches/feature-g2")
        );
        let old_notes = run(&mut Command::new("svn").args([
            "cat",
            "-r",
            &notes_rev.to_string(),
            &format!("{svn_url}/branches/feature/notes.txt"),
            "--non-interactive",
        ]))
        .unwrap();
        assert_eq!(old_notes, "svn-only");
        let retained = run(&mut Command::new("git").args([
            "-C",
            pair_git.to_str().unwrap(),
            "rev-parse",
            "refs/reposync/pair-refresh/generation/1",
        ]))
        .unwrap();
        assert_eq!(retained, pair_origin);
        let branch = run(&mut Command::new("git").args([
            "-C",
            pair_git.to_str().unwrap(),
            "rev-parse",
            "refs/heads/feature",
        ]))
        .unwrap();
        assert_eq!(branch, resumed.git_sha.clone().unwrap());
        assert!(parent_svn > feature_base);
    }

    #[tokio::test]
    async fn local_service_refuses_a_network_svn_url() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let engine = Arc::new(engine_for(&root));
        for id in ["parent", "child"] {
            let git = root.join("repos").join(id).join("git-repo");
            std::fs::create_dir_all(&git).unwrap();
            command("git", &["init", "-b", "main", git.to_str().unwrap()]);
            let _ = git_commit(&git, "readme.txt", "origin\n");
        }
        let parent_sha = run(&mut Command::new("git").args([
            "-C",
            root.join("repos/parent/git-repo").to_str().unwrap(),
            "rev-parse",
            "HEAD",
        ]))
        .unwrap();
        let child_sha = run(&mut Command::new("git").args([
            "-C",
            root.join("repos/child/git-repo").to_str().unwrap(),
            "rev-parse",
            "HEAD",
        ]))
        .unwrap();
        engine
            .db()
            .insert_repository(&repo(
                "parent",
                None,
                "https://svn.example.invalid/repo",
                "trunk",
                1,
                &parent_sha,
            ))
            .unwrap();
        engine
            .db()
            .insert_repository(&repo(
                "child",
                Some("parent"),
                "https://svn.example.invalid/repo",
                "branches/feature",
                1,
                &child_sha,
            ))
            .unwrap();
        let service = RefreshService::new(engine);
        let error = service
            .run_admitted_local(
                "local-session",
                "child",
                LocalRequest {
                    request_id: "network".into(),
                    approval_digest: String::new(),
                    operation: LocalOperation::Update,
                    same_name: false,
                    retire: false,
                    approve_current: true,
                    stop_after: None,
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("non-local SVN URL"), "{error}");
    }
}
