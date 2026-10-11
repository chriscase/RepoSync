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
use crate::git::resolve_git_http_auth_token_for_workdir;
use crate::git::startup_handoff;

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
        || reason == "ancestry_command_failed"
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
    http_auth_token: Option<&str>,
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
        crate::git::subprocess_auth::git_cli_output(path, args, http_auth_token)
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
    match crate::pending_frontier::verify_pending_range(&inspect_repo, checkpoint, &fetched) {
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

/// Repository-owned Git cursor for personal mode without borrowing global state.
///
/// Personal inspect is intentionally unscoped: [`Database::list_repositories`] counts
/// every row (disabled repos and child rows included). Multiple rows fail closed
/// (no single P). One managed repo uses its `last_git_sha` column when populated;
/// an empty column does not fall back to the import watermark.
///
/// Personal writers advance `commit_map` on each mapping but keep the `git_sha`
/// watermark at the import baseline. When both are present, the live mapping tip
/// is authoritative when it descends from the watermark; unrelated copies fail
/// closed. A missing cursor is not invented.
pub fn resolve_personal_checkpoint(
    db: &Database,
    git_path: &Path,
) -> Result<Option<String>, SyncError> {
    let repos = db.list_repositories().map_err(SyncError::DatabaseError)?;
    let checkpoint = crate::sync_status::resolve_scoped_checkpoint_tip(db)
        .map_err(SyncError::DatabaseError)?
        .filter(|value| !value.is_empty());
    let watermark = db
        .get_watermark("git_sha")
        .map_err(SyncError::DatabaseError)?
        .filter(|value| !value.is_empty());
    match (checkpoint.as_deref(), watermark.as_deref()) {
        (Some(mapping), Some(watermark)) if mapping == watermark => Ok(Some(mapping.to_string())),
        (Some(mapping), Some(watermark)) => {
            if !is_full_git_oid(mapping) || !is_full_git_oid(watermark) {
                return Err(SyncError::HistoryBlocked {
                    reason: "ambiguous_checkpoint".into(),
                    detail: "stored Git cursor copies are not full object IDs".into(),
                });
            }
            match personal_checkpoint_ancestry(git_path, watermark, mapping) {
                Ok(true) => Ok(Some(mapping.to_string())),
                Ok(false) => Err(SyncError::HistoryBlocked {
                    reason: "ambiguous_checkpoint".into(),
                    detail: "commit_map handled Git cursor and git_sha watermark disagree".into(),
                }),
                Err(reject) => Err(SyncError::HistoryBlocked {
                    reason: reject.reason,
                    detail: reject.detail,
                }),
            }
        }
        (Some(mapping), None) => Ok(Some(mapping.to_string())),
        // Import watermark is a legacy personal fallback only when the repositories
        // table is empty; managed-repo installs must not borrow a global import SHA.
        (None, Some(watermark)) if repos.is_empty() => Ok(Some(watermark.to_string())),
        (None, Some(_)) | (None, None) => Ok(None),
    }
}

fn personal_checkpoint_ancestry(
    git_path: &Path,
    watermark: &str,
    mapping: &str,
) -> Result<bool, HistoryInspectReject> {
    match Command::new("git")
        .args(["merge-base", "--is-ancestor", watermark, mapping])
        .current_dir(git_path)
        .output()
    {
        Ok(output) if output.status.code() == Some(0) => Ok(true),
        Ok(output) if output.status.code() == Some(1) => Ok(false),
        _ => Err(HistoryInspectReject {
            reason: "ancestry_command_failed".into(),
            detail: "personal checkpoint ancestry could not be established".into(),
            o: None,
            r: None,
            l: None,
        }),
    }
}

fn resolve_personal_history_http_auth(
    db: &Database,
    git_path: &Path,
    scope_id: &str,
    http_auth_token: Option<&str>,
    config_token: Option<&str>,
) -> Option<String> {
    let chain_state = db.resolve_credential_chain_state(scope_id, "secret_git_token");
    if chain_state.explicitly_revoked {
        startup_handoff::forget_startup_http_auth(git_path);
        return None;
    }
    if let Some(token) = http_auth_token.filter(|value| !value.is_empty()) {
        return Some(token.to_string());
    }
    resolve_git_http_auth_token_for_workdir(db, scope_id, config_token, git_path)
}

fn block_personal_history(
    db: &Database,
    key: &str,
    scope_id: &str,
    reject: HistoryInspectReject,
    checkpoint: Option<&str>,
) -> Result<Option<HistoryInspectAdmission>, SyncError> {
    persist_history_block(db, key, Some(scope_id), &reject, checkpoint)?;
    Err(SyncError::HistoryBlocked {
        reason: reject.reason,
        detail: reject.detail,
    })
}

/// Personal-mode Git→SVN gate: same P/O/R/L inspect before PR replay.
///
/// Missing origin, missing checkpoint, or conflicting checkpoint provenance
/// fail closed before SVN writes. Initial import owns initialization and does
/// not call this gate. A durable rewrite block still refuses writes after
/// restart. HTTP(S) auth is taken from `http_auth_token`, the scoped
/// `secret_git_token` chain, config token, or same-process startup clone handoff.
pub fn inspect_personal_history_with_http_auth(
    db: &Database,
    git_path: &Path,
    branch: &str,
    scope_id: &str,
    http_auth_token: Option<&str>,
    config_token: Option<&str>,
) -> Result<Option<HistoryInspectAdmission>, SyncError> {
    let key = history_block_key(Some(scope_id));
    if scope_id == crate::db::personal_scope::PERSONAL_SCOPE_KEY {
        for block_key in crate::db::personal_scope::personal_history_block_keys() {
            enforce_durable_history_block(db, &block_key)?;
        }
    } else {
        enforce_durable_history_block(db, &key)?;
    }
    let origin = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(git_path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();
    if !origin.is_ok_and(|output| output.status.success()) {
        return block_personal_history(
            db,
            &key,
            scope_id,
            HistoryInspectReject {
                reason: "missing_origin".into(),
                detail:
                    "origin remote is not configured; run initial import to establish provenance"
                        .into(),
                o: None,
                r: None,
                l: None,
            },
            None,
        );
    }
    let checkpoint = match resolve_personal_checkpoint(db, git_path) {
        Ok(checkpoint) => checkpoint,
        Err(SyncError::HistoryBlocked { reason, detail }) => {
            return block_personal_history(
                db,
                &key,
                scope_id,
                HistoryInspectReject {
                    reason,
                    detail,
                    o: None,
                    r: None,
                    l: None,
                },
                None,
            );
        }
        Err(err) => return Err(err),
    };
    let Some(checkpoint) = checkpoint else {
        let repos = db.list_repositories().map_err(SyncError::DatabaseError)?;
        let detail = if repos.len() > 1 {
            "multiple managed repositories; cannot infer a single handled Git cursor"
        } else if repos.len() == 1 {
            "managed repository lacks a handled Git cursor in its last_git_sha column"
        } else {
            "no repository-owned handled Git cursor exists; run initial import first"
        };
        return block_personal_history(
            db,
            &key,
            scope_id,
            HistoryInspectReject {
                reason: "missing_checkpoint".into(),
                detail: detail.into(),
                o: None,
                r: None,
                l: None,
            },
            None,
        );
    };
    if !is_full_git_oid(&checkpoint) {
        return block_personal_history(
            db,
            &key,
            scope_id,
            HistoryInspectReject {
                reason: "ambiguous_checkpoint".into(),
                detail: "stored Git cursor is not a full object ID".into(),
                o: None,
                r: None,
                l: None,
            },
            Some(&checkpoint),
        );
    }
    let http_auth_token =
        resolve_personal_history_http_auth(db, git_path, scope_id, http_auth_token, config_token);
    match inspect_fetched_history(
        git_path,
        branch,
        Some(checkpoint.clone()),
        http_auth_token.as_deref(),
    ) {
        Ok(admission) => Ok(Some(admission)),
        Err(reject) => block_personal_history(db, &key, scope_id, reject, Some(&checkpoint)),
    }
}

pub fn inspect_personal_history(
    db: &Database,
    git_path: &Path,
    branch: &str,
    scope_id: &str,
) -> Result<Option<HistoryInspectAdmission>, SyncError> {
    inspect_personal_history_with_http_auth(db, git_path, branch, scope_id, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_git_with_origin(work_dir: &Path, bare_dir: &Path) {
        Command::new("git")
            .args(["init", "--bare", bare_dir.to_str().unwrap()])
            .status()
            .unwrap();
        Command::new("git")
            .args(["init", work_dir.to_str().unwrap()])
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C", work_dir.to_str().unwrap(), "remote", "add", "origin"])
            .arg(bare_dir)
            .status()
            .unwrap();
        std::fs::write(work_dir.join("seed.txt"), "seed\n").unwrap();
        Command::new("git")
            .args(["-C", work_dir.to_str().unwrap(), "add", "seed.txt"])
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-C",
                work_dir.to_str().unwrap(),
                "commit",
                "-m",
                "seed",
                "--author",
                "Test <test@example.com>",
            ])
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C", work_dir.to_str().unwrap(), "branch", "-M", "main"])
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-C",
                work_dir.to_str().unwrap(),
                "push",
                "-u",
                "origin",
                "main",
            ])
            .status()
            .unwrap();
    }

    fn git_head(work_dir: &Path) -> String {
        String::from_utf8(
            Command::new("git")
                .args(["-C", work_dir.to_str().unwrap(), "rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    fn insert_managed_repo(db: &Database, id: &str, last_git_sha: &str) {
        use crate::models::Repository;
        let now = chrono::Utc::now().to_rfc3339();
        db.insert_repository(&Repository {
            id: id.into(),
            name: id.into(),
            svn_url: "file:///tmp/svn".into(),
            svn_branch: "trunk".into(),
            svn_username: "fixture".into(),
            git_provider: "github".into(),
            git_api_url: "http://127.0.0.1:1".into(),
            git_repo: "org/repo".into(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 60,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now,
            last_svn_rev: 1,
            last_git_sha: last_git_sha.into(),
            last_sync_at: None,
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        })
        .unwrap();
    }

    #[test]
    fn resolve_personal_checkpoint_legacy_uses_commit_map_not_global_kv() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
        db.insert_commit_map(
            1,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "git_to_svn",
            "test",
            "Test",
        )
        .unwrap();
        assert_eq!(
            resolve_personal_checkpoint(&db, Path::new("/tmp/unused"))
                .unwrap()
                .as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "legacy personal checkpoint must read commit-map tip, not global kv"
        );
    }

    #[test]
    fn resolve_personal_checkpoint_multi_repo_fails_closed_not_foreign_tip() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "dddddddddddddddddddddddddddddddddddddddd")
            .unwrap();
        db.insert_commit_map(
            1,
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "git_to_svn",
            "test",
            "Test",
        )
        .unwrap();
        db.set_watermark("git_sha", "ffffffffffffffffffffffffffffffffffffffff")
            .unwrap();
        insert_managed_repo(&db, "alpha", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        insert_managed_repo(&db, "beta", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_eq!(
            resolve_personal_checkpoint(&db, Path::new("/tmp/unused")).unwrap(),
            None,
            "multi-repo installs must not adopt a foreign commit-map tip or import watermark"
        );
    }

    #[test]
    fn resolve_personal_checkpoint_single_repo_uses_column_not_foreign_tip() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "dddddddddddddddddddddddddddddddddddddddd")
            .unwrap();
        db.insert_commit_map(
            1,
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "git_to_svn",
            "test",
            "Test",
        )
        .unwrap();
        insert_managed_repo(&db, "only", "cccccccccccccccccccccccccccccccccccccccc");
        assert_eq!(
            resolve_personal_checkpoint(&db, Path::new("/tmp/unused"))
                .unwrap()
                .as_deref(),
            Some("cccccccccccccccccccccccccccccccccccccccc"),
            "single managed repo must use its column tip, not foreign global kv or commit-map tip"
        );
    }

    #[test]
    fn resolve_personal_checkpoint_single_repo_empty_column_ignores_watermark() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "dddddddddddddddddddddddddddddddddddddddd")
            .unwrap();
        db.set_state(
            "last_git_sha_only",
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        )
        .unwrap();
        db.insert_commit_map(
            1,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "git_to_svn",
            "test",
            "Test",
        )
        .unwrap();
        db.set_watermark("git_sha", "ffffffffffffffffffffffffffffffffffffffff")
            .unwrap();
        insert_managed_repo(&db, "only", "");
        assert_eq!(
            resolve_personal_checkpoint(&db, Path::new("/tmp/unused")).unwrap(),
            None,
            "empty managed-repo column must not fall back to import watermark, global kv, or commit-map tip"
        );
    }

    #[test]
    fn resolve_personal_checkpoint_legacy_watermark_fallback_without_mapping() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.set_state("last_git_hash", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();
        db.set_watermark("git_sha", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();
        assert_eq!(
            resolve_personal_checkpoint(&db, Path::new("/tmp/unused"))
                .unwrap()
                .as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "legacy personal installs without mapping rows may still use import watermark"
        );
    }

    #[test]
    fn resolve_personal_checkpoint_rejects_unrelated_watermark() {
        let tmp = TempDir::new().unwrap();
        let git_work = tmp.path().join("git");
        let bare = tmp.path().join("origin.git");
        init_git_with_origin(&git_work, &bare);
        let watermark = git_head(&git_work);
        git_cmd(&git_work, &["checkout", "--orphan", "foreign"]);
        std::fs::write(git_work.join("foreign.txt"), "foreign\n").unwrap();
        git_cmd(&git_work, &["add", "foreign.txt"]);
        git_cmd(
            &git_work,
            &[
                "commit",
                "-m",
                "foreign",
                "--author",
                "Test <test@example.com>",
            ],
        );
        let unrelated = git_head(&git_work);

        let db_path = tmp.path().join("personal.db");
        let db = Database::new(&db_path).unwrap();
        db.initialize().unwrap();
        db.insert_commit_map(1, &unrelated, "git_to_svn", "test", "Test")
            .unwrap();
        db.set_watermark("git_sha", &watermark).unwrap();

        let err = resolve_personal_checkpoint(&db, &git_work).unwrap_err();
        assert!(matches!(
            err,
            SyncError::HistoryBlocked {
                reason,
                ..
            } if reason == "ambiguous_checkpoint"
        ));
    }

    #[test]
    fn resolve_personal_checkpoint_admits_mapping_ahead_of_watermark() {
        let tmp = TempDir::new().unwrap();
        let git_work = tmp.path().join("git");
        let bare = tmp.path().join("origin.git");
        init_git_with_origin(&git_work, &bare);
        let watermark = git_head(&git_work);
        std::fs::write(git_work.join("progress.txt"), "progress\n").unwrap();
        git_cmd(&git_work, &["add", "progress.txt"]);
        git_cmd(
            &git_work,
            &[
                "commit",
                "-m",
                "progress",
                "--author",
                "Test <test@example.com>",
            ],
        );
        let mapping = git_head(&git_work);

        let db_path = tmp.path().join("personal.db");
        let db = Database::new(&db_path).unwrap();
        db.initialize().unwrap();
        db.insert_commit_map(1, &mapping, "svn_to_git", "test", "Test")
            .unwrap();
        db.set_watermark("git_sha", &watermark).unwrap();

        let checkpoint = resolve_personal_checkpoint(&db, &git_work)
            .expect("mapping ahead of import watermark must be admitted");
        assert_eq!(checkpoint.as_deref(), Some(mapping.as_str()));
    }

    fn git_cmd(work_dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(work_dir)
            .args(args)
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap();
        assert!(status.success(), "git {:?} failed", args);
    }

    #[test]
    fn inspect_personal_history_blocks_missing_origin() {
        let tmp = TempDir::new().unwrap();
        let git_work = tmp.path().join("git");
        Command::new("git")
            .args(["init", git_work.to_str().unwrap()])
            .status()
            .unwrap();

        let db_path = tmp.path().join("personal.db");
        let db = Database::new(&db_path).unwrap();
        db.initialize().unwrap();
        let handled = "cccccccccccccccccccccccccccccccccccccccc";
        db.insert_commit_map(1, handled, "git_to_svn", "test", "Test")
            .unwrap();

        let err = inspect_personal_history(&db, &git_work, "main", "personal").unwrap_err();
        assert!(matches!(
            err,
            SyncError::HistoryBlocked {
                reason,
                ..
            } if reason == "missing_origin"
        ));
        let block = load_history_block(&db, &history_block_key(Some("personal")))
            .unwrap()
            .unwrap();
        assert_eq!(block["reason"], "missing_origin");
    }

    #[test]
    fn inspect_personal_history_blocks_missing_checkpoint() {
        let tmp = TempDir::new().unwrap();
        let git_work = tmp.path().join("git");
        let bare = tmp.path().join("origin.git");
        init_git_with_origin(&git_work, &bare);

        let db_path = tmp.path().join("personal.db");
        let db = Database::new(&db_path).unwrap();
        db.initialize().unwrap();

        let err = inspect_personal_history(&db, &git_work, "main", "personal").unwrap_err();
        assert!(matches!(
            err,
            SyncError::HistoryBlocked {
                reason,
                detail,
                ..
            } if reason == "missing_checkpoint"
                && detail.contains("run initial import first")
        ));
        let block = load_history_block(&db, &history_block_key(Some("personal")))
            .unwrap()
            .unwrap();
        assert_eq!(block["reason"], "missing_checkpoint");
    }

    #[test]
    fn inspect_personal_history_blocks_single_repo_empty_column_without_import_copy() {
        let tmp = TempDir::new().unwrap();
        let git_work = tmp.path().join("git");
        let bare = tmp.path().join("origin.git");
        init_git_with_origin(&git_work, &bare);

        let db_path = tmp.path().join("personal.db");
        let db = Database::new(&db_path).unwrap();
        db.initialize().unwrap();
        insert_managed_repo(&db, "only", "");

        let err = inspect_personal_history(&db, &git_work, "main", "personal").unwrap_err();
        assert!(matches!(
            err,
            SyncError::HistoryBlocked {
                reason,
                detail,
                ..
            } if reason == "missing_checkpoint"
                && detail.contains("last_git_sha column")
                && !detail.contains("run initial import first")
        ));
    }

    #[test]
    fn inspect_personal_history_blocks_multi_repo_with_watermark() {
        let tmp = TempDir::new().unwrap();
        let git_work = tmp.path().join("git");
        let bare = tmp.path().join("origin.git");
        init_git_with_origin(&git_work, &bare);
        let watermark = git_head(&git_work);

        let db_path = tmp.path().join("personal.db");
        let db = Database::new(&db_path).unwrap();
        db.initialize().unwrap();
        db.set_watermark("git_sha", &watermark).unwrap();
        insert_managed_repo(&db, "alpha", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        insert_managed_repo(&db, "beta", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

        let err = inspect_personal_history(&db, &git_work, "main", "personal").unwrap_err();
        assert!(matches!(
            err,
            SyncError::HistoryBlocked {
                reason,
                detail,
                ..
            } if reason == "missing_checkpoint"
                && detail.contains("multiple managed repositories")
        ));
        let block = load_history_block(&db, &history_block_key(Some("personal")))
            .unwrap()
            .unwrap();
        assert_eq!(block["reason"], "missing_checkpoint");
        assert!(block["detail"]
            .as_str()
            .unwrap()
            .contains("multiple managed repositories"));
    }

    #[test]
    fn inspect_personal_history_admits_when_provenance_present() {
        let tmp = TempDir::new().unwrap();
        let git_work = tmp.path().join("git");
        let bare = tmp.path().join("origin.git");
        init_git_with_origin(&git_work, &bare);
        let handled = git_head(&git_work);

        let db_path = tmp.path().join("personal.db");
        let db = Database::new(&db_path).unwrap();
        db.initialize().unwrap();
        db.insert_commit_map(1, &handled, "git_to_svn", "test", "Test")
            .unwrap();

        let admission = inspect_personal_history(&db, &git_work, "main", "personal")
            .expect("qualified personal history must be admitted")
            .expect("origin and checkpoint require inspection");
        assert_eq!(admission.checkpoint, handled);
        assert_eq!(admission.remote_tip, handled);
    }

    #[test]
    fn personal_history_auth_revocation_beats_handoff_and_stale_tokens() {
        use crate::db::personal_scope::PERSONAL_SCOPE_KEY;
        use crate::git::startup_handoff;

        let tmp = TempDir::new().unwrap();
        let git_work = tmp.path().join("git");
        std::fs::create_dir_all(&git_work).unwrap();
        startup_handoff::remember_startup_http_auth(&git_work, "handoff-token");

        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let chain_key = format!("secret_git_token_{}", PERSONAL_SCOPE_KEY);
        db.set_state(&chain_key, "").unwrap();

        assert!(resolve_personal_history_http_auth(
            &db,
            &git_work,
            PERSONAL_SCOPE_KEY,
            Some("stale-caller-token"),
            Some("stale-config-token"),
        )
        .is_none());
        assert!(startup_handoff::peek_startup_http_auth(&git_work).is_none());
    }
}
