//! #67 late-pair publish: SVN copy at verified baseline + Git→SVN replay.
//!
//! Uses the production `SyncEngine` and the durable `late_pair_publish` journal.
//! Does not treat an existing SVN target as equivalent; does not apply skip_import
//! watermarks at the Git tip.

use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::info;

use crate::config::AppConfig;
use crate::db::late_pair_publish_operations::{
    late_pair_publish_fingerprint, LatePairPublishState,
};
use crate::db::Database;
use crate::errors::DatabaseError;
use crate::git::client::GitClient;
use crate::identity::IdentityMapper;
use crate::late_pair::{LatePairPlan, LatePairRequest, SvnTargetProbe};
use crate::models::Repository;
use crate::svn::SvnClient;
use crate::sync_engine::SyncEngine;
use std::sync::Arc;

pub const PUBLISH_POLICY_VERSION: &str = "late_pair_publish_v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatePairPublishRefusal {
    pub reason: String,
    pub detail: String,
    pub plan: Option<Box<LatePairPlan>>,
}

impl LatePairPublishRefusal {
    pub fn error_message(&self) -> String {
        format!("{}: {}", self.reason, self.detail)
    }
}

fn db_err(e: DatabaseError) -> LatePairPublishRefusal {
    LatePairPublishRefusal {
        reason: "database_error".into(),
        detail: e.to_string(),
        plan: None,
    }
}

pub struct PublishCredentials {
    pub svn_password: String,
    pub git_token: Option<String>,
}

fn parse_svn_branch(svn_branch: &str) -> Result<(&str, &str), LatePairPublishRefusal> {
    if let Some(pos) = svn_branch.rfind('/') {
        let (branches_path, name) = svn_branch.split_at(pos);
        Ok((branches_path, name.trim_start_matches('/')))
    } else {
        Ok(("branches", svn_branch))
    }
}

fn parent_svn_branch_path(parent: &Repository) -> String {
    if parent.svn_branch.is_empty() {
        "trunk".into()
    } else {
        parent.svn_branch.trim_start_matches('/').to_string()
    }
}

