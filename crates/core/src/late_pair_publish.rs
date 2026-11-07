//! #67 late-pair publish: SVN copy at verified baseline + Git→SVN replay.
//!
//! Uses the production `SyncEngine` and the durable `late_pair_publish` journal.
//! Does not treat an existing SVN target as equivalent; does not apply skip_import
//! watermarks at the Git tip.

use std::path::{Path, PathBuf};

#[cfg(debug_assertions)]
use std::collections::HashMap;
#[cfg(debug_assertions)]
use std::sync::{LazyLock, Mutex};

use tracing::info;

use crate::config::AppConfig;
use crate::db::late_pair_publish_operations::LatePairPublishOperation;
use crate::db::late_pair_publish_operations::{
    late_pair_publish_fingerprint, LatePairPublishState,
};
use crate::db::queries::CredentialChainState;
use crate::db::Database;
use crate::errors::{redact_vcs_error_detail, DatabaseError};
use crate::git::apply_git_credential_chain_state;
use crate::git::client::GitClient;
use crate::git::remote_url::derive_git_remote_url;
use crate::git::subprocess_auth::build_git_ls_remote_command;
use crate::identity::IdentityMapper;
use crate::late_pair::{LatePairPlan, LatePairRequest, SvnTargetProbe};
use crate::models::{Repository, SyncDirection, SyncRecord, SyncRecordStatus};
use crate::svn::SvnClient;
use crate::sync_engine::SyncEngine;
use std::sync::Arc;

pub const PUBLISH_POLICY_VERSION: &str = "late_pair_publish_v1";

/// Recapture the entire source/result before claiming a reviewed operation.
/// The HTTP caller supplies only the opaque pin, never a serialized plan.
pub async fn begin_reviewed_reconciliation(
    context: crate::late_pair::reconciliation::PrepareContext<'_>,
    review_pin: &str,
    request_id: &str,
) -> anyhow::Result<LatePairPublishOperation> {
    anyhow::ensure!(!review_pin.is_empty(), "a fresh reviewed pin is required");
    let db = context.db;
    let _busy = crate::busy::try_acquire(&context.parent.id)
        .ok_or_else(|| anyhow::anyhow!("parent repository is busy"))?;
    let plan = crate::late_pair::reconciliation::prepare(context).await?;
    anyhow::ensure!(
        plan.review_pin == review_pin,
        "stale late-pair review; capture and review both sides again"
    );
    Ok(db.claim_late_pair_reconciliation(plan, request_id)?)
}

/// The v2 producer shares the existing #64 durable SVN/Git effect journals.
/// It never re-enters the legacy replay loop with a partially handled plan.
pub mod reviewed {
    use super::*;
    use crate::db::late_pair_publish_operations::{
        CopyEvidence, ReconciliationPhase as Phase, ReconciliationStepReceipt,
    };
    use crate::late_pair::reconciliation::{self as model, PreparedPlan, Tree};
    use anyhow::{bail, ensure, Context, Result};

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Fault {
        AfterCopy,
        AfterSvnCommit(usize),
        AfterGitPublication,
    }
    #[cfg(debug_assertions)]
    static FAULTS: LazyLock<Mutex<HashMap<String, Fault>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    #[cfg(debug_assertions)]
    pub fn set_fault(parent: &str, fault: Fault) {
        FAULTS.lock().unwrap().insert(parent.into(), fault);
    }
    fn fault(parent: &str, point: Fault) -> Result<()> {
        #[cfg(debug_assertions)]
        {
            let mut faults = FAULTS.lock().unwrap();
            if faults.get(parent) == Some(&point) {
                faults.remove(parent);
                bail!("controlled late-pair interruption at {point:?}");
            }
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = (parent, point);
        }
        Ok(())
    }

    fn progress(
        op: &LatePairPublishOperation,
    ) -> Result<&crate::db::late_pair_publish_operations::ReconciliationProgress> {
        ensure!(op.version == 2, "operation requires explicit v2 recovery");
        op.reconciliation.as_deref().context("v2 progress absent")
    }

    fn save(
        db: &Database,
        op: &mut LatePairPublishOperation,
        mut next: LatePairPublishOperation,
    ) -> Result<()> {
        let p = progress(&next)?.clone();
        next.state = match p.phase {
            Phase::Prepared => LatePairPublishState::Queued,
            Phase::CopyPending => LatePairPublishState::SvnCopyPending,
            Phase::Completed => LatePairPublishState::Completed,
            Phase::Cancelled => LatePairPublishState::Cancelled,
            Phase::Held => LatePairPublishState::ReconciliationHeld,
            _ => LatePairPublishState::ReplayInProgress,
        };
        next.svn_branch_head_rev = p.svn_revision;
        next.replayed_git_shas = p
            .receipts
            .iter()
            .filter_map(|receipt| match &p.plan.steps[receipt.index].source {
                model::StepSource::Git { sha, .. } => Some(sha.clone()),
                _ => None,
            })
            .collect();
        *op = db.transition_late_pair_reconciliation(op, next)?;
        Ok(())
    }

    async fn check_svn(
        target: &SvnClient,
        plan: &PreparedPlan,
        revision: i64,
        tree: &Tree,
    ) -> Result<()> {
        let info = target.info().await?;
        ensure!(
            info.uuid == plan.svn_uuid
                && info.url.trim_end_matches('/') == plan.target_url.trim_end_matches('/')
                && info.root_url == plan.svn_root_url
                && info.latest_rev == revision,
            "stale or foreign SVN target"
        );
        ensure!(
            model::capture_svn_tree(target, revision).await? == *tree,
            "SVN target bytes or modes differ from durable intent"
        );
        Ok(())
    }

    fn check_parent(db: &Database, plan: &PreparedPlan) -> Result<()> {
        ensure!(
            model::parent_generation(db, &plan.parent_id)? == plan.parent_generation,
            "parent registration, policy or credentials changed; reconciliation is held"
        );
        Ok(())
    }

    async fn check_parent_source(
        parent: &Repository,
        password: &str,
        plan: &PreparedPlan,
    ) -> Result<()> {
        let source = SvnClient::new(&plan.source_url, &parent.svn_username, password);
        let info = source.info().await?;
        ensure!(
            info.uuid == plan.svn_uuid && info.latest_rev >= plan.parent_revision,
            "parent SVN identity changed"
        );
        ensure!(
            model::tree_digest(&model::capture_svn_tree(&source, info.latest_rev).await?)?
                == plan.parent_tree_digest,
            "parent SVN work changed after review"
        );
        if info.latest_rev > plan.parent_revision {
            let root = SvnClient::new(&plan.svn_root_url, &parent.svn_username, password);
            for entry in root.log(plan.parent_revision + 1, info.latest_rev).await? {
                ensure!(
                    !entry.changed_paths.iter().any(|path| {
                        let name = path.path.trim_start_matches('/');
                        name == plan.source_path
                            || name.starts_with(&format!("{}/", plan.source_path))
                    }),
                    "parent SVN history changed after review"
                );
            }
        }
        Ok(())
    }

    fn git_client(db: &Database, parent: &Repository, workdir: &Path) -> Result<GitClient> {
        model::verify_git_remote(workdir, parent)?;
        let git = GitClient::new(workdir)?;
        let state = db.resolve_credential_chain_state(&parent.id, "secret_git_token");
        apply_git_credential_chain_state(&git, "origin", &state)?;
        Ok(git)
    }

    // Starting another effect requires the reviewed source, regardless of the
    // outer journal phase. Inspecting an existing native intent is separate:
    // in particular, a consumed Git publication legitimately changed this ref.
    fn check_git_before_new_effect(
        db: &Database,
        parent: &Repository,
        workdir: &Path,
        plan: &PreparedPlan,
    ) -> Result<()> {
        ensure!(
            git_client(db, parent, workdir)?
                .ls_remote_ref("origin", &plan.git_branch)?
                .as_deref()
                == Some(plan.git_tip.as_str()),
            "Git tip changed before effects"
        );
        Ok(())
    }

