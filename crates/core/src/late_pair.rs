//! #67 late-pair admission and preview (no publish / no replay).
//!
//! Prove that a candidate Git tip descends from a verified SVN-import mapping
//! using stored mappings plus actual Git ancestry. Commit messages are not
//! lineage proof. Preview pins tips and revisions; it does not copy SVN
//! branches, write checkpoints, mutate remotes, or activate the scheduler.

use std::path::Path;
use std::process::{Command, Output};

use serde::Serialize;

use crate::db::import_operations::ImportOperationState;
use crate::db::Database;
use crate::errors::DatabaseError;
use crate::history_inspect::is_full_git_oid;
use crate::svn::SvnClient;

/// Policy identity pinned into every preview plan from this slice.
pub const POLICY_VERSION: &str = "late_pair_admission_v1";

const INSPECT_REF: &str = "refs/reposync/late-pair/inspect";

/// A verified SVN→Git mapping that may serve as a late-pair baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedMapping {
    pub git_sha: String,
    pub svn_revision: i64,
    pub evidence: String,
}

/// Read-only SVN observations included in the preview. Existence is not
/// equivalence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SvnTargetProbe {
    pub exists: bool,
    pub equivalent: bool,
    pub uuid: Option<String>,
    pub revision: Option<i64>,
    pub url: String,
    pub note: String,
}

/// Pending Git work relative to the verified baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingGitWork {
    pub count: usize,
    pub summary: Vec<String>,
}

/// Pending SVN work if a parent HEAD revision is knowable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingSvnWork {
    pub knowable: bool,
    pub parent_head_revision: Option<i64>,
    pub baseline_revision: Option<i64>,
    pub note: String,
}

/// Dry-run plan. `scheduler_active` is always false in this slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LatePairPlan {
    pub mode: String,
    pub published: bool,
    pub admitted: bool,
    pub pair_state: String,
    pub scheduler_active: bool,
    pub policy_version: String,
    pub parent_id: String,
    pub git_branch: String,
    pub svn_branch: String,
    pub git_tip: Option<String>,
    pub svn_source_revision: Option<i64>,
    pub svn_target_revision: Option<i64>,
    pub verified_baseline: Option<VerifiedMapping>,
    pub baseline_missing_reason: Option<String>,
    pub inherited_work: Vec<String>,
    pub pending_git: PendingGitWork,
    pub pending_svn: PendingSvnWork,
    pub conflicts: Vec<String>,
    pub unknowns: Vec<String>,
    pub proposed_svn_copy_source_revision: Option<i64>,
    pub existing_svn_target: SvnTargetProbe,
    pub skip_import_requested: bool,
    pub skip_import_applied: bool,
    pub skip_import_note: String,
}

/// Fail-closed refusal. Callers must not copy SVN, write checkpoints, or
/// insert a scheduler-active pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatePairRefusal {
    pub reason: String,
    pub detail: String,
    pub plan: Option<Box<LatePairPlan>>,
}

impl LatePairRefusal {
    pub fn error_message(&self) -> String {
        format!("{}: {}", self.reason, self.detail)
    }
}

/// Inputs for admission. Git objects are inspected from `git_workdir` when
/// present; a provider SHA without objects is accepted only when it *is* a
/// verified mapping SHA (pending Git work then unknown/zero).
#[derive(Debug, Clone)]
pub struct LatePairRequest {
    pub parent_id: String,
    pub git_branch: String,
    pub svn_branch: String,
    pub skip_import: bool,
    pub compatibility_skip_import: bool,
    pub dry_run: bool,
}

