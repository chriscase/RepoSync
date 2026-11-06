//! A no-advance result is an independently observed, signed receipt. It never
//! manufactures a merge commit, an SVN revision or a completed publication job.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::actor_policy::LiveRefreshActorPolicy;
use super::execution_configuration::{ExecutionConfigurationPin, LiveExecutionConfiguration};
use super::git_sources::GitSourceSnapshot;
use super::lineage::{SvnLineagePins, SvnRefreshLineagePins, SvnRefreshLineageProof};
use super::svn_sources::SvnSourceSnapshot;
use super::svn_visibility::SvnPresencePin;
use super::workspace::RefreshWorkspace;
use crate::busy::PairBusyGuard;
use crate::db::pair_refresh_operations::{PairRefreshOperation, RefreshRegistrationPin};
use crate::db::Database;
use crate::errors::DatabaseError;
use crate::sync_engine::SyncEngine;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoopRefreshPins {
    pub operation_id: Uuid,
    pub child: RefreshRegistrationPin,
    pub parent: RefreshRegistrationPin,
    pub configuration: Value,
    pub baseline_operation_id: Option<Uuid>,
    pub parent_baseline_sha: String,
    pub git_tree: String,
    pub lineage: SvnRefreshLineageProof,
    pub previous_mergeinfo: Option<String>,
    pub inspected_head_revision: i64,
    pub visibility: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoopRefreshReceipt {
    pub version: u8,
    pub id: Uuid,
    pub initiator_id: String,
    pub request_id: String,
    pub approval_digest: String,
    pub admitted_writer_epoch: u64,
    pub completed_at: String,
    pub pins: NoopRefreshPins,
}

/// Only actual source observation constructs this value. Serialized pins cannot
/// authorize completion, ownership transfer or cleanup.
pub struct LivePreparedNoopRefresh {
    pins: NoopRefreshPins,
    configuration: LiveExecutionConfiguration,
    git: GitSourceSnapshot,
    svn: SvnSourceSnapshot,
    visibility: Vec<SvnPresencePin>,
    baseline: Option<PairRefreshOperation>,
    workspace: RefreshWorkspace,
}

impl LivePreparedNoopRefresh {
    pub fn pins(&self) -> &NoopRefreshPins {
        &self.pins
    }
    pub(crate) fn configuration(&self) -> &LiveExecutionConfiguration {
        &self.configuration
    }
    pub(crate) fn workspace(&self) -> &RefreshWorkspace {
        &self.workspace
    }
    pub(crate) fn visibility(&self) -> &[SvnPresencePin] {
        &self.visibility
    }
    pub(crate) fn baseline(&self) -> &Option<PairRefreshOperation> {
        &self.baseline
    }
}

pub(crate) struct NoopObservationProof<'source> {
    engine: &'source SyncEngine,
    prepared: &'source LivePreparedNoopRefresh,
    configuration: ExecutionConfigurationPin,
}