    async fn copy_evidence(
        target: &SvnClient,
        parent: &Repository,
        password: &str,
        plan: &PreparedPlan,
    ) -> Result<CopyEvidence> {
        let revision = verify_existing_svn_target_matches_copy_intent(
            parent,
            password,
            &plan.svn_branch,
            &plan.source_path,
            plan.baseline_svn_revision,
        )
        .await
        .map_err(|e| anyhow::anyhow!(e.error_message()))?;
        check_svn(target, plan, revision, &plan.initial_target_tree).await?;
        // A copied target with any independent subsequent commit is not the
        // one pending copy outcome. Examine its complete stop-on-copy history.
        let entries = target.copy_history_at_rev(revision).await?;
        ensure!(
            entries.len() == 1,
            "pending copy has additional or ambiguous target history"
        );
        let entry = &entries[0];
        ensure!(
            entry.revision == revision
                && entry
                    .changed_paths
                    .iter()
                    .any(|path| path.path.trim_start_matches('/') == plan.target_path
                        && path.action == "A"
                        && path
                            .copy_from_path
                            .as_deref()
                            .map(|p| p.trim_start_matches('/'))
                            == Some(plan.source_path.as_str())
                        && path.copy_from_rev == Some(plan.baseline_svn_revision)),
            "pending copy lineage differs"
        );
        Ok(CopyEvidence {
            revision,
            uuid: plan.svn_uuid.clone(),
            source_path: plan.source_path.clone(),
            source_revision: plan.baseline_svn_revision,
            target_path: plan.target_path.clone(),
            tree_digest: model::tree_digest(&plan.initial_target_tree)?,
        })
    }

    fn register_child(
        db: &Database,
        parent: &Repository,
        op: &mut LatePairPublishOperation,
    ) -> Result<Repository> {
        let plan = progress(op)?.plan.clone();
        if op.child_repo_id.is_none() {
            let mut next = op.clone();
            next.child_repo_id = Some(crate::db::repo_scope_identity::new_human_repository_id());
            save(db, op, next)?;
        }
        let id = op
            .child_repo_id
            .as_deref()
            .context("child reservation absent")?;
        if let Some(child) = db.get_repository(id)? {
            let expected_scope = progress(op)?
                .child_scope_uuid
                .as_deref()
                .context("child registration outcome was not durably captured; held")?;
            ensure!(
                crate::db::repo_scope_identity::repository_scope_uuid(&db.conn(), id)?
                    == expected_scope,
                "child registration incarnation changed"
            );
            ensure!(
                !child.enabled
                    && child.parent_id.as_deref() == Some(&parent.id)
                    && child.git_branch == plan.git_branch
                    && child.svn_branch == plan.svn_branch
                    && child.svn_url == parent.svn_url
                    && child.git_repo == parent.git_repo,
                "child registration was replaced or activated before completion"
            );
            return Ok(child);
        }
        let mut child = parent.clone();
        child.id = id.into();
        child.name = format!("{} / {}", parent.name, plan.git_branch);
        child.parent_id = Some(parent.id.clone());
        child.git_branch = plan.git_branch.clone();
        child.svn_branch = plan.svn_branch.clone();
        child.enabled = false;
        child.sync_status = "reconciling".into();
        child.last_git_sha = plan.baseline_git_sha.clone();
        child.last_svn_rev = progress(op)?
            .svn_revision
            .context("initial SVN observation absent")?;
        child.last_sync_at = None;
        child.total_syncs = 0;
        child.total_errors = 0;
        child.consecutive_errors = 0;
        child.created_at = chrono::Utc::now().to_rfc3339();
        child.updated_at = child.created_at.clone();
        db.insert_repository(&child)?;
        let scope = crate::db::repo_scope_identity::repository_scope_uuid(&db.conn(), &child.id)?;
        let mut next = op.clone();
        next.reconciliation.as_deref_mut().unwrap().child_scope_uuid = Some(scope);
        save(db, op, next)?;
        Ok(child)
    }

    async fn stage(svn: &SvnClient, wc: &Path, before: &Tree, after: &Tree) -> Result<()> {
        let confined = crate::svn::working_copy::ConfinedWorkingCopy::open(wc)?;
        let paths = model::producer_changed_paths(before, after);
        for path in &paths {
            confined.inspect_file(path.path.trim_start_matches('/'))?;
        }
        for path in &paths {
            let name = path.path.trim_start_matches('/');
            match after.get(name) {
                None => {
                    svn.rm(wc, &[name]).await?;
                }
                Some(file) => {
                    confined.write_file(name, &file.bytes)?;
                    if !before.contains_key(name) {
                        svn.add(wc, &[name]).await?;
                    }
                    if file.mode == 0o100755 {
                        svn.run_svn_in_dir_public(wc, &["propset", "svn:executable", "*", name])
                            .await?;
                    } else if before.get(name).is_some_and(|prior| prior.mode == 0o100755) {
                        svn.run_svn_in_dir_public(wc, &["propdel", "svn:executable", name])
                            .await?;
                    }
                }
            }
        }
        ensure!(
            model::capture_export_tree(wc)? == *after,
            "staged bytes or executable modes differ from pinned result"
        );
        svn.require_supported_working_copy(wc).await?;
        Ok(())
    }

