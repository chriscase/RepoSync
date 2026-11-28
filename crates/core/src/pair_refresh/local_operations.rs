//! Local file:// Git and SVN refresh and re-anchor.
//!
//! Network URLs are refused. Published history is not deleted and nothing is
//! force-pushed. A journal row is written before each side effect.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use rusqlite::params;
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
    mapped_pair_svn: i64,
    mapped_parent_svn: i64,
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
    validate_request_id(&request.request_id)?;
    let key = journal_key(child_id, &request.request_id);
    if let Some(existing) = load(engine, &key)? {
        let digest_ok = if request.approval_digest.is_empty() {
            request.approve_current
        } else {
            existing.digest == request.approval_digest
        };
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
        let _busy = claim_pair(engine, child_id, &existing.parent_id, &request.request_id)?;
        return drive(engine, existing, request.stop_after.as_deref());
    }
    let observed = observe(engine, child_id)?;
    let _busy = claim_pair(engine, child_id, &observed.parent_id, &request.request_id)?;
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
        mapped_pair_svn: observed.mapped_pair_svn,
        mapped_parent_svn: observed.mapped_parent_svn,
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
    adopt_own_svn(engine, &key, &mut journal)?;
    let observed = observe(engine, &journal.child_id)?;
    require_pins(&journal, &observed)?;
    if journal.noop {
        journal.phase = "completed".into();
        save(engine, &key, &journal)?;
        return Ok(receipt_of(&journal));
    }
    mark(engine, &key, &mut journal, "svn_intent", stop_after)?;
    if journal.new_svn.is_none() {
        let observed = observe(engine, &journal.child_id)?;
        adopt_own_svn(engine, &key, &mut journal)?;
        if journal.new_svn.is_none() {
            require_pins(&journal, &observed)?;
            let revision = match publish_svn(engine, &journal, &observed) {
                Ok(revision) => revision,
                Err(error) => {
                    if error.to_string().contains("resolution required") {
                        clear_local_attempt(engine, &key, &journal)?;
                    }
                    return Err(error);
                }
            };
            if stop_after == Some("svn_committed") {
                bail!("crashed after svn commit before the journal receipt");
            }
            journal.new_svn = Some(revision);
            journal.new_svn_path = Some(published_path(&journal, &observed));
            save(engine, &key, &journal)?;
        }
    }
    mark(engine, &key, &mut journal, "svn_observed", stop_after)?;
    mark(engine, &key, &mut journal, "git_intent", stop_after)?;
    if journal.new_git.is_none() {
        let observed = observe(engine, &journal.child_id)?;
        require_pins(&journal, &observed)?;
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
    let parent_mark = (journal.operation == LocalOperation::Update)
        .then(|| (journal.parent_svn, journal.parent_tip.clone()));
    journal.phase = "checkpoint".into();
    save(engine, &key, &journal)?;
    if stop_after == Some("checkpoint") {
        bail!("crashed after checkpoint");
    }
    journal.phase = "completed".into();
    write_journal(
        engine,
        &key,
        &journal,
        Some((svn_rev, git_sha)),
        parent_mark,
    )?;
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
    let token = publication_token(journal);
    let path = published_path(journal, observed);
    if let Some(existing) = own_revision(&observed.svn_root, &token, &path, journal.pair_svn)? {
        return Ok(existing);
    }
    let before = svnlook_youngest(&observed.svn_root)?;
    let pair_rev = journal.pair_svn.to_string();
    let parent_rev = journal.parent_svn.to_string();
    match journal.operation {
        LocalOperation::Update => {
            let space = workspace(engine, &journal.child_id, &journal.request_id)?;
            let wc = space.join("pair-wc");
            let pair_tree = space.join("pair-tree");
            let parent_new = space.join("parent-new");
            let parent_old = space.join("parent-old");
            remove_confined(&wc, &space)?;
            let pair_url = join_svn(&observed.svn_root, &observed.pair_path);
            let parent_url = join_svn(&observed.svn_root, &observed.parent_path);
            svn(&[
                "checkout",
                "-r",
                &pair_rev,
                &format!("{pair_url}@{pair_rev}"),
                wc.to_str().context("wc path")?,
            ])?;
            export_revision(&pair_url, &pair_rev, &pair_tree)?;
            export_revision(&parent_url, &parent_rev, &parent_new)?;
            export_revision(
                &parent_url,
                &journal.mapped_parent_svn.to_string(),
                &parent_old,
            )?;
            let merged = merge_projection(
                TreeSide {
                    url: &parent_url,
                    revision: journal.mapped_parent_svn,
                    dir: &parent_old,
                },
                TreeSide {
                    url: &parent_url,
                    revision: journal.parent_svn,
                    dir: &parent_new,
                },
                TreeSide {
                    url: &pair_url,
                    revision: journal.pair_svn,
                    dir: &pair_tree,
                },
            )?;
            apply_tree(&wc, &merged)?;
            record_mergeinfo(
                &wc,
                &observed.parent_path,
                journal.mapped_parent_svn,
                journal.parent_svn,
            )?;
            svn_in(&["commit", "-m", &token], Some(&wc))?;
        }
        LocalOperation::Reanchor if journal.same_name => {
            let space = workspace(engine, &journal.child_id, &journal.request_id)?;
            let wc = space.join("pair-wc");
            let parent_new = space.join("parent-new");
            remove_confined(&wc, &space)?;
            svn(&[
                "checkout",
                "-r",
                &pair_rev,
                &join_svn(&observed.svn_root, &observed.pair_path),
                wc.to_str().context("wc path")?,
            ])?;
            export_revision(
                &join_svn(&observed.svn_root, &observed.parent_path),
                &parent_rev,
                &parent_new,
            )?;
            let tree = list_nodes(
                &parent_new,
                &join_svn(&observed.svn_root, &observed.parent_path),
                journal.parent_svn,
            )?;
            apply_tree(&wc, &tree)?;
            svn_in(&["commit", "-m", &token], Some(&wc))?;
        }
        LocalOperation::Reanchor => {
            let destination = format!("{}-g2", observed.pair_path);
            svn(&[
                "copy",
                "-r",
                &parent_rev,
                &join_svn(&observed.svn_root, &observed.parent_path),
                &join_svn(&observed.svn_root, &destination),
                "-m",
                &token,
            ])?;
        }
    }
    let published = own_revision(&observed.svn_root, &token, &path, journal.pair_svn)?
        .context("SVN publication did not record this operation")?;
    if published <= before {
        bail!("SVN publication did not create a revision");
    }
    Ok(published)
}

