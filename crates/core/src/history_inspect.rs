//! Shared Git P/O/R/L inspection and repository-scoped history blocks.
//!
//! Team and personal engines use the same classifications. Durable
//! `reconciliation_required` blocks persist for rewrites (`non_fast_forward`,
//! `observed_remote_rewrite`) and UnsupportedHistory reasons
//! (`unsupported_merge_dag`, `unsupported_backlog`, `unproven_pending_range`).
//! They survive restart and later polls even when a subsequent fetch looks
//! admissible.

use std::path::Path;
use std::process::{Command, Output};

use chrono::Utc;
use serde_json::Value;

use crate::db::Database;
use crate::errors::{DatabaseError, SyncError};

/// Rewrite of already-handled Git cursor (P→R ancestry failure).
pub const DURABLE_HISTORY_REASON: &str = "non_fast_forward";

/// Prior observed remote tip (O) is no longer an ancestor of fresh R.
pub const REASON_OBSERVED_REMOTE_REWRITE: &str = "observed_remote_rewrite";

pub fn history_block_key(repo_id: Option<&str>) -> String {
    match repo_id {
        Some(id) => format!("team_history_block_{}", id),
        None => "team_history_block_global".to_string(),
    }
}

pub fn is_full_git_oid(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn is_durable_history_reason(reason: &str) -> bool {
    reason == DURABLE_HISTORY_REASON
        || reason == REASON_OBSERVED_REMOTE_REWRITE
        || reason == crate::pending_frontier::REASON_MERGE_DAG
        || reason == crate::pending_frontier::REASON_BACKLOG
        || reason == crate::pending_frontier::REASON_UNPROVEN_RANGE
}

#[derive(Debug, Clone)]
pub struct HistoryInspectAdmission {
    pub checkpoint: String,
    pub remote_tip: String,
}

#[derive(Debug, Clone)]
pub struct HistoryInspectReject {
    pub reason: String,
    pub detail: String,
    pub o: Option<String>,
    pub r: Option<String>,
    pub l: Option<String>,
}

pub fn load_history_block(db: &Database, key: &str) -> Result<Option<Value>, DatabaseError> {
    let raw = db.get_state(key)?;
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    match serde_json::from_str(&raw) {
        Ok(value) => Ok(Some(value)),
        Err(_) => Ok(Some(serde_json::json!({
            "state": "reconciliation_required",
            "reason": "malformed_history_block",
            "detail": "durable history block could not be parsed",
            "durable": true,
        }))),
    }
}

pub fn is_durable_history_record(block: &Value) -> bool {
    if block["state"] != "reconciliation_required" {
        return false;
    }
    if block["durable"] == true {
        return true;
    }
    block["reason"]
        .as_str()
        .is_some_and(is_durable_history_reason)
}

pub fn enforce_durable_history_block(db: &Database, key: &str) -> Result<(), SyncError> {
    let Some(block) = load_history_block(db, key).map_err(SyncError::DatabaseError)? else {
        return Ok(());
    };
    if !is_durable_history_record(&block) {
        return Ok(());
    }
    let mut updated = block.clone();
    updated["last_checked_at"] = serde_json::Value::String(Utc::now().to_rfc3339());
    let _ = db.set_state(key, &updated.to_string());
    Err(SyncError::HistoryBlocked {
        reason: block["reason"]
            .as_str()
            .unwrap_or(DURABLE_HISTORY_REASON)
            .to_string(),
        detail: block["detail"]
            .as_str()
            .unwrap_or("durable history block remains; reconciliation required")
            .to_string(),
    })
}

pub fn persist_history_block(
    db: &Database,
    key: &str,
    repo_id: Option<&str>,
    reject: &HistoryInspectReject,
    p: Option<&str>,
) -> Result<(), DatabaseError> {
    if let Some(existing) = load_history_block(db, key)? {
        if is_durable_history_record(&existing) {
            let mut updated = existing;
            updated["last_checked_at"] = serde_json::Value::String(Utc::now().to_rfc3339());
            db.set_state(key, &updated.to_string())?;
            return Ok(());
        }
    }
    let now = Utc::now().to_rfc3339();
    let durable = is_durable_history_reason(&reject.reason);
    let record = serde_json::json!({
        "state": "reconciliation_required",
        "reason": reject.reason,
        "detail": reject.detail,
        "repo_id": repo_id,
        "p_handled": p,
        "o_prior_observed": reject.o,
        "r_fresh_remote": reject.r,
        "l_bridge": reject.l,
        "observed_at": now,
        "first_blocked_at": now,
        "durable": durable,
    });
    db.set_state(key, &record.to_string())
}

pub fn clear_transient_history_block(db: &Database, key: &str) -> Result<(), DatabaseError> {
    if let Some(existing) = load_history_block(db, key)? {
        if is_durable_history_record(&existing) {
            return Ok(());
        }
    }
    db.set_state(key, "")
}

/// Fetch the configured branch into an inspection ref and classify P→R.
/// Does not reset the checkout or write remotes.
pub fn inspect_fetched_history(
    path: &Path,
    branch: &str,
    p: Option<String>,
) -> Result<HistoryInspectAdmission, HistoryInspectReject> {
    let run = |args: &[&str]| -> std::io::Result<Output> {
        #[cfg(debug_assertions)]
        let fault = std::env::var("REPOSYNC_TEST_INSPECTION_FAULT").ok();
        #[cfg(debug_assertions)]
        let fixture_fault = fault
            .as_deref()
            .and_then(|value| value.split_once('|'))
            .filter(|(_, fixture_path)| Path::new(fixture_path) == path)
            .map(|(kind, _)| kind);
        #[cfg(debug_assertions)]
        match (fixture_fault, args.first().copied()) {
            (Some("remote_auth"), Some("ls-remote")) => {
                let mut output = Command::new("false").output()?;
                output.stderr = b"fatal: Authentication failed".to_vec();
                return Ok(output);
            }
            (Some("remote_fetch"), Some("fetch")) => return Command::new("false").output(),
            (Some("ancestry_exit_128"), Some("merge-base")) => {
                return Command::new("sh").args(["-c", "exit 128"]).output();
            }
            _ => (),
        }
        Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
    };
    let mut o: Option<String> = None;
    let mut r: Option<String> = None;
    let mut l: Option<String> = None;
    macro_rules! blocked {
        ($reason:expr, $detail:expr) => {{
            return Err(HistoryInspectReject {
                reason: $reason.to_string(),
                detail: $detail.to_string(),
                o,
                r,
                l,
            });
        }};
    }

    let branch_check = run(&["check-ref-format", "--branch", branch]);
    if !branch_check.is_ok_and(|output| output.status.success()) {
        blocked!(
            "invalid_branch",
            "configured Git branch is not a valid branch name"
        );
    }
    let local = match run(&["rev-parse", "--verify", "HEAD^{commit}"]) {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => blocked!("unknown_local_tip", "bridge HEAD is missing or unreadable"),
    };
    l = Some(local.clone());
    let status = match run(&[
        "--no-optional-locks",
        "status",
        "--porcelain",
        "--untracked-files=all",
    ]) {
        Ok(output) if output.status.success() => output,
        _ => blocked!(
            "local_status_error",
            "bridge index/worktree could not be inspected"
        ),
    };
    if !status.stdout.is_empty() {
        blocked!(
            "local_dirty",
            "bridge index or worktree has unpublished changes"
        );
    }

    let inspection_ref = "refs/reposync/inspection/incoming";
    let prior_inspection = format!("{}^{{commit}}", inspection_ref);
    if let Ok(output) = run(&["rev-parse", "--verify", &prior_inspection]) {
        if output.status.success() {
            o = Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
    }
    if o.is_none() {
        let remote_tracking = format!("refs/remotes/origin/{}^{{commit}}", branch);
        if let Ok(output) = run(&["rev-parse", "--verify", &remote_tracking]) {
            if output.status.success() {
                o = Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
            }
        }
    }
    let remote_branch = format!("refs/heads/{}", branch);
    let advertised = match run(&[
        "ls-remote",
        "--exit-code",
        "--heads",
        "origin",
        &remote_branch,
    ]) {
        Ok(output) if output.status.success() => output,
        Ok(output) if output.status.code() == Some(2) => blocked!(
            "remote_branch_missing",
            "configured remote branch is absent"
        ),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            if stderr.contains("authentication")
                || stderr.contains("could not read username")
                || stderr.contains("401")
            {
                blocked!("remote_auth_failed", "remote branch inspection was denied");
            }
            blocked!("remote_transport_failed", "remote branch inspection failed");
        }
        Err(_) => blocked!("inspection_command_failed", "git ls-remote could not start"),
    };
    let advertised_text = String::from_utf8_lossy(&advertised.stdout);
    let lines: Vec<&str> = advertised_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() != 1 {
        blocked!(
            "ambiguous_remote_ref",
            "remote returned an unexpected branch result"
        );
    }
    let advertised_sha = lines[0].split_whitespace().next().unwrap_or("");
    if !is_full_git_oid(advertised_sha) || !lines[0].ends_with(&remote_branch) {
        blocked!("ambiguous_remote_ref", "remote branch result is malformed");
    }

    let refspec = format!("+{}:{}", remote_branch, inspection_ref);
    match run(&[
        "fetch",
        "--no-tags",
        "--no-write-fetch-head",
        "origin",
        &refspec,
    ]) {
        Ok(output) if output.status.success() => (),
        Ok(_) => blocked!(
            "remote_fetch_failed",
            "fresh branch fetch failed after advertisement"
        ),
        Err(_) => blocked!("inspection_command_failed", "git fetch could not start"),
    }
    let fetched_ref = format!("{}^{{commit}}", inspection_ref);
    let fetched = match run(&["rev-parse", "--verify", &fetched_ref]) {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => blocked!(
            "inspection_object_missing",
            "fresh fetch did not produce a commit"
        ),
    };
    r = Some(fetched.clone());
    if fetched != advertised_sha {
        blocked!(
            "remote_changed_during_inspection",
            "remote branch moved between advertisement and fetch"
        );
    }

    let ignored = match run(&[
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
        "-z",
    ]) {
        Ok(output) if output.status.success() => output.stdout,
        _ => blocked!(
            "local_status_error",
            "ignored bridge paths could not be inspected"
        ),
    };
    let target = match run(&["ls-tree", "-r", "--name-only", "-z", &fetched]) {
        Ok(output) if output.status.success() => output.stdout,
        _ => blocked!(
            "inspection_command_failed",
            "incoming Git tree could not be inspected"
        ),
    };
    let ignored_paths = ignored
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty());
    let target_paths: Vec<&[u8]> = target
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect();
    for ignored in ignored_paths {
        let ignored = ignored.strip_suffix(b"/").unwrap_or(ignored);
        if target_paths.iter().any(|tracked| {
            *tracked == ignored
                || (tracked.starts_with(ignored) && tracked.get(ignored.len()) == Some(&b'/'))
                || (ignored.starts_with(tracked) && ignored.get(tracked.len()) == Some(&b'/'))
        }) {
            blocked!(
                "ignored_path_collision",
                "incoming tracked paths overlap ignored bridge data"
            );
        }
    }

    let checkpoint = match p.as_deref() {
        Some(sha) if is_full_git_oid(sha) => sha,
        Some(_) => blocked!(
            "ambiguous_checkpoint",
            "stored Git cursor is not a full object ID"
        ),
        None => blocked!(
            "missing_checkpoint",
            "no repository-owned handled Git cursor exists"
        ),
    };
    let checkpoint_expr = format!("{}^{{commit}}", checkpoint);
    if !run(&["cat-file", "-e", &checkpoint_expr]).is_ok_and(|output| output.status.success()) {
        blocked!(
            "missing_checkpoint_object",
            "handled Git cursor object is absent or invalid"
        );
    }
    match run(&["rev-parse", "--is-shallow-repository"]) {
        Ok(output)
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim() == "false" => {}
        Ok(output) if output.status.success() => blocked!(
            "incomplete_history",
            "bridge is shallow; ancestry is incomplete"
        ),
        _ => blocked!(
            "inspection_command_failed",
            "shallow-history inspection failed"
        ),
    }
    match run(&["merge-base", "--is-ancestor", checkpoint, &fetched]) {
        Ok(output) if output.status.code() == Some(0) => (),
        Ok(output) if output.status.code() == Some(1) => blocked!(
            "non_fast_forward",
            "fresh remote tip does not descend from handled Git cursor"
        ),
        _ => blocked!(
            "ancestry_command_failed",
            "Git ancestry could not be established"
        ),
    }
    if let Some(ref prior) = o {
        if prior != &fetched {
            match run(&["merge-base", "--is-ancestor", prior, &fetched]) {
                Ok(output) if output.status.code() == Some(0) => {}
                Ok(output) if output.status.code() == Some(1) => blocked!(
                    REASON_OBSERVED_REMOTE_REWRITE,
                    "prior observed remote tip is not an ancestor of the fresh remote tip"
                ),
                _ => blocked!(
                    "ancestry_command_failed",
                    "prior observed remote ancestry could not be established"
                ),
            }
        }
    }
    for (ancestor, descendant) in [
        (checkpoint, local.as_str()),
        (local.as_str(), fetched.as_str()),
    ] {
        match run(&["merge-base", "--is-ancestor", ancestor, descendant]) {
            Ok(output) if output.status.code() == Some(0) => (),
            Ok(output) if output.status.code() == Some(1) => blocked!(
                "unpublished_local_history",
                "bridge tip is outside the handled-to-remote path"
            ),
            _ => blocked!(
                "ancestry_command_failed",
                "bridge ancestry could not be established"
            ),
        }
    }
    let inspect_repo = match git2::Repository::open(path) {
        Ok(repo) => repo,
        Err(_) => blocked!(
            "selection_command_failed",
            "pending Git repository could not be opened"
        ),
    };
    match crate::pending_frontier::select_pending_oids(
        &inspect_repo,
        checkpoint,
        &fetched,
        crate::pending_frontier::DEFAULT_PENDING_COMMIT_CAP,
    ) {
        Ok(_) => {}
        Err(crate::errors::GitError::UnsupportedHistory { reason, detail }) => {
            blocked!(reason, detail);
        }
        Err(_) => blocked!(
            "selection_command_failed",
            "pending Git topology could not be inspected"
        ),
    }
    Ok(HistoryInspectAdmission {
        checkpoint: checkpoint.to_string(),
        remote_tip: fetched,
    })
}