    async fn apply_step(
        db: &Database,
        parent: &Repository,
        workdir: &Path,
        target: &SvnClient,
        op: &mut LatePairPublishOperation,
    ) -> Result<()> {
        use crate::db::svn_commit_operations::{
            svn_commit_target_fingerprint, SvnCommitIntent, SvnCommitOperationState,
        };
        let p = progress(op)?.clone();
        let plan = &p.plan;
        let index = p.receipts.len();
        let step = plan.steps.get(index).context("no next replay step")?;
        let before = if index == 0 {
            &plan.initial_target_tree
        } else {
            &plan.steps[index - 1].intended_tree
        };
        let revision = p.svn_revision.context("SVN frontier absent")?;
        let child_id = op.child_repo_id.clone().context("child absent")?;
        let child = child_id.as_str();
        let orphan = db
            .latest_svn_commit_operation(child)?
            .filter(|producer| producer.request_id == format!("{}:{index}", op.id));
        let producer = if let Some(id) = p.pending_svn_operation.as_deref() {
            db.get_svn_commit_operation(child, id)?
                .context("pending native SVN intent absent")?
        } else if let Some(producer) = orphan {
            let mut next = op.clone();
            next.reconciliation
                .as_deref_mut()
                .unwrap()
                .pending_svn_operation = Some(producer.id.clone());
            save(db, op, next)?;
            producer
        } else {
            check_git_before_new_effect(db, parent, workdir, plan)?;
            check_svn(target, plan, revision, before).await?;
            check_parent(db, plan)?;
            if step.before_digest == step.intended_digest {
                let mut next = op.clone();
                let q = next.reconciliation.as_deref_mut().unwrap();
                q.phase = Phase::Applying;
                q.receipts.push(ReconciliationStepReceipt {
                    index,
                    svn_revision: revision,
                    tree_digest: step.intended_digest.clone(),
                    svn_operation: None,
                });
                save(db, op, next)?;
                return Ok(());
            }
            let fingerprint = svn_commit_target_fingerprint(
                child,
                &plan.svn_uuid,
                &plan.target_url,
                &plan.target_path,
                model::POLICY,
            );
            let producer = db.begin_git_to_svn_commit(SvnCommitIntent {
                repo_id: child,
                initiator_id: &op.initiator_id,
                request_id: &format!("{}:{index}", op.id),
                target_fingerprint: &fingerprint,
                source_git_sha: &step.git_result.sha,
                source_git_parent: step.git_result.parent.as_deref(),
                source_git_tree: &step.git_result.tree,
                target_svn_uuid: &plan.svn_uuid,
                target_svn_path: &plan.target_url,
                target_svn_root_url: &plan.svn_root_url,
                target_svn_branch_path: &plan.target_path,
                pre_write_svn_rev: revision,
                pre_write_svn_tree: &model::producer_tree_digest(before),
                projection: model::POLICY,
                intended_changed_paths: model::producer_changed_paths(before, &step.intended_tree),
                intended_svn_tree: &model::producer_tree_digest(&step.intended_tree),
                author: &parent.svn_username,
                source_message: &step.message,
            })?;
            let mut next = op.clone();
            let q = next.reconciliation.as_deref_mut().unwrap();
            q.phase = Phase::Applying;
            q.pending_svn_operation = Some(producer.id.clone());
            save(db, op, next)?;
            let temporary = tempfile::tempdir()?;
            let wc = temporary.path().join("wc");
            target
                .checkout_full_pinned_without_externals(&wc, revision)
                .await?;
            ensure!(
                model::capture_export_tree(&wc)? == *before,
                "fresh checkout differs from pre-write intent"
            );
            stage(target, &wc, before, &step.intended_tree).await?;
            check_parent(db, plan)?;
            check_svn(target, plan, revision, before).await?;
            check_git_before_new_effect(db, parent, workdir, plan)?;
            let message = crate::svn_commit::operation_commit_message(
                &step.message,
                &step.git_result.sha,
                &producer.id,
            );
            let result = target.commit(&wc, &message, &parent.svn_username).await;
            if let Err(error) = result {
                db.hold_git_to_svn_reconciliation(
                    child,
                    &producer.id,
                    "late-pair SVN outcome requires native inspection",
                )?;
                return Err(error.into());
            }
            fault(&parent.id, Fault::AfterSvnCommit(index + 1))?;
            producer
        };
        ensure!(
            producer.repo_id == child
                && producer.source_git_sha == step.git_result.sha
                && producer.source_git_parent == step.git_result.parent
                && producer.source_git_tree == step.git_result.tree
                && producer.pre_write_svn_rev == revision
                && producer.pre_write_svn_tree == model::producer_tree_digest(before)
                && producer.intended_svn_tree == model::producer_tree_digest(&step.intended_tree)
                && producer.intended_changed_paths
                    == model::producer_changed_paths(before, &step.intended_tree)
                && producer.target_svn_uuid == plan.svn_uuid
                && producer.target_svn_path == plan.target_url
                && producer.projection == model::POLICY
                && producer.source_message == step.message
                && producer.initiator_id == op.initiator_id
                && producer.request_id == format!("{}:{index}", op.id),
            "native SVN intent does not match this exact replay step"
        );
        // Both the live and recovered path prove the server result before a
        // mapping/checkpoint is acknowledged. Absence never authorizes replay.
        let completed = if producer.state == SvnCommitOperationState::Completed {
            producer
        } else {
            let observed = crate::svn_commit::inspect_git_to_svn_commit(target, &producer).await;
            match observed {
                crate::svn_commit::SvnCommitInspect::UniqueMatch {
                    svn_rev, svn_tree, ..
                } => {
                    ensure!(
                        model::capture_svn_tree(target, svn_rev).await? == step.intended_tree,
                        "confirmed SVN modes or bytes differ"
                    );
                    if producer.state == SvnCommitOperationState::Running {
                        db.confirm_git_to_svn_commit(child, &producer.id, svn_rev, &svn_tree)?
                    } else {
                        db.finalize_verified_svn_commit(
                            child,
                            &producer.id,
                            svn_rev,
                            &svn_tree,
                            &producer.target_fingerprint,
                        )?
                        .operation
                    }
                }
                other => bail!("late-pair native SVN outcome remains held: {other:?}"),
            }
        };
        let revision = completed
            .last_confirmed_svn_rev
            .context("completed SVN producer lacks revision")?;
        check_svn(target, plan, revision, &step.intended_tree).await?;
        let mut next = op.clone();
        let q = next.reconciliation.as_deref_mut().unwrap();
        q.phase = Phase::Applying;
        q.pending_svn_operation = None;
        q.svn_revision = Some(revision);
        q.receipts.push(ReconciliationStepReceipt {
            index,
            svn_revision: revision,
            tree_digest: step.intended_digest.clone(),
            svn_operation: Some(completed),
        });
        save(db, op, next)
    }

    async fn publish_git(
        db: &Database,
        parent: &Repository,
        workdir: &Path,
        target: &SvnClient,
        op: &mut LatePairPublishOperation,
    ) -> Result<()> {
        use crate::db::git_push_operations::{
            git_push_target_fingerprint, GitPushIntent, GitPushOperationState,
        };
        let p = progress(op)?.clone();
        let plan = &p.plan;
        let child_id = op.child_repo_id.clone().context("child absent")?;
        let child = child_id.as_str();
        let revision = p.svn_revision.context("SVN revision absent")?;
        check_parent(db, plan)?;
        check_svn(target, plan, revision, &plan.intended_tree).await?;
        let intended = plan
            .intended_git_sha
            .as_deref()
            .context("blocked intended Git result")?;
        let tree = plan
            .intended_git_tree
            .as_deref()
            .context("intended Git tree absent")?;
        ensure!(
            model::capture_git_tree(workdir, intended)? == plan.intended_tree,
            "private Git result is unavailable or changed"
        );
        let git = git_client(db, parent, workdir)?;
        let orphan = db
            .latest_git_push_operation(child)?
            .filter(|producer| producer.request_id == op.id);
        let mut producer = if let Some(id) = &p.git_push_operation {
            db.get_git_push_operation(child, id)?
                .context("native Git intent absent")?
        } else if let Some(producer) = orphan {
            let mut next = op.clone();
            next.reconciliation
                .as_deref_mut()
                .unwrap()
                .git_push_operation = Some(producer.id.clone());
            save(db, op, next)?;
            producer
        } else {
            ensure!(
                git.ls_remote_ref("origin", &plan.git_branch)?.as_deref()
                    == Some(plan.git_tip.as_str()),
                "Git remote changed before publication intent"
            );
            let fingerprint =
                git_push_target_fingerprint(child, &plan.git_remote_url, &plan.git_branch);
            let producer = db.begin_svn_to_git_push(GitPushIntent {
                repo_id: child,
                initiator_id: &op.initiator_id,
                request_id: &op.id,
                target_fingerprint: &fingerprint,
                source_svn_rev: revision,
                source_svn_author: &parent.svn_username,
                source_svn_message: "Verified late-pair reconciliation",
                pre_push_git_remote: &plan.git_remote_url,
                pre_push_git_branch: &plan.git_branch,
                pre_push_git_sha: &plan.git_tip,
                pre_push_git_tree: Some(
                    &git2::Repository::open(workdir)?
                        .find_commit(git2::Oid::from_str(&plan.git_tip)?)?
                        .tree_id()
                        .to_string(),
                ),
                intended_local_git_sha: intended,
                intended_local_git_parent: plan.intended_git_parents.first().map(String::as_str),
                intended_local_git_tree: tree,
            })?;
            let mut next = op.clone();
            let q = next.reconciliation.as_deref_mut().unwrap();
            q.phase = Phase::Publishing;
            q.git_push_operation = Some(producer.id.clone());
            save(db, op, next)?;
            if intended != plan.git_tip {
                let repository = git2::Repository::open(workdir)?;
                ensure!(
                    repository.graph_descendant_of(
                        git2::Oid::from_str(intended)?,
                        git2::Oid::from_str(&plan.git_tip)?
                    )?,
                    "publication would rewrite Git history"
                );
                let token = db.resolve_credential_chain(&parent.id, "secret_git_token");
                let refspec = format!("{intended}:refs/heads/{}", plan.git_branch);
                // Exact expected-tip lease closes the ref race. The proven
                // ancestry above still forbids every non-fast-forward result.
                let lease = format!(
                    "--force-with-lease=refs/heads/{}:{}",
                    plan.git_branch, plan.git_tip
                );
                let output = crate::git::subprocess_auth::git_cli_output(
                    workdir,
                    &["push", &lease, "origin", &refspec],
                    token.as_deref(),
                )?;
                if !output.status.success() {
                    db.hold_svn_to_git_reconciliation(
                        child,
                        &producer.id,
                        "late-pair Git publication requires native inspection",
                    )?;
                    bail!("Git publication did not report success; durable intent retained");
                }
            }
            fault(&parent.id, Fault::AfterGitPublication)?;
            producer
        };
        ensure!(
            producer.repo_id == child
                && producer.request_id == op.id
                && producer.initiator_id == op.initiator_id
                && producer.source_svn_rev == revision
                && producer.pre_push_git_sha == plan.git_tip
                && producer.pre_push_git_branch == plan.git_branch
                && producer.pre_push_git_remote == plan.git_remote_url
                && producer.intended_local_git_sha == intended
                && producer.intended_local_git_tree == tree,
            "native Git intent does not match this exact publication"
        );
        ensure!(
            git.ls_remote_ref("origin", &plan.git_branch)?.as_deref() == Some(intended),
            "native Git outcome remains held; no blind republication"
        );
        if producer.state != GitPushOperationState::Completed {
            producer = if producer.state == GitPushOperationState::Running {
                db.confirm_svn_to_git_push(child, &producer.id, intended, tree)?
            } else {
                db.finalize_verified_svn_to_git_push(
                    child,
                    &producer.id,
                    intended,
                    tree,
                    &producer.target_fingerprint,
                )?
                .operation
            };
        }
        let mut next = op.clone();
        let q = next.reconciliation.as_deref_mut().unwrap();
        q.publication = Some(producer);
        q.phase = Phase::Checkpointing;
        save(db, op, next)
    }