fn git(workdir: &Path, args: &[&str]) -> std::io::Result<Output> {
    Command::new("git")
        .args(args)
        .current_dir(workdir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
}

fn stdout_trim(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Collect SVN-origin mappings for `repo_id` from scoped applied records and
/// a completed import's confirmed SHA. Unscoped commit-map rows and commit
/// messages are not sufficient on their own.
pub fn collect_verified_mappings(
    db: &Database,
    repo_id: &str,
) -> Result<Vec<VerifiedMapping>, DatabaseError> {
    let mut mappings = Vec::new();
    {
        let conn = db.conn();
        let mut stmt = conn.prepare(
            "SELECT svn_rev, git_sha FROM sync_records
             WHERE repo_id = ?1
               AND direction = 'svn_to_git'
               AND status = 'applied'
               AND git_sha IS NOT NULL AND git_sha != ''
               AND svn_rev IS NOT NULL
             ORDER BY svn_rev ASC",
        )?;
        let rows = stmt.query_map([repo_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (svn_revision, git_sha) = row?;
            if is_full_git_oid(&git_sha) {
                mappings.push(VerifiedMapping {
                    git_sha,
                    svn_revision,
                    evidence: "applied_sync_record".into(),
                });
            }
        }
    }

    if let Some(op) = db.latest_import_operation(repo_id)? {
        if op.state == ImportOperationState::Completed {
            if let (Some(sha), Some(rev)) = (op.last_confirmed_git_sha, op.last_confirmed_svn_rev) {
                if is_full_git_oid(&sha)
                    && !mappings
                        .iter()
                        .any(|item| item.git_sha == sha && item.svn_revision == rev)
                {
                    mappings.push(VerifiedMapping {
                        git_sha: sha,
                        svn_revision: rev,
                        evidence: if op.snapshot_pin.is_some()
                            || op.operation_type == "snapshot_import"
                        {
                            "snapshot_import_confirmed".into()
                        } else {
                            "import_confirmed".into()
                        },
                    });
                }
            }
        }
    }

    Ok(mappings)
}

/// Fetch `branch` into the late-pair inspect ref without checkout or reset.
pub fn resolve_candidate_tip(workdir: &Path, branch: &str) -> Result<String, LatePairRefusal> {
    if !workdir.join(".git").exists() && !workdir.join("HEAD").exists() {
        return Err(LatePairRefusal {
            reason: "missing_git_objects".into(),
            detail: "parent Git workdir is missing; ancestry cannot be proved".into(),
            plan: None,
        });
    }
    let fetch_refspec = format!("refs/heads/{branch}:{INSPECT_REF}");
    let fetched = git(
        workdir,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--",
            "origin",
            &fetch_refspec,
        ],
    );
    let inspect = git(
        workdir,
        &[
            "rev-parse",
            "--verify",
            &format!("{INSPECT_REF}^{{commit}}"),
        ],
    );
    if inspect
        .as_ref()
        .is_ok_and(|output| output.status.success() && is_full_git_oid(&stdout_trim(output)))
    {
        return Ok(stdout_trim(inspect.as_ref().unwrap()));
    }
    let local = git(
        workdir,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
    );
    if local
        .as_ref()
        .is_ok_and(|output| output.status.success() && is_full_git_oid(&stdout_trim(output)))
    {
        return Ok(stdout_trim(local.as_ref().unwrap()));
    }
    let fetch_err = fetched
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stderr).trim().to_string())
        .filter(|text| !text.is_empty());
    Err(LatePairRefusal {
        reason: "missing_git_ref".into(),
        detail: format!(
            "Git branch '{branch}' could not be fetched for ancestry proof{}",
            fetch_err
                .map(|text| format!(": {text}"))
                .unwrap_or_default()
        ),
        plan: None,
    })
}

fn is_ancestor(workdir: &Path, ancestor: &str, descendant: &str) -> Result<bool, LatePairRefusal> {
    match git(
        workdir,
        &["merge-base", "--is-ancestor", ancestor, descendant],
    ) {
        Ok(output) if output.status.code() == Some(0) => Ok(true),
        Ok(output) if output.status.code() == Some(1) => Ok(false),
        Ok(_) | Err(_) => Err(LatePairRefusal {
            reason: "ancestry_command_failed".into(),
            detail: "Git ancestry could not be established from mappings plus objects".into(),
            plan: None,
        }),
    }
}