/// Personal-mode Git→SVN gate: same P/O/R/L inspect before PR replay.
///
/// A missing origin remote is not inspected (existing clones without `origin`
/// keep working). A missing handled Git SHA is not invented as a baseline.
/// A durable rewrite block still refuses writes after restart.
pub fn inspect_personal_history(
    db: &Database,
    git_path: &Path,
    branch: &str,
    scope_id: &str,
) -> Result<Option<HistoryInspectAdmission>, SyncError> {
    let key = history_block_key(Some(scope_id));
    enforce_durable_history_block(db, &key)?;
    let origin = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(git_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    if !origin.is_ok_and(|output| output.status.success()) {
        return Ok(None);
    }
    let checkpoint = db
        .get_last_git_hash()
        .map_err(SyncError::DatabaseError)?
        .filter(|value| !value.is_empty());
    let Some(checkpoint) = checkpoint else {
        return Ok(None);
    };
    match inspect_fetched_history(git_path, branch, Some(checkpoint.clone())) {
        Ok(admission) => Ok(Some(admission)),
        Err(reject) => {
            persist_history_block(db, &key, Some(scope_id), &reject, Some(&checkpoint))?;
            Err(SyncError::HistoryBlocked {
                reason: reject.reason,
                detail: reject.detail,
            })
        }
    }
}