    /// Continue one actor-owned native journal. A restart may finish a proven
    /// consumed effect, but cannot reissue an intent with an unknown outcome.
    pub async fn run(
        db: &Database,
        config: &AppConfig,
        parent: &Repository,
        password: &str,
        workdir: &Path,
        operation_id: &str,
        actor: &str,
    ) -> Result<LatePairPublishOperation> {
        let _busy = crate::busy::try_acquire(&parent.id).context("parent repository is busy")?;
        model::require_registered_parent(db, parent)?;
        let mut op = db
            .get_late_pair_publish_operation(&parent.id, operation_id)?
            .context("late-pair operation absent")?;
        ensure!(
            op.initiator_id == actor,
            "operation belongs to a different actor"
        );
        let plan = progress(&op)?.plan.clone();
        if matches!(
            op.state,
            LatePairPublishState::Completed | LatePairPublishState::Cancelled
        ) {
            return Ok(op);
        }
        ensure!(
            !progress(&op)?.cancel_requested,
            "cancellation retains this operation's uncertainty hold"
        );
        check_parent(db, &plan)?;
        check_parent_source(parent, password, &plan).await?;
        let target = SvnClient::new(&plan.target_url, &parent.svn_username, password);
        let result: Result<()> = async {
            if progress(&op)?.phase == Phase::Prepared {
                check_git_before_new_effect(db, parent, workdir, &plan)?;
                let mut next = op.clone();
                next.reconciliation.as_deref_mut().unwrap().phase =
                    if plan.target_revision.is_some() {
                        Phase::SourceReady
                    } else {
                        Phase::CopyPending
                    };
                save(db, &mut op, next)?;
                if plan.target_revision.is_none() {
                    svn_copy_at_baseline(
                        parent,
                        password,
                        &plan.svn_branch,
                        plan.baseline_svn_revision,
                        &plan.source_path,
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!(error.error_message()))?;
                    fault(&parent.id, Fault::AfterCopy)?;
                }
            }
            if plan.target_revision.is_none() && progress(&op)?.copy_evidence.is_none() {
                let evidence = copy_evidence(&target, parent, password, &plan).await?;
                let mut next = op.clone();
                let p = next.reconciliation.as_deref_mut().unwrap();
                p.svn_revision = Some(evidence.revision);
                p.copy_evidence = Some(evidence);
                p.phase = Phase::SourceReady;
                save(db, &mut op, next)?;
            }
            if progress(&op)?.child_scope_uuid.is_none() {
                check_git_before_new_effect(db, parent, workdir, &plan)?;
            }
            register_child(db, parent, &mut op)?;
            while progress(&op)?.receipts.len() < plan.steps.len() {
                check_parent_source(parent, password, &plan).await?;
                apply_step(db, parent, workdir, &target, &mut op).await?;
            }
            check_parent_source(parent, password, &plan).await?;
            if progress(&op)?.publication.is_none() {
                publish_git(db, parent, workdir, &target, &mut op).await?;
            }
            let revision = progress(&op)?
                .svn_revision
                .context("completed source revision absent")?;
            check_svn(&target, &plan, revision, &plan.intended_tree).await?;
            ensure!(
                git_client(db, parent, workdir)?.ls_remote_ref("origin", &plan.git_branch)?
                    == plan.intended_git_sha,
                "Git changed before checkpoint finalization"
            );
            ensure_child_git_workdir(
                &config.daemon.data_dir,
                op.child_repo_id.as_deref().context("child absent")?,
                parent,
                &plan.git_branch,
                db,
            )
            .map_err(|error| anyhow::anyhow!(error.error_message()))?;
            let mut next = op.clone();
            let p = next.reconciliation.as_deref_mut().unwrap();
            p.phase = Phase::Completed;
            p.checkpoint_verified = true;
            save(db, &mut op, next)?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            let mut next = op.clone();
            next.reconciliation.as_deref_mut().unwrap().phase = Phase::Held;
            let detail = redact_vcs_error_detail(&format!("{error:#}"));
            next.outcome_detail = Some(detail.chars().take(2048).collect());
            save(db, &mut op, next)?;
            return Err(error);
        }
        Ok(op)
    }
}

/// Cancel only an unstarted claim. Once an effect intent exists, retain the
/// active reservation until native reconciliation can establish its outcome.
pub fn cancel_reviewed_reconciliation(
    db: &Database,
    parent_id: &str,
    operation_id: &str,
    actor: &str,
) -> anyhow::Result<LatePairPublishOperation> {
    use crate::db::late_pair_publish_operations::{
        ControlKind, ReconciliationControl, ReconciliationPhase,
    };
    let expected = db
        .get_late_pair_publish_operation(parent_id, operation_id)?
        .ok_or_else(|| anyhow::anyhow!("late-pair operation absent"))?;
    anyhow::ensure!(
        expected.version == 2 && expected.initiator_id == actor,
        "late-pair actor or version mismatch"
    );
    if matches!(
        expected.state,
        LatePairPublishState::Completed | LatePairPublishState::Cancelled
    ) {
        return Ok(expected);
    }
    let mut next = expected.clone();
    let progress = next
        .reconciliation
        .as_deref_mut()
        .ok_or_else(|| anyhow::anyhow!("v2 progress absent"))?;
    if progress.cancel_requested {
        return Ok(expected);
    }
    progress.cancel_requested = true;
    progress.controls.push(ReconciliationControl {
        kind: ControlKind::Cancel,
        actor: actor.into(),
        review_pin: progress.plan.review_pin.clone(),
        request_id: uuid::Uuid::new_v4().to_string(),
        recorded_at: chrono::Utc::now().to_rfc3339(),
    });
    if progress.phase == ReconciliationPhase::Prepared {
        progress.phase = ReconciliationPhase::Cancelled;
        next.state = LatePairPublishState::Cancelled;
    } else {
        progress.phase = ReconciliationPhase::Held;
        next.state = LatePairPublishState::ReconciliationHeld;
        next.outcome_detail=Some("Cancellation requested after intent; effects remain held pending native reconciliation".into());
    }
    Ok(db.transition_late_pair_reconciliation(&expected, next)?)
}

/// Explicit actor continuation records the prior cancellation rather than
/// deleting it. It grants no replay of an uncertain native effect: `run`
/// still performs all current-source and consumed-producer checks.
pub fn continue_reviewed_reconciliation(
    db: &Database,
    parent_id: &str,
    operation_id: &str,
    actor: &str,
    review_pin: &str,
) -> anyhow::Result<LatePairPublishOperation> {
    use crate::db::late_pair_publish_operations::{ControlKind, ReconciliationControl};
    let expected = db
        .get_late_pair_publish_operation(parent_id, operation_id)?
        .ok_or_else(|| anyhow::anyhow!("operation absent"))?;
    let p = expected
        .reconciliation
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("v2 progress absent"))?;
    anyhow::ensure!(
        expected.version == 2 && expected.initiator_id == actor && p.plan.review_pin == review_pin,
        "continuation requires the same actor and reviewed intended result"
    );
    anyhow::ensure!(
        expected.state != LatePairPublishState::Cancelled,
        "pre-effect cancellation is terminal; request a fresh review"
    );
    if !p.cancel_requested {
        return Ok(expected);
    }
    let mut next = expected.clone();
    let p = next.reconciliation.as_deref_mut().unwrap();
    p.cancel_requested = false;
    p.controls.push(ReconciliationControl {
        kind: ControlKind::Continue,
        actor: actor.into(),
        review_pin: review_pin.into(),
        request_id: uuid::Uuid::new_v4().to_string(),
        recorded_at: chrono::Utc::now().to_rfc3339(),
    });
    Ok(db.transition_late_pair_reconciliation(&expected, next)?)
}