fn git_env(workdir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(workdir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
}

fn ensure_child_git_workdir(
    data_dir: &Path,
    child_id: &str,
    clone_url: &str,
    git_branch: &str,
    git_token: Option<&str>,
) -> Result<PathBuf, LatePairPublishRefusal> {
    let git_repo_path = data_dir.join("repos").join(child_id).join("git-repo");
    if git_repo_path.join(".git").exists() || git_repo_path.join("HEAD").exists() {
        return Ok(git_repo_path);
    }
    std::fs::create_dir_all(&git_repo_path).map_err(|e| LatePairPublishRefusal {
        reason: "git_workdir_failed".into(),
        detail: format!("failed to create child git dir: {e}"),
        plan: None,
    })?;
    let authenticated_url = match (
        git_token,
        clone_url.strip_prefix("https://"),
        clone_url.strip_prefix("http://"),
    ) {
        (Some(token), Some(rest), _) => format!("https://x-access-token:{token}@{rest}"),
        (Some(token), _, Some(rest)) => format!("http://x-access-token:{token}@{rest}"),
        _ => clone_url.to_string(),
    };
    let output = Command::new("git")
        .args([
            "clone",
            "--",
            &authenticated_url,
            git_repo_path.to_str().unwrap(),
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| LatePairPublishRefusal {
            reason: "git_workdir_failed".into(),
            detail: format!("git clone failed: {e}"),
            plan: None,
        })?;
    if !output.status.success() {
        return Err(LatePairPublishRefusal {
            reason: "git_workdir_failed".into(),
            detail: format!(
                "git clone failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            plan: None,
        });
    }
    let fetch = git_env(
        &git_repo_path,
        &[
            "fetch",
            "--no-tags",
            "origin",
            &format!("refs/heads/{git_branch}:refs/heads/{git_branch}"),
        ],
    )
    .map_err(|e| LatePairPublishRefusal {
        reason: "git_workdir_failed".into(),
        detail: format!("git fetch feature branch failed: {e}"),
        plan: None,
    })?;
    if !fetch.status.success() {
        return Err(LatePairPublishRefusal {
            reason: "git_workdir_failed".into(),
            detail: "git fetch feature branch failed".into(),
            plan: None,
        });
    }
    let checkout =
        git_env(&git_repo_path, &["checkout", "-B", git_branch, git_branch]).map_err(|e| {
            LatePairPublishRefusal {
                reason: "git_workdir_failed".into(),
                detail: format!("git checkout failed: {e}"),
                plan: None,
            }
        })?;
    if !checkout.status.success() {
        return Err(LatePairPublishRefusal {
            reason: "git_workdir_failed".into(),
            detail: "git checkout feature branch failed".into(),
            plan: None,
        });
    }
    Ok(git_repo_path)
}

fn apply_baseline_watermarks(
    db: &Database,
    child_id: &str,
    baseline_git_sha: &str,
    svn_rev: i64,
) -> Result<(), LatePairPublishRefusal> {
    db.set_state(&format!("last_git_sha_{child_id}"), baseline_git_sha)
        .map_err(|e| LatePairPublishRefusal {
            reason: "watermark_failed".into(),
            detail: format!("failed to set scoped git watermark: {e}"),
            plan: None,
        })?;
    db.set_state(&format!("last_svn_rev_{child_id}"), &svn_rev.to_string())
        .map_err(|e| LatePairPublishRefusal {
            reason: "watermark_failed".into(),
            detail: format!("failed to set scoped svn watermark: {e}"),
            plan: None,
        })?;
    if let Some(mut repo) = db
        .get_repository(child_id)
        .map_err(|e| LatePairPublishRefusal {
            reason: "watermark_failed".into(),
            detail: e.to_string(),
            plan: None,
        })?
    {
        repo.last_git_sha = baseline_git_sha.to_string();
        repo.last_svn_rev = svn_rev;
        db.update_repository(&repo)
            .map_err(|e| LatePairPublishRefusal {
                reason: "watermark_failed".into(),
                detail: e.to_string(),
                plan: None,
            })?;
    }
    Ok(())
}

async fn svn_copy_at_baseline(
    parent: &Repository,
    svn_password: &str,
    svn_branch: &str,
    copy_source_rev: i64,
    source_path: &str,
) -> Result<i64, LatePairPublishRefusal> {
    let root_client = SvnClient::new(&parent.svn_url, &parent.svn_username, svn_password);
    let (branches_path, branch_name) = parse_svn_branch(svn_branch)?;
    root_client
        .create_branch(branch_name, source_path, branches_path, copy_source_rev)
        .await
        .map_err(|e| {
            let err = e.to_string();
            if err.contains("already exists") || err.contains("E160020") {
                LatePairPublishRefusal {
                    reason: "existing_svn_target_blocks_publish".into(),
                    detail: "SVN target already exists; publish requires a new target copied from the verified baseline revision".into(),
                    plan: None,
                }
            } else {
                LatePairPublishRefusal {
                    reason: "svn_copy_failed".into(),
                    detail: err,
                    plan: None,
                }
            }
        })?;
    let target_url = format!(
        "{}/{}",
        parent.svn_url.trim_end_matches('/'),
        svn_branch.trim_start_matches('/')
    );
    let target = SvnClient::new(&target_url, &parent.svn_username, svn_password);
    let info = target.info().await.map_err(|e| LatePairPublishRefusal {
        reason: "svn_copy_failed".into(),
        detail: e.to_string(),
        plan: None,
    })?;
    Ok(info.latest_rev)
}

#[allow(clippy::too_many_arguments)]
async fn replay_pending_git(
    app_config: &AppConfig,
    db: &Database,
    child: &Repository,
    git_workdir: &Path,
    svn_password: &str,
    _git_token: Option<&str>,
    identity: &Arc<IdentityMapper>,
    pinned_tip: &str,
    baseline_git_sha: &str,
) -> Result<Vec<String>, LatePairPublishRefusal> {
    let svn_url = if child.svn_branch.is_empty() {
        child.svn_url.clone()
    } else {
        format!(
            "{}/{}",
            child.svn_url.trim_end_matches('/'),
            child.svn_branch.trim_start_matches('/')
        )
    };
    let svn_client = SvnClient::new(&svn_url, &child.svn_username, svn_password);
    let git_client = GitClient::new(git_workdir).map_err(|e| LatePairPublishRefusal {
        reason: "git_workdir_failed".into(),
        detail: e.to_string(),
        plan: None,
    })?;
    let token_state = db.resolve_credential_chain_state(&child.id, "secret_git_token");
    crate::git::apply_git_credential_chain_state(&git_client, "origin", &token_state).ok();
    let mut repo_config = app_config.clone();
    repo_config.svn.trunk_path = String::new();
    repo_config.svn.layout = crate::config::SvnLayout::Custom;
    repo_config.github.default_branch = child.git_branch.clone();
    repo_config.svn.url = child.svn_url.clone();

    let db_path = app_config.daemon.data_dir.join("reposync.db");
    let engine_db = Database::new(&db_path).map_err(|e| LatePairPublishRefusal {
        reason: "engine_db_failed".into(),
        detail: e.to_string(),
        plan: None,
    })?;
    let mut engine = SyncEngine::new(
        repo_config,
        engine_db,
        svn_client,
        git_client,
        identity.clone(),
    );
    engine.set_repo_id(child.id.clone());

    let mut replayed = Vec::new();
    for _ in 0..32 {
        let handled_before = engine
            .db()
            .get_repository(&child.id)
            .ok()
            .flatten()
            .map(|r| r.last_git_sha.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                engine
                    .db()
                    .get_state(&format!("last_git_sha_{}", child.id))
                    .ok()
                    .flatten()
            })
            .unwrap_or_else(|| baseline_git_sha.to_string());

        if handled_before == pinned_tip {
            break;
        }
        let stats = engine
            .run_sync_cycle()
            .await
            .map_err(|e| LatePairPublishRefusal {
                reason: "replay_failed".into(),
                detail: e.to_string(),
                plan: None,
            })?;
        if stats.git_to_svn_count == 0 && stats.svn_to_git_count == 0 {
            break;
        }
        let handled_after = engine
            .db()
            .get_repository(&child.id)
            .ok()
            .flatten()
            .map(|r| r.last_git_sha.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                engine
                    .db()
                    .get_state(&format!("last_git_sha_{}", child.id))
                    .ok()
                    .flatten()
            })
            .unwrap_or_else(|| handled_before.clone());
        if handled_after != handled_before {
            replayed.push(handled_after.clone());
        }
        if handled_after == pinned_tip {
            break;
        }
    }

    let final_sha = engine
        .db()
        .get_repository(&child.id)
        .ok()
        .flatten()
        .map(|r| r.last_git_sha.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            engine
                .db()
                .get_state(&format!("last_git_sha_{}", child.id))
                .ok()
                .flatten()
        })
        .unwrap_or_default();
    if final_sha != pinned_tip {
        return Err(LatePairPublishRefusal {
            reason: "replay_incomplete".into(),
            detail: format!(
                "Git replay stopped before pinned tip (handled={final_sha}, tip={pinned_tip})"
            ),
            plan: None,
        });
    }
    Ok(replayed)
}