fn pending_git(
    workdir: &Path,
    baseline: &str,
    tip: &str,
) -> Result<PendingGitWork, LatePairRefusal> {
    let range = format!("{baseline}..{tip}");
    let count_out =
        git(workdir, &["rev-list", "--count", &range]).map_err(|_| LatePairRefusal {
            reason: "selection_command_failed".into(),
            detail: "pending Git commits could not be counted".into(),
            plan: None,
        })?;
    if !count_out.status.success() {
        return Err(LatePairRefusal {
            reason: "selection_command_failed".into(),
            detail: "pending Git commits could not be counted".into(),
            plan: None,
        });
    }
    let count: usize = stdout_trim(&count_out)
        .parse()
        .map_err(|_| LatePairRefusal {
            reason: "selection_command_failed".into(),
            detail: "pending Git count was invalid".into(),
            plan: None,
        })?;
    let summary_out =
        git(workdir, &["rev-list", "--reverse", "--format=%s", &range]).map_err(|_| {
            LatePairRefusal {
                reason: "selection_command_failed".into(),
                detail: "pending Git subjects could not be listed".into(),
                plan: None,
            }
        })?;
    let mut summary = Vec::new();
    if summary_out.status.success() {
        for line in String::from_utf8_lossy(&summary_out.stdout).lines() {
            if let Some(subject) = line.strip_prefix("    ") {
                if !subject.is_empty() {
                    summary.push(subject.to_string());
                }
            } else if !line.starts_with("commit ") && !line.is_empty() {
                summary.push(line.to_string());
            }
        }
        summary.truncate(20);
    }
    Ok(PendingGitWork { count, summary })
}

fn empty_target_probe(svn_branch: &str) -> SvnTargetProbe {
    SvnTargetProbe {
        exists: false,
        equivalent: false,
        uuid: None,
        revision: None,
        url: svn_branch.to_string(),
        note: "SVN target was not probed".into(),
    }
}

fn skeleton_plan(request: &LatePairRequest) -> LatePairPlan {
    LatePairPlan {
        mode: if request.dry_run {
            "preview".into()
        } else {
            "publish_refused".into()
        },
        published: false,
        admitted: false,
        pair_state: "preparing".into(),
        scheduler_active: false,
        policy_version: POLICY_VERSION.into(),
        parent_id: request.parent_id.clone(),
        git_branch: request.git_branch.clone(),
        svn_branch: request.svn_branch.clone(),
        git_tip: None,
        svn_source_revision: None,
        svn_target_revision: None,
        verified_baseline: None,
        baseline_missing_reason: None,
        inherited_work: Vec::new(),
        pending_git: PendingGitWork {
            count: 0,
            summary: Vec::new(),
        },
        pending_svn: PendingSvnWork {
            knowable: false,
            parent_head_revision: None,
            baseline_revision: None,
            note: "SVN parent HEAD was not probed".into(),
        },
        conflicts: Vec::new(),
        unknowns: Vec::new(),
        proposed_svn_copy_source_revision: None,
        existing_svn_target: empty_target_probe(&request.svn_branch),
        skip_import_requested: request.skip_import,
        skip_import_applied: false,
        skip_import_note: String::new(),
    }
}