/// Debug-only hooks for integration tests (keyed by parent repository id).
#[cfg(debug_assertions)]
#[derive(Debug, Clone, Default)]
pub struct LatePairPublishTestHook {
    pub fail_replay_once: bool,
    pub abort_after_svn_copy_before_journal: bool,
    pub fail_svn_copy_once: bool,
    /// Fail inside `replay_pending_git` after this many newly replayed commits (0 = disabled).
    pub fail_replay_after_commits: u32,
}

#[cfg(debug_assertions)]
static PUBLISH_TEST_HOOKS: LazyLock<Mutex<HashMap<String, LatePairPublishTestHook>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(debug_assertions)]
static PENDING_PUBLISH_REFUSALS: LazyLock<Mutex<HashMap<String, LatePairPublishRefusal>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(debug_assertions)]
pub fn set_late_pair_publish_test_hook(parent_repo_id: &str, hook: LatePairPublishTestHook) {
    PUBLISH_TEST_HOOKS
        .lock()
        .unwrap()
        .insert(parent_repo_id.to_string(), hook);
}

#[cfg(debug_assertions)]
pub fn clear_late_pair_publish_test_hook(parent_repo_id: &str) {
    PUBLISH_TEST_HOOKS.lock().unwrap().remove(parent_repo_id);
}

#[cfg(not(debug_assertions))]
#[derive(Debug, Clone, Default)]
pub struct LatePairPublishTestHook {
    pub fail_replay_once: bool,
    pub abort_after_svn_copy_before_journal: bool,
    pub fail_svn_copy_once: bool,
    /// Fail inside `replay_pending_git` after this many newly replayed commits (0 = disabled).
    pub fail_replay_after_commits: u32,
}

#[cfg(not(debug_assertions))]
pub fn set_late_pair_publish_test_hook(_parent_repo_id: &str, _hook: LatePairPublishTestHook) {}

#[cfg(not(debug_assertions))]
pub fn clear_late_pair_publish_test_hook(_parent_repo_id: &str) {}