/// Resume or run publish for an admitted plan. Refuses existing SVN targets.
#[allow(clippy::too_many_arguments)]
pub async fn publish_admitted_late_pair(
    db: &Database,
    app_config: &AppConfig,
    creds: &PublishCredentials,
    parent: &Repository,
    request: &LatePairRequest,
    plan: &LatePairPlan,
    probe: &SvnTargetProbe,
    child_id: &str,
    initiator_id: &str,
    request_id: &str,
    identity: &Arc<IdentityMapper>,
) -> Result<LatePairPlan, LatePairPublishRefusal> {
    if request.dry_run {
        return Err(LatePairPublishRefusal {
            reason: "preview_only".into(),
            detail: "dry_run/preview cannot publish".into(),
            plan: Some(Box::new(plan.clone())),
        });
    }
    if !plan.admitted {
        return Err(LatePairPublishRefusal {
            reason: "not_admitted".into(),
            detail: "late-pair admission did not succeed".into(),
            plan: Some(Box::new(plan.clone())),
        });
    }
    if probe.exists {
        return Err(LatePairPublishRefusal {
            reason: "existing_svn_target_blocks_publish".into(),
            detail: "existing SVN target requires lineage verification before publish".into(),
            plan: Some(Box::new(plan.clone())),
        });
    }
    let git_tip = plan.git_tip.clone().ok_or_else(|| LatePairPublishRefusal {
        reason: "missing_git_tip".into(),
        detail: "admitted plan is missing git_tip".into(),
        plan: Some(Box::new(plan.clone())),
    })?;
    let baseline = plan
        .verified_baseline
        .clone()
        .ok_or_else(|| LatePairPublishRefusal {
            reason: "missing_baseline".into(),
            detail: "admitted plan is missing verified baseline".into(),
            plan: Some(Box::new(plan.clone())),
        })?;
    let copy_rev =
        plan.proposed_svn_copy_source_revision
            .ok_or_else(|| LatePairPublishRefusal {
                reason: "missing_copy_source".into(),
                detail: "admitted plan is missing proposed_svn_copy_source_revision".into(),
                plan: Some(Box::new(plan.clone())),
            })?;

    let fingerprint = late_pair_publish_fingerprint(
        &parent.id,
        &request.git_branch,
        &request.svn_branch,
        &git_tip,
        &baseline.git_sha,
        baseline.svn_revision,
    );

    let mut op = match db
        .latest_late_pair_publish_operation(&parent.id)
        .map_err(db_err)?
    {
        Some(existing) if !existing.state.is_terminal() => existing,
        _ => db
            .create_late_pair_publish_operation(
                &parent.id,
                initiator_id,
                request_id,
                &fingerprint,
                &request.git_branch,
                &request.svn_branch,
                &git_tip,
                &baseline.git_sha,
                baseline.svn_revision,
                copy_rev,
                PUBLISH_POLICY_VERSION,
            )
            .map_err(db_err)?,
    };

    let source_path = parent_svn_branch_path(parent);
    if op.state == LatePairPublishState::Queued {
        let svn_head = svn_copy_at_baseline(
            parent,
            &creds.svn_password,
            &request.svn_branch,
            copy_rev,
            &source_path,
        )
        .await?;
        op.svn_branch_head_rev = Some(svn_head);
        op.state = LatePairPublishState::SvnCopied;
        op = db.update_late_pair_publish_operation(op).map_err(db_err)?;
        info!(
            parent_id = %parent.id,
            svn_branch = %request.svn_branch,
            svn_head,
            "late-pair SVN branch copied from verified baseline revision"
        );
    }

    let now = chrono::Utc::now().to_rfc3339();
    let root_name = parent.name.clone();
    let child_name = format!("{} / {}", root_name, request.git_branch);
    let svn_head = op
        .svn_branch_head_rev
        .ok_or_else(|| LatePairPublishRefusal {
            reason: "publish_inconsistent".into(),
            detail: "operation missing svn_branch_head_rev after copy".into(),
            plan: Some(Box::new(plan.clone())),
        })?;

    if op.state == LatePairPublishState::SvnCopied {
        let child = Repository {
            id: child_id.to_string(),
            name: child_name,
            svn_url: parent.svn_url.clone(),
            svn_branch: request.svn_branch.clone(),
            svn_username: parent.svn_username.clone(),
            git_provider: parent.git_provider.clone(),
            git_api_url: parent.git_api_url.clone(),
            git_repo: parent.git_repo.clone(),
            git_branch: request.git_branch.clone(),
            sync_mode: parent.sync_mode.clone(),
            poll_interval_secs: parent.poll_interval_secs,
            lfs_threshold_mb: parent.lfs_threshold_mb,
            auto_merge: parent.auto_merge,
            enabled: false,
            created_by: parent.created_by.clone(),
            parent_id: Some(parent.id.clone()),
            created_at: now.clone(),
            updated_at: now.clone(),
            last_svn_rev: svn_head,
            last_git_sha: baseline.git_sha.clone(),
            last_sync_at: None,
            sync_status: "reconciling".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: parent.allowed_paths.clone(),
            blocked_patterns: parent.blocked_patterns.clone(),
            consecutive_errors: 0,
            teams_webhook_url: parent.teams_webhook_url.clone(),
        };
        if db.get_repository(child_id).map_err(db_err)?.is_none() {
            db.insert_repository(&child)
                .map_err(|e| LatePairPublishRefusal {
                    reason: "child_insert_failed".into(),
                    detail: e.to_string(),
                    plan: Some(Box::new(plan.clone())),
                })?;
        }
        apply_baseline_watermarks(db, child_id, &baseline.git_sha, svn_head)?;
        op.child_repo_id = Some(child_id.to_string());
        op.state = LatePairPublishState::ChildRegistered;
        op = db.update_late_pair_publish_operation(op).map_err(db_err)?;
    }

    let child = db
        .get_repository(child_id)
        .map_err(db_err)?
        .ok_or_else(|| LatePairPublishRefusal {
            reason: "child_missing".into(),
            detail: "child repository row missing after registration".into(),
            plan: Some(Box::new(plan.clone())),
        })?;

    let clone_url = crate::git::remote_url::derive_git_remote_url(
        &child.git_api_url,
        creds.git_token.as_deref(),
        &child.git_repo,
    );
    let git_workdir = ensure_child_git_workdir(
        &app_config.daemon.data_dir,
        child_id,
        &clone_url,
        &child.git_branch,
        creds.git_token.as_deref(),
    )?;

    if matches!(
        op.state,
        LatePairPublishState::ChildRegistered | LatePairPublishState::ReplayInProgress
    ) {
        op.state = LatePairPublishState::ReplayInProgress;
        op = db.update_late_pair_publish_operation(op).map_err(db_err)?;
        let replayed = replay_pending_git(
            app_config,
            db,
            &child,
            &git_workdir,
            &creds.svn_password,
            creds.git_token.as_deref(),
            identity,
            &git_tip,
            &baseline.git_sha,
        )
        .await?;
        op.replayed_git_shas = replayed;
        op = db.update_late_pair_publish_operation(op).map_err(db_err)?;
    }

    let mut child = db.get_repository(child_id).map_err(db_err)?.unwrap();
    child.enabled = true;
    child.sync_status = "idle".into();
    child.last_git_sha = git_tip.clone();
    child.updated_at = chrono::Utc::now().to_rfc3339();
    db.update_repository(&child)
        .map_err(|e| LatePairPublishRefusal {
            reason: "finalize_failed".into(),
            detail: e.to_string(),
            plan: Some(Box::new(plan.clone())),
        })?;

    db.finalize_late_pair_publish_operation(&parent.id, &op.id)
        .map_err(db_err)?;

    Ok(LatePairPlan {
        mode: "published".into(),
        published: true,
        admitted: true,
        pair_state: "active".into(),
        scheduler_active: true,
        policy_version: PUBLISH_POLICY_VERSION.into(),
        parent_id: parent.id.clone(),
        git_branch: request.git_branch.clone(),
        svn_branch: request.svn_branch.clone(),
        git_tip: Some(git_tip),
        svn_source_revision: plan.svn_source_revision,
        svn_target_revision: op.svn_branch_head_rev,
        verified_baseline: Some(baseline),
        baseline_missing_reason: None,
        inherited_work: plan.inherited_work.clone(),
        pending_git: plan.pending_git.clone(),
        pending_svn: plan.pending_svn.clone(),
        conflicts: plan.conflicts.clone(),
        unknowns: plan.unknowns.clone(),
        proposed_svn_copy_source_revision: plan.proposed_svn_copy_source_revision,
        existing_svn_target: probe.clone(),
        skip_import_requested: request.skip_import,
        skip_import_applied: false,
        skip_import_note: plan.skip_import_note.clone(),
    })
}