/// Admit or refuse a late pair. Never mutates Git remotes, SVN, or DB.
pub fn evaluate_admission(
    mappings: &[VerifiedMapping],
    git_workdir: Option<&Path>,
    provider_tip: Option<&str>,
    request: &LatePairRequest,
) -> Result<LatePairPlan, LatePairRefusal> {
    let mut plan = skeleton_plan(request);

    if mappings.is_empty() {
        plan.baseline_missing_reason = Some(
            "no verified SVN-import mapping for the parent pair (mappings + ancestry required)"
                .into(),
        );
        return Err(LatePairRefusal {
            reason: "missing_baseline".into(),
            detail: plan.baseline_missing_reason.clone().unwrap(),
            plan: Some(Box::new(plan)),
        });
    }

    let tip = if let Some(workdir) = git_workdir {
        match resolve_candidate_tip(workdir, &request.git_branch) {
            Ok(sha) => sha,
            Err(mut refuse) => {
                if let Some(provider) = provider_tip.filter(|sha| is_full_git_oid(sha)) {
                    if mappings.iter().any(|item| item.git_sha == *provider) {
                        provider.to_string()
                    } else {
                        refuse.plan = Some(Box::new(plan));
                        return Err(refuse);
                    }
                } else {
                    refuse.plan = Some(Box::new(plan));
                    return Err(refuse);
                }
            }
        }
    } else if let Some(provider) = provider_tip.filter(|sha| is_full_git_oid(sha)) {
        if mappings.iter().any(|item| item.git_sha == *provider) {
            provider.to_string()
        } else {
            plan.git_tip = Some(provider.to_string());
            plan.unknowns.push(
                "Git objects were not available; a provider SHA that is not a verified mapping cannot prove descent"
                    .into(),
            );
            return Err(LatePairRefusal {
                reason: "unrelated_git_first".into(),
                detail: "Git tip is not a verified SVN-import mapping and ancestry objects are unavailable"
                    .into(),
                plan: Some(Box::new(plan)),
            });
        }
    } else {
        return Err(LatePairRefusal {
            reason: "missing_git_objects".into(),
            detail: "candidate Git tip could not be resolved from a local workdir or trusted mapping SHA"
                .into(),
            plan: Some(Box::new(plan)),
        });
    };
    plan.git_tip = Some(tip.clone());

    let mut baseline: Option<VerifiedMapping> = None;
    if let Some(workdir) = git_workdir {
        for mapping in mappings {
            match is_ancestor(workdir, &mapping.git_sha, &tip) {
                Ok(true) => {
                    let take = baseline
                        .as_ref()
                        .map(|current| mapping.svn_revision >= current.svn_revision)
                        .unwrap_or(true);
                    if take {
                        baseline = Some(mapping.clone());
                    }
                }
                Ok(false) => {}
                Err(mut refuse) => {
                    refuse.plan = Some(Box::new(plan));
                    return Err(refuse);
                }
            }
        }
        if let Some(found) = baseline.clone() {
            match pending_git(workdir, &found.git_sha, &tip) {
                Ok(pending) => plan.pending_git = pending,
                Err(mut refuse) => {
                    refuse.plan = Some(Box::new(plan));
                    return Err(refuse);
                }
            }
        }
    } else {
        baseline = mappings.iter().find(|item| item.git_sha == tip).cloned();
        plan.pending_git = PendingGitWork {
            count: 0,
            summary: Vec::new(),
        };
        if baseline.is_some() {
            plan.unknowns
                .push("pending Git work is 0 because the tip equals the mapping SHA; no object walk was possible".into());
        }
    }

    let Some(baseline) = baseline else {
        plan.baseline_missing_reason = Some(
            "Git tip does not descend from a verified SVN-import mapping (unrelated Git-first or orphan root)"
                .into(),
        );
        return Err(LatePairRefusal {
            reason: "unrelated_git_first".into(),
            detail: plan.baseline_missing_reason.clone().unwrap(),
            plan: Some(Box::new(plan)),
        });
    };

    plan.verified_baseline = Some(baseline.clone());
    plan.svn_source_revision = Some(baseline.svn_revision);
    plan.proposed_svn_copy_source_revision = Some(baseline.svn_revision);
    plan.inherited_work = mappings
        .iter()
        .filter(|item| item.svn_revision <= baseline.svn_revision)
        .map(|item| {
            format!(
                "r{} → {} ({})",
                item.svn_revision,
                &item.git_sha[..8.min(item.git_sha.len())],
                item.evidence
            )
        })
        .collect();
    plan.pending_svn.baseline_revision = Some(baseline.svn_revision);
    plan.admitted = true;

    if request.skip_import && !request.compatibility_skip_import {
        plan.skip_import_note =
            "unsafe skip_import / start-from-now is refused; it would acknowledge unreconnciled Git commits or pair different trees".into();
        return Err(LatePairRefusal {
            reason: "unsafe_skip_import".into(),
            detail: plan.skip_import_note.clone(),
            plan: Some(Box::new(plan)),
        });
    }
    if request.skip_import && request.compatibility_skip_import {
        plan.skip_import_note =
            "compatibility_skip_import acknowledged the unsafe start-from-now request; watermarks are still not applied in this slice".into();
    } else {
        plan.skip_import_note =
            "skip_import was not applied; checkpoints are not set to the current tips".into();
    }
    plan.skip_import_applied = false;

    plan.mode = if request.dry_run {
        "preview".into()
    } else {
        "publish_pending".into()
    };
    if request.dry_run {
        plan.pair_state = "preparing".into();
    } else {
        plan.unknowns.push(
            "admission succeeded; SVN copy, replay, and scheduler activation happen in publish"
                .into(),
        );
        plan.pair_state = "preparing".into();
    }
    Ok(plan)
}