fn publish_git(engine: &SyncEngine, journal: &Journal, observed: &Observed) -> Result<String> {
    pin_parent_commit(
        &observed.pair_git,
        &observed.parent_git,
        &journal.parent_tip,
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
    let space = workspace(engine, &journal.child_id, &journal.request_id)?;
    let export = space.join("git-source");
    remove_confined(&export, &space)?;
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

fn publication_token(journal: &Journal) -> String {
    format!("reposync-pair-refresh {}", journal.operation_id)
}

fn own_revision(root_url: &str, token: &str, path: &str, predecessor: i64) -> Result<Option<i64>> {
    let youngest = svnlook_youngest(root_url)?;
    if youngest <= predecessor {
        return Ok(None);
    }
    let root = file_path(root_url)?;
    let mut found = None;
    for revision in (predecessor + 1)..=youngest {
        let log = svnlook(&root, &["log", "-r", &revision.to_string()])?;
        if log.trim() != token {
            continue;
        }
        let changed = svnlook(&root, &["changed", "-r", &revision.to_string()])?;
        if !revision_touches(&changed, path) {
            bail!("pair refresh publication identity was recorded on a different path");
        }
        if found.is_some() {
            bail!("ambiguous pair refresh publication");
        }
        found = Some(revision);
    }
    if let Some(revision) = found {
        for later in (revision + 1)..=youngest {
            let changed = svnlook(&root, &["changed", "-r", &later.to_string()])?;
            if revision_touches(&changed, path) {
                bail!("intervening svn revision hides the pair refresh publication");
            }
        }
    }
    Ok(found)
}

fn revision_touches(changed: &str, path: &str) -> bool {
    let prefix = path.trim_matches('/');
    changed.lines().any(|line| {
        let entry = line.split_whitespace().nth(1).unwrap_or("");
        entry == prefix || entry.starts_with(&format!("{prefix}/"))
    })
}

fn svnlook(root: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("svnlook");
    command.args(args).arg(root);
    run(&mut command)
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
    write_journal(engine, key, journal, None, None)
}

fn write_journal(
    engine: &SyncEngine,
    key: &str,
    journal: &Journal,
    child_mark: Option<(i64, String)>,
    parent_mark: Option<(i64, String)>,
) -> Result<()> {
    let payload = serde_json::to_string(journal)?;
    let hold_key = format!("pair_refresh_local_hold:{}", journal.child_id);
    let hold = serde_json::json!({
        "child": journal.child_id,
        "parent": journal.parent_id,
        "request_id": journal.request_id,
        "operation_id": journal.operation_id,
    })
    .to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let child_id = journal.child_id.clone();
    let parent_id = journal.parent_id.clone();
    let completed = journal.phase == "completed";
    engine
        .db()
        .immediate_transaction(|tx| {
            tx.execute(
                "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, payload, now],
            )?;
            if completed || journal.phase == "refused" {
                tx.execute("DELETE FROM kv_state WHERE key = ?1", params![hold_key])?;
                if let Some((rev, sha)) = &child_mark {
                    tx.execute(
                        "UPDATE repositories SET last_svn_rev = ?1, last_git_sha = ?2, last_sync_at = datetime('now') WHERE id = ?3",
                        params![rev, sha, child_id],
                    )?;
                    tx.execute(
                        "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
                         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                        params![format!("last_svn_rev_{child_id}"), rev.to_string(), now],
                    )?;
                }
                if let Some((rev, sha)) = &parent_mark {
                    tx.execute(
                        "UPDATE repositories SET last_svn_rev = ?1, last_git_sha = ?2, last_sync_at = datetime('now') WHERE id = ?3",
                        params![rev, sha, parent_id],
                    )?;
                    tx.execute(
                        "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
                         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                        params![format!("last_svn_rev_{parent_id}"), rev.to_string(), now],
                    )?;
                }
            } else {
                tx.execute(
                    "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                    params![hold_key, hold, now],
                )?;
            }
            Ok(())
        })
        .context("refresh journal was not stored")
}

fn clear_local_attempt(engine: &SyncEngine, key: &str, journal: &Journal) -> Result<()> {
    let hold_key = format!("pair_refresh_local_hold:{}", journal.child_id);
    engine
        .db()
        .immediate_transaction(|tx| {
            tx.execute("DELETE FROM kv_state WHERE key = ?1", params![key])?;
            tx.execute("DELETE FROM kv_state WHERE key = ?1", params![hold_key])?;
            Ok(())
        })
        .context("refresh reservation was not released")
}

fn adopt_own_svn(engine: &SyncEngine, key: &str, journal: &mut Journal) -> Result<()> {
    if journal.new_svn.is_some() {
        return Ok(());
    }
    let observed = observe(engine, &journal.child_id)?;
    let path = published_path(journal, &observed);
    if let Some(revision) = own_revision(
        &observed.svn_root,
        &publication_token(journal),
        &path,
        journal.pair_svn,
    )? {
        journal.new_svn = Some(revision);
        journal.new_svn_path = Some(path);
        save(engine, key, journal)?;
    }
    Ok(())
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

fn validate_request_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 200
        || id.contains("..")
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
        || Path::new(id).is_absolute()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
    {
        bail!("refresh request id is not a confined workspace key");
    }
    Ok(())
}

fn claim_pair(
    engine: &SyncEngine,
    child: &str,
    parent: &str,
    request_id: &str,
) -> Result<crate::busy::PairBusyGuard> {
    {
        let conn = engine.db().conn();
        crate::db::pair_refresh_operations::require_no_versioned_pair_refresh_hold(
            &conn,
            Some(child),
        )
        .map_err(|error| anyhow::anyhow!("durable pair refresh hold: {error}"))?;
        crate::db::pair_refresh_operations::require_no_versioned_pair_refresh_hold(
            &conn,
            Some(parent),
        )
        .map_err(|error| anyhow::anyhow!("durable pair refresh hold: {error}"))?;
        crate::db::pair_refresh_operations::require_no_foreign_local_hold(
            &conn, child, parent, request_id,
        )
        .map_err(|error| anyhow::anyhow!("durable pair refresh hold: {error}"))?;
    }
    crate::busy::try_acquire_pair(child, parent).context("pair refresh writer is busy")
}

fn pin_parent_commit(pair: &Path, parent: &Path, sha: &str) -> Result<()> {
    git(
        pair,
        &[
            "fetch",
            "--no-tags",
            parent.to_str().context("parent git")?,
            &format!("{sha}:refs/reposync/pair-refresh/parent"),
        ],
    )?;
    let fetched = git(pair, &["rev-parse", "refs/reposync/pair-refresh/parent"])?;
    if fetched != sha {
        bail!("parent git publication was not the admitted commit");
    }
    Ok(())
}

fn require_pins(journal: &Journal, observed: &Observed) -> Result<()> {
    if observed.parent_tip != journal.parent_tip {
        bail!("refresh pins drifted; resume was refused");
    }
    let pair_tip_ok = observed.pair_tip == journal.pair_tip
        || journal.new_git.as_deref() == Some(observed.pair_tip.as_str());
    if !pair_tip_ok {
        bail!("refresh pins drifted; resume was refused");
    }
    if let Some(published) = journal.new_svn {
        let path = journal
            .new_svn_path
            .clone()
            .unwrap_or_else(|| published_path(journal, observed));
        let found = own_revision(
            &observed.svn_root,
            &publication_token(journal),
            &path,
            journal.pair_svn,
        )?;
        if found != Some(published) {
            bail!("refresh pins drifted; resume was refused");
        }
    } else if observed.pair_svn != journal.pair_svn || observed.parent_svn != journal.parent_svn {
        bail!("refresh pins drifted; resume was refused");
    }
    Ok(())
}

fn workspace(engine: &SyncEngine, child: &str, request: &str) -> Result<PathBuf> {
    validate_request_id(request)?;
    let root = engine.config().daemon.data_dir.join("pair-refresh-local");
    std::fs::create_dir_all(&root)?;
    let path = root.join(hash(&[child])).join(hash(&[request]));
    if path.exists() {
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            bail!("refresh workspace symlink is refused");
        }
    }
    std::fs::create_dir_all(&path)?;
    let canonical = path.canonicalize()?;
    let root = root.canonicalize()?;
    if !canonical.starts_with(&root) {
        bail!("refresh workspace escaped its directory");
    }
    Ok(canonical)
}

fn remove_confined(dir: &Path, workspace: &Path) -> Result<()> {
    if !dir.exists() && std::fs::symlink_metadata(dir).is_err() {
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        bail!("refresh workspace symlink is refused");
    }
    if !meta.is_dir() {
        bail!("refresh workspace entry is not a directory");
    }
    let canonical = dir.canonicalize()?;
    let root = workspace.canonicalize()?;
    if !canonical.starts_with(&root) {
        bail!("refresh workspace escaped its directory");
    }
    std::fs::remove_dir_all(canonical)?;
    Ok(())
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

fn export_revision(url: &str, revision: &str, dest: &Path) -> Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    svn(&[
        "export",
        "-r",
        revision,
        &format!("{url}@{revision}"),
        dest.to_str().context("export path")?,
    ])?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NodeKind {
    File,
    Dir,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Node {
    kind: NodeKind,
    bytes: Vec<u8>,
    props: BTreeMap<String, Vec<u8>>,
}

fn list_nodes(dir: &Path, url: &str, revision: i64) -> Result<BTreeMap<String, Node>> {
    let mut out = BTreeMap::new();
    let revision = revision.to_string();
    let source = Some((url, revision.as_str()));
    record_node(dir, "", source, &mut out)?;
    walk_nodes(dir, dir, source, &mut out)?;
    Ok(out)
}

fn list_working_copy(dir: &Path) -> Result<BTreeMap<String, Node>> {
    let mut out = BTreeMap::new();
    record_node(dir, "", None, &mut out)?;
    walk_nodes(dir, dir, None, &mut out)?;
    Ok(out)
}

fn record_node(
    root: &Path,
    rel: &str,
    source: Option<(&str, &str)>,
    out: &mut BTreeMap<String, Node>,
) -> Result<()> {
    let full = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let target = if let Some((url, _)) = source {
        if rel.is_empty() {
            url.to_string()
        } else {
            join_svn(url, rel)
        }
    } else {
        full.to_str().context("working copy path")?.to_string()
    };
    let revision = source.map(|(_, revision)| revision);
    let mut props = svn_props(&target, revision)?;
    props.remove("svn:mergeinfo");
    let meta = std::fs::symlink_metadata(&full)?;
    if meta.file_type().is_symlink() {
        bail!("refresh refuses a symlink in the published tree");
    }
    if meta.is_dir() {
        out.insert(
            rel.to_string(),
            Node {
                kind: NodeKind::Dir,
                bytes: Vec::new(),
                props,
            },
        );
    } else {
        out.insert(
            rel.to_string(),
            Node {
                kind: NodeKind::File,
                bytes: std::fs::read(&full)?,
                props,
            },
        );
    }
    Ok(())
}

fn walk_nodes(
    root: &Path,
    dir: &Path,
    source: Option<(&str, &str)>,
    out: &mut BTreeMap<String, Node>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == ".svn" {
            continue;
        }
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.file_type().is_symlink() {
            bail!("refresh refuses a symlink in the published tree");
        }
        let full = entry.path();
        let rel = full
            .strip_prefix(root)
            .context("published path escaped the export")?
            .to_string_lossy()
            .replace('\\', "/");
        record_node(root, &rel, source, out)?;
        if meta.is_dir() {
            walk_nodes(root, &full, source, out)?;
        }
    }
    Ok(())
}

struct TreeSide<'a> {
    url: &'a str,
    revision: i64,
    dir: &'a Path,
}

fn merge_projection(
    parent_old: TreeSide<'_>,
    parent_new: TreeSide<'_>,
    pair_now: TreeSide<'_>,
) -> Result<BTreeMap<String, Node>> {
    let old = list_nodes(parent_old.dir, parent_old.url, parent_old.revision)?;
    let new = list_nodes(parent_new.dir, parent_new.url, parent_new.revision)?;
    let ours = list_nodes(pair_now.dir, pair_now.url, pair_now.revision)?;
    let mut paths = std::collections::BTreeSet::new();
    paths.extend(old.keys().cloned());
    paths.extend(new.keys().cloned());
    paths.extend(ours.keys().cloned());
    let mut result = BTreeMap::new();
    for path in paths {
        if let Some(node) = merge_node(old.get(&path), new.get(&path), ours.get(&path), &path)? {
            result.insert(path, node);
        }
    }
    Ok(result)
}

fn merge_node(
    base: Option<&Node>,
    parent: Option<&Node>,
    pair: Option<&Node>,
    path: &str,
) -> Result<Option<Node>> {
    if pair == parent || pair == base {
        return Ok(if pair == base {
            parent.cloned()
        } else {
            pair.cloned()
        });
    }
    if parent == base {
        return Ok(pair.cloned());
    }
    let same_kind = base.map(|node| node.kind.clone()) == parent.map(|node| node.kind.clone())
        && parent.map(|node| node.kind.clone()) == pair.map(|node| node.kind.clone());
    let same_bytes = base.map(|node| &node.bytes) == parent.map(|node| &node.bytes)
        && parent.map(|node| &node.bytes) == pair.map(|node| &node.bytes);
    if same_kind && same_bytes {
        let source = pair.or(parent).or(base).context("empty projection")?;
        return Ok(Some(Node {
            kind: source.kind.clone(),
            bytes: source.bytes.clone(),
            props: merge_props(base, parent, pair, path)?,
        }));
    }
    bail!("resolution required for {path}");
}

fn merge_props(
    base: Option<&Node>,
    parent: Option<&Node>,
    pair: Option<&Node>,
    path: &str,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut names = std::collections::BTreeSet::new();
    for node in [base, parent, pair].into_iter().flatten() {
        names.extend(node.props.keys().cloned());
    }
    let mut merged = BTreeMap::new();
    for name in names {
        let base_value = base.and_then(|node| node.props.get(&name));
        let parent_value = parent.and_then(|node| node.props.get(&name));
        let pair_value = pair.and_then(|node| node.props.get(&name));
        let value = if pair_value == parent_value || pair_value == base_value {
            if pair_value == base_value {
                parent_value
            } else {
                pair_value
            }
        } else if parent_value == base_value {
            pair_value
        } else {
            bail!("resolution required for {path} property {name}");
        };
        if let Some(value) = value {
            merged.insert(name, value.clone());
        }
    }
    Ok(merged)
}

fn apply_tree(wc: &Path, tree: &BTreeMap<String, Node>) -> Result<()> {
    let current = list_working_copy(wc)?;
    let mut doomed = Vec::new();
    for (path, node) in &current {
        match tree.get(path) {
            None => doomed.push(path.clone()),
            Some(wanted) if wanted.kind != node.kind => doomed.push(path.clone()),
            Some(_) => {}
        }
    }
    doomed.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
    for path in doomed {
        if !path.is_empty() && wc.join(&path).exists() {
            svn_in(&["delete", "--force", &path], Some(wc))?;
        }
    }
    let mut create: Vec<_> = tree.iter().collect();
    create.sort_by_key(|(path, _)| path.matches('/').count());
    for (path, node) in create {
        if path.is_empty() {
            continue;
        }
        let target = wc.join(path);
        match node.kind {
            NodeKind::Dir => std::fs::create_dir_all(&target)?,
            NodeKind::File => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target, &node.bytes)?;
            }
        }
    }
    svn_in(&["add", "--force", "."], Some(wc))?;
    for (path, node) in tree {
        sync_props(wc, path, &node.props)?;
    }
    Ok(())
}

