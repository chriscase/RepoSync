//! #67 late-pair admission and preview (no publish / no replay).
//!
//! Prove that a candidate Git tip descends from a verified SVN-import mapping
//! using stored mappings plus actual Git ancestry. Commit messages are not
//! lineage proof. Preview pins tips and revisions; it does not copy SVN
//! branches, write checkpoints, mutate remotes, or activate the scheduler.

use std::path::Path;
use std::process::Output;

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

fn git(workdir: &Path, args: &[&str], http_auth_token: Option<&str>) -> std::io::Result<Output> {
    crate::git::subprocess_auth::git_cli_output(workdir, args, http_auth_token)
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
                if is_full_git_oid(&sha) {
                    let snapshot =
                        op.snapshot_pin.is_some() || op.operation_type == "snapshot_import";
                    if let Some(existing) = mappings
                        .iter()
                        .position(|item| item.git_sha == sha && item.svn_revision == rev)
                    {
                        // The atomic snapshot finalizer also emits the applied
                        // record consumed by ordinary sync. Preserve the richer
                        // history-boundary classification for this identical
                        // completed-import SHA/revision, without admitting any
                        // additional baseline or relabeling ordinary full imports.
                        if snapshot {
                            mappings[existing].evidence = "snapshot_import_confirmed".into();
                        }
                    } else {
                        mappings.push(VerifiedMapping {
                            git_sha: sha,
                            svn_revision: rev,
                            evidence: if snapshot {
                                "snapshot_import_confirmed".into()
                            } else {
                                "import_confirmed".into()
                            },
                        });
                    }
                }
            }
        }
    }

    Ok(mappings)
}

