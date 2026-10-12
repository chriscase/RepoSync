//! Assemble independently observed inputs and both unreferenced/local results.
//! This preparation does not authorize an actor, admit a job or publish.

use anyhow::{bail, Context, Result};
use uuid::Uuid;

use crate::db::pair_refresh_operations::{PairRefreshOperation, RefreshJobPins};
use crate::db::Database;

use super::execution_configuration::LiveExecutionConfiguration;
use super::git_sources::{observe_registered_git_sources, GitSourceSnapshot};
use super::lineage::{
    inspect_svn_copy_origin, inspect_svn_refresh_lineage, SvnLineagePins, SvnRefreshLineagePins,
};
use super::preparation::{prepare_clean_child_git, CleanChildGitPins};
use super::svn_preparation::{prepare_clean_child_svn, CleanChildSvnPins};
use super::svn_sources::{observe_registered_svn_sources, SvnSourceSnapshot};

/// These resources belong only to this preparation. A durable execution caller
/// must arrange workspace retention/reconstruction before using the pins for
/// admission. Dropping this read-only preparation cleans its private resources.
/// There is intentionally no Debug/Serialize implementation for live clients.
pub struct LivePreparedRefresh {
    pins: RefreshJobPins,
    configuration: LiveExecutionConfiguration,
    git: GitSourceSnapshot,
    svn: SvnSourceSnapshot,
    _operation_directory: tempfile::TempDir,
}

impl LivePreparedRefresh {
    pub fn pins(&self) -> &RefreshJobPins {
        &self.pins
    }
    pub fn configuration(&self) -> &LiveExecutionConfiguration {
        &self.configuration
    }
    pub fn git(&self) -> &GitSourceSnapshot {
        &self.git
    }
    pub fn svn(&self) -> &SvnSourceSnapshot {
        &self.svn
    }
}

/// Prepare the bounded first/next refresh from actual registered sources.
/// Read-only Git observation runs on a blocking thread, separately from the
/// asynchronous SVN commands. No shared worktree, ref or checkpoint changes.
pub async fn prepare_registered_pair_refresh(
    db: &Database,
    child_id: &str,
    operation_id: Uuid,
    prepared_at: i64,
) -> Result<LivePreparedRefresh> {
    if operation_id.is_nil() || prepared_at < 0 {
        bail!("pair refresh preparation requires a valid operation identity and time");
    }
    let configuration = db.capture_pair_refresh_execution_configuration(child_id)?;
    let baseline = db.capture_completed_pair_refresh_baseline(&configuration)?;
    let (configuration, git) = tokio::task::spawn_blocking(move || {
        let git = observe_registered_git_sources(&configuration)?;
        Ok::<_, anyhow::Error>((configuration, git))
    })
    .await
    .context("pair refresh Git observation task failed")??;
    let svn = observe_registered_svn_sources(&configuration).await?;
    prepare_observed_sources(
        db,
        configuration,
        git,
        svn,
        baseline,
        operation_id,
        prepared_at,
    )
    .await
}