/// Debug-only: refusal stashed by a test hook when returning `Ok` avoids async-drop hangs.
pub fn take_pending_publish_refusal(_parent_repo_id: &str) -> Option<LatePairPublishRefusal> {
    #[cfg(debug_assertions)]
    {
        return PENDING_PUBLISH_REFUSALS
            .lock()
            .unwrap()
            .remove(_parent_repo_id);
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

#[cfg(debug_assertions)]
fn stash_pending_publish_refusal(parent_repo_id: &str, refusal: LatePairPublishRefusal) {
    PENDING_PUBLISH_REFUSALS
        .lock()
        .unwrap()
        .insert(parent_repo_id.to_string(), refusal);
}

// Release builds disable the test hooks and keep no pending refusal stash.
#[cfg(not(debug_assertions))]
fn stash_pending_publish_refusal(_parent_repo_id: &str, _refusal: LatePairPublishRefusal) {}

fn refused_publish_plan_stub(
    plan: &LatePairPlan,
    parent: &Repository,
    request: &LatePairRequest,
    probe: &SvnTargetProbe,
    baseline: &crate::late_pair::VerifiedMapping,
    git_tip: &str,
) -> LatePairPlan {
    LatePairPlan {
        mode: "publish_refused".into(),
        published: false,
        admitted: plan.admitted,
        pair_state: plan.pair_state.clone(),
        scheduler_active: false,
        policy_version: PUBLISH_POLICY_VERSION.into(),
        parent_id: parent.id.clone(),
        git_branch: request.git_branch.clone(),
        svn_branch: request.svn_branch.clone(),
        git_tip: Some(git_tip.to_string()),
        svn_source_revision: plan.svn_source_revision,
        svn_target_revision: plan.svn_target_revision,
        verified_baseline: Some(baseline.clone()),
        baseline_missing_reason: plan.baseline_missing_reason.clone(),
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
    }
}

fn take_fail_replay_once(_parent_repo_id: &str) -> bool {
    #[cfg(debug_assertions)]
    {
        let mut hooks = PUBLISH_TEST_HOOKS.lock().unwrap();
        if let Some(hook) = hooks.get_mut(_parent_repo_id) {
            if hook.fail_replay_once {
                hook.fail_replay_once = false;
                return true;
            }
        }
    }
    false
}

fn take_fail_svn_copy_once(_parent_repo_id: &str) -> bool {
    #[cfg(debug_assertions)]
    {
        let mut hooks = PUBLISH_TEST_HOOKS.lock().unwrap();
        if let Some(hook) = hooks.get_mut(_parent_repo_id) {
            if hook.fail_svn_copy_once {
                hook.fail_svn_copy_once = false;
                return true;
            }
        }
    }
    false
}

fn replay_fail_after_commits(_parent_repo_id: &str) -> u32 {
    #[cfg(debug_assertions)]
    {
        let hooks = PUBLISH_TEST_HOOKS.lock().unwrap();
        if let Some(hook) = hooks.get(_parent_repo_id) {
            return hook.fail_replay_after_commits;
        }
    }
    0
}

fn take_abort_after_svn_copy(_parent_repo_id: &str) -> bool {
    #[cfg(debug_assertions)]
    {
        let mut hooks = PUBLISH_TEST_HOOKS.lock().unwrap();
        if let Some(hook) = hooks.get_mut(_parent_repo_id) {
            if hook.abort_after_svn_copy_before_journal {
                hook.abort_after_svn_copy_before_journal = false;
                return true;
            }
        }
    }
    false
}

fn resolve_svn_copy_source_revision(
    in_flight: Option<&LatePairPublishOperation>,
    plan: &LatePairPlan,
    baseline_svn_rev: i64,
) -> i64 {
    if let Some(op) = in_flight {
        return op.svn_copy_source_rev;
    }
    plan.proposed_svn_copy_source_revision
        .unwrap_or(baseline_svn_rev)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatePairPublishRefusal {
    pub reason: String,
    pub detail: String,
    pub plan: Option<Box<LatePairPlan>>,
}

impl LatePairPublishRefusal {
    pub fn error_message(&self) -> String {
        format!("{}: {}", self.reason, redact_vcs_error_detail(&self.detail))
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
}

/// HTTPS clone URL for the parent's Git remote (never embeds credentials).
pub fn parent_git_clone_url(parent: &Repository) -> String {
    derive_git_remote_url(&parent.git_api_url, None, &parent.git_repo)
}

fn clone_url_needs_http_credentials(clone_url: &str) -> bool {
    clone_url.starts_with("https://") || clone_url.starts_with("http://")
}

/// Resolve an HTTP(S) token for clone/ls-remote. Never treats the token as a URL host.
pub fn http_git_token_for_clone<'a>(
    clone_url: &str,
    state: &'a CredentialChainState,
) -> Result<Option<&'a str>, LatePairPublishRefusal> {
    if !clone_url_needs_http_credentials(clone_url) {
        return Ok(None);
    }
    if state.explicitly_revoked {
        return Err(LatePairPublishRefusal {
            reason: "git_credentials_revoked".into(),
            detail: "Git token was explicitly revoked for this repository".into(),
            plan: None,
        });
    }
    match state.value.as_deref().filter(|t| !t.is_empty()) {
        Some(tok) => Ok(Some(tok)),
        None => Err(LatePairPublishRefusal {
            reason: "git_credentials_missing".into(),
            detail: "HTTP(S) Git remote requires a non-empty token from the credential chain"
                .into(),
            plan: None,
        }),
    }
}

/// Prove the feature branch is reachable before any SVN mutation.
pub fn validate_git_publish_preflight(
    parent: &Repository,
    git_branch: &str,
    db: &Database,
) -> Result<(), LatePairPublishRefusal> {
    let clone_url = parent_git_clone_url(parent);
    let token_state = db.resolve_credential_chain_state(&parent.id, "secret_git_token");
    let token = http_git_token_for_clone(&clone_url, &token_state)?;
    if !clone_url_needs_http_credentials(&clone_url) {
        return Ok(());
    }
    let refspec = format!("refs/heads/{git_branch}");
    let output = build_git_ls_remote_command(&clone_url, token, &refspec)
        .output()
        .map_err(|e| LatePairPublishRefusal {
            reason: "git_preflight_failed".into(),
            detail: format!("git ls-remote failed to start: {e}"),
            plan: None,
        })?;
    if !output.status.success() {
        return Err(LatePairPublishRefusal {
            reason: "git_preflight_failed".into(),
            detail:
                "Git feature branch is not reachable with stored credentials (ls-remote failed)"
                    .into(),
            plan: None,
        });
    }
    Ok(())
}

/// Child id for a new publish, or the journaled child when resuming the same fingerprint.
pub fn resolve_publish_child_id(
    db: &Database,
    parent_id: &str,
    fingerprint: &str,
    fallback: &str,
) -> Result<String, DatabaseError> {
    let Some(op) = db.latest_late_pair_publish_operation(parent_id)? else {
        return Ok(fallback.to_string());
    };
    if op.state.is_terminal() || op.target_fingerprint != fingerprint {
        return Ok(fallback.to_string());
    }
    Ok(op
        .child_repo_id
        .clone()
        .unwrap_or_else(|| fallback.to_string()))
}

pub fn publish_resuming(op: &LatePairPublishOperation) -> bool {
    !matches!(op.state, LatePairPublishState::Queued) && !op.state.is_terminal()
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

fn normalize_svn_repo_path(path: &str) -> String {
    path.trim_start_matches('/').to_string()
}

/// When the SVN target already exists, prove it is our journaled copy (not a foreign branch).
async fn verify_existing_svn_target_matches_copy_intent(
    parent: &Repository,
    svn_password: &str,
    svn_branch: &str,
    expected_source_path: &str,
    expected_copy_rev: i64,
) -> Result<i64, LatePairPublishRefusal> {
    let target_url = crate::svn::operand::configured_branch_url(&parent.svn_url, svn_branch)
        .map_err(|error| LatePairPublishRefusal {
            reason: "invalid_svn_target".into(),
            detail: redact_vcs_error_detail(&error.to_string()),
            plan: None,
        })?;
    let target = SvnClient::new(&target_url, &parent.svn_username, svn_password);
    let branch_tip = target
        .last_changed_revision()
        .await
        .map_err(|e| LatePairPublishRefusal {
            reason: "svn_verify_failed".into(),
            detail: e.to_string(),
            plan: None,
        })?;
    let root = SvnClient::new(
        parent.svn_url.trim_end_matches('/'),
        &parent.svn_username,
        svn_password,
    );
    let expected_src = normalize_svn_repo_path(expected_source_path);
    let branch_path = format!("/{}", svn_branch.trim_start_matches('/'));
    if branch_tip < 1 {
        return Err(LatePairPublishRefusal {
            reason: "existing_svn_target_blocks_publish".into(),
            detail:
                "existing SVN target does not match the journaled copy-from baseline for this publish"
                    .into(),
            plan: None,
        });
    }
    let start_rev = branch_tip.saturating_sub(8).max(1);
    for rev in (start_rev..=branch_tip).rev() {
        let entries = root
            .log(rev, rev)
            .await
            .map_err(|e| LatePairPublishRefusal {
                reason: "svn_verify_failed".into(),
                detail: e.to_string(),
                plan: None,
            })?;
        for entry in entries {
            for path in entry.changed_paths {
                let path_norm = normalize_svn_repo_path(&path.path);
                let branch_norm = normalize_svn_repo_path(&branch_path);
                if path_norm != branch_norm {
                    continue;
                }
                if let (Some(cf_path), Some(cf_rev)) =
                    (path.copy_from_path.as_deref(), path.copy_from_rev)
                {
                    if normalize_svn_repo_path(cf_path) == expected_src
                        && cf_rev == expected_copy_rev
                    {
                        return Ok(branch_tip);
                    }
                }
            }
        }
    }
    Err(LatePairPublishRefusal {
        reason: "existing_svn_target_blocks_publish".into(),
        detail:
            "existing SVN target does not match the journaled copy-from baseline for this publish"
                .into(),
        plan: None,
    })
}

fn ensure_child_git_workdir(
    data_dir: &Path,
    child_id: &str,
    parent: &Repository,
    git_branch: &str,
    db: &Database,
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
    let clone_url = parent_git_clone_url(parent);
    let token_state = db.resolve_credential_chain_state(&parent.id, "secret_git_token");
    let token = http_git_token_for_clone(&clone_url, &token_state)?;
    let git_client = GitClient::clone_repo(&clone_url, &git_repo_path, token).map_err(|_| {
        LatePairPublishRefusal {
            reason: "git_workdir_failed".into(),
            detail: "git clone failed for child workdir".into(),
            plan: None,
        }
    })?;
    apply_git_credential_chain_state(&git_client, "origin", &token_state).map_err(|_| {
        LatePairPublishRefusal {
            reason: "git_workdir_failed".into(),
            detail: "failed to apply git credentials to child workdir".into(),
            plan: None,
        }
    })?;
    let fetch = crate::git::subprocess_auth::git_cli_output(
        &git_repo_path,
        &[
            "fetch",
            "--no-tags",
            "origin",
            &format!("refs/heads/{git_branch}:refs/heads/{git_branch}"),
        ],
        token,
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
    let checkout = crate::git::subprocess_auth::git_cli_output(
        &git_repo_path,
        &["checkout", "-B", git_branch, git_branch],
        None,
    )
    .map_err(|e| LatePairPublishRefusal {
        reason: "git_workdir_failed".into(),
        detail: format!("git checkout failed: {e}"),
        plan: None,
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
    svn_uuid: Option<&str>,
) -> Result<(), LatePairPublishRefusal> {
    let now = chrono::Utc::now().to_rfc3339();
    crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
        &db.conn(),
        child_id,
        baseline_git_sha,
        &now,
    )
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
    let baseline_record = SyncRecord {
        id: uuid::Uuid::new_v4().to_string(),
        repo_id: Some(child_id.to_string()),
        svn_revision: Some(svn_rev),
        git_hash: Some(baseline_git_sha.to_string()),
        direction: SyncDirection::SvnToGit,
        author: "late_pair_publish".into(),
        message: "verified import baseline for late-pair publish".into(),
        timestamp: chrono::Utc::now(),
        synced_at: chrono::Utc::now(),
        status: SyncRecordStatus::Applied,
    };
    db.insert_sync_record(&baseline_record)
        .map_err(|e| LatePairPublishRefusal {
            reason: "watermark_failed".into(),
            detail: format!("failed to record baseline provenance: {e}"),
            plan: None,
        })?;
    if let Some(uuid) = svn_uuid.map(str::trim).filter(|uuid| !uuid.is_empty()) {
        crate::db::lineage_checkpoints::record_independently_established_svn_uuid(
            db, child_id, uuid,
        )
        .map_err(|e| LatePairPublishRefusal {
            reason: "watermark_failed".into(),
            detail: format!("failed to record independently established SVN UUID: {e}"),
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
    if take_fail_svn_copy_once(&parent.id) {
        return Err(LatePairPublishRefusal {
            reason: "svn_copy_failed".into(),
            detail: "test hook: simulated svn copy failure".into(),
            plan: None,
        });
    }
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
                    detail: redact_vcs_error_detail(&err),
                    plan: None,
                }
            }
        })?;
    let target_url = crate::svn::operand::configured_branch_url(&parent.svn_url, svn_branch)
        .map_err(|error| LatePairPublishRefusal {
            reason: "invalid_svn_target".into(),
            detail: redact_vcs_error_detail(&error.to_string()),
            plan: None,
        })?;
    let target = SvnClient::new(&target_url, &parent.svn_username, svn_password);
    let info = target.info().await.map_err(|e| LatePairPublishRefusal {
        reason: "svn_copy_failed".into(),
        detail: redact_vcs_error_detail(&e.to_string()),
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
    identity: &Arc<IdentityMapper>,
    pinned_tip: &str,
    baseline_git_sha: &str,
) -> Result<Vec<String>, LatePairPublishRefusal> {
    let handled = db
        .get_repository(&child.id)
        .ok()
        .flatten()
        .map(|r| r.last_git_sha.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            crate::echo_receipt_scope::read_scoped_last_git_sha_kv(db, &child.id)
                .ok()
                .flatten()
        });
    if handled.as_deref() == Some(pinned_tip) {
        return Ok(vec![]);
    }
    let svn_url = crate::svn::operand::configured_branch_url(&child.svn_url, &child.svn_branch)
        .map_err(|error| LatePairPublishRefusal {
            reason: "invalid_svn_target".into(),
            detail: redact_vcs_error_detail(&error.to_string()),
            plan: None,
        })?;
    let svn_client = SvnClient::new(&svn_url, &child.svn_username, svn_password);
    let git_client = GitClient::new(git_workdir).map_err(|e| LatePairPublishRefusal {
        reason: "git_workdir_failed".into(),
        detail: e.to_string(),
        plan: None,
    })?;
    crate::git::sync_git_http_auth_from_resolution(&git_client, git_workdir, db, &child.id, None)
        .ok();
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
    engine.set_git_credential_apply_clean(true);

    let mut replayed = Vec::new();
    let fail_after = child
        .parent_id
        .as_deref()
        .map(replay_fail_after_commits)
        .unwrap_or(0);
    for _ in 0..32 {
        let handled_before = engine
            .db()
            .get_repository(&child.id)
            .ok()
            .flatten()
            .map(|r| r.last_git_sha.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                crate::echo_receipt_scope::read_scoped_last_git_sha_kv(engine.db(), &child.id)
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
                detail: redact_vcs_error_detail(&e.to_string()),
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
                crate::echo_receipt_scope::read_scoped_last_git_sha_kv(engine.db(), &child.id)
                    .ok()
                    .flatten()
            })
            .unwrap_or_else(|| handled_before.clone());
        if handled_after != handled_before {
            replayed.push(handled_after.clone());
            if fail_after > 0 && replayed.len() as u32 >= fail_after {
                return Err(LatePairPublishRefusal {
                    reason: "replay_failed".into(),
                    detail: "test hook: simulated mid-replay failure".into(),
                    plan: None,
                });
            }
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
            crate::echo_receipt_scope::read_scoped_last_git_sha_kv(engine.db(), &child.id)
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
    let fingerprint = late_pair_publish_fingerprint(
        &parent.id,
        &request.git_branch,
        &request.svn_branch,
        &git_tip,
        &baseline.git_sha,
        baseline.svn_revision,
    );

    let mut test_hook_short_circuit: Option<LatePairPlan> = None;

    let latest = db
        .latest_late_pair_publish_operation(&parent.id)
        .map_err(db_err)?;

    if let Some(existing) = latest.as_ref().filter(|op| !op.state.is_terminal()) {
        if existing.target_fingerprint != fingerprint {
            return Err(LatePairPublishRefusal {
                reason: "publish_fingerprint_mismatch".into(),
                detail: "an in-flight late-pair publish targets a different plan".into(),
                plan: Some(Box::new(plan.clone())),
            });
        }
        if existing.git_branch != request.git_branch || existing.svn_branch != request.svn_branch {
            return Err(LatePairPublishRefusal {
                reason: "publish_branch_mismatch".into(),
                detail: "an in-flight late-pair publish targets different branch names".into(),
                plan: Some(Box::new(plan.clone())),
            });
        }
    }

    let in_flight = latest
        .filter(|existing| !existing.state.is_terminal())
        .filter(|existing| existing.target_fingerprint == fingerprint)
        .filter(|existing| {
            existing.git_branch == request.git_branch && existing.svn_branch == request.svn_branch
        });

    let copy_rev =
        resolve_svn_copy_source_revision(in_flight.as_ref(), plan, baseline.svn_revision);

    if in_flight.is_none() && probe.exists {
        let source_path = parent_svn_branch_path(parent);
        match verify_existing_svn_target_matches_copy_intent(
            parent,
            &creds.svn_password,
            &request.svn_branch,
            &source_path,
            copy_rev,
        )
        .await
        {
            Ok(_) => {
                return Err(LatePairPublishRefusal {
                    reason: "existing_svn_target_blocks_publish".into(),
                    detail: "existing SVN target without a resumable late-pair publish journal"
                        .into(),
                    plan: Some(Box::new(plan.clone())),
                });
            }
            Err(refusal) => return Err(refusal),
        }
    }

    let mut op = match in_flight {
        Some(existing) => existing,
        None => db
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

    let effective_child_id = op
        .child_repo_id
        .clone()
        .unwrap_or_else(|| child_id.to_string());

    if matches!(
        op.state,
        LatePairPublishState::Queued | LatePairPublishState::SvnCopyPending
    ) {
        validate_git_publish_preflight(parent, &request.git_branch, db)?;
    }

    let source_path = parent_svn_branch_path(parent);
    if op.state == LatePairPublishState::Queued {
        op.svn_copy_source_path = Some(source_path.clone());
        op.state = LatePairPublishState::SvnCopyPending;
        op = db.update_late_pair_publish_operation(op).map_err(db_err)?;
        info!(
            parent_id = %parent.id,
            svn_branch = %request.svn_branch,
            copy_source_rev = copy_rev,
            copy_source_path = %source_path,
            "late-pair SVN copy intent journaled before svn copy"
        );
    }

    if op.state == LatePairPublishState::SvnCopyPending {
        let intent_source = op
            .svn_copy_source_path
            .clone()
            .unwrap_or_else(|| source_path.clone());
        let svn_head = if probe.exists {
            verify_existing_svn_target_matches_copy_intent(
                parent,
                &creds.svn_password,
                &request.svn_branch,
                &intent_source,
                copy_rev,
            )
            .await?
        } else {
            svn_copy_at_baseline(
                parent,
                &creds.svn_password,
                &request.svn_branch,
                copy_rev,
                &intent_source,
            )
            .await?
        };
        if take_abort_after_svn_copy(&parent.id) {
            stash_pending_publish_refusal(
                &parent.id,
                LatePairPublishRefusal {
                    reason: "publish_test_hook".into(),
                    detail: "simulated failure after svn copy before journal".into(),
                    plan: None,
                },
            );
            op.svn_branch_head_rev = Some(svn_head);
            op = db.update_late_pair_publish_operation(op).map_err(db_err)?;
            test_hook_short_circuit = Some(refused_publish_plan_stub(
                plan, parent, request, probe, &baseline, &git_tip,
            ));
        } else if test_hook_short_circuit.is_none() {
            op.svn_branch_head_rev = Some(svn_head);
            op.state = LatePairPublishState::SvnCopied;
            op = db.update_late_pair_publish_operation(op).map_err(db_err)?;
        }
        info!(
            parent_id = %parent.id,
            svn_branch = %request.svn_branch,
            svn_head,
            resumed_existing = probe.exists,
            "late-pair SVN branch at verified baseline revision"
        );
    }

    if let Some(stub) = test_hook_short_circuit {
        return Ok(stub);
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
            id: effective_child_id.clone(),
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
        if db
            .get_repository(&effective_child_id)
            .map_err(db_err)?
            .is_none()
        {
            if let Err(error) =
                crate::db::repo_scope_identity::validate_human_repository_id(&child.id)
            {
                return Err(LatePairPublishRefusal {
                    reason: "child_insert_failed".into(),
                    detail: error.to_string(),
                    plan: Some(Box::new(plan.clone())),
                });
            }
            db.insert_repository(&child)
                .map_err(|e| LatePairPublishRefusal {
                    reason: "child_insert_failed".into(),
                    detail: e.to_string(),
                    plan: Some(Box::new(plan.clone())),
                })?;
        }
        let child_uuid = if let Some(uuid) = probe
            .uuid
            .as_deref()
            .map(str::trim)
            .filter(|uuid| !uuid.is_empty())
        {
            Some(uuid.to_string())
        } else {
            crate::db::lineage_checkpoints::independently_established_svn_uuid(
                &db.conn(),
                &parent.id,
            )
            .map_err(db_err)?
        };
        apply_baseline_watermarks(
            db,
            &effective_child_id,
            &baseline.git_sha,
            svn_head,
            child_uuid.as_deref(),
        )?;
        op.child_repo_id = Some(effective_child_id.clone());
        if take_fail_replay_once(&parent.id) {
            stash_pending_publish_refusal(
                &parent.id,
                LatePairPublishRefusal {
                    reason: "replay_incomplete".into(),
                    detail: "test hook: simulated replay failure".into(),
                    plan: None,
                },
            );
            op.state = LatePairPublishState::ReplayInProgress;
            op.outcome_detail = Some("test hook: simulated replay failure".into());
            op = db.update_late_pair_publish_operation(op).map_err(db_err)?;
            test_hook_short_circuit = Some(refused_publish_plan_stub(
                plan, parent, request, probe, &baseline, &git_tip,
            ));
        } else if test_hook_short_circuit.is_none() {
            op.state = LatePairPublishState::ChildRegistered;
            op = db.update_late_pair_publish_operation(op).map_err(db_err)?;
        }
    }

    if let Some(stub) = test_hook_short_circuit {
        return Ok(stub);
    }

    let child = db
        .get_repository(&effective_child_id)
        .map_err(db_err)?
        .ok_or_else(|| LatePairPublishRefusal {
            reason: "child_missing".into(),
            detail: "child repository row missing after registration".into(),
            plan: Some(Box::new(plan.clone())),
        })?;

    let git_workdir = ensure_child_git_workdir(
        &app_config.daemon.data_dir,
        &effective_child_id,
        parent,
        &child.git_branch,
        db,
    )?;

    if matches!(
        op.state,
        LatePairPublishState::ChildRegistered | LatePairPublishState::ReplayInProgress
    ) {
        op.state = LatePairPublishState::ReplayInProgress;
        op = db.update_late_pair_publish_operation(op).map_err(db_err)?;
        let replayed = match replay_pending_git(
            app_config,
            db,
            &child,
            &git_workdir,
            &creds.svn_password,
            identity,
            &git_tip,
            &baseline.git_sha,
        )
        .await
        {
            Ok(replayed) => replayed,
            Err(err) => {
                op.state = LatePairPublishState::ReplayInProgress;
                op.outcome_detail = Some(redact_vcs_error_detail(&err.detail));
                db.update_late_pair_publish_operation(op).map_err(db_err)?;
                return Err(err);
            }
        };
        op.replayed_git_shas = replayed;
        op = db.update_late_pair_publish_operation(op).map_err(db_err)?;
    }

    let finalized = db
        .finalize_late_pair_publish_enabling_child(
            &parent.id,
            &op.id,
            &effective_child_id,
            &git_tip,
        )
        .map_err(|e| LatePairPublishRefusal {
            reason: "finalize_failed".into(),
            detail: redact_vcs_error_detail(&e.to_string()),
            plan: Some(Box::new(plan.clone())),
        })?;
    if finalized.state != LatePairPublishState::Completed {
        return Err(LatePairPublishRefusal {
            reason: "finalize_refused".into(),
            detail: finalized
                .outcome_detail
                .clone()
                .unwrap_or_else(|| "late-pair publish finalize refused".into()),
            plan: Some(Box::new(plan.clone())),
        });
    }

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

/// Integration-test entry point for the production late-pair Git replay loop.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn replay_pending_git_for_integration_test(
    app_config: &AppConfig,
    db: &Database,
    child: &Repository,
    git_workdir: &std::path::Path,
    svn_password: &str,
    identity: &Arc<IdentityMapper>,
    pinned_tip: &str,
    baseline_git_sha: &str,
) -> Result<Vec<String>, LatePairPublishRefusal> {
    replay_pending_git(
        app_config,
        db,
        child,
        git_workdir,
        svn_password,
        identity,
        pinned_tip,
        baseline_git_sha,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::queries::{CredentialChainState, CredentialOrigin};

    #[test]
    fn parent_git_clone_url_derives_from_api_not_credentials() {
        let url = parent_git_clone_url(&Repository {
            id: "p".into(),
            name: "P".into(),
            svn_url: String::new(),
            svn_branch: String::new(),
            svn_username: String::new(),
            git_provider: "github".into(),
            git_api_url: "https://api.github.com".into(),
            git_repo: "acme/widget".into(),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 60,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: String::new(),
            updated_at: String::new(),
            last_svn_rev: 0,
            last_git_sha: String::new(),
            last_sync_at: None,
            sync_status: String::new(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: None,
            blocked_patterns: None,
            consecutive_errors: 0,
            teams_webhook_url: None,
        });
        assert_eq!(url, "https://github.com/acme/widget.git");
        assert!(!url.contains("ghp_"));
        assert!(!url.contains("x-access-token"));
    }

    #[test]
    fn http_git_token_rejects_empty_on_https() {
        let state = CredentialChainState {
            value: Some(String::new()),
            explicitly_revoked: false,
            origin: CredentialOrigin::Explicit,
        };
        let err = http_git_token_for_clone("https://github.com/o/r.git", &state).unwrap_err();
        assert_eq!(err.reason, "git_credentials_missing");
    }

    #[test]
    fn http_git_token_accepts_non_empty_on_https() {
        let state = CredentialChainState {
            value: Some("ghp_secret".into()),
            explicitly_revoked: false,
            origin: CredentialOrigin::Explicit,
        };
        assert_eq!(
            http_git_token_for_clone("https://github.com/o/r.git", &state).unwrap(),
            Some("ghp_secret")
        );
    }

    #[test]
    fn http_git_token_skipped_for_file_remote() {
        let state = CredentialChainState {
            value: None,
            explicitly_revoked: false,
            origin: CredentialOrigin::Absent,
        };
        assert_eq!(
            http_git_token_for_clone("file:///tmp/repo.git", &state).unwrap(),
            None
        );
    }

    #[test]
    fn preflight_ls_remote_command_argv_has_no_token() {
        use crate::git::subprocess_auth::command_args_contain_secret;

        let token = "ghp_test_preflight_argv_secret";
        let cmd = build_git_ls_remote_command(
            "https://github.com/acme/widget.git",
            Some(token),
            "refs/heads/feature",
        );
        assert!(!command_args_contain_secret(&cmd, token));
        assert!(!command_args_contain_secret(&cmd, "x-access-token"));
    }

    #[test]
    fn refusal_error_message_redacts_credentialed_remote_detail() {
        let refusal = LatePairPublishRefusal {
            reason: "replay_failed".into(),
            detail: "git fetch failed: fatal: https://x-access-token:ghp_LEAKED@github.com/o/r.git"
                .into(),
            plan: None,
        };
        let msg = refusal.error_message();
        assert!(!msg.contains("ghp_LEAKED"));
        assert!(msg.contains("[REDACTED]"));
    }

    #[test]
    fn publish_resuming_after_svn_copied_phase() {
        use crate::db::late_pair_publish_operations::LatePairPublishOperation;

        let op = LatePairPublishOperation {
            version: 1,
            id: "op".into(),
            parent_repo_id: "parent".into(),
            child_repo_id: None,
            operation_type: "late_pair_publish".into(),
            initiator_id: "i".into(),
            request_id: "r".into(),
            target_fingerprint: "fp".into(),
            created_at: String::new(),
            updated_at: String::new(),
            state: LatePairPublishState::Queued,
            policy_version: PUBLISH_POLICY_VERSION.into(),
            git_branch: "feature".into(),
            svn_branch: "branches/feature".into(),
            pinned_git_tip: "tip".into(),
            baseline_git_sha: "base".into(),
            baseline_svn_rev: 2,
            svn_copy_source_rev: 2,
            svn_copy_source_path: None,
            svn_branch_head_rev: None,
            replayed_git_shas: Vec::new(),
            outcome_detail: None,
            reconciliation: None,
        };
        assert!(!publish_resuming(&op));
        let mut copied = op.clone();
        copied.state = LatePairPublishState::SvnCopied;
        copied.svn_branch_head_rev = Some(10);
        assert!(publish_resuming(&copied));
        let mut pending = op.clone();
        pending.state = LatePairPublishState::SvnCopyPending;
        pending.svn_copy_source_path = Some("trunk".into());
        assert!(publish_resuming(&pending));
    }
}
