//! Exact per-commit skip disposition (#66 / RS-C04).
//!
//! Skips operate on a pinned cursor, an explicit selected-commit set, and
//! observed tip tokens. Exclusion receipts persist skipped identity so restart
//! and poll do not replay excluded work.

use std::path::Path;
use std::process::{Command, Output};

use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::errors::DatabaseError;
use crate::history_inspect::is_full_git_oid;

pub const EXCLUSION_RECEIPT_VERSION: u64 = 1;

/// Stable refusal reason codes returned to API callers.
pub mod reason {
    pub const CURSOR_MISMATCH: &str = "skip_commit_cursor_mismatch";
    pub const TIP_MISMATCH: &str = "skip_commit_tip_mismatch";
    pub const UNKNOWN_COMMIT: &str = "skip_commit_unknown_commit";
    pub const UNPROVEN_ANCESTRY: &str = "skip_commit_unproven_ancestry";
    pub const EMPTY_SELECTION: &str = "skip_commit_empty_selection";
    pub const NON_CONTIGUOUS: &str = "skip_commit_non_contiguous_selection";
    pub const ALREADY_EXCLUDED: &str = "skip_commit_already_excluded";
    pub const WORKDIR_UNAVAILABLE: &str = "skip_commit_workdir_unavailable";
}