/// Read-only SVN probe. A live path is never treated as an equivalent pair.
pub async fn probe_svn_target(
    parent: &SvnClient,
    target: &SvnClient,
    parent_branch_url: &str,
    target_url: &str,
    baseline_revision: Option<i64>,
) -> (SvnTargetProbe, PendingSvnWork) {
    let parent_info = parent.info().await.ok();
    let parent_head = parent_info.as_ref().map(|info| info.latest_rev);
    let mut pending = PendingSvnWork {
        knowable: parent_head.is_some(),
        parent_head_revision: parent_head,
        baseline_revision,
        note: match (parent_head, baseline_revision) {
            (Some(head), Some(base)) if head > base => format!(
                "parent SVN HEAD r{head} is ahead of verified baseline r{base}; later parent revisions are not copied blindly"
            ),
            (Some(head), Some(base)) if head == base => {
                format!("parent SVN HEAD r{head} matches the verified baseline")
            }
            (Some(head), Some(base)) => format!(
                "parent SVN HEAD r{head} is behind verified baseline r{base}"
            ),
            (Some(head), None) => format!("parent SVN HEAD r{head}; baseline unknown"),
            _ => "parent SVN HEAD could not be read".into(),
        },
    };
    if parent_head.is_none() {
        pending.knowable = false;
    }

    let probe = match target.info().await {
        Ok(info) => {
            let same_uuid = parent_info
                .as_ref()
                .map(|parent_info| parent_info.uuid == info.uuid)
                .unwrap_or(false);
            SvnTargetProbe {
                exists: true,
                equivalent: false,
                uuid: Some(info.uuid),
                revision: Some(info.latest_rev),
                url: target_url.to_string(),
                note: if same_uuid {
                    format!(
                        "SVN target already exists at r{}; existence is not equivalence with Git baseline or parent {}",
                        info.latest_rev, parent_branch_url
                    )
                } else {
                    "SVN target exists with a different UUID; not equivalent".into()
                },
            }
        }
        Err(error) => {
            let err = error.to_string();
            let missing = err.contains("E170000")
                || err.contains("E160013")
                || err.contains("non-existent")
                || err.to_lowercase().contains("not found");
            SvnTargetProbe {
                exists: false,
                equivalent: false,
                uuid: None,
                revision: None,
                url: target_url.to_string(),
                note: if missing {
                    "SVN target does not exist; a later slice may copy from the verified baseline revision, not current parent HEAD".into()
                } else {
                    format!("SVN target identity is unknown ({err})")
                },
            }
        }
    };
    (probe, pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::models::{SyncDirection, SyncRecord, SyncRecordStatus};
    use chrono::Utc;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn git_env<'a>(dir: &'a Path, args: &'a [&'a str]) -> std::process::Output {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn init_repo(root: &Path) -> (PathBuf, PathBuf, String) {
        let bare = root.join("origin.git");
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        git_env(root, &["init", "--bare", bare.to_str().unwrap()]);
        git_env(&work, &["init", "-b", "main"]);
        git_env(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        fs::write(work.join("origin.txt"), "SVN origin\n").unwrap();
        git_env(&work, &["add", "origin.txt"]);
        git_env(&work, &["commit", "-m", "Verified SVN origin"]);
        git_env(&work, &["push", "-u", "origin", "main"]);
        let sha = stdout_trim(&git_env(&work, &["rev-parse", "HEAD"]));
        (bare, work, sha)
    }

    fn request(skip_import: bool, dry_run: bool) -> LatePairRequest {
        LatePairRequest {
            parent_id: "parent".into(),
            git_branch: "feature".into(),
            svn_branch: "branches/feature".into(),
            skip_import,
            compatibility_skip_import: false,
            dry_run,
        }
    }

    #[test]
    fn admits_svn_derived_feature_and_pins_pending_git() {
        let tmp = TempDir::new().unwrap();
        let (_bare, work, base) = init_repo(tmp.path());
        git_env(&work, &["checkout", "-b", "feature"]);
        fs::write(work.join("feature.txt"), "one\n").unwrap();
        git_env(&work, &["add", "feature.txt"]);
        git_env(&work, &["commit", "-m", "Feature step one"]);
        fs::write(work.join("feature.txt"), "two\n").unwrap();
        git_env(&work, &["commit", "-am", "Feature step two"]);
        git_env(&work, &["push", "-u", "origin", "feature"]);
        let mappings = vec![VerifiedMapping {
            git_sha: base.clone(),
            svn_revision: 2,
            evidence: "applied_sync_record".into(),
        }];
        let plan = evaluate_admission(&mappings, Some(&work), None, &request(false, true)).unwrap();
        assert!(plan.admitted);
        assert!(!plan.published);
        assert!(!plan.scheduler_active);
        assert_eq!(plan.pair_state, "preparing");
        assert_eq!(plan.policy_version, POLICY_VERSION);
        assert_eq!(plan.verified_baseline.as_ref().unwrap().git_sha, base);
        assert_eq!(plan.svn_source_revision, Some(2));
        assert_eq!(plan.proposed_svn_copy_source_revision, Some(2));
        assert_eq!(plan.pending_git.count, 2);
        assert!(plan
            .pending_git
            .summary
            .iter()
            .any(|s| s.contains("step one")));
        assert!(plan.git_tip.as_ref().unwrap() != &base);
    }

    #[test]
    fn refuses_unrelated_git_first_orphan() {
        let tmp = TempDir::new().unwrap();
        let (_bare, work, base) = init_repo(tmp.path());
        git_env(&work, &["checkout", "--orphan", "feature"]);
        git_env(&work, &["rm", "-rf", "."]);
        fs::write(work.join("other.txt"), "git first\n").unwrap();
        git_env(&work, &["add", "other.txt"]);
        git_env(&work, &["commit", "-m", "Unrelated root"]);
        git_env(&work, &["push", "-u", "origin", "feature"]);
        let mappings = vec![VerifiedMapping {
            git_sha: base,
            svn_revision: 2,
            evidence: "applied_sync_record".into(),
        }];
        let err =
            evaluate_admission(&mappings, Some(&work), None, &request(false, true)).unwrap_err();
        assert_eq!(err.reason, "unrelated_git_first");
    }

    #[test]
    fn refuses_unsafe_skip_import_on_admitted_history() {
        let tmp = TempDir::new().unwrap();
        let (_bare, work, base) = init_repo(tmp.path());
        git_env(&work, &["checkout", "-b", "feature"]);
        fs::write(work.join("feature.txt"), "one\n").unwrap();
        git_env(&work, &["add", "feature.txt"]);
        git_env(&work, &["commit", "-m", "Feature"]);
        git_env(&work, &["push", "-u", "origin", "feature"]);
        let mappings = vec![VerifiedMapping {
            git_sha: base,
            svn_revision: 2,
            evidence: "snapshot_import_confirmed".into(),
        }];
        let err =
            evaluate_admission(&mappings, Some(&work), None, &request(true, true)).unwrap_err();
        assert_eq!(err.reason, "unsafe_skip_import");
        assert!(!err.plan.as_ref().unwrap().skip_import_applied);
        assert!(!err.plan.as_ref().unwrap().scheduler_active);
    }

    #[test]
    fn snapshot_root_baseline_is_enough_without_full_history() {
        let tmp = TempDir::new().unwrap();
        let (_bare, work, base) = init_repo(tmp.path());
        // The snapshot commit is an orphan-looking root (one commit) that is
        // still a verified mapping. Descendants must be admitted.
        git_env(&work, &["checkout", "-b", "feature"]);
        fs::write(work.join("later.txt"), "after snapshot\n").unwrap();
        git_env(&work, &["add", "later.txt"]);
        git_env(&work, &["commit", "-m", "Work after snapshot"]);
        git_env(&work, &["push", "-u", "origin", "feature"]);
        let mappings = vec![VerifiedMapping {
            git_sha: base.clone(),
            svn_revision: 4,
            evidence: "snapshot_import_confirmed".into(),
        }];
        let plan = evaluate_admission(&mappings, Some(&work), None, &request(false, true)).unwrap();
        assert_eq!(plan.verified_baseline.unwrap().git_sha, base);
        assert_eq!(plan.pending_git.count, 1);
        assert_eq!(plan.svn_source_revision, Some(4));
    }

    #[test]
    fn publish_request_stays_admitted_but_not_published_in_admission_only() {
        let tmp = TempDir::new().unwrap();
        let (_bare, work, base) = init_repo(tmp.path());
        git_env(&work, &["checkout", "-b", "feature"]);
        git_env(&work, &["push", "-u", "origin", "feature"]);
        let mappings = vec![VerifiedMapping {
            git_sha: base,
            svn_revision: 2,
            evidence: "import_confirmed".into(),
        }];
        let plan =
            evaluate_admission(&mappings, Some(&work), None, &request(false, false)).unwrap();
        assert_eq!(plan.mode, "publish_pending");
        assert!(!plan.published);
        assert!(!plan.scheduler_active);
        assert_eq!(plan.pair_state, "preparing");
    }

    #[test]
    fn commit_message_is_not_lineage_proof() {
        let tmp = TempDir::new().unwrap();
        let (_bare, work, base) = init_repo(tmp.path());
        git_env(&work, &["checkout", "--orphan", "feature"]);
        git_env(&work, &["rm", "-rf", "."]);
        fs::write(work.join("forged.txt"), "looks like svn\n").unwrap();
        git_env(&work, &["add", "forged.txt"]);
        git_env(
            &work,
            &[
                "commit",
                "-m",
                "git-svn-id: file:///svn/trunk@2 uuid-does-not-count",
            ],
        );
        git_env(&work, &["push", "-u", "origin", "feature"]);
        let mappings = vec![VerifiedMapping {
            git_sha: base,
            svn_revision: 2,
            evidence: "applied_sync_record".into(),
        }];
        let err =
            evaluate_admission(&mappings, Some(&work), None, &request(false, true)).unwrap_err();
        assert_eq!(err.reason, "unrelated_git_first");
    }

    #[test]
    fn collect_mappings_uses_scoped_applied_records() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let now = Utc::now();
        db.insert_sync_record(&SyncRecord {
            id: "m1".into(),
            repo_id: Some("parent".into()),
            svn_revision: Some(2),
            git_hash: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
            direction: SyncDirection::SvnToGit,
            author: "svn".into(),
            message: "import".into(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        })
        .unwrap();
        db.insert_sync_record(&SyncRecord {
            id: "other".into(),
            repo_id: Some("other".into()),
            svn_revision: Some(9),
            git_hash: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
            direction: SyncDirection::SvnToGit,
            author: "svn".into(),
            message: "other pair".into(),
            timestamp: now,
            synced_at: now,
            status: SyncRecordStatus::Applied,
        })
        .unwrap();
        let mappings = collect_verified_mappings(&db, "parent").unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].svn_revision, 2);
        assert_eq!(mappings[0].evidence, "applied_sync_record");
    }
}