fn sync_props(wc: &Path, rel: &str, wanted: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let target_path = if rel.is_empty() {
        wc.to_path_buf()
    } else {
        wc.join(rel)
    };
    let target = target_path.to_str().context("property path")?;
    let mut current = svn_props(target, None)?;
    current.remove("svn:mergeinfo");
    for (name, value) in wanted {
        if current.get(name) != Some(value) {
            let payload = wc.with_file_name(format!("prop-{}", hash(&[rel, name])));
            std::fs::write(&payload, value)?;
            let payload = payload.to_str().context("property payload")?;
            svn(&["propset", name, "-F", payload, target])?;
            let _ = std::fs::remove_file(payload);
        }
    }
    for name in current.keys() {
        if !wanted.contains_key(name) {
            svn(&["propdel", name, target])?;
        }
    }
    Ok(())
}

fn record_mergeinfo(wc: &Path, parent_path: &str, mapped: i64, parent_rev: i64) -> Result<()> {
    if parent_rev <= mapped {
        return Ok(());
    }
    let current = svn_in(&["propget", "svn:mergeinfo", "."], Some(wc)).unwrap_or_default();
    let path = format!("/{}", parent_path.trim_matches('/'));
    let merged = union_mergeinfo(&current, &path, mapped + 1, parent_rev)?;
    svn_in(&["propset", "svn:mergeinfo", &merged, "."], Some(wc))?;
    Ok(())
}

#[derive(Clone, Copy)]
struct MergeRange {
    start: i64,
    end: i64,
    non_inheritable: bool,
}

fn union_mergeinfo(existing: &str, path: &str, start: i64, end: i64) -> Result<String> {
    let mut lines = Vec::new();
    let mut found = false;
    for line in existing.lines().filter(|line| !line.trim().is_empty()) {
        let Some((entry, spec)) = line.split_once(':') else {
            lines.push(line.to_string());
            continue;
        };
        if entry != path {
            lines.push(line.to_string());
            continue;
        }
        found = true;
        let mut ranges = parse_ranges(spec)?;
        let blocked = ranges
            .iter()
            .filter(|range| range.non_inheritable)
            .map(|range| (range.start, range.end))
            .collect::<Vec<_>>();
        for (piece_start, piece_end) in subtract_ranges(start, end, &blocked) {
            ranges.push(MergeRange {
                start: piece_start,
                end: piece_end,
                non_inheritable: false,
            });
        }
        lines.push(format!("{path}:{}", format_ranges(&coalesce(ranges))));
    }
    if !found {
        lines.push(format!(
            "{path}:{}",
            format_ranges(&coalesce(vec![MergeRange {
                start,
                end,
                non_inheritable: false,
            }]))
        ));
    }
    Ok(lines.join("\n"))
}