/// Fetch `branch` into the late-pair inspect ref without checkout or reset.
pub fn resolve_candidate_tip(
    workdir: &Path,
    branch: &str,
    http_auth_token: Option<&str>,
) -> Result<String, LatePairRefusal> {
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
        http_auth_token,
    );
    let inspect = git(
        workdir,
        &[
            "rev-parse",
            "--verify",
            &format!("{INSPECT_REF}^{{commit}}"),
        ],
        None,
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
        None,
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
        None,
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
        git(workdir, &["rev-list", "--count", &range], None).map_err(|_| LatePairRefusal {
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
    let summary_out = git(
        workdir,
        &["rev-list", "--reverse", "--format=%s", &range],
        None,
    )
    .map_err(|_| LatePairRefusal {
        reason: "selection_command_failed".into(),
        detail: "pending Git subjects could not be listed".into(),
        plan: None,
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
    git_http_token: Option<&str>,
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
        match resolve_candidate_tip(workdir, &request.git_branch, git_http_token) {
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
    use std::process::Command;
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
        let plan =
            evaluate_admission(&mappings, Some(&work), None, &request(false, true), None).unwrap();
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
        let err = evaluate_admission(&mappings, Some(&work), None, &request(false, true), None)
            .unwrap_err();
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
        let err = evaluate_admission(&mappings, Some(&work), None, &request(true, true), None)
            .unwrap_err();
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
        let plan =
            evaluate_admission(&mappings, Some(&work), None, &request(false, true), None).unwrap();
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
            evaluate_admission(&mappings, Some(&work), None, &request(false, false), None).unwrap();
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
        let err = evaluate_admission(&mappings, Some(&work), None, &request(false, true), None)
            .unwrap_err();
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

/// Regular-file tree reconciliation used by the pinned late-pair planner.
/// This module produces data only: it cannot publish, acknowledge a checkpoint,
/// activate a pair or authorize any #69 refresh operation.
pub mod reconciliation {
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::Write;

    use anyhow::{bail, Context, Result};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};

    pub const POLICY: &str = "late_pair_reconciliation_v2";
    /// Bound captured review data before it can become a durable intent.
    pub const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct File {
        pub mode: u32,
        pub bytes: Vec<u8>,
    }

    pub type Tree = BTreeMap<String, File>;

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Conflict {
        pub path: String,
        pub reason: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct MergedTree {
        pub tree: Tree,
        pub conflicts: Vec<Conflict>,
    }

    pub fn validate_tree(tree: &Tree) -> Result<()> {
        let mut bytes = 0usize;
        for (path, file) in tree {
            crate::svn::operand::validate_relative_path(path)?;
            if path.split('/').any(|part| matches!(part, ".git" | ".svn")) {
                bail!("reserved metadata path in late-pair tree: {path}");
            }
            if !matches!(file.mode, 0o100644 | 0o100755) {
                bail!("unsupported node mode at {path}");
            }
            bytes = bytes
                .checked_add(path.len())
                .and_then(|n| n.checked_add(file.bytes.len()))
                .context("late-pair capture size overflow")?;
            if bytes > MAX_CAPTURE_BYTES {
                bail!("late-pair captured tree exceeds the 16 MiB review bound");
            }
            let mut prefix = path.as_str();
            while let Some((parent, _)) = prefix.rsplit_once('/') {
                if tree.contains_key(parent) {
                    bail!("file/directory collision in late-pair tree: {parent}");
                }
                prefix = parent;
            }
        }
        Ok(())
    }

    /// Canonical ordering includes exact path, mode, length and content hash.
    pub fn tree_digest(tree: &Tree) -> Result<String> {
        validate_tree(tree)?;
        let entries = tree
            .iter()
            .map(|(path, file)| {
                (
                    path,
                    file.mode,
                    file.bytes.len(),
                    hex::encode(Sha256::digest(&file.bytes)),
                )
            })
            .collect::<Vec<_>>();
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(&(
            POLICY, entries,
        ))?)))
    }

    /// Read immutable Git objects, never the mutable checkout or index.
    pub fn capture_git_tree(workdir: &std::path::Path, sha: &str) -> Result<Tree> {
        if !crate::history_inspect::is_full_git_oid(sha) {
            bail!("late-pair Git capture requires a full commit OID");
        }
        let repository = git2::Repository::open(workdir)?;
        let commit = repository.find_commit(git2::Oid::from_str(sha)?)?;
        fn walk(
            repository: &git2::Repository,
            tree: &git2::Tree<'_>,
            prefix: &str,
            captured: &mut Tree,
            total: &mut usize,
        ) -> Result<()> {
            for entry in tree {
                let name = std::str::from_utf8(entry.name_bytes()).context("non-UTF-8 Git path")?;
                let path = format!("{prefix}{name}");
                match entry.filemode() {
                    0o040000 => walk(
                        repository,
                        &repository.find_tree(entry.id())?,
                        &format!("{path}/"),
                        captured,
                        total,
                    )?,
                    mode @ (0o100644 | 0o100755) => {
                        let blob = repository.find_blob(entry.id())?;
                        *total = total
                            .checked_add(path.len())
                            .and_then(|n| n.checked_add(blob.size()))
                            .context("late-pair Git capture overflow")?;
                        if *total > MAX_CAPTURE_BYTES {
                            bail!("late-pair Git capture exceeds 16 MiB");
                        }
                        captured.insert(
                            path,
                            File {
                                mode: mode as u32,
                                bytes: blob.content().to_vec(),
                            },
                        );
                    }
                    _ => bail!("unsupported Git symlink, submodule or node at {path}"),
                }
            }
            Ok(())
        }
        let mut captured = Tree::new();
        walk(&repository, &commit.tree()?, "", &mut captured, &mut 0)?;
        validate_tree(&captured)?;
        Ok(captured)
    }

    /// Independently inventory an owned export before admitting its bytes.
    pub fn capture_export_tree(root: &std::path::Path) -> Result<Tree> {
        let manifest = crate::import::require_supported_export(root)?;
        let mut captured = Tree::new();
        let mut total = 0usize;
        for (path, entry) in manifest.entries {
            match entry {
                crate::import::ManifestEntry::File { mode, size, sha256 } => {
                    let size = usize::try_from(size).context("late-pair file size overflow")?;
                    total = total
                        .checked_add(path.len())
                        .and_then(|n| n.checked_add(size))
                        .context("late-pair export size overflow")?;
                    if total > MAX_CAPTURE_BYTES {
                        bail!("late-pair SVN capture exceeds 16 MiB");
                    }
                    let bytes = std::fs::read(root.join(&path))?;
                    if bytes.len() != size || hex::encode(Sha256::digest(&bytes)) != sha256 {
                        bail!("late-pair export changed during capture at {path}");
                    }
                    captured.insert(
                        path,
                        File {
                            mode: if mode & 0o100 != 0 {
                                0o100755
                            } else {
                                0o100644
                            },
                            bytes,
                        },
                    );
                }
                crate::import::ManifestEntry::Directory { .. }
                | crate::import::ManifestEntry::Reserved { .. } => {}
                crate::import::ManifestEntry::Unsupported { .. } => {
                    bail!("unsupported SVN node at {path}")
                }
            }
        }
        validate_tree(&captured)?;
        Ok(captured)
    }

    /// Capture exactly the requested immutable revision. The shared exporter
    /// checks declared externals/special nodes before any local materialization.
    pub async fn capture_svn_tree(client: &crate::svn::SvnClient, revision: i64) -> Result<Tree> {
        if revision < 1 {
            bail!("late-pair SVN capture requires a positive revision");
        }
        let directory = tempfile::tempdir()?;
        client.export("", revision, directory.path()).await?;
        capture_export_tree(directory.path())
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum StepSource {
        Parent { revision: i64 },
        Git { sha: String, parent: String },
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ReplayObject {
        pub sha: String,
        pub parent: Option<String>,
        pub tree: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct PlannedStep {
        pub source: StepSource,
        pub before_digest: String,
        pub intended_digest: String,
        pub intended_tree: Tree,
        pub message: String,
        pub git_result: ReplayObject,
    }

    /// Private captured plan. HTTP clients receive a summary and pin, not an
    /// executable document or permission to submit replacement captured bytes.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct PreparedPlan {
        pub version: u8,
        pub policy: String,
        pub actor: String,
        pub parent_id: String,
        pub parent_generation: String,
        pub parent_scope_uuid: String,
        pub git_remote_url: String,
        pub projection: String,
        pub git_branch: String,
        pub git_tip: String,
        pub baseline_git_sha: String,
        pub baseline_svn_revision: i64,
        pub svn_root_url: String,
        pub source_url: String,
        pub source_path: String,
        pub svn_uuid: String,
        pub parent_revision: i64,
        pub parent_tree_digest: String,
        pub target_url: String,
        pub target_path: String,
        pub svn_branch: String,
        pub target_revision: Option<i64>,
        pub target_copy_from_revision: Option<i64>,
        pub include_parent: bool,
        pub baseline_tree: Tree,
        pub initial_target_tree: Tree,
        pub initial_target_git: ReplayObject,
        pub pending_git_shas: Vec<String>,
        pub steps: Vec<PlannedStep>,
        pub conflicts: Vec<Conflict>,
        pub intended_tree: Tree,
        pub intended_tree_digest: String,
        pub intended_git_sha: Option<String>,
        pub intended_git_tree: Option<String>,
        pub intended_git_parents: Vec<String>,
        pub review_pin: String,
    }

    impl PreparedPlan {
        pub fn digest(&self) -> Result<String> {
            let mut plan = self.clone();
            plan.review_pin.clear();
            let bytes = serde_json::to_vec(&plan)?;
            if bytes.len() > MAX_CAPTURE_BYTES {
                bail!("complete late-pair review exceeds the 16 MiB durable-record bound");
            }
            Ok(hex::encode(Sha256::digest(bytes)))
        }

        pub fn validate(&self) -> Result<()> {
            if self.version != 2
                || self.policy != POLICY
                || self.actor.is_empty()
                || self.actor.len() > 256
                || self.git_remote_url.is_empty()
                || uuid::Uuid::parse_str(&self.parent_scope_uuid).is_err()
                || self.review_pin != self.digest()?
            {
                bail!("invalid or unsupported pinned late-pair plan");
            }
            let endpoint = reqwest::Url::parse(&self.git_remote_url)?;
            if !matches!(endpoint.scheme(), "https" | "http" | "git" | "file")
                || endpoint.as_str() != self.git_remote_url
                || !endpoint.username().is_empty()
                || endpoint.password().is_some()
                || endpoint.query().is_some()
                || endpoint.fragment().is_some()
                || self.parent_generation.len() != 64
                || !self
                    .parent_generation
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
            {
                bail!("invalid late-pair endpoint or registered generation pin");
            }
            if self.baseline_svn_revision < 1
                || self.svn_uuid.is_empty()
                || !crate::history_inspect::is_full_git_oid(&self.git_tip)
                || !crate::history_inspect::is_full_git_oid(&self.baseline_git_sha)
                || self
                    .intended_git_sha
                    .as_ref()
                    .is_some_and(|s| !crate::history_inspect::is_full_git_oid(s))
                || self.intended_git_sha.is_some() != self.intended_git_tree.is_some()
                || self.target_revision.is_some() != self.target_copy_from_revision.is_some()
            {
                bail!("incomplete late-pair source identity");
            }
            validate_tree(&self.baseline_tree)?;
            validate_tree(&self.initial_target_tree)?;
            let source =
                crate::path_projection::svn_path_identity(&self.svn_root_url, &self.source_url)?;
            let target =
                crate::path_projection::svn_path_identity(&self.svn_root_url, &self.target_url)?;
            if source.branch_path != self.source_path
                || target.branch_path != self.target_path
                || self.source_path == self.target_path
                || self
                    .target_path
                    .starts_with(&format!("{}/", self.source_path))
                || self
                    .source_path
                    .starts_with(&format!("{}/", self.target_path))
                || self
                    .target_path
                    .rsplit_once('/')
                    .is_none_or(|(parent, name)| parent.is_empty() || name.is_empty())
                || self.parent_revision < self.baseline_svn_revision
                || self
                    .target_revision
                    .is_some_and(|rev| rev < self.baseline_svn_revision)
                || self.target_copy_from_revision.is_some_and(|rev| {
                    rev < self.baseline_svn_revision || Some(rev) > self.target_revision
                })
            {
                bail!("unsupported or contradictory SVN source/target topology");
            }
            if self.pending_git_shas.len() > 32
                || self.pending_git_shas.iter().collect::<BTreeSet<_>>().len()
                    != self.pending_git_shas.len()
                || self
                    .pending_git_shas
                    .iter()
                    .any(|s| !crate::history_inspect::is_full_git_oid(s))
                || !crate::history_inspect::is_full_git_oid(&self.initial_target_git.sha)
                || !crate::history_inspect::is_full_git_oid(&self.initial_target_git.tree)
                || self
                    .intended_git_tree
                    .as_ref()
                    .is_some_and(|s| !crate::history_inspect::is_full_git_oid(s))
                || self
                    .intended_git_parents
                    .iter()
                    .any(|s| !crate::history_inspect::is_full_git_oid(s))
            {
                bail!("invalid late-pair Git object inventory");
            }
            if self.initial_target_tree == self.baseline_tree {
                if self.initial_target_git.sha != self.baseline_git_sha {
                    bail!("baseline object changed without a target delta");
                }
            } else if self.initial_target_git.parent.as_deref()
                != Some(self.baseline_git_sha.as_str())
            {
                bail!("target observation does not preserve baseline ancestry");
            }
            if self.intended_tree_digest != tree_digest(&self.intended_tree)? {
                bail!("late-pair intended tree differs from its pin");
            }
            let mut prior = tree_digest(&self.initial_target_tree)?;
            let mut prior_git = &self.initial_target_git.sha;
            let mut source_git = self.baseline_git_sha.as_str();
            let mut source_index = 0usize;
            for step in &self.steps {
                match &step.source {
                    StepSource::Parent { revision }
                        if self.include_parent
                            && source_index == 0
                            && *revision == self.parent_revision
                            && self.steps.first() == Some(step) => {}
                    StepSource::Git { sha, parent }
                        if self.pending_git_shas.get(source_index) == Some(sha)
                            && parent == source_git =>
                    {
                        source_index += 1;
                        source_git = sha;
                    }
                    _ => bail!("unproved or reordered replay source"),
                }
                if step.before_digest != prior
                    || step.intended_digest != tree_digest(&step.intended_tree)?
                {
                    bail!("contradictory late-pair replay tree chain");
                }
                if !crate::history_inspect::is_full_git_oid(&step.git_result.sha)
                    || !crate::history_inspect::is_full_git_oid(&step.git_result.tree)
                    || (step.before_digest != step.intended_digest
                        && step.git_result.parent.as_ref() != Some(prior_git))
                    || (step.before_digest == step.intended_digest
                        && &step.git_result.sha != prior_git)
                {
                    bail!("contradictory late-pair reconciliation object chain");
                }
                prior = step.intended_digest.clone();
                prior_git = &step.git_result.sha;
            }
            if prior != self.intended_tree_digest
                || self.conflicts.is_empty() != self.intended_git_sha.is_some()
            {
                bail!("incomplete late-pair intended-result proof");
            }
            if self.conflicts.is_empty()
                && (source_index != self.pending_git_shas.len() || source_git != self.git_tip)
            {
                bail!("some pending Git work lacks a planned outcome");
            }
            if self
                .intended_git_sha
                .as_deref()
                .is_some_and(|sha| sha != self.git_tip)
                && (self.intended_git_parents.first() != Some(&self.git_tip)
                    || self.intended_git_parents.last() != Some(prior_git))
            {
                bail!("reconciliation publication does not preserve both histories");
            }
            Ok(())
        }
    }

    pub struct PrepareContext<'a> {
        pub db: &'a crate::db::Database,
        pub parent: &'a crate::models::Repository,
        pub git_workdir: &'a std::path::Path,
        pub request: &'a super::LatePairRequest,
        pub admission: &'a super::LatePairPlan,
        pub svn_password: &'a str,
        pub actor: &'a str,
        pub include_parent: bool,
    }

    /// Pin configuration/registration and credential *generations*, without
    /// storing a password/token hash or exposing secret material in the plan.
    /// The existing #64 producer's content-only digest. The v2 plan retains
    /// the separate byte-and-mode digest; neither substitutes for the other.
    pub(crate) fn producer_tree_digest(tree: &Tree) -> String {
        let mut digest = Sha256::new();
        for (path, file) in tree {
            digest.update(path.as_bytes());
            digest.update([0]);
            digest.update(hex::encode(Sha256::digest(&file.bytes)).as_bytes());
            digest.update(b"\n");
        }
        hex::encode(digest.finalize())
    }

    pub(crate) fn producer_changed_paths(
        before: &Tree,
        after: &Tree,
    ) -> Vec<crate::db::svn_commit_operations::IntendedPath> {
        let changes = before
            .keys()
            .chain(after.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|path| before.get(*path) != after.get(*path))
            .map(|path| {
                let action = match (before.get(path), after.get(path)) {
                    (None, Some(_)) => "A",
                    (Some(_), None) => "D",
                    _ => "M",
                };
                (
                    action.to_string(),
                    path.clone(),
                    after.get(path).map(|file| file.bytes.clone()),
                )
            })
            .collect::<Vec<_>>();
        crate::svn_commit::intended_paths_from_contents(&changes)
    }

    fn canonical_remote(value: &str) -> Result<String> {
        if let Ok(url) = reqwest::Url::parse(value) {
            if url.scheme() == "file" {
                let path = url
                    .to_file_path()
                    .map_err(|_| anyhow::anyhow!("invalid local Git endpoint"))?;
                return reqwest::Url::from_file_path(std::fs::canonicalize(path)?)
                    .map(|u| u.to_string())
                    .map_err(|_| anyhow::anyhow!("invalid canonical Git endpoint"));
            }
            if !matches!(url.scheme(), "http" | "https" | "git")
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                bail!("unsupported or credential-bearing Git endpoint");
            }
            return Ok(url.to_string());
        }
        let path = std::path::Path::new(value);
        if !path.is_absolute() {
            bail!("Git endpoint must be an explicit URL or absolute local fixture path");
        }
        reqwest::Url::from_file_path(std::fs::canonicalize(path)?)
            .map(|u| u.to_string())
            .map_err(|_| anyhow::anyhow!("invalid local Git endpoint"))
    }

    pub(crate) fn verify_git_remote(
        workdir: &std::path::Path,
        parent: &crate::models::Repository,
    ) -> Result<String> {
        let registered = canonical_remote(&crate::git::remote_url::derive_git_remote_url(
            &parent.git_api_url,
            None,
            &parent.git_repo,
        ))?;
        let repository = git2::Repository::open(workdir)?;
        let config = repository.config()?;
        let mut entries = config.entries(None)?;
        let mut origins = 0;
        let mut pushes = 0;
        while let Some(entry) = entries.next() {
            let entry = entry?;
            let name = entry
                .name()
                .context("non-UTF8 Git configuration")?
                .to_ascii_lowercase();
            if name.starts_with("url.")
                && (name.ends_with(".insteadof") || name.ends_with(".pushinsteadof"))
            {
                bail!("Git endpoint rewriting is not admitted for reviewed late pairing");
            }
            if name == "remote.origin.url" || name == "remote.origin.pushurl" {
                ensure_remote_value(entry.value().context("non-UTF8 Git endpoint")?, &registered)?;
                if name == "remote.origin.url" {
                    origins += 1;
                } else {
                    pushes += 1;
                }
            }
        }
        if origins != 1 || pushes > 1 {
            bail!("ambiguous or missing Git origin endpoint");
        }
        for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_CONFIG_PARAMETERS"] {
            if std::env::var_os(name).is_some() {
                bail!("ambient Git routing overrides are not admitted");
            }
        }
        if std::env::var("GIT_CONFIG_COUNT").is_ok_and(|value| value != "0") {
            bail!("ambient Git configuration injection is not admitted");
        }
        Ok(registered)
    }

    pub(crate) fn require_registered_parent(
        db: &crate::db::Database,
        parent: &crate::models::Repository,
    ) -> Result<()> {
        let current = db
            .get_repository(&parent.id)?
            .context("registered parent disappeared")?;
        if serde_json::to_value(&current)? != serde_json::to_value(parent)? {
            bail!("parent input differs from the current registered source");
        }
        Ok(())
    }

    fn ensure_remote_value(value: &str, registered: &str) -> Result<()> {
        if canonical_remote(value)? != registered {
            bail!("Git workdir origin differs from registered repository");
        }
        Ok(())
    }

    fn observed_remote_tip(
        workdir: &std::path::Path,
        branch: &str,
        token: Option<&str>,
    ) -> Result<String> {
        let reference = format!("refs/heads/{branch}");
        if !git2::Reference::is_valid_name(&reference) {
            bail!("invalid reviewed Git branch");
        }
        let output = super::git(
            workdir,
            &[
                "ls-remote",
                "--exit-code",
                "--refs",
                "--",
                "origin",
                &reference,
            ],
            token,
        )?;
        if !output.status.success() {
            bail!("fresh Git remote tip is unavailable");
        }
        let text = std::str::from_utf8(&output.stdout)?;
        let mut rows = text.lines();
        let (sha, returned_ref) = rows
            .next()
            .and_then(|line| line.split_once('\t'))
            .context("fresh Git remote tip is missing or malformed")?;
        if rows.next().is_some() || returned_ref != reference || !super::is_full_git_oid(sha) {
            bail!("fresh Git remote tip is ambiguous or malformed");
        }
        Ok(sha.to_owned())
    }

    pub fn parent_generation(db: &crate::db::Database, parent_id: &str) -> Result<String> {
        parent_generation_in_connection(&db.conn(), parent_id)
    }

    /// The same observation can be compared under the claim's IMMEDIATE
    /// transaction, without reopening a connection or dropping the writer lock.
    pub fn parent_generation_in_connection(
        conn: &rusqlite::Connection,
        parent_id: &str,
    ) -> Result<String> {
        use rusqlite::OptionalExtension;
        let mut lineage = Vec::new();
        let mut owners = Vec::new();
        let mut seen = BTreeSet::new();
        let mut next = Some(parent_id.to_string());
        while let Some(id) = next {
            if !seen.insert(id.clone()) || lineage.len() >= 4 {
                bail!("ambiguous late-pair parent registration lineage");
            }
            let observation = conn
                .query_row("SELECT * FROM repositories WHERE id=?1", [&id], |row| {
                    let mut values = Vec::new();
                    for i in 0..row.as_ref().column_count() {
                        let value = match row.get_ref(i)? {
                            rusqlite::types::ValueRef::Null => serde_json::json!(["null"]),
                            rusqlite::types::ValueRef::Integer(n) => {
                                serde_json::json!(["integer", n])
                            }
                            rusqlite::types::ValueRef::Real(n) => {
                                serde_json::json!(["real", n.to_bits().to_string()])
                            }
                            rusqlite::types::ValueRef::Text(b) => {
                                serde_json::json!(["text", hex::encode(b)])
                            }
                            rusqlite::types::ValueRef::Blob(b) => {
                                serde_json::json!(["blob", hex::encode(b)])
                            }
                        };
                        values.push((row.as_ref().column_name(i)?.to_string(), value));
                    }
                    Ok((row.get::<_, Option<String>>("parent_id")?, values))
                })
                .optional()?
                .context("late-pair parent registration missing")?;
            next = observation.0;
            lineage.push(observation.1);
            owners.push(id);
        }
        let mut generations = Vec::new();
        for prefix in ["secret_svn_password", "secret_git_token"] {
            let keys = owners
                .iter()
                .map(|id| format!("{prefix}_{id}"))
                .chain(std::iter::once(prefix.to_string()));
            for key in keys {
                let generation: Option<(String, bool)> = conn
                    .query_row(
                        "SELECT updated_at,value='' FROM kv_state WHERE key=?1",
                        [&key],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                generations.push((key, generation));
            }
        }
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(&(
            POLICY,
            lineage,
            generations,
        ))?)))
    }

    fn result_git_commit(
        workdir: &std::path::Path,
        tip: &str,
        target: &Tree,
        message: &str,
        extra_parent: Option<&str>,
    ) -> Result<String> {
        let repository = git2::Repository::open(workdir)?;
        let parent = repository.find_commit(git2::Oid::from_str(tip)?)?;
        let retained = match extra_parent {
            None => true,
            Some(sha) if sha == tip => true,
            Some(sha) => repository.graph_descendant_of(parent.id(), git2::Oid::from_str(sha)?)?,
        };
        if retained && &capture_git_tree(workdir, tip)? == target {
            return Ok(tip.to_string());
        }
        let mut index = git2::Index::new()?;
        for (path, file) in target {
            let oid = repository.blob(&file.bytes)?;
            index.add(&git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: file.mode,
                uid: 0,
                gid: 0,
                file_size: u32::try_from(file.bytes.len())?,
                id: oid,
                flags: 0,
                flags_extended: 0,
                path: path.as_bytes().to_vec(),
            })?;
        }
        let tree = repository.find_tree(index.write_tree_to(&repository)?)?;
        let timestamp = parent
            .committer()
            .when()
            .seconds()
            .checked_add(1)
            .context("Git timestamp overflow")?;
        let signature = git2::Signature::new(
            "RepoSync",
            "reposync@localhost",
            &git2::Time::new(timestamp, 0),
        )?;
        // No ref, index or worktree update: publication still needs a fresh pin
        // and an exclusive durable intent. The new commit preserves the old tip.
        let extra = extra_parent
            .filter(|s| *s != tip)
            .map(|sha| repository.find_commit(git2::Oid::from_str(sha)?))
            .transpose()?;
        let mut parents = vec![&parent];
        if let Some(extra) = extra.as_ref() {
            parents.push(extra);
        }
        Ok(repository
            .commit(None, &signature, &signature, message, &tree, &parents)?
            .to_string())
    }

    fn replay_object(workdir: &std::path::Path, sha: &str) -> Result<ReplayObject> {
        let repository = git2::Repository::open(workdir)?;
        let commit = repository.find_commit(git2::Oid::from_str(sha)?)?;
        Ok(ReplayObject {
            sha: sha.into(),
            parent: if commit.parent_count() > 0 {
                Some(commit.parent_id(0)?.to_string())
            } else {
                None
            },
            tree: commit.tree_id().to_string(),
        })
    }

    fn plan_replay_object(
        workdir: &std::path::Path,
        previous: &ReplayObject,
        intended: &Tree,
        message: &str,
        original: Option<&str>,
    ) -> Result<ReplayObject> {
        if let Some(sha) = original {
            let object = replay_object(workdir, sha)?;
            if object.parent.as_deref() == Some(&previous.sha)
                && capture_git_tree(workdir, sha)? == *intended
            {
                return Ok(object);
            }
        }
        let sha = result_git_commit(workdir, &previous.sha, intended, message, None)?;
        replay_object(workdir, &sha)
    }

    fn missing_target(error: &crate::errors::SvnError) -> bool {
        matches!(error,crate::errors::SvnError::CommandFailed {stderr,..}
            if stderr.contains("E160013") || (stderr.contains("W170000")
                && stderr.contains("non-existent in revision") && stderr.contains("E200009")))
    }

    /// Fresh, read-only remote observations plus private Git object planning.
    /// No remote copy/push, registration, journal or checkpoint is written.
    pub async fn prepare(context: PrepareContext<'_>) -> Result<PreparedPlan> {
        let PrepareContext {
            db,
            parent,
            git_workdir,
            request,
            admission,
            svn_password,
            actor,
            include_parent,
        } = context;
        if actor.is_empty()
            || actor.len() > 256
            || parent.svn_username.len() > 512
            || !admission.admitted
            || parent.id != request.parent_id
        {
            bail!("late-pair preview lacks an authenticated admitted source");
        }
        require_registered_parent(db, parent)?;
        let generation = parent_generation(db, &parent.id)?;
        let parent_scope_uuid =
            crate::db::repo_scope_identity::repository_scope_uuid(&db.conn(), &parent.id)?;
        let git_remote_url = verify_git_remote(git_workdir, parent)?;
        let baseline = admission
            .verified_baseline
            .as_ref()
            .context("missing late-pair baseline")?;
        let tip = admission
            .git_tip
            .as_deref()
            .context("missing late-pair Git tip")?;
        let token = db.resolve_credential_chain(&parent.id, "secret_git_token");
        // A rejected non-fast-forward fetch can leave the legacy inspect ref
        // unchanged. It is never fresh remote authority for a reviewed v2 plan.
        if observed_remote_tip(git_workdir, &request.git_branch, token.as_deref())? != tip {
            bail!("Git tip differs from fresh remote observation; review again");
        }
        let mappings = super::collect_verified_mappings(db, &parent.id)?;
        if !mappings
            .iter()
            .any(|m| m.git_sha == baseline.git_sha && m.svn_revision == baseline.svn_revision)
        {
            bail!("baseline no longer has scoped producer evidence");
        }
        let mut mapped = BTreeMap::new();
        for mapping in &mappings {
            if mapped
                .insert(&mapping.git_sha, mapping.svn_revision)
                .is_some_and(|r| r != mapping.svn_revision)
            {
                bail!("ambiguous SVN revision for a mapped Git baseline");
            }
        }
        let source_url =
            crate::svn::operand::configured_branch_url(&parent.svn_url, &parent.svn_branch)?;
        let target_url =
            crate::svn::operand::configured_branch_url(&parent.svn_url, &request.svn_branch)?;
        let source = crate::svn::SvnClient::new(&source_url, &parent.svn_username, svn_password);
        let target = crate::svn::SvnClient::new(&target_url, &parent.svn_username, svn_password);
        let info = source.info().await?;
        let identity = crate::path_projection::svn_path_identity(&info.root_url, &info.url)?;
        let target_identity =
            crate::path_projection::svn_path_identity(&info.root_url, &target_url)?;
        if let Some(operation) = db.latest_import_operation(&parent.id)? {
            if operation.target_fingerprint
                != crate::db::import_operations::import_target_fingerprint(parent, git_workdir)
            {
                bail!("parent source or policy differs from the confirmed import");
            }
            if let Some(pin) = operation.snapshot_pin {
                if pin.svn_uuid != info.uuid
                    || pin.canonical_url.trim_end_matches('/') != info.url.trim_end_matches('/')
                {
                    bail!("SVN identity differs from the authenticated snapshot baseline");
                }
            }
        }
        let baseline_tree = capture_svn_tree(&source, baseline.svn_revision).await?;
        if capture_git_tree(git_workdir, &baseline.git_sha)? != baseline_tree {
            bail!("verified Git/SVN baseline bytes or modes differ");
        }
        let parent_tree = capture_svn_tree(&source, info.latest_rev).await?;
        let target_info = match target.info().await {
            Ok(target_info) => {
                if target_info.uuid != info.uuid || target_info.root_url != info.root_url {
                    bail!("foreign SVN target identity");
                }
                Some(target_info)
            }
            Err(error) if missing_target(&error) => None,
            Err(error) => return Err(error).context("SVN target identity unavailable"),
        };
        let root = crate::svn::SvnClient::new(&info.root_url, &parent.svn_username, svn_password);
        let history = root.log(baseline.svn_revision, info.latest_rev).await?;
        if history.len() > 1024 {
            bail!("SVN lineage observation exceeds the 1024-entry review bound");
        }
        let mut copy_from = None;
        let mut copy_event = 0;
        for entry in &history {
            for path in &entry.changed_paths {
                let name = path.path.trim_start_matches('/');
                if entry.revision > baseline.svn_revision
                    && name == identity.branch_path
                    && path.action != "M"
                {
                    bail!("SVN source branch was replaced after the verified baseline");
                }
                if name == target_identity.branch_path
                    && matches!(path.action.as_str(), "A" | "R")
                    && entry.revision > copy_event
                {
                    copy_event = entry.revision;
                    copy_from = Some((path.copy_from_path.clone(), path.copy_from_rev));
                }
            }
        }
        let target_copy_from_revision = if target_info.is_some() {
            match copy_from {
                Some((Some(path), Some(revision)))
                    if path.trim_start_matches('/') == identity.branch_path
                        && revision >= baseline.svn_revision =>
                {
                    Some(revision)
                }
                _ => bail!("existing target lacks proven copy ancestry from the verified baseline"),
            }
        } else {
            None
        };
        let initial = match &target_info {
            Some(i) => capture_svn_tree(&target, i.latest_rev).await?,
            None => baseline_tree.clone(),
        };
        let allowed: Vec<String> = parent
            .allowed_paths
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        let blocked: Vec<String> = parent
            .blocked_patterns
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        let projection =
            serde_json::json!({"allowed_paths":allowed,"blocked_patterns":blocked}).to_string();
        let check_scope = |before: &Tree, after: &Tree| -> Result<()> {
            for path in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
                if before.get(path) != after.get(path)
                    && !crate::path_projection::path_is_projected(path, &allowed, &blocked)
                {
                    bail!("out_of_scope_path: {path}");
                }
            }
            Ok(())
        };
        check_scope(&baseline_tree, &initial)?;
        let repository = git2::Repository::open(git_workdir)?;
        let range = format!("{}..{tip}", baseline.git_sha);
        let output = super::git(git_workdir, &["rev-list", "--reverse", &range], None)?;
        if !output.status.success() {
            bail!("Git pending history unavailable");
        }
        let pending = std::str::from_utf8(&output.stdout)?
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if pending.len() > 32 {
            bail!("late-pair preview exceeds the 32-commit replay bound");
        }
        let target_git = result_git_commit(
            git_workdir,
            &baseline.git_sha,
            &initial,
            &format!(
                "Capture verified SVN target {} at r{}\n",
                target_url,
                target_info
                    .as_ref()
                    .map(|i| i.latest_rev)
                    .unwrap_or(baseline.svn_revision)
            ),
            None,
        )?;
        let initial_target_git = replay_object(git_workdir, &target_git)?;
        let mut replay_head = initial_target_git.clone();
        let mut steps = Vec::new();
        let mut conflicts = Vec::new();
        let mut current = initial.clone();
        if include_parent {
            check_scope(&baseline_tree, &parent_tree)?;
            let merged = merge_trees(&baseline_tree, &current, &parent_tree)?;
            conflicts = merged.conflicts;
            if conflicts.is_empty() && merged.tree != current {
                let message = format!(
                    "Reconcile explicitly selected parent SVN r{}",
                    info.latest_rev
                );
                let git_result =
                    plan_replay_object(git_workdir, &replay_head, &merged.tree, &message, None)?;
                replay_head = git_result.clone();
                steps.push(PlannedStep {
                    source: StepSource::Parent {
                        revision: info.latest_rev,
                    },
                    before_digest: tree_digest(&current)?,
                    intended_digest: tree_digest(&merged.tree)?,
                    intended_tree: merged.tree.clone(),
                    message,
                    git_result,
                });
                current = merged.tree;
            }
        }
        let mut previous = baseline.git_sha.clone();
        let mut before = baseline_tree.clone();
        for sha in &pending {
            let commit = repository.find_commit(git2::Oid::from_str(sha)?)?;
            if commit.parent_count() != 1 || commit.parent_id(0)?.to_string() != previous {
                bail!("ambiguous or nonlinear pending Git history");
            }
            let after = capture_git_tree(git_workdir, sha)?;
            check_scope(&before, &after)?;
            for (path, file) in &after {
                if before.get(path) != Some(file) && crate::lfs::is_lfs_pointer(&file.bytes) {
                    bail!("LFS or pointer-shaped Git change at {path} needs verified object resolution before late-pair reconciliation");
                }
            }
            if conflicts.is_empty() {
                let merged = merge_trees(&before, &current, &after)?;
                conflicts = merged.conflicts;
                if conflicts.is_empty() {
                    let message = std::str::from_utf8(commit.message_bytes())?.to_string();
                    let git_result = plan_replay_object(
                        git_workdir,
                        &replay_head,
                        &merged.tree,
                        &message,
                        Some(sha),
                    )?;
                    replay_head = git_result.clone();
                    steps.push(PlannedStep {
                        source: StepSource::Git {
                            sha: sha.clone(),
                            parent: previous.clone(),
                        },
                        before_digest: tree_digest(&current)?,
                        intended_digest: tree_digest(&merged.tree)?,
                        intended_tree: merged.tree.clone(),
                        message,
                        git_result,
                    });
                    current = merged.tree;
                }
            }
            previous = sha.clone();
            before = after;
        }
        let result = if conflicts.is_empty() {
            Some(result_git_commit(
                git_workdir,
                tip,
                &current,
                &format!(
                    "Reconcile late pairing with SVN {} at r{}\n\nBaseline r{}; policy {}\n",
                    target_url,
                    target_info
                        .as_ref()
                        .map(|i| i.latest_rev)
                        .unwrap_or(baseline.svn_revision),
                    baseline.svn_revision,
                    POLICY
                ),
                Some(&replay_head.sha),
            )?)
        } else {
            None
        };
        let intended_git_parents = if let Some(sha) = result.as_ref() {
            repository
                .find_commit(git2::Oid::from_str(sha)?)?
                .parent_ids()
                .map(|id| id.to_string())
                .collect()
        } else {
            Vec::new()
        };
        let intended_git_tree = result
            .as_ref()
            .map(|sha| {
                repository
                    .find_commit(git2::Oid::from_str(sha)?)
                    .map(|c| c.tree_id().to_string())
            })
            .transpose()?;
        let mut plan = PreparedPlan {
            version: 2,
            policy: POLICY.into(),
            actor: actor.into(),
            parent_id: parent.id.clone(),
            parent_generation: generation.clone(),
            parent_scope_uuid,
            git_remote_url,
            projection,
            git_branch: request.git_branch.clone(),
            git_tip: tip.into(),
            baseline_git_sha: baseline.git_sha.clone(),
            baseline_svn_revision: baseline.svn_revision,
            svn_root_url: info.root_url.clone(),
            source_url,
            source_path: identity.branch_path,
            svn_uuid: info.uuid,
            parent_revision: info.latest_rev,
            parent_tree_digest: tree_digest(&parent_tree)?,
            target_url,
            target_path: target_identity.branch_path,
            svn_branch: request.svn_branch.clone(),
            target_revision: target_info.as_ref().map(|i| i.latest_rev),
            target_copy_from_revision,
            include_parent,
            baseline_tree,
            initial_target_tree: initial,
            initial_target_git,
            pending_git_shas: pending,
            steps,
            conflicts,
            intended_tree_digest: tree_digest(&current)?,
            intended_tree: current,
            intended_git_sha: result,
            intended_git_tree,
            intended_git_parents,
            review_pin: String::new(),
        };
        if parent_generation(db, &parent.id)? != generation {
            bail!("parent or credential generation changed during preview");
        }
        let fresh_source = source.info().await?;
        if fresh_source.uuid != plan.svn_uuid
            || fresh_source.url != info.url
            || fresh_source.latest_rev != plan.parent_revision
        {
            bail!("SVN source changed during preview");
        }
        match (plan.target_revision, target.info().await) {
            (Some(revision), Ok(fresh))
                if fresh.uuid == plan.svn_uuid && fresh.latest_rev == revision => {}
            (None, Err(error)) if missing_target(&error) => {}
            _ => bail!("SVN target changed or became unavailable during preview"),
        }
        let observed =
            super::resolve_candidate_tip(git_workdir, &request.git_branch, token.as_deref())
                .map_err(|e| anyhow::anyhow!(e.error_message()))?;
        if observed != tip
            || observed_remote_tip(git_workdir, &request.git_branch, token.as_deref())? != tip
        {
            bail!("Git tip changed during preview");
        }
        plan.review_pin = plan.digest()?;
        plan.validate()?;
        Ok(plan)
    }

    fn merge_text(ours: &[u8], base: &[u8], theirs: &[u8]) -> Result<Option<Vec<u8>>> {
        let directory = tempfile::tempdir()?;
        for (name, bytes) in [("ours", ours), ("base", base), ("theirs", theirs)] {
            std::fs::File::create(directory.path().join(name))?.write_all(bytes)?;
        }
        let output = std::process::Command::new("git")
            .args([
                "merge-file",
                "--stdout",
                "--diff3",
                "--",
                "ours",
                "base",
                "theirs",
            ])
            .current_dir(directory.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .context("late-pair text merge could not run")?;
        match output.status.code() {
            Some(0) => Ok(Some(output.stdout)),
            Some(count) if (1..128).contains(&count) => Ok(None),
            _ => bail!(
                "late-pair text merge failed: {}",
                crate::errors::redact_vcs_error_detail(&String::from_utf8_lossy(&output.stderr))
            ),
        }
    }

    /// Merge the next Git delta onto the independently captured SVN tree.
    /// Deletion, mode, binary and text conflicts are explicit, never overwrites.
    pub fn merge_trees(base: &Tree, svn: &Tree, git: &Tree) -> Result<MergedTree> {
        for tree in [base, svn, git] {
            validate_tree(tree)?;
        }
        let paths = base
            .keys()
            .chain(svn.keys())
            .chain(git.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut result = svn.clone();
        let mut conflicts = Vec::new();
        for path in paths {
            let (before, left, right) = (base.get(&path), svn.get(&path), git.get(&path));
            let chosen = if left == right || right == before {
                left.cloned()
            } else if left == before {
                right.cloned()
            } else {
                let reason = match (before, left, right) {
                    (Some(b), Some(l), Some(r)) => {
                        let mode = if l.mode == r.mode || r.mode == b.mode {
                            Some(l.mode)
                        } else if l.mode == b.mode {
                            Some(r.mode)
                        } else {
                            None
                        };
                        let content = if l.bytes == r.bytes || r.bytes == b.bytes {
                            Some(l.bytes.clone())
                        } else if l.bytes == b.bytes {
                            Some(r.bytes.clone())
                        } else if [&b.bytes, &l.bytes, &r.bytes]
                            .iter()
                            .all(|bytes| !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok())
                        {
                            merge_text(&l.bytes, &b.bytes, &r.bytes)?
                        } else {
                            None
                        };
                        if let (Some(mode), Some(bytes)) = (mode, content) {
                            result.insert(path.clone(), File { mode, bytes });
                            continue;
                        }
                        "overlapping text, binary or mode changes"
                    }
                    (None, Some(_), Some(_)) => "different additions at the same path",
                    _ => "deletion conflicts with an independent change",
                };
                conflicts.push(Conflict {
                    path,
                    reason: reason.into(),
                });
                continue;
            };
            match chosen {
                Some(file) => {
                    result.insert(path, file);
                }
                None => {
                    result.remove(&path);
                }
            }
        }
        // A cross-side file/directory collision is a refusal too.
        validate_tree(&result)?;
        Ok(MergedTree {
            tree: result,
            conflicts,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn tree(entries: &[(&str, &[u8])]) -> Tree {
            entries
                .iter()
                .map(|(name, bytes)| {
                    (
                        (*name).into(),
                        File {
                            mode: 0o100644,
                            bytes: bytes.to_vec(),
                        },
                    )
                })
                .collect()
        }

        #[test]
        fn rs67_merge_retains_both_independent_path_changes_and_deletion() {
            let base = tree(&[("old.txt", b"base\n"), ("keep.txt", b"keep\n")]);
            let svn = tree(&[
                ("old.txt", b"base\n"),
                ("keep.txt", b"keep\n"),
                ("svn.txt", b"SVN\n"),
            ]);
            let git = tree(&[("keep.txt", b"keep\n"), ("git.txt", b"Git\n")]);
            let result = merge_trees(&base, &svn, &git).unwrap();
            assert!(result.conflicts.is_empty());
            assert_eq!(
                result.tree,
                tree(&[
                    ("keep.txt", b"keep\n"),
                    ("svn.txt", b"SVN\n"),
                    ("git.txt", b"Git\n")
                ])
            );
        }

        #[test]
        fn rs67_merge_retains_nonoverlapping_text_hunks() {
            let base = tree(&[("both.txt", b"one\ntwo\nthree\nfour\nfive\nsix\nseven\n")]);
            let svn = tree(&[("both.txt", b"SVN\ntwo\nthree\nfour\nfive\nsix\nseven\n")]);
            let git = tree(&[("both.txt", b"one\ntwo\nthree\nfour\nfive\nsix\nGit\n")]);
            let result = merge_trees(&base, &svn, &git).unwrap();
            assert!(result.conflicts.is_empty());
            assert_eq!(
                result.tree["both.txt"].bytes,
                b"SVN\ntwo\nthree\nfour\nfive\nsix\nGit\n"
            );
        }

        #[test]
        fn rs67_overlap_binary_and_delete_conflicts_never_replace_svn_bytes() {
            for (base, svn, git) in [
                (
                    tree(&[("a", b"base\n")]),
                    tree(&[("a", b"SVN\n")]),
                    tree(&[("a", b"Git\n")]),
                ),
                (
                    tree(&[("a", b"\0base")]),
                    tree(&[("a", b"\0SVN")]),
                    tree(&[("a", b"\0Git")]),
                ),
                (tree(&[("a", b"base")]), tree(&[("a", b"SVN")]), Tree::new()),
            ] {
                let result = merge_trees(&base, &svn, &git).unwrap();
                assert_eq!(result.conflicts.len(), 1);
                assert_eq!(result.tree, svn);
            }
        }

        #[test]
        fn rs67_modes_paths_and_content_are_bound_by_the_tree_digest() {
            let base = tree(&[("file", b"data")]);
            let digest = tree_digest(&base).unwrap();
            let mut executable = base.clone();
            executable.get_mut("file").unwrap().mode = 0o100755;
            assert_ne!(tree_digest(&executable).unwrap(), digest);
            assert_ne!(tree_digest(&tree(&[("file", b"DATA")])).unwrap(), digest);
            assert_ne!(tree_digest(&tree(&[("renamed", b"data")])).unwrap(), digest);
            assert!(validate_tree(&tree(&[(".git/config", b"bad")])).is_err());
            assert!(validate_tree(&tree(&[("../outside", b"bad")])).is_err());
            assert!(validate_tree(&tree(&[("a", b"file"), ("a/b", b"child")])).is_err());
        }

        #[test]
        fn rs67_git_capture_uses_pinned_objects_and_refuses_unsupported_nodes() {
            let temporary = tempfile::tempdir().unwrap();
            let repository = git2::Repository::init(temporary.path()).unwrap();
            let blob = repository.blob(b"pinned bytes\n").unwrap();
            let mut builder = repository.treebuilder(None).unwrap();
            builder.insert(".ordinary", blob, 0o100644).unwrap();
            builder.insert("script", blob, 0o100755).unwrap();
            let tree = repository.find_tree(builder.write().unwrap()).unwrap();
            let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
            let sha = repository
                .commit(None, &signature, &signature, "pinned", &tree, &[])
                .unwrap();
            std::fs::write(temporary.path().join("script"), b"dirty worktree\n").unwrap();
            let captured = capture_git_tree(temporary.path(), &sha.to_string()).unwrap();
            assert_eq!(captured["script"].bytes, b"pinned bytes\n");
            assert_eq!(captured["script"].mode, 0o100755);
            assert_eq!(captured[".ordinary"].mode, 0o100644);
            builder.insert("link", blob, 0o120000).unwrap();
            let tree = repository.find_tree(builder.write().unwrap()).unwrap();
            let unsupported = repository
                .commit(None, &signature, &signature, "unsupported", &tree, &[])
                .unwrap();
            assert!(capture_git_tree(temporary.path(), &unsupported.to_string()).is_err());
            assert!(capture_git_tree(temporary.path(), "HEAD").is_err());
        }

        #[cfg(unix)]
        #[test]
        fn rs67_export_capture_preserves_dotfiles_and_refuses_aliases() {
            use std::os::unix::fs::PermissionsExt;
            let temporary = tempfile::tempdir().unwrap();
            std::fs::write(temporary.path().join(".ordinary"), b"bytes\n").unwrap();
            std::fs::write(temporary.path().join("script"), b"script\n").unwrap();
            std::fs::set_permissions(
                temporary.path().join("script"),
                std::fs::Permissions::from_mode(0o654),
            )
            .unwrap();
            let captured = capture_export_tree(temporary.path()).unwrap();
            assert_eq!(captured[".ordinary"].bytes, b"bytes\n");
            assert_eq!(
                captured["script"].mode, 0o100644,
                "Git canonical mode uses owner execute only"
            );
            std::fs::hard_link(
                temporary.path().join("script"),
                temporary.path().join("alias"),
            )
            .unwrap();
            assert!(capture_export_tree(temporary.path()).is_err());
        }

        #[test]
        fn rs67_missing_target_requires_positive_missing_diagnostics() {
            let missing = |s: &str| {
                missing_target(&crate::errors::SvnError::CommandFailed {
                    exit_code: 1,
                    stderr: s.into(),
                })
            };
            assert!(missing("W170000: URL file:///fixture/branches/feature non-existent in revision 3\nE200009: Could not display info"));
            assert!(missing("E160013: path not found"));
            assert!(!missing(
                "E170000: Unable to connect to a repository at URL"
            ));
            assert!(!missing(
                "W170000: unavailable\nE200009: Could not display info"
            ));
            assert!(!missing("E175013: Access denied"));
            assert!(!missing_target(&crate::errors::SvnError::XmlParseError(
                "incomplete info".into()
            )));
        }
    }
}