impl NoopObservationProof<'_> {
    pub(crate) fn require_binding(
        &self,
        engine: &SyncEngine,
        prepared: &LivePreparedNoopRefresh,
        current: &LiveExecutionConfiguration,
    ) -> Result<(), DatabaseError> {
        if !std::ptr::eq(self.engine, engine)
            || !std::ptr::eq(self.prepared, prepared)
            || self.configuration != *current.pin()
        {
            return Err(DatabaseError::Other(
                "fresh no-op evidence belongs to different inputs".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) async fn prepare_registered_noop(
    db: &Database,
    child_id: &str,
    workspace: RefreshWorkspace,
) -> Result<LivePreparedNoopRefresh> {
    prepare_registered_noop_with_client(
        db,
        child_id,
        workspace,
        super::git_sources::source_client()?,
    )
    .await
}

pub(crate) async fn prepare_registered_noop_with_client(
    db: &Database,
    child_id: &str,
    workspace: RefreshWorkspace,
    client: reqwest::Client,
) -> Result<LivePreparedNoopRefresh> {
    workspace.require_current()?;
    let configuration = db.capture_pair_refresh_execution_configuration(child_id)?;
    let baseline = db.capture_completed_pair_refresh_baseline(&configuration)?;
    let git_workspace = workspace.clone();
    let (configuration, git) = tokio::task::spawn_blocking(move || {
        let git = super::git_sources::observe_in_workspace_with_client(
            &configuration,
            git_workspace,
            client,
        )?;
        Ok::<_, anyhow::Error>((configuration, git))
    })
    .await
    .context("owned no-op Git observation task failed")??;
    let svn = super::svn_sources::observe_registered_svn_sources(&configuration).await?;
    let (pins, visibility) = observe_noop(
        &configuration,
        &git,
        &svn,
        baseline.as_ref(),
        workspace.pin().operation_id,
    )
    .await?;
    let current = db.capture_pair_refresh_execution_configuration(child_id)?;
    if current.pin() != configuration.pin()
        || db.capture_completed_pair_refresh_baseline(&current)? != baseline
    {
        bail!("registered no-op inputs or baseline changed during observation");
    }
    workspace.require_current()?;
    Ok(LivePreparedNoopRefresh {
        pins,
        configuration,
        git,
        svn,
        visibility,
        baseline,
        workspace,
    })
}

async fn observe_noop(
    configuration: &LiveExecutionConfiguration,
    git: &GitSourceSnapshot,
    svn: &SvnSourceSnapshot,
    baseline: Option<&PairRefreshOperation>,
    operation_id: Uuid,
) -> Result<(NoopRefreshPins, Vec<SvnPresencePin>)> {
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
    let copy = super::lineage::inspect_svn_copy_origin(svn.child_client(), &probe).await?;
    let (baseline_sha, baseline_revision, previous_mergeinfo) = if let Some(prior) = baseline {
        let receipt = prior
            .svn_receipt
            .as_ref()
            .context("completed no-op baseline has no receipt")?;
        let mut expected_copy = receipt.child_copy.clone();
        expected_copy.inspected_revision = source.child.last_svn_rev;
        if copy != expected_copy {
            bail!("no-op copy origin differs from signed completed baseline");
        }
        (
            prior.pins.parent.last_git_sha.clone(),
            prior.pins.parent.last_svn_rev,
            Some(receipt.mergeinfo.clone()),
        )
    } else {
        if source.child.last_svn_rev != copy.created_revision {
            bail!("first no-op requires the unchanged original SVN copy");
        }
        (
            source.child.last_git_sha.clone(),
            copy.copy_from_revision,
            None,
        )
    };
    if source.parent.last_git_sha != baseline_sha || source.parent.last_svn_rev != baseline_revision
    {
        bail!("no-op requires no parent advance in either history");
    }
    let lineage = super::lineage::inspect_svn_refresh_lineage(
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
    if baseline.is_some_and(|prior| {
        prior.pins.svn.pins.lineage.parent_originating_revision
            != lineage.parent_originating_revision
    }) {
        bail!("no-op parent incarnation differs from signed completed baseline");
    }
    super::publication::require_complete_git_graph(git.repository())?;
    let mut tree = None;
    for sha in [
        &source.child.last_git_sha,
        &source.parent.last_git_sha,
        &baseline_sha,
    ] {
        let commit = git.repository().find_commit(git2::Oid::from_str(sha)?)?;
        super::preparation::require_supported_refresh_tree(git.repository(), &commit.tree()?)?;
        if tree.is_some_and(|expected| expected != commit.tree_id()) {
            bail!("no-op input trees disagree");
        }
        tree = Some(commit.tree_id());
    }
    let tree = tree.context("no-op tree is unavailable")?.to_string();
    if baseline.is_some_and(|prior| prior.pins.git.tree_sha != tree) {
        bail!("no-op tree differs from completed publication evidence");
    }
    let mut visibility = Vec::new();
    for (client, revision, checkpoint, role, mergeinfo) in [
        (
            svn.parent_client(),
            baseline_revision,
            baseline_revision,
            "baseline",
            None,
        ),
        (
            svn.parent_client(),
            source.parent.last_svn_rev,
            source.parent.last_svn_rev,
            "parent",
            None,
        ),
        (
            svn.child_client(),
            source.child.last_svn_rev,
            source.child.last_svn_rev,
            "child",
            previous_mergeinfo.as_deref(),
        ),
        (
            svn.parent_client(),
            observed.inspected_head_revision,
            source.parent.last_svn_rev,
            "current_parent",
            None,
        ),
        (
            svn.child_client(),
            observed.inspected_head_revision,
            source.child.last_svn_rev,
            "current_child",
            previous_mergeinfo.as_deref(),
        ),
    ] {
        let checkout = super::svn_visibility::observe_subtree(client, revision).await?;
        checkout.require_unchanged_since(checkpoint)?;
        let info = checkout.info();
        if info.uuid != lineage.pins.child.repository_uuid
            || info.root_url != lineage.pins.repository_root_url
            || info.url != client.url()
        {
            bail!("no-op visibility differs from approved lineage");
        }
        let mut pin = super::svn_visibility::presence_pin(role, checkout.path(), info)?;
        pin.ancestors =
            super::svn_visibility::require_empty_visible_ancestors(client, info).await?;
        let remote = super::svn_preparation::bound_checkout_property_xml(
            &client.tree_properties_at_rev(revision).await?,
            client.url(),
            checkout.path(),
            info,
        )?;
        let local =
            super::svn_preparation::visible_checkout_properties(client, checkout.path(), info)
                .await?;
        let meaningful = |mut properties: super::svn_preparation::SvnPropertySnapshot| {
            properties.retain(|path, values| path.is_empty() || !values.is_empty());
            properties
        };
        if meaningful(remote.clone()) != meaningful(local) {
            bail!("no-op remote and local property proof disagree");
        }
        super::svn_preparation::verify_refresh_svn_tree(
            git.repository().path(),
            &tree,
            checkout.path(),
            &remote,
            mergeinfo,
            &lineage.pins.child.parent_path,
        )?;
        visibility.push(pin);
    }
    for client in [svn.child_client(), svn.parent_client()] {
        let info = client.info().await?;
        if info.uuid != observed.repository_uuid
            || info.root_url != observed.repository_root_url
            || info.url != client.url()
            || info.latest_rev != observed.inspected_head_revision
        {
            bail!("SVN changed during no-op observation");
        }
    }
    Ok((
        NoopRefreshPins {
            operation_id,
            child: source.child.clone(),
            parent: source.parent.clone(),
            configuration: serde_json::to_value(source)?,
            baseline_operation_id: baseline.map(|op| op.id),
            parent_baseline_sha: baseline_sha,
            git_tree: tree,
            lineage,
            previous_mergeinfo,
            inspected_head_revision: observed.inspected_head_revision,
            visibility: serde_json::to_value(&visibility)?,
        },
        visibility,
    ))
}

pub async fn complete_owned_noop_refresh(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedNoopRefresh,
    request: &str,
    approved_digest: &str,
    busy: &PairBusyGuard,
) -> Result<NoopRefreshReceipt> {
    complete_owned_noop_with_client(
        engine,
        actor,
        prepared,
        request,
        approved_digest,
        busy,
        super::git_sources::source_client()?,
    )
    .await
}

pub(crate) async fn complete_owned_noop_with_client(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedNoopRefresh,
    request: &str,
    approved_digest: &str,
    busy: &PairBusyGuard,
    client: reqwest::Client,
) -> Result<NoopRefreshReceipt> {
    actor.require_current(engine)?;
    prepared
        .workspace
        .require_binding(engine.db(), actor.pin())?;
    let configuration = engine
        .db()
        .capture_pair_refresh_execution_configuration(&prepared.pins.child.repo_id)?;
    if configuration.pin() != prepared.configuration.pin() {
        bail!("no-op configuration drifted after approval");
    }
    let baseline = engine
        .db()
        .capture_completed_pair_refresh_baseline(&configuration)?;
    if baseline != prepared.baseline {
        bail!("no-op completed baseline drifted after approval");
    }
    let (configuration, fresh_git) = tokio::task::spawn_blocking(move || {
        let git = super::git_sources::observe_with_client(&configuration, client)?;
        Ok::<_, anyhow::Error>((configuration, git))
    })
    .await
    .context("fresh no-op Git task failed")??;
    let fresh_svn = super::svn_sources::observe_registered_svn_sources(&configuration).await?;
    if fresh_git.pins() != prepared.git.pins() || fresh_svn.pins() != prepared.svn.pins() {
        bail!("no-op sources drifted after approval");
    }
    let (pins, _) = observe_noop(
        &configuration,
        &fresh_git,
        &fresh_svn,
        baseline.as_ref(),
        prepared.pins.operation_id,
    )
    .await?;
    if pins != prepared.pins {
        bail!("complete no-op evidence changed after approval");
    }
    let proof = NoopObservationProof {
        engine,
        prepared,
        configuration: configuration.pin().clone(),
    };
    Ok(engine.db().complete_authenticated_noop(
        engine,
        actor,
        prepared,
        (request, approved_digest),
        busy,
        proof,
    )?)
}