fn parse_ranges(spec: &str) -> Result<Vec<MergeRange>> {
    let mut ranges = Vec::new();
    for token in spec.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let (body, non_inheritable) = match token.strip_suffix('*') {
            Some(body) => (body, true),
            None => (token, false),
        };
        let (start, end) = if let Some((start, end)) = body.split_once('-') {
            (
                start
                    .parse::<i64>()
                    .context("unsupported mergeinfo range")?,
                end.parse::<i64>().context("unsupported mergeinfo range")?,
            )
        } else {
            let revision = body.parse::<i64>().context("unsupported mergeinfo range")?;
            (revision, revision)
        };
        if start <= 0 || end < start {
            bail!("unsupported mergeinfo range");
        }
        ranges.push(MergeRange {
            start,
            end,
            non_inheritable,
        });
    }
    Ok(ranges)
}

fn subtract_ranges(start: i64, end: i64, blocked: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut pieces = vec![(start, end)];
    for &(block_start, block_end) in blocked {
        let mut next = Vec::new();
        for (piece_start, piece_end) in pieces {
            if piece_end < block_start || piece_start > block_end {
                next.push((piece_start, piece_end));
                continue;
            }
            if piece_start < block_start {
                next.push((piece_start, block_start - 1));
            }
            if piece_end > block_end {
                next.push((block_end + 1, piece_end));
            }
        }
        pieces = next;
    }
    pieces
}

fn coalesce(mut ranges: Vec<MergeRange>) -> Vec<MergeRange> {
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by_key(|range| (range.start, range.end, range.non_inheritable));
    let mut out = vec![ranges[0]];
    for range in ranges.into_iter().skip(1) {
        let last = out.last_mut().expect("range");
        if last.non_inheritable == range.non_inheritable && range.start <= last.end + 1 {
            last.end = last.end.max(range.end);
        } else {
            out.push(range);
        }
    }
    out
}