async fn prepare_observed_sources(
    db: &Database,
    configuration: LiveExecutionConfiguration,
    git: GitSourceSnapshot,
    svn: SvnSourceSnapshot,
    baseline: Option<PairRefreshOperation>,
    operation_id: Uuid,
    prepared_at: i64,
) -> Result<LivePreparedRefresh> {
    let source = configuration.pin();
    let observed = svn.pins();
    let probe = SvnLineagePins {
        repository_uuid: observed.repository_uuid.clone(),
        child_path: observed.child_path.clone(),
        parent_path: observed.parent_path.clone(),
        child_revision: source.child.last_svn_rev,
        child_mapping_revision: source.child.last_svn_rev,
        parent_baseline_revision: source.parent.last_svn_rev,
    };
    let copy = inspect_svn_copy_origin(svn.child_client(), &probe).await?;
    let (baseline_sha, baseline_revision, previous_mergeinfo) = match baseline.as_ref() {
        Some(prior) => {
            let receipt = prior
                .svn_receipt
                .as_ref()
                .context("completed refresh baseline lacks its receipt")?;
            let mut expected_copy = receipt.child_copy.clone();
            expected_copy.inspected_revision = source.child.last_svn_rev;
            if copy != expected_copy {
                bail!("current copy origin differs from completed refresh evidence");
            }
            (
                prior.pins.parent.last_git_sha.clone(),
                prior.pins.parent.last_svn_rev,
                Some(receipt.mergeinfo.clone()),
            )
        }
        None => {
            // The first supported case has no SVN work after the original copy.
            // Its CURRENT scoped Git checkpoint must itself be a shared parent
            // commit. No historical human-ID mapping or arbitrary equal-tree
            // commit is selected as a replacement baseline.
            if source.child.last_svn_rev != copy.created_revision {
                bail!("first refresh requires the unchanged original SVN copy checkpoint");
            }
            (
                source.child.last_git_sha.clone(),
                copy.copy_from_revision,
                None,
            )
        }
    };
    let lineage = inspect_svn_refresh_lineage(
        svn.child_client(),
        svn.parent_client(),
        &SvnRefreshLineagePins {
            child: SvnLineagePins {
                parent_baseline_revision: baseline_revision,
                ..probe
            },
            repository_root_url: observed.repository_root_url.clone(),
            child_url: source.child_svn_url.clone(),
            parent_url: source.parent_svn_url.clone(),
            parent_revision: source.parent.last_svn_rev,
        },
    )
    .await?;
    if let Some(prior) = baseline.as_ref() {
        if lineage.parent_originating_revision
            != prior.pins.svn.pins.lineage.parent_originating_revision
        {
            bail!("parent incarnation differs from completed refresh evidence");
        }
    }
    let prepared_git = prepare_clean_child_git(
        git.repository(),
        &CleanChildGitPins {
            child_tip: source.child.last_git_sha.clone(),
            parent_tip: source.parent.last_git_sha.clone(),
            parent_baseline_tip: baseline_sha.clone(),
        },
        operation_id,
        prepared_at,
    )?;
    let baseline_tree = git
        .repository()
        .find_commit(git2::Oid::from_str(&baseline_sha)?)?
        .tree_id()
        .to_string();
    if let Some(prior) = baseline.as_ref() {
        if baseline_tree != prior.pins.git.tree_sha {
            bail!("completed baseline Git tree differs from its publication evidence");
        }
    }
    let operation_directory = tempfile::Builder::new()
        .prefix("reposync-refresh-preparation-")
        .tempdir()?;
    let prepared_svn = prepare_clean_child_svn(
        svn.child_client(),
        svn.parent_client(),
        git.repository().path(),
        operation_directory.path(),
        operation_id,
        &CleanChildSvnPins {
            lineage,
            baseline_git_tree: baseline_tree,
            parent_git_tree: prepared_git.tree_sha.clone(),
            previous_mergeinfo,
        },
    )
    .await?;
    let pins = RefreshJobPins {
        child: source.child.clone(),
        parent: source.parent.clone(),
        git_remote_url: source.git_remote_url.clone(),
        git_branch: source.child_git_branch.clone(),
        transport_policy: source.transport_policy,
        credential_context_digest: source.credential_context_digest.clone(),
        git: prepared_git,
        svn: prepared_svn,
    };
    // Preparation takes time. Drift or a newly admitted hold cannot be hidden
    // behind the earlier read transaction. Fresh remote checks at publication
    // intent, actor approval, effective policy and durable retention are still
    // mandatory responsibilities of the unfinished execution caller.
    let current = db.capture_pair_refresh_execution_configuration(&source.child.repo_id)?;
    if current.pin() != source || db.capture_completed_pair_refresh_baseline(&current)? != baseline
    {
        bail!("registered inputs or baseline evidence changed during preparation");
    }
    db.require_registered_pair_refresh_preparation(&current, &pins)?;
    Ok(LivePreparedRefresh {
        pins,
        configuration,
        git,
        svn,
        _operation_directory: operation_directory,
    })
}

#[cfg(test)]
#[path = "live_preparation_tests.rs"]
mod tests;