pub fn exclusion_receipt_key(repo_id: &str, sha: &str) -> String {
    format!("handled_git_excluded_{}_{}", repo_id, sha)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkipCommitRequest {
    pub pinned_cursor: String,
    pub selected_commits: Vec<String>,
    pub expected_remote_tip: String,
    #[serde(default)]
    pub expected_bridge_tip: Option<String>,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkipCommitContext {
    pub pinned_cursor: String,
    pub observed_remote_tip: Option<String>,
    pub observed_bridge_tip: Option<String>,
    pub pending_commits: Vec<PendingCommit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingCommit {
    pub sha: String,
    pub subject: String,
    pub excluded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkipCommitOutcome {
    pub old_cursor: String,
    pub new_cursor: String,
    pub excluded_commits: Vec<String>,
    pub remaining_pending: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SkipCommitRefusal {
    pub code: String,
    pub detail: String,
}

impl SkipCommitRefusal {
    pub fn message(&self) -> String {
        format!("{}: {}", self.code, self.detail)
    }
}

fn git(workdir: &Path, args: &[&str]) -> Result<Output, SkipCommitRefusal> {
    Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .map_err(|e| SkipCommitRefusal {
            code: reason::UNPROVEN_ANCESTRY.into(),
            detail: format!("git {:?} failed: {}", args, e),
        })
}

pub fn git_success(workdir: &Path, args: &[&str]) -> Result<Output, SkipCommitRefusal> {
    let output = git(workdir, args)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(SkipCommitRefusal {
            code: reason::UNPROVEN_ANCESTRY.into(),
            detail: format!(
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        })
    }
}

fn stdout_trim(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub fn is_ancestor(
    workdir: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, SkipCommitRefusal> {
    let output = git(
        workdir,
        &["merge-base", "--is-ancestor", ancestor, descendant],
    )?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(SkipCommitRefusal {
            code: reason::UNPROVEN_ANCESTRY.into(),
            detail: format!(
                "ancestry between {} and {} could not be established",
                ancestor, descendant
            ),
        }),
    }
}

pub fn object_exists(workdir: &Path, sha: &str) -> Result<bool, SkipCommitRefusal> {
    let output = git(workdir, &["cat-file", "-e", &format!("{sha}^{{commit}}")])?;
    Ok(output.status.success())
}

pub fn pending_commits(
    workdir: &Path,
    cursor: &str,
    tip: &str,
) -> Result<Vec<String>, SkipCommitRefusal> {
    if cursor == tip {
        return Ok(Vec::new());
    }
    let range = format!("{cursor}..{tip}");
    // Topo-order reverse is the ancestry frontier (parents before children),
    // not a date/visited walk that can omit older merge-side commits.
    let output = git_success(workdir, &["rev-list", "--topo-order", "--reverse", &range])?;
    Ok(stdout_trim(&output)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

pub fn commit_subject(workdir: &Path, sha: &str) -> Result<String, SkipCommitRefusal> {
    let output = git_success(workdir, &["log", "-1", "--format=%s", sha])?;
    Ok(stdout_trim(&output))
}

pub fn normalize_sha(sha: &str) -> Result<String, SkipCommitRefusal> {
    if !is_full_git_oid(sha) {
        return Err(SkipCommitRefusal {
            code: reason::UNKNOWN_COMMIT.into(),
            detail: format!("invalid Git object id: {}", sha),
        });
    }
    Ok(sha.to_ascii_lowercase())
}

fn stored_cursor(db: &Database, repo_id: &str) -> Result<String, DatabaseError> {
    let repo = db
        .get_repository(repo_id)?
        .ok_or_else(|| DatabaseError::Other("repository not found".into()))?;
    let kv = db
        .get_state(&format!("last_git_sha_{}", repo_id))?
        .filter(|value| !value.is_empty());
    let column = repo.last_git_sha.clone();
    match (column.is_empty(), kv) {
        (true, None) => Ok(String::new()),
        (false, None) => Ok(column),
        (true, Some(kv)) => Ok(kv),
        (false, Some(kv)) if kv == column => Ok(column),
        (false, Some(_)) => Err(DatabaseError::Other(
            "repository cursor copies disagree; reconcile before skip".into(),
        )),
    }
}

pub fn load_exclusion_receipt(
    db: &Database,
    repo_id: &str,
    sha: &str,
) -> Result<Option<serde_json::Value>, DatabaseError> {
    let key = exclusion_receipt_key(repo_id, sha);
    let raw = db.get_state(&key)?;
    raw.map(|value| serde_json::from_str(&value).map_err(|e| DatabaseError::Other(e.to_string())))
        .transpose()
}

pub fn is_commit_excluded(db: &Database, repo_id: &str, sha: &str) -> Result<bool, DatabaseError> {
    Ok(load_exclusion_receipt(db, repo_id, sha)?.is_some())
}

pub fn build_skip_context(
    db: &Database,
    repo_id: &str,
    workdir: &Path,
    observed_remote_tip: Option<&str>,
) -> Result<SkipCommitContext, SkipCommitRefusal> {
    let pinned_cursor = stored_cursor(db, repo_id).map_err(|e| SkipCommitRefusal {
        code: reason::CURSOR_MISMATCH.into(),
        detail: e.to_string(),
    })?;
    if pinned_cursor.is_empty() {
        return Err(SkipCommitRefusal {
            code: reason::CURSOR_MISMATCH.into(),
            detail: "repository has no pinned Git cursor".into(),
        });
    }
    let observed_bridge_tip = git_success(workdir, &["rev-parse", "HEAD"])
        .map(|output| stdout_trim(&output))
        .ok();
    let remote_tip = observed_remote_tip.map(str::to_string);
    let pending = if let Some(tip) = remote_tip.as_deref() {
        pending_commits(workdir, &pinned_cursor, tip)?
    } else {
        Vec::new()
    };
    let pending_commits = pending
        .into_iter()
        .map(|sha| {
            let excluded = is_commit_excluded(db, repo_id, &sha).unwrap_or(false);
            let subject = commit_subject(workdir, &sha).unwrap_or_else(|_| sha.clone());
            PendingCommit {
                sha,
                subject,
                excluded,
            }
        })
        .collect();
    Ok(SkipCommitContext {
        pinned_cursor,
        observed_remote_tip: remote_tip,
        observed_bridge_tip,
        pending_commits,
    })
}

pub fn execute_exact_skip(
    db: &Database,
    repo_id: &str,
    workdir: &Path,
    observed_remote_tip: &str,
    _observed_bridge_tip: Option<&str>,
    request: &SkipCommitRequest,
) -> Result<SkipCommitOutcome, SkipCommitRefusal> {
    let pinned_cursor = normalize_sha(&request.pinned_cursor)?;
    let expected_remote_tip = normalize_sha(&request.expected_remote_tip)?;
    let expected_bridge_tip = request
        .expected_bridge_tip
        .as_deref()
        .map(normalize_sha)
        .transpose()?;
    let selected: Vec<String> = request
        .selected_commits
        .iter()
        .map(|sha| normalize_sha(sha))
        .collect::<Result<Vec<_>, _>>()?;

    if selected.is_empty() {
        return Err(SkipCommitRefusal {
            code: reason::EMPTY_SELECTION.into(),
            detail: "at least one commit must be selected for exact skip".into(),
        });
    }

    let stored = stored_cursor(db, repo_id).map_err(|e| SkipCommitRefusal {
        code: reason::CURSOR_MISMATCH.into(),
        detail: e.to_string(),
    })?;
    if stored != pinned_cursor {
        return Err(SkipCommitRefusal {
            code: reason::CURSOR_MISMATCH.into(),
            detail: format!(
                "pinned cursor {} does not match stored cursor {}",
                pinned_cursor, stored
            ),
        });
    }

    let actual_remote = normalize_sha(observed_remote_tip)?;
    if actual_remote != expected_remote_tip {
        return Err(SkipCommitRefusal {
            code: reason::TIP_MISMATCH.into(),
            detail: format!(
                "expected remote tip {} does not match observed {}",
                expected_remote_tip, actual_remote
            ),
        });
    }

    if let Some(expected_bridge) = expected_bridge_tip {
        let actual_bridge = normalize_sha(
            &git_success(workdir, &["rev-parse", "HEAD"]).map(|output| stdout_trim(&output))?,
        )?;
        if actual_bridge != expected_bridge {
            return Err(SkipCommitRefusal {
                code: reason::TIP_MISMATCH.into(),
                detail: format!(
                    "expected bridge tip {} does not match observed {}",
                    expected_bridge, actual_bridge
                ),
            });
        }
    }

    if !object_exists(workdir, &pinned_cursor)? {
        return Err(SkipCommitRefusal {
            code: reason::UNPROVEN_ANCESTRY.into(),
            detail: "pinned cursor object is missing from the workdir".into(),
        });
    }
    if !object_exists(workdir, &expected_remote_tip)? {
        return Err(SkipCommitRefusal {
            code: reason::UNPROVEN_ANCESTRY.into(),
            detail: "expected remote tip object is missing from the workdir".into(),
        });
    }
    if !is_ancestor(workdir, &pinned_cursor, &expected_remote_tip)? {
        return Err(SkipCommitRefusal {
            code: reason::UNPROVEN_ANCESTRY.into(),
            detail: "remote tip is not a descendant of the pinned cursor".into(),
        });
    }

    let pending = pending_commits(workdir, &pinned_cursor, &expected_remote_tip)?;
    if pending.is_empty() {
        return Err(SkipCommitRefusal {
            code: reason::UNKNOWN_COMMIT.into(),
            detail: "no pending commits exist between pinned cursor and remote tip".into(),
        });
    }

    for sha in &selected {
        if !pending.contains(sha) {
            return Err(SkipCommitRefusal {
                code: reason::UNKNOWN_COMMIT.into(),
                detail: format!("selected commit {} is not pending at observed tips", sha),
            });
        }
        if is_commit_excluded(db, repo_id, sha).unwrap_or(false) {
            return Err(SkipCommitRefusal {
                code: reason::ALREADY_EXCLUDED.into(),
                detail: format!("commit {} already has an exclusion receipt", sha),
            });
        }
        if !object_exists(workdir, sha)? {
            return Err(SkipCommitRefusal {
                code: reason::UNKNOWN_COMMIT.into(),
                detail: format!("selected commit {} is missing from the workdir", sha),
            });
        }
        if !is_ancestor(workdir, &pinned_cursor, sha)? {
            return Err(SkipCommitRefusal {
                code: reason::UNPROVEN_ANCESTRY.into(),
                detail: format!(
                    "selected commit {} is not descended from pinned cursor",
                    sha
                ),
            });
        }
        if !is_ancestor(workdir, sha, &expected_remote_tip)? {
            return Err(SkipCommitRefusal {
                code: reason::UNPROVEN_ANCESTRY.into(),
                detail: format!(
                    "selected commit {} is not an ancestor of the observed remote tip",
                    sha
                ),
            });
        }
    }

    let prefix = pending
        .iter()
        .take(selected.len())
        .cloned()
        .collect::<Vec<_>>();
    if prefix != selected {
        return Err(SkipCommitRefusal {
            code: reason::NON_CONTIGUOUS.into(),
            detail:
                "selected commits must form the oldest contiguous pending prefix; unselected pending work must remain"
                    .into(),
        });
    }

    let new_cursor = selected.last().cloned().unwrap();
    let skip_reason = if request.reason.trim().is_empty() {
        "operator_skip".to_string()
    } else {
        request.reason.trim().to_string()
    };

    db.advance_exact_skip_watermarks(repo_id, &new_cursor, &selected, &skip_reason)
        .map_err(|e| SkipCommitRefusal {
            code: reason::UNPROVEN_ANCESTRY.into(),
            detail: e.to_string(),
        })?;

    let remaining_pending = pending_commits(workdir, &new_cursor, &expected_remote_tip)?;
    Ok(SkipCommitOutcome {
        old_cursor: pinned_cursor,
        new_cursor,
        excluded_commits: selected,
        remaining_pending,
    })
}