fn format_ranges(ranges: &[MergeRange]) -> String {
    ranges
        .iter()
        .map(|range| {
            let body = if range.start == range.end {
                range.start.to_string()
            } else {
                format!("{}-{}", range.start, range.end)
            };
            if range.non_inheritable {
                format!("{body}*")
            } else {
                body
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn svn_props(target: &str, revision: Option<&str>) -> Result<BTreeMap<String, Vec<u8>>> {
    let xml = if let Some(revision) = revision {
        let pegged = format!("{target}@{revision}");
        svn(&["proplist", "-v", "--xml", "-r", revision, &pegged])?
    } else {
        svn(&["proplist", "-v", "--xml", target])?
    };
    parse_svn_proplist(&xml)
}

fn parse_svn_proplist(xml: &str) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<property") {
        rest = &rest[start..];
        let Some(name_at) = rest.find("name=\"") else {
            break;
        };
        let name_start = name_at + "name=\"".len();
        let Some(name_end) = rest[name_start..].find('"') else {
            break;
        };
        let name = xml_unescape(&rest[name_start..name_start + name_end]);
        let Some(body_at) = rest.find('>') else {
            break;
        };
        let Some(close) = rest.find("</property>") else {
            break;
        };
        let header = &rest[..body_at];
        let raw = &rest[body_at + 1..close];
        let value = if header.contains("encoding=\"base64\"") {
            let encoded: String = raw.chars().filter(|ch| !ch.is_whitespace()).collect();
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
                .context("svn property encoding was not preserved")?
        } else {
            xml_unescape(raw).into_bytes()
        };
        out.insert(name, value);
        rest = &rest[close + "</property>".len()..];
    }
    Ok(out)
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
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

        let parent_git = root.join("repos/rs69-keep-parent/git-repo");
        let pair_git = root.join("repos/rs69-keep-child/git-repo");
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
                "rs69-keep-parent",
                None,
                &svn_url,
                "trunk",
                feature_base,
                &origin,
            ))
            .unwrap();
        // Parent checkpoint stays at the origin SVN revision so the parent advance is pending.
        let mut parent = engine
            .db()
            .get_repository("rs69-keep-parent")
            .unwrap()
            .unwrap();
        parent.last_svn_rev = feature_base;
        parent.last_git_sha = origin.clone();
        engine.db().update_repository(&parent).unwrap();
        engine
            .db()
            .insert_repository(&repo(
                "rs69-keep-child",
                Some("rs69-keep-parent"),
                &svn_url,
                "branches/feature",
                feature_base,
                &pair_origin,
            ))
            .unwrap();
        let mut child = engine
            .db()
            .get_repository("rs69-keep-child")
            .unwrap()
            .unwrap();
        child.last_svn_rev = feature_base;
        child.last_git_sha = pair_origin.clone();
        engine.db().update_repository(&child).unwrap();

        let service = RefreshService::new(engine.clone());
        let youngest_before = svnlook_youngest(&svn_url).unwrap();
        let error = service
            .run_admitted_local(
                "local-session",
                "rs69-keep-child",
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
        let mut child = engine
            .db()
            .get_repository("rs69-keep-child")
            .unwrap()
            .unwrap();
        child.last_svn_rev = notes_rev;
        engine.db().update_repository(&child).unwrap();
        let digest = current_digest(
            &engine,
            "rs69-keep-child",
            LocalOperation::Update,
            false,
            false,
        )
        .unwrap();
        let receipt = service
            .run_admitted_local(
                "local-session",
                "rs69-keep-child",
                LocalRequest {
                    request_id: "parent-advance".into(),
                    approval_digest: digest.clone(),
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
                "rs69-keep-child",
                LocalRequest {
                    request_id: "parent-advance".into(),
                    approval_digest: digest,
                    operation: LocalOperation::Update,
                    same_name: false,
                    retire: false,
                    approve_current: false,
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
                "rs69-keep-child",
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
                "rs69-keep-child",
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
                "rs69-keep-child",
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
        for id in ["rs69-net-parent", "rs69-net-child"] {
            let git = root.join("repos").join(id).join("git-repo");
            std::fs::create_dir_all(&git).unwrap();
            command("git", &["init", "-b", "main", git.to_str().unwrap()]);
            let _ = git_commit(&git, "readme.txt", "origin\n");
        }
        let parent_sha = run(&mut Command::new("git").args([
            "-C",
            root.join("repos/rs69-net-parent/git-repo")
                .to_str()
                .unwrap(),
            "rev-parse",
            "HEAD",
        ]))
        .unwrap();
        let child_sha = run(&mut Command::new("git").args([
            "-C",
            root.join("repos/rs69-net-child/git-repo").to_str().unwrap(),
            "rev-parse",
            "HEAD",
        ]))
        .unwrap();
        engine
            .db()
            .insert_repository(&repo(
                "rs69-net-parent",
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
                "rs69-net-child",
                Some("rs69-net-parent"),
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
                "rs69-net-child",
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

    fn call(
        id: &str,
        digest: &str,
        operation: LocalOperation,
        approve_current: bool,
        stop_after: Option<&str>,
    ) -> LocalRequest {
        LocalRequest {
            request_id: id.into(),
            approval_digest: digest.into(),
            operation,
            same_name: false,
            retire: false,
            approve_current,
            stop_after: stop_after.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn request_id_is_bound_to_operation_and_digest_before_resume() {
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
        assert!(Command::new("svn")
            .current_dir(&parent_wc)
            .args(["add", "readme.txt", "--non-interactive"])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("svn")
            .current_dir(&parent_wc)
            .args(["commit", "-m", "origin", "--non-interactive"])
            .status()
            .unwrap()
            .success());
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
        let trunk = svn_revision(&format!("{svn_url}/trunk")).unwrap();
        let feature = svn_revision(&format!("{svn_url}/branches/feature")).unwrap();
        let parent_git = root.join("repos/rs69-bind-parent/git-repo");
        let pair_git = root.join("repos/rs69-bind-child/git-repo");
        std::fs::create_dir_all(&parent_git).unwrap();
        std::fs::create_dir_all(&pair_git).unwrap();
        command("git", &["init", "-b", "main", parent_git.to_str().unwrap()]);
        command(
            "git",
            &["init", "-b", "feature", pair_git.to_str().unwrap()],
        );
        let parent_sha = git_commit(&parent_git, "readme.txt", "origin\n");
        let pair_sha = git_commit(&pair_git, "readme.txt", "origin\n");
        engine
            .db()
            .insert_repository(&repo(
                "rs69-bind-parent",
                None,
                &svn_url,
                "trunk",
                trunk,
                &parent_sha,
            ))
            .unwrap();
        engine
            .db()
            .insert_repository(&repo(
                "rs69-bind-child",
                Some("rs69-bind-parent"),
                &svn_url,
                "branches/feature",
                feature,
                &pair_sha,
            ))
            .unwrap();
        let service = RefreshService::new(engine.clone());
        let digest = current_digest(
            &engine,
            "rs69-bind-child",
            LocalOperation::Update,
            false,
            false,
        )
        .unwrap();
        let youngest = svnlook_youngest(&svn_url).unwrap();
        let receipt = service
            .run_admitted_local(
                "local-session",
                "rs69-bind-child",
                call("bound", &digest, LocalOperation::Update, false, None),
            )
            .await
            .unwrap();
        assert!(receipt.noop);
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), youngest);
        let retry = service
            .run_admitted_local(
                "local-session",
                "rs69-bind-child",
                call("bound", &digest, LocalOperation::Update, false, None),
            )
            .await
            .unwrap();
        assert_eq!(retry.operation_id, receipt.operation_id);
        let changed = service
            .run_admitted_local(
                "local-session",
                "rs69-bind-child",
                call("bound", "other-digest", LocalOperation::Update, true, None),
            )
            .await
            .unwrap_err();
        assert!(changed.to_string().contains("already bound"), "{changed}");
        let crossed = service
            .run_admitted_local(
                "local-session",
                "rs69-bind-child",
                call("bound", "", LocalOperation::Reanchor, true, None),
            )
            .await
            .unwrap_err();
        assert!(crossed.to_string().contains("already bound"), "{crossed}");
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), youngest);
        assert!(
            Command::new("svn")
                .args([
                    "info",
                    &format!("{svn_url}/branches/feature-g2"),
                    "--non-interactive",
                ])
                .status()
                .unwrap()
                .code()
                != Some(0)
        );

        std::fs::write(parent_wc.join("readme.txt"), "parent\n").unwrap();
        assert!(Command::new("svn")
            .current_dir(&parent_wc)
            .args(["commit", "-m", "parent", "--non-interactive"])
            .status()
            .unwrap()
            .success());
        let _parent_tip = git_commit(&parent_git, "readme.txt", "parent\n");
        let mut child = engine
            .db()
            .get_repository("rs69-bind-child")
            .unwrap()
            .unwrap();
        child.last_svn_rev = svn_revision(&format!("{svn_url}/branches/feature")).unwrap();
        engine.db().update_repository(&child).unwrap();
        let active = current_digest(
            &engine,
            "rs69-bind-child",
            LocalOperation::Update,
            false,
            false,
        )
        .unwrap();
        let before = svnlook_youngest(&svn_url).unwrap();
        let crashed = service
            .run_admitted_local(
                "local-session",
                "rs69-bind-child",
                call(
                    "active",
                    &active,
                    LocalOperation::Update,
                    false,
                    Some("svn_intent"),
                ),
            )
            .await
            .unwrap_err();
        assert!(crashed.to_string().contains("svn_intent"), "{crashed}");
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), before);
        let changed = service
            .run_admitted_local(
                "local-session",
                "rs69-bind-child",
                call("active", "other-digest", LocalOperation::Update, true, None),
            )
            .await
            .unwrap_err();
        assert!(changed.to_string().contains("already bound"), "{changed}");
        let crossed = service
            .run_admitted_local(
                "local-session",
                "rs69-bind-child",
                call("active", &active, LocalOperation::Reanchor, false, None),
            )
            .await
            .unwrap_err();
        assert!(crossed.to_string().contains("already bound"), "{crossed}");
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), before);
        let resumed = service
            .run_admitted_local(
                "local-session",
                "rs69-bind-child",
                call("active", &active, LocalOperation::Update, false, None),
            )
            .await
            .unwrap();
        assert!(!resumed.noop);
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), before + 1);
        let again = service
            .run_admitted_local(
                "local-session",
                "rs69-bind-child",
                call("active", &active, LocalOperation::Update, false, None),
            )
            .await
            .unwrap();
        assert_eq!(again.operation_id, resumed.operation_id);
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), before + 1);
    }

    #[tokio::test]
    async fn local_refresh_keeps_mapped_edits_and_applies_parent_deletions() {
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
        std::fs::write(parent_wc.join("keep.txt"), "keep\n").unwrap();
        command_in(&parent_wc, &["add", "readme.txt", "keep.txt"]);
        command_in(&parent_wc, &["commit", "-m", "origin"]);
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
        let mapped_parent = svn_revision(&format!("{svn_url}/trunk")).unwrap();
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
        std::fs::write(feature_wc.join("readme.txt"), "child-edit\n").unwrap();
        command_in(&feature_wc, &["commit", "-m", "mapped child edit"]);
        let mapped_pair = svn_revision(&format!("{svn_url}/branches/feature")).unwrap();
        std::fs::remove_file(parent_wc.join("keep.txt")).unwrap();
        std::fs::write(parent_wc.join("extra.txt"), "extra\n").unwrap();
        command_in(&parent_wc, &["delete", "keep.txt"]);
        command_in(&parent_wc, &["add", "extra.txt"]);
        command_in(&parent_wc, &["commit", "-m", "parent projection"]);
        command_in(&parent_wc, &["update"]);
        command_in(&feature_wc, &["update"]);

        let parent_git = root.join("repos/rs69-merge-parent/git-repo");
        let pair_git = root.join("repos/rs69-merge-child/git-repo");
        std::fs::create_dir_all(&parent_git).unwrap();
        std::fs::create_dir_all(&pair_git).unwrap();
        command("git", &["init", "-b", "main", parent_git.to_str().unwrap()]);
        command(
            "git",
            &["init", "-b", "feature", pair_git.to_str().unwrap()],
        );
        let parent_sha = git_commit(&parent_git, "readme.txt", "origin\n");
        let pair_sha = git_commit(&pair_git, "readme.txt", "child-edit\n");
        engine
            .db()
            .insert_repository(&repo(
                "rs69-merge-parent",
                None,
                &svn_url,
                "trunk",
                mapped_parent,
                &parent_sha,
            ))
            .unwrap();
        engine
            .db()
            .insert_repository(&repo(
                "rs69-merge-child",
                Some("rs69-merge-parent"),
                &svn_url,
                "branches/feature",
                mapped_pair,
                &pair_sha,
            ))
            .unwrap();
        let service = RefreshService::new(engine.clone());
        std::fs::write(parent_wc.join("readme.txt"), "parent-edit\n").unwrap();
        command_in(&parent_wc, &["commit", "-m", "overlap parent"]);
        std::fs::write(feature_wc.join("readme.txt"), "both-edit\n").unwrap();
        command_in(&feature_wc, &["commit", "-m", "overlap pair"]);
        let overlap_youngest = svnlook_youngest(&svn_url).unwrap();
        let mut child = engine
            .db()
            .get_repository("rs69-merge-child")
            .unwrap()
            .unwrap();
        child.last_svn_rev = svn_revision(&format!("{svn_url}/branches/feature")).unwrap();
        engine.db().update_repository(&child).unwrap();
        let mut parent = engine
            .db()
            .get_repository("rs69-merge-parent")
            .unwrap()
            .unwrap();
        parent.last_git_sha = parent_sha.clone();
        engine.db().update_repository(&parent).unwrap();
        let overlap = current_digest(
            &engine,
            "rs69-merge-child",
            LocalOperation::Update,
            false,
            false,
        )
        .unwrap();
        let refused = service
            .run_admitted_local(
                "local-session",
                "rs69-merge-child",
                call("overlap", &overlap, LocalOperation::Update, false, None),
            )
            .await
            .unwrap_err();
        assert!(
            refused.to_string().contains("resolution required"),
            "{refused}"
        );
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), overlap_youngest);

        std::fs::write(parent_wc.join("readme.txt"), "origin\n").unwrap();
        command_in(&parent_wc, &["commit", "-m", "restore parent readme"]);
        std::fs::write(feature_wc.join("readme.txt"), "child-edit\n").unwrap();
        command_in(&feature_wc, &["commit", "-m", "restore mapped edit"]);
        let mut child = engine
            .db()
            .get_repository("rs69-merge-child")
            .unwrap()
            .unwrap();
        child.last_svn_rev = svn_revision(&format!("{svn_url}/branches/feature")).unwrap();
        child.last_git_sha = pair_sha.clone();
        engine.db().update_repository(&child).unwrap();
        let digest = current_digest(
            &engine,
            "rs69-merge-child",
            LocalOperation::Update,
            false,
            false,
        )
        .unwrap();
        let receipt = service
            .run_admitted_local(
                "local-session",
                "rs69-merge-child",
                call("projection", &digest, LocalOperation::Update, false, None),
            )
            .await
            .unwrap();
        let published = receipt.svn_revision.unwrap();
        let readme = run(&mut Command::new("svn").args([
            "cat",
            "-r",
            &published.to_string(),
            &format!("{svn_url}/branches/feature/readme.txt"),
            "--non-interactive",
        ]))
        .unwrap();
        assert_eq!(readme, "child-edit");
        let extra = run(&mut Command::new("svn").args([
            "cat",
            "-r",
            &published.to_string(),
            &format!("{svn_url}/branches/feature/extra.txt"),
            "--non-interactive",
        ]))
        .unwrap();
        assert_eq!(extra, "extra");
        let missing = Command::new("svn")
            .args([
                "cat",
                "-r",
                &published.to_string(),
                &format!("{svn_url}/branches/feature/keep.txt"),
                "--non-interactive",
            ])
            .status()
            .unwrap();
        assert!(!missing.success());
        let mergeinfo = run(&mut Command::new("svn").args([
            "propget",
            "svn:mergeinfo",
            "-r",
            &published.to_string(),
            &format!("{svn_url}/branches/feature"),
            "--non-interactive",
        ]))
        .unwrap();
        assert!(mergeinfo.contains("trunk:"), "{mergeinfo}");
        let kept = run(&mut Command::new("git").args([
            "-C",
            pair_git.to_str().unwrap(),
            "show",
            "HEAD:readme.txt",
        ]))
        .unwrap();
        assert_eq!(kept, "child-edit");
    }

    #[tokio::test]
    async fn local_refresh_does_not_adopt_a_generic_message_or_bypass_guards() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let sentinel = root.join("sentinel");
        std::fs::write(&sentinel, "keep").unwrap();
        let engine = Arc::new(engine_for(&root));
        let service = RefreshService::new(engine.clone());
        let escaped = service
            .run_admitted_local(
                "local-session",
                "rs69-guard-child",
                call("../sentinel", "", LocalOperation::Update, true, None),
            )
            .await
            .unwrap_err();
        assert!(
            escaped.to_string().contains("confined workspace key"),
            "{escaped}"
        );
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "keep");

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
        command_in(&parent_wc, &["add", "readme.txt"]);
        command_in(&parent_wc, &["commit", "-m", "origin"]);
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
        command_in(&parent_wc, &["commit", "-m", "pair refresh integration"]);
        let decoy = svnlook_youngest(&svn_url).unwrap();
        let parent_git = root.join("repos/rs69-guard-parent/git-repo");
        let pair_git = root.join("repos/rs69-guard-child/git-repo");
        std::fs::create_dir_all(&parent_git).unwrap();
        std::fs::create_dir_all(&pair_git).unwrap();
        command("git", &["init", "-b", "main", parent_git.to_str().unwrap()]);
        command(
            "git",
            &["init", "-b", "feature", pair_git.to_str().unwrap()],
        );
        let origin = git_commit(&parent_git, "readme.txt", "origin\n");
        let pair_sha = git_commit(&pair_git, "readme.txt", "origin\n");
        let parent_tip = git_commit(&parent_git, "readme.txt", "parent\n");
        engine
            .db()
            .insert_repository(&repo(
                "rs69-guard-parent",
                None,
                &svn_url,
                "trunk",
                feature_base,
                &origin,
            ))
            .unwrap();
        engine
            .db()
            .insert_repository(&repo(
                "rs69-guard-child",
                Some("rs69-guard-parent"),
                &svn_url,
                "branches/feature",
                feature_base,
                &pair_sha,
            ))
            .unwrap();
        let mut child = engine
            .db()
            .get_repository("rs69-guard-child")
            .unwrap()
            .unwrap();
        child.last_svn_rev = svn_revision(&format!("{svn_url}/branches/feature")).unwrap();
        engine.db().update_repository(&child).unwrap();
        let mut parent = engine
            .db()
            .get_repository("rs69-guard-parent")
            .unwrap()
            .unwrap();
        parent.last_git_sha = parent_tip.clone();
        engine.db().update_repository(&parent).unwrap();
        let digest = current_digest(
            &engine,
            "rs69-guard-child",
            LocalOperation::Update,
            false,
            false,
        )
        .unwrap();
        let held = crate::busy::try_acquire_pair("rs69-guard-child", "rs69-guard-parent").unwrap();
        let busy = service
            .run_admitted_local(
                "local-session",
                "rs69-guard-child",
                call("publish", &digest, LocalOperation::Update, false, None),
            )
            .await
            .unwrap_err();
        assert!(busy.to_string().contains("busy"), "{busy}");
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), decoy);
        drop(held);

        let drift = service
            .run_admitted_local(
                "local-session",
                "rs69-guard-child",
                call(
                    "drift",
                    &digest,
                    LocalOperation::Update,
                    false,
                    Some("svn_intent"),
                ),
            )
            .await
            .unwrap_err();
        assert!(drift.to_string().contains("svn_intent"), "{drift}");
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), decoy);
        git_commit(&parent_git, "readme.txt", "moved\n");
        let drifted = service
            .run_admitted_local(
                "local-session",
                "rs69-guard-child",
                call("drift", &digest, LocalOperation::Update, false, None),
            )
            .await
            .unwrap_err();
        assert!(drifted.to_string().contains("drifted"), "{drifted}");
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), decoy);
        command(
            "git",
            &[
                "-C",
                parent_git.to_str().unwrap(),
                "update-ref",
                "refs/heads/main",
                &parent_tip,
            ],
        );

        let published = service
            .run_admitted_local(
                "local-session",
                "rs69-guard-child",
                call(
                    "drift",
                    &digest,
                    LocalOperation::Update,
                    false,
                    Some("svn_observed"),
                ),
            )
            .await
            .unwrap_err();
        assert!(
            published.to_string().contains("svn_observed"),
            "{published}"
        );
        let own = svnlook_youngest(&svn_url).unwrap();
        assert!(own > decoy);
        let log = run(Command::new("svnlook")
            .arg("log")
            .arg("-r")
            .arg(own.to_string())
            .arg(&svn_dir))
        .unwrap();
        assert!(log.contains("reposync-pair-refresh"), "{log}");
        assert_ne!(log, "pair refresh integration");
        let space = root
            .join("pair-refresh-local")
            .join(hash(&["rs69-guard-child"]))
            .join(hash(&["drift"]));
        let outside = root.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), "keep").unwrap();
        std::os::unix::fs::symlink(&outside, space.join("git-source")).unwrap();
        let linked = service
            .run_admitted_local(
                "local-session",
                "rs69-guard-child",
                call("drift", &digest, LocalOperation::Update, false, None),
            )
            .await
            .unwrap_err();
        assert!(linked.to_string().contains("symlink"), "{linked}");
        assert_eq!(
            std::fs::read_to_string(outside.join("sentinel")).unwrap(),
            "keep"
        );
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), own);
        std::fs::remove_file(space.join("git-source")).unwrap();

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
        std::fs::write(feature_wc.join("later.txt"), "later\n").unwrap();
        command_in(&feature_wc, &["add", "later.txt"]);
        command_in(&feature_wc, &["commit", "-m", "intervening"]);
        let hidden = svnlook_youngest(&svn_url).unwrap();
        let resumed = service
            .run_admitted_local(
                "local-session",
                "rs69-guard-child",
                call("drift", &digest, LocalOperation::Update, false, None),
            )
            .await
            .unwrap_err();
        assert!(
            resumed.to_string().contains("reconciliation")
                || resumed.to_string().contains("drifted")
                || resumed.to_string().contains("intervening"),
            "{resumed}"
        );
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), hidden);
    }

    #[test]
    fn parent_fetch_uses_the_admitted_commit() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let pair = temp.path().join("pair");
        command("git", &["init", "-b", "main", parent.to_str().unwrap()]);
        command("git", &["init", "-b", "feature", pair.to_str().unwrap()]);
        let admitted = git_commit(&parent, "readme.txt", "admitted\n");
        let _moved = git_commit(&parent, "readme.txt", "moved\n");
        pin_parent_commit(&pair, &parent, &admitted).unwrap();
        let fetched = run(&mut Command::new("git").args([
            "-C",
            pair.to_str().unwrap(),
            "rev-parse",
            "refs/reposync/pair-refresh/parent",
        ]))
        .unwrap();
        assert_eq!(fetched, admitted);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_refresh_reconciles_its_own_commit_and_keeps_the_reservation() {
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
        command_in(&parent_wc, &["add", "readme.txt"]);
        command_in(&parent_wc, &["commit", "-m", "origin"]);
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
        command_in(&parent_wc, &["commit", "-m", "parent advance"]);
        let admitted_parent = svnlook_youngest(&svn_url).unwrap();
        let parent_git = root.join("repos/rs69-receipt-parent/git-repo");
        let pair_git = root.join("repos/rs69-receipt-child/git-repo");
        std::fs::create_dir_all(&parent_git).unwrap();
        std::fs::create_dir_all(&pair_git).unwrap();
        command("git", &["init", "-b", "main", parent_git.to_str().unwrap()]);
        command(
            "git",
            &["init", "-b", "feature", pair_git.to_str().unwrap()],
        );
        let origin = git_commit(&parent_git, "readme.txt", "origin\n");
        let pair_sha = git_commit(&pair_git, "readme.txt", "origin\n");
        let parent_tip = git_commit(&parent_git, "readme.txt", "parent\n");
        engine
            .db()
            .insert_repository(&repo(
                "rs69-receipt-parent",
                None,
                &svn_url,
                "trunk",
                feature_base,
                &origin,
            ))
            .unwrap();
        engine
            .db()
            .insert_repository(&repo(
                "rs69-receipt-child",
                Some("rs69-receipt-parent"),
                &svn_url,
                "branches/feature",
                feature_base,
                &pair_sha,
            ))
            .unwrap();
        let mut child = engine
            .db()
            .get_repository("rs69-receipt-child")
            .unwrap()
            .unwrap();
        child.last_svn_rev = svn_revision(&format!("{svn_url}/branches/feature")).unwrap();
        engine.db().update_repository(&child).unwrap();
        let mut parent = engine
            .db()
            .get_repository("rs69-receipt-parent")
            .unwrap()
            .unwrap();
        parent.last_git_sha = parent_tip.clone();
        engine.db().update_repository(&parent).unwrap();
        let service = RefreshService::new(engine.clone());
        let digest = current_digest(
            &engine,
            "rs69-receipt-child",
            LocalOperation::Update,
            false,
            false,
        )
        .unwrap();
        let intent = service
            .run_admitted_local(
                "local-session",
                "rs69-receipt-child",
                call(
                    "publish",
                    &digest,
                    LocalOperation::Update,
                    false,
                    Some("svn_intent"),
                ),
            )
            .await
            .unwrap_err();
        assert!(intent.to_string().contains("svn_intent"), "{intent}");
        assert_reservation_blocks(&engine, &service).await;
        let committed = service
            .run_admitted_local(
                "local-session",
                "rs69-receipt-child",
                call(
                    "publish",
                    &digest,
                    LocalOperation::Update,
                    false,
                    Some("svn_committed"),
                ),
            )
            .await
            .unwrap_err();
        assert!(
            committed.to_string().contains("journal receipt"),
            "{committed}"
        );
        let own = svnlook_youngest(&svn_url).unwrap();
        let log = run(Command::new("svnlook")
            .arg("log")
            .arg("-r")
            .arg(own.to_string())
            .arg(&svn_dir))
        .unwrap();
        assert!(log.contains("reposync-pair-refresh"), "{log}");
        let stored = engine
            .db()
            .get_state(&format!("{JOURNAL_PREFIX}rs69-receipt-child:publish"))
            .unwrap()
            .unwrap();
        assert!(stored.contains("\"new_svn\":null"), "{stored}");
        assert_reservation_blocks(&engine, &service).await;
        std::fs::write(parent_wc.join("other.txt"), "later\n").unwrap();
        command_in(&parent_wc, &["add", "other.txt"]);
        command_in(&parent_wc, &["commit", "-m", "unrelated other path"]);
        let hidden = svnlook_youngest(&svn_url).unwrap();
        assert!(hidden > own);
        for boundary in ["svn_observed", "git_intent", "git_observed", "checkpoint"] {
            let stopped = service
                .run_admitted_local(
                    "local-session",
                    "rs69-receipt-child",
                    call(
                        "publish",
                        &digest,
                        LocalOperation::Update,
                        false,
                        Some(boundary),
                    ),
                )
                .await
                .unwrap_err();
            assert!(stopped.to_string().contains(boundary), "{stopped}");
            assert_eq!(svnlook_youngest(&svn_url).unwrap(), hidden);
            assert_reservation_blocks(&engine, &service).await;
            if boundary == "git_observed" || boundary == "checkpoint" {
                let paused = engine
                    .db()
                    .get_repository("rs69-receipt-parent")
                    .unwrap()
                    .unwrap();
                assert_ne!(paused.last_svn_rev, hidden);
            }
        }
        let done = service
            .run_admitted_local(
                "local-session",
                "rs69-receipt-child",
                call("publish", &digest, LocalOperation::Update, false, None),
            )
            .await
            .unwrap();
        assert!(!done.noop);
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), hidden);
        assert_eq!(done.svn_revision, Some(own));
        let parent_row = engine
            .db()
            .get_repository("rs69-receipt-parent")
            .unwrap()
            .unwrap();
        assert_eq!(parent_row.last_svn_rev, admitted_parent);
        assert_eq!(parent_row.last_git_sha, parent_tip);
        assert_ne!(parent_row.last_svn_rev, hidden);
        let absent = Command::new("git")
            .args([
                "-C",
                parent_git.to_str().unwrap(),
                "cat-file",
                "-e",
                "HEAD:other.txt",
            ])
            .status()
            .unwrap();
        assert!(!absent.success());
        import_parent_svn(&root, &svn_url, &parent_git).await;
        let imported = run(&mut Command::new("git").args([
            "-C",
            parent_git.to_str().unwrap(),
            "show",
            "HEAD:other.txt",
        ]))
        .unwrap();
        assert_eq!(imported, "later");
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(
            &engine.db().conn(),
            Some("rs69-receipt-child"),
        )
        .unwrap();
        let integrated = run(&mut Command::new("git").args([
            "-C",
            pair_git.to_str().unwrap(),
            "show",
            "HEAD:readme.txt",
        ]))
        .unwrap();
        assert_eq!(integrated, "parent");
    }

    async fn import_parent_svn(root: &Path, svn_url: &str, parent_git: &Path) {
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
[identity]
email_domain = "localhost"
"#,
        )
        .unwrap();
        config.daemon.data_dir = root.to_path_buf();
        config.svn.url = format!("{svn_url}/trunk");
        let origin = parent_git.with_file_name("parent-origin.git");
        command(
            "git",
            &["init", "--bare", "-b", "main", origin.to_str().unwrap()],
        );
        command(
            "git",
            &[
                "-C",
                parent_git.to_str().unwrap(),
                "remote",
                "add",
                "origin",
                origin.to_str().unwrap(),
            ],
        );
        command(
            "git",
            &[
                "-C",
                parent_git.to_str().unwrap(),
                "push",
                "origin",
                "HEAD:refs/heads/main",
            ],
        );
        let db = Database::new(root.join("reposync.db")).unwrap();
        let uuid = run(&mut Command::new("svn").args([
            "info",
            "--show-item",
            "repos-uuid",
            svn_url,
            "--non-interactive",
        ]))
        .unwrap();
        crate::db::lineage_checkpoints::record_independently_established_svn_uuid(
            &db,
            "rs69-receipt-parent",
            &uuid,
        )
        .unwrap();
        let identity = Arc::new(IdentityMapper::new(&config.identity).unwrap());
        let mut engine = SyncEngine::new(
            config,
            db,
            SvnClient::new(&format!("{svn_url}/trunk"), "", ""),
            GitClient::new(parent_git).unwrap(),
            identity,
        );
        engine.set_repo_id("rs69-receipt-parent".into());
        engine
            .run_sync_cycle()
            .await
            .expect("ordinary sync imports the later parent commit");
    }

    async fn assert_reservation_blocks(engine: &SyncEngine, service: &RefreshService) {
        for id in ["rs69-receipt-child", "rs69-receipt-parent"] {
            let message = match engine.db().begin_ordinary_writer_window(Some(id)) {
                Err(error) => error.to_string(),
                Ok(_) => panic!("ordinary sync was admitted for {id}"),
            };
            assert!(
                message.contains("durable pair refresh"),
                "{id} ordinary sync: {message}"
            );
            let watermark = engine.db().update_repo_watermark(id, 1, "abc");
            let message = watermark.unwrap_err().to_string();
            assert!(
                message.contains("durable pair refresh"),
                "{id} watermark: {message}"
            );
        }
        let competing = service
            .run_admitted_local(
                "local-session",
                "rs69-receipt-child",
                call("other", "", LocalOperation::Update, true, None),
            )
            .await
            .unwrap_err();
        assert!(
            competing.to_string().contains("durable pair refresh"),
            "{competing}"
        );
    }

    #[tokio::test]
    async fn local_refresh_preserves_properties_directories_and_mergeinfo() {
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
        std::fs::create_dir(parent_wc.join("slot")).unwrap();
        std::fs::write(parent_wc.join("slot/inside.txt"), "dir\n").unwrap();
        command_in(&parent_wc, &["add", "readme.txt", "slot"]);
        command_in(&parent_wc, &["commit", "-m", "origin"]);
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
        let mapped_parent = svn_revision(&format!("{svn_url}/trunk")).unwrap();
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
        command_in(
            &feature_wc,
            &["propset", "custom:side", "child", "readme.txt"],
        );
        command_in(&feature_wc, &["commit", "-m", "child prop"]);
        command_in(
            &parent_wc,
            &["propset", "custom:side", "parent", "readme.txt"],
        );
        command_in(&parent_wc, &["commit", "-m", "parent prop"]);
        let parent_git = root.join("repos/rs69-prop-parent/git-repo");
        let pair_git = root.join("repos/rs69-prop-child/git-repo");
        std::fs::create_dir_all(&parent_git).unwrap();
        std::fs::create_dir_all(&pair_git).unwrap();
        command("git", &["init", "-b", "main", parent_git.to_str().unwrap()]);
        command(
            "git",
            &["init", "-b", "feature", pair_git.to_str().unwrap()],
        );
        let parent_sha = git_commit(&parent_git, "readme.txt", "origin\n");
        let pair_sha = git_commit(&pair_git, "readme.txt", "origin\n");
        engine
            .db()
            .insert_repository(&repo(
                "rs69-prop-parent",
                None,
                &svn_url,
                "trunk",
                mapped_parent,
                &parent_sha,
            ))
            .unwrap();
        engine
            .db()
            .insert_repository(&repo(
                "rs69-prop-child",
                Some("rs69-prop-parent"),
                &svn_url,
                "branches/feature",
                svn_revision(&format!("{svn_url}/branches/feature")).unwrap(),
                &pair_sha,
            ))
            .unwrap();
        let service = RefreshService::new(engine.clone());
        let before = svnlook_youngest(&svn_url).unwrap();
        let overlap = current_digest(
            &engine,
            "rs69-prop-child",
            LocalOperation::Update,
            false,
            false,
        )
        .unwrap();
        let refused = service
            .run_admitted_local(
                "local-session",
                "rs69-prop-child",
                call(
                    "prop-overlap",
                    &overlap,
                    LocalOperation::Update,
                    false,
                    None,
                ),
            )
            .await
            .unwrap_err();
        assert!(
            refused.to_string().contains("resolution required"),
            "{refused}"
        );
        assert_eq!(svnlook_youngest(&svn_url).unwrap(), before);

        command_in(&feature_wc, &["update"]);
        command_in(&feature_wc, &["propdel", "custom:side", "readme.txt"]);
        command_in(
            &feature_wc,
            &["propset", "custom:note", "keep", "readme.txt"],
        );
        let root_note = root.join("root-note");
        std::fs::write(&root_note, b"keep-root").unwrap();
        command_in(
            &feature_wc,
            &[
                "propset",
                "custom:root",
                "-F",
                root_note.to_str().unwrap(),
                ".",
            ],
        );
        command_in(
            &feature_wc,
            &["propset", "svn:mergeinfo", "/trunk:2-4*,6", "."],
        );
        command_in(&feature_wc, &["commit", "-m", "child retained"]);
        command_in(&parent_wc, &["update"]);
        command_in(&parent_wc, &["propdel", "custom:side", "readme.txt"]);
        command_in(
            &parent_wc,
            &["propset", "svn:executable", "*", "readme.txt"],
        );
        let padded = root.join("padded-value");
        std::fs::write(&padded, b"  spaced \n").unwrap();
        command_in(
            &parent_wc,
            &[
                "propset",
                "custom:pad",
                "-F",
                padded.to_str().unwrap(),
                "readme.txt",
            ],
        );
        let binary = root.join("binary-value");
        std::fs::write(&binary, [0_u8, 32, 255, 10]).unwrap();
        command_in(
            &parent_wc,
            &[
                "propset",
                "custom:bin",
                "-F",
                binary.to_str().unwrap(),
                "readme.txt",
            ],
        );
        let ignore = root.join("ignore-value");
        std::fs::write(&ignore, b"generated\n").unwrap();
        command_in(
            &parent_wc,
            &["propset", "svn:ignore", "-F", ignore.to_str().unwrap(), "."],
        );
        std::fs::create_dir(parent_wc.join("emptydir")).unwrap();
        command_in(&parent_wc, &["add", "emptydir"]);
        command_in(&parent_wc, &["delete", "slot"]);
        std::fs::write(parent_wc.join("slot"), "file\n").unwrap();
        command_in(&parent_wc, &["add", "slot"]);
        command_in(&parent_wc, &["commit", "-m", "parent structure"]);
        let mut child = engine
            .db()
            .get_repository("rs69-prop-child")
            .unwrap()
            .unwrap();
        child.last_svn_rev = svn_revision(&format!("{svn_url}/branches/feature")).unwrap();
        engine.db().update_repository(&child).unwrap();
        let digest = current_digest(
            &engine,
            "rs69-prop-child",
            LocalOperation::Update,
            false,
            false,
        )
        .unwrap();
        let receipt = service
            .run_admitted_local(
                "local-session",
                "rs69-prop-child",
                call("projection", &digest, LocalOperation::Update, false, None),
            )
            .await
            .unwrap();
        let published = receipt.svn_revision.unwrap();
        let executable = run(&mut Command::new("svn").args([
            "propget",
            "svn:executable",
            "-r",
            &published.to_string(),
            &format!("{svn_url}/branches/feature/readme.txt"),
            "--non-interactive",
        ]))
        .unwrap();
        assert_eq!(executable, "*");
        let note = run(&mut Command::new("svn").args([
            "propget",
            "custom:note",
            "-r",
            &published.to_string(),
            &format!("{svn_url}/branches/feature/readme.txt"),
            "--non-interactive",
        ]))
        .unwrap();
        assert_eq!(note, "keep");
        let listed = run(&mut Command::new("svn").args([
            "list",
            "-r",
            &published.to_string(),
            &format!("{svn_url}/branches/feature"),
            "--non-interactive",
        ]))
        .unwrap();
        assert!(listed.contains("emptydir/"), "{listed}");
        let slot = run(&mut Command::new("svn").args([
            "cat",
            "-r",
            &published.to_string(),
            &format!("{svn_url}/branches/feature/slot"),
            "--non-interactive",
        ]))
        .unwrap();
        assert_eq!(slot, "file");
        let mergeinfo = run(&mut Command::new("svn").args([
            "propget",
            "svn:mergeinfo",
            "-r",
            &published.to_string(),
            &format!("{svn_url}/branches/feature"),
            "--non-interactive",
        ]))
        .unwrap();
        assert!(
            mergeinfo.contains("2-3") || mergeinfo.contains('2'),
            "{mergeinfo}"
        );
        assert!(mergeinfo.contains("trunk:"), "{mergeinfo}");
        assert!(mergeinfo.contains("2-4*"), "{mergeinfo}");
        let branch = svn_props(
            &format!("{svn_url}/branches/feature"),
            Some(&published.to_string()),
        )
        .unwrap();
        assert_eq!(
            branch.get("svn:ignore").map(Vec::as_slice),
            Some(b"generated\n".as_slice())
        );
        assert_eq!(
            branch.get("custom:root").map(Vec::as_slice),
            Some(b"keep-root".as_slice())
        );
        let readme = svn_props(
            &format!("{svn_url}/branches/feature/readme.txt"),
            Some(&published.to_string()),
        )
        .unwrap();
        assert_eq!(
            readme.get("custom:pad").map(Vec::as_slice),
            Some(b"  spaced \n".as_slice())
        );
        assert_eq!(
            readme.get("custom:bin").map(Vec::as_slice),
            Some([0_u8, 32, 255, 10].as_slice())
        );
    }

    fn command_in(dir: &Path, args: &[&str]) {
        let status = Command::new("svn")
            .current_dir(dir)
            .args(args)
            .arg("--non-interactive")
            .status()
            .unwrap();
        assert!(status.success(), "svn {args:?}");
    }
}
