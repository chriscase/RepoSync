//! Owned SVN/Git publication and atomic child checkpoint completion.
//! An existing intent is observation/reconciliation state, never retry permission.

use anyhow::{bail, Context, Result};

use super::actor_policy::LiveRefreshActorPolicy;
use super::execution_configuration::{ExecutionConfigurationPin, LiveExecutionConfiguration};
use super::live_preparation::LivePreparedRefresh;
use super::svn_observation::{SvnPublicationObservation, SvnRefreshPublicationReceipt};
use crate::busy::PairBusyGuard;
use crate::db::pair_refresh_operations::{PairRefreshOperation, PairRefreshPhase};
use crate::errors::DatabaseError;
use crate::sync_engine::SyncEngine;

pub(crate) struct SvnIntentProof<'source> {
    engine: &'source SyncEngine,
    prepared: &'source LivePreparedRefresh,
    configuration: ExecutionConfigurationPin,
}

impl SvnIntentProof<'_> {
    pub(crate) fn require_binding(
        &self,
        engine: &SyncEngine,
        prepared: &LivePreparedRefresh,
        configuration: &LiveExecutionConfiguration,
    ) -> Result<(), DatabaseError> {
        if !std::ptr::eq(self.engine, engine)
            || !std::ptr::eq(self.prepared, prepared)
            || self.configuration != *configuration.pin()
        {
            return Err(DatabaseError::Other(
                "fresh SVN intent evidence belongs to different inputs".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) struct SvnReceiptProof<'source> {
    engine: &'source SyncEngine,
    prepared: &'source LivePreparedRefresh,
    configuration: ExecutionConfigurationPin,
    receipt: SvnRefreshPublicationReceipt,
}

pub(crate) struct GitIntentProof<'source> {
    engine: &'source SyncEngine,
    prepared: &'source LivePreparedRefresh,
    configuration: ExecutionConfigurationPin,
    svn_receipt: SvnRefreshPublicationReceipt,
}

impl GitIntentProof<'_> {
    pub(crate) fn require_binding(
        &self,
        engine: &SyncEngine,
        prepared: &LivePreparedRefresh,
        configuration: &LiveExecutionConfiguration,
    ) -> Result<(), DatabaseError> {
        if !std::ptr::eq(self.engine, engine)
            || !std::ptr::eq(self.prepared, prepared)
            || self.configuration != *configuration.pin()
        {
            return Err(DatabaseError::Other(
                "fresh Git intent evidence belongs to different inputs".into(),
            ));
        }
        Ok(())
    }
    pub(crate) fn svn_receipt(&self) -> &SvnRefreshPublicationReceipt {
        &self.svn_receipt
    }
}

pub(crate) struct GitReceiptProof<'source> {
    engine: &'source SyncEngine,
    prepared: &'source LivePreparedRefresh,
    configuration: ExecutionConfigurationPin,
    svn_receipt: SvnRefreshPublicationReceipt,
    sha: String,
}

impl GitReceiptProof<'_> {
    pub(crate) fn require_binding(
        &self,
        engine: &SyncEngine,
        prepared: &LivePreparedRefresh,
        configuration: &LiveExecutionConfiguration,
    ) -> Result<(), DatabaseError> {
        if !std::ptr::eq(self.engine, engine)
            || !std::ptr::eq(self.prepared, prepared)
            || self.configuration != *configuration.pin()
            || self.sha != prepared.pins().git.prepared_sha
        {
            return Err(DatabaseError::Other(
                "complete Git receipt evidence belongs to different inputs".into(),
            ));
        }
        Ok(())
    }
    pub(crate) fn svn_receipt(&self) -> &SvnRefreshPublicationReceipt {
        &self.svn_receipt
    }
    pub(crate) fn sha(&self) -> &str {
        &self.sha
    }
}

impl SvnReceiptProof<'_> {
    pub(crate) fn require_binding(
        &self,
        engine: &SyncEngine,
        prepared: &LivePreparedRefresh,
        configuration: &LiveExecutionConfiguration,
    ) -> Result<(), DatabaseError> {
        if !std::ptr::eq(self.engine, engine)
            || !std::ptr::eq(self.prepared, prepared)
            || self.configuration != *configuration.pin()
        {
            return Err(DatabaseError::Other(
                "complete SVN receipt evidence belongs to different inputs".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn receipt(&self) -> &SvnRefreshPublicationReceipt {
        &self.receipt
    }
}

/// Execute only the first SVN stage for this actual owned preparation. A
/// returned SvnObserved receipt keeps the global barrier for the remaining Git
/// stage. This entry point does not enable the legacy HTTP execute request.
pub async fn publish_owned_svn_stage(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
    request: &str,
    approved_digest: &str,
    busy: &PairBusyGuard,
) -> Result<PairRefreshOperation> {
    publish_owned_svn_stage_with_client(
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

/// Complete the owned advanced-parent integration through normal SVN then Git
/// publication and the authenticated atomic child checkpoint transaction.
/// Retained uncertain intents require a separate observer/recovery entrance.
pub async fn complete_owned_refresh(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
    request: &str,
    approved_digest: &str,
    busy: &PairBusyGuard,
) -> Result<PairRefreshOperation> {
    complete_owned_refresh_with_client(
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

/// Independently observe an existing intent using the actual original owner,
/// retained preparation and current credentials. This never republishes, adopts
/// resources or transfers a writer epoch. An absent result retains the intent.
pub async fn reconcile_owned_refresh_intent(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
    busy: &PairBusyGuard,
) -> Result<PairRefreshOperation> {
    reconcile_owned_refresh_intent_with_client(
        engine,
        actor,
        prepared,
        busy,
        super::git_sources::source_client()?,
    )
    .await
}

pub(crate) async fn reconcile_owned_refresh_intent_with_client(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
    busy: &PairBusyGuard,
    client: reqwest::Client,
) -> Result<PairRefreshOperation> {
    let operation = engine
        .db()
        .pair_refresh_operation(prepared.pins().git.operation_id)?;
    let configuration = engine.db().require_authenticated_reconciliation_inputs(
        engine,
        actor,
        prepared,
        busy,
        operation.phase,
    )?;
    let receipt = observe_complete_svn_receipt(prepared, &configuration).await?;
    if operation.phase == PairRefreshPhase::SvnIntent {
        let proof = SvnReceiptProof {
            engine,
            prepared,
            configuration: configuration.pin().clone(),
            receipt,
        };
        return Ok(engine
            .db()
            .record_authenticated_svn_receipt(engine, actor, prepared, busy, proof)?);
    }
    if operation.svn_receipt.as_ref() != Some(&receipt) {
        bail!("SVN result changed during intent reconciliation; the global hold is retained");
    }
    let path = prepared.git().repository().path().to_owned();
    let git = prepared.pins().git.clone();
    let (configuration, sha) = tokio::task::spawn_blocking(move || {
        let repository = git2::Repository::open_bare(path)?;
        let sha = super::git_sources::observe_completed_pair_with_client(
            &configuration,
            &repository,
            &git,
            client,
        )?;
        Ok::<_, anyhow::Error>((configuration, sha.to_string()))
    })
    .await
    .context("independent Git reconciliation task failed; the intent remains held")??;
    // Re-read SVN after both Git observations. No old or absent observation
    // grants another push, and fresh actor/input checks still precede signing.
    let current = observe_complete_svn_receipt(prepared, &configuration).await?;
    if current != receipt {
        bail!("SVN result changed during Git reconciliation; the global hold is retained");
    }
    let proof = GitReceiptProof {
        engine,
        prepared,
        configuration: configuration.pin().clone(),
        svn_receipt: current,
        sha,
    };
    Ok(engine
        .db()
        .record_authenticated_git_receipt(engine, actor, prepared, busy, &proof)?)
}

pub(crate) async fn complete_owned_refresh_with_client(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
    request: &str,
    approved_digest: &str,
    busy: &PairBusyGuard,
    client: reqwest::Client,
) -> Result<PairRefreshOperation> {
    let op = publish_owned_svn_stage_with_client(
        engine,
        actor,
        prepared,
        request,
        approved_digest,
        busy,
        client.clone(),
    )
    .await?;
    match op.phase {
        PairRefreshPhase::Completed | PairRefreshPhase::CancelledBeforePublication => return Ok(op),
        PairRefreshPhase::SvnObserved | PairRefreshPhase::GitObserved => {}
        _ => bail!("retained publication intent requires independent reconciliation; no publication was retried"),
    }
    let configuration = engine
        .db()
        .require_authenticated_git_inputs(engine, actor, prepared, busy, op.phase)?;
    let configuration = if op.phase == PairRefreshPhase::SvnObserved {
        let source_client = client.clone();
        let (configuration, fresh_git) = tokio::task::spawn_blocking(move || {
            let git = super::git_sources::observe_with_client(&configuration, source_client)?;
            Ok::<_, anyhow::Error>((configuration, git))
        })
        .await
        .context("fresh pre-push Git observation task failed")??;
        if fresh_git.pins() != prepared.git().pins() {
            bail!("Git inputs drifted after SVN publication; the global hold is retained");
        }
        let receipt = observe_complete_svn_receipt(prepared, &configuration).await?;
        if op.svn_receipt.as_ref() != Some(&receipt) {
            bail!("SVN result changed before Git intent; the global hold is retained");
        }
        let proof = GitIntentProof {
            engine,
            prepared,
            configuration: configuration.pin().clone(),
            svn_receipt: receipt,
        };
        let configuration = engine
            .db()
            .record_authenticated_git_intent(engine, actor, prepared, busy, proof)?;
        let path = prepared.git().repository().path().to_owned();
        let push_client = client.clone();
        let old = prepared.pins().git.pins.child_tip.clone();
        let new = prepared.pins().git.prepared_sha.clone();
        let (configuration, _push_response) = tokio::task::spawn_blocking(move || {
            let repository = git2::Repository::open_bare(path)?;
            let response = super::publication::publish_http_with_client(
                &repository,
                &configuration.pin().git_remote_url,
                &configuration.pin().child_git_branch,
                git2::Oid::from_str(&old)?,
                git2::Oid::from_str(&new)?,
                configuration.credentials().child_git_token(),
                push_client,
            );
            Ok::<_, anyhow::Error>((configuration, response))
        })
        .await
        .context("Git publication task lost its result; the intent remains held")??;
        configuration
    } else {
        configuration
    };
    // Verify both current Git refs with their separate credentials, and verify
    // the complete SVN result again. Push responses never substitute for these.
    let path = prepared.git().repository().path().to_owned();
    let git = prepared.pins().git.clone();
    let observer_client = client;
    let (configuration, sha) = tokio::task::spawn_blocking(move || {
        let repository = git2::Repository::open_bare(path)?;
        let sha = super::git_sources::observe_completed_pair_with_client(
            &configuration,
            &repository,
            &git,
            observer_client,
        )?;
        Ok::<_, anyhow::Error>((configuration, sha.to_string()))
    })
    .await
    .context("independent Git receipt task failed; the intent remains held")??;
    let receipt = observe_complete_svn_receipt(prepared, &configuration).await?;
    if op.svn_receipt.as_ref() != Some(&receipt) {
        bail!("SVN result changed during Git publication; the global hold is retained");
    }
    let proof = GitReceiptProof {
        engine,
        prepared,
        configuration: configuration.pin().clone(),
        svn_receipt: receipt,
        sha,
    };
    if op.phase == PairRefreshPhase::SvnObserved {
        engine
            .db()
            .record_authenticated_git_receipt(engine, actor, prepared, busy, &proof)?;
    }
    Ok(engine
        .db()
        .finalize_authenticated_refresh(engine, actor, prepared, busy, proof)?)
}

async fn observe_complete_svn_receipt(
    prepared: &LivePreparedRefresh,
    configuration: &LiveExecutionConfiguration,
) -> Result<SvnRefreshPublicationReceipt> {
    let (child, parent) = configuration.isolated_svn_clients()?;
    let directory = tempfile::Builder::new()
        .prefix("reposync-refresh-complete-receipt-")
        .tempdir()?;
    match super::svn_observation::observe_prepared_child_svn_visible(
        &child,
        &parent,
        prepared.git().repository().path(),
        directory.path(),
        &prepared.pins().svn,
    )
    .await?
    {
        SvnPublicationObservation::Observed(receipt) => Ok(*receipt),
        SvnPublicationObservation::NotObserved => {
            bail!("complete SVN publication receipt is unavailable; the global hold is retained")
        }
    }
}

pub(crate) async fn publish_owned_svn_stage_with_client(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
    request: &str,
    approved_digest: &str,
    busy: &PairBusyGuard,
    client: reqwest::Client,
) -> Result<PairRefreshOperation> {
    let admitted = actor.enroll_prepared(engine, prepared, request, approved_digest, busy)?;
    match admitted.phase {
        PairRefreshPhase::Prepared => {}
        PairRefreshPhase::SvnObserved
        | PairRefreshPhase::GitIntent
        | PairRefreshPhase::GitObserved
        | PairRefreshPhase::Completed
        | PairRefreshPhase::CancelledBeforePublication => return Ok(admitted),
        PairRefreshPhase::SvnIntent => bail!(
            "retained SVN intent requires independent reconciliation; publication was not retried"
        ),
    }
    let configuration = engine
        .db()
        .require_authenticated_svn_preparation(engine, actor, prepared, busy)?;
    let (configuration, fresh_git) = tokio::task::spawn_blocking(move || {
        let git = super::git_sources::observe_with_client(&configuration, client)?;
        Ok::<_, anyhow::Error>((configuration, git))
    })
    .await
    .context("fresh Git observation task failed")??;
    if fresh_git.pins() != prepared.git().pins() {
        bail!("Git source tips or endpoints changed before SVN intent");
    }
    let fresh_svn = super::svn_sources::observe_registered_svn_sources(&configuration).await?;
    if fresh_svn.pins() != prepared.svn().pins() {
        bail!("SVN source identity or HEAD changed before SVN intent");
    }
    require_fresh_svn_inputs(prepared, &fresh_svn).await?;
    require_owned_working_copy(prepared, fresh_svn.child_client()).await?;
    actor.require_current(engine)?;
    let proof = SvnIntentProof {
        engine,
        prepared,
        configuration: configuration.pin().clone(),
    };
    let configuration = engine
        .db()
        .record_authenticated_svn_intent(engine, actor, prepared, busy, proof)?;
    let (child, parent) = configuration.isolated_svn_clients()?;
    // This invocation owns the one local call after its newly persisted intent.
    // Re-entering this function finds SvnIntent and refuses another commit.
    let message = super::svn_observation::refresh_svn_commit_message(admitted.id)?;
    let commit_result = child
        .commit(&prepared.pins().svn.working_copy, &message, "RepoSync")
        .await;
    // A CLI response is not publication proof. A lost/error response may still
    // have committed, so use the independent complete observer either way.
    let observation = tempfile::Builder::new()
        .prefix("reposync-refresh-svn-receipt-")
        .tempdir()?;
    let observed = super::svn_observation::observe_prepared_child_svn_visible(
        &child,
        &parent,
        prepared.git().repository().path(),
        observation.path(),
        &prepared.pins().svn,
    )
    .await;
    match observed {
        Ok(SvnPublicationObservation::Observed(receipt)) => {
            let proof = SvnReceiptProof {
                engine,
                prepared,
                configuration: configuration.pin().clone(),
                receipt: *receipt,
            };
            Ok(engine
                .db()
                .record_authenticated_svn_receipt(engine, actor, prepared, busy, proof)?)
        }
        Ok(SvnPublicationObservation::NotObserved) | Err(_) => {
            // The signed intent itself preserves the hold even if drift or
            // revocation prevents this best-effort diagnostic update.
            let _ = engine
                .db()
                .retain_authenticated_svn_uncertainty(engine, actor, prepared, busy);
            if commit_result.is_err() {
                bail!("SVN commit response was unsuccessful; retained intent requires independent reconciliation");
            }
            bail!("complete SVN receipt was not proved; retained intent requires independent reconciliation")
        }
    }
}

async fn require_fresh_svn_inputs(
    prepared: &LivePreparedRefresh,
    sources: &super::svn_sources::SvnSourceSnapshot,
) -> Result<()> {
    let pins = prepared.pins();
    let head = sources.pins().inspected_head_revision;
    let lineage = &pins.svn.pins.lineage.pins;
    if super::lineage::inspect_svn_refresh_lineage(
        sources.child_client(),
        sources.parent_client(),
        lineage,
    )
    .await?
        != pins.svn.pins.lineage
    {
        bail!("SVN copy lineage changed before first intent");
    }
    for (client, checkpoint, tree, mergeinfo) in [
        (
            sources.parent_client(),
            pins.parent.last_svn_rev,
            pins.svn.pins.parent_git_tree.as_str(),
            None,
        ),
        (
            sources.child_client(),
            pins.child.last_svn_rev,
            pins.svn.pins.baseline_git_tree.as_str(),
            pins.svn.pins.previous_mergeinfo.as_deref(),
        ),
    ] {
        let checkout = super::svn_visibility::observe_subtree(client, head).await?;
        checkout.require_unchanged_since(checkpoint)?;
        super::svn_visibility::require_empty_visible_ancestors(client, checkout.info()).await?;
        let properties = super::svn_preparation::bound_checkout_property_xml(
            &client.tree_properties_at_rev(head).await?,
            client.url(),
            checkout.path(),
            checkout.info(),
        )?;
        super::svn_preparation::verify_refresh_svn_tree(
            prepared.git().repository().path(),
            tree,
            checkout.path(),
            &properties,
            mergeinfo,
            &lineage.child.parent_path,
        )?;
    }
    Ok(())
}

async fn require_owned_working_copy(
    prepared: &LivePreparedRefresh,
    child: &crate::svn::SvnClient,
) -> Result<()> {
    prepared.workspace()?.require_current()?;
    let pins = &prepared.pins().svn;
    let path = pins
        .working_copy
        .to_str()
        .context("owned working copy must be UTF-8")?;
    let info = crate::svn::parser::parse_svn_info(
        &child
            .run_svn_in_dir_public(&pins.working_copy, &["info", "--xml", path])
            .await?,
    )?;
    let lineage = &pins.pins.lineage.pins;
    if info.uuid != lineage.child.repository_uuid
        || info.root_url != lineage.repository_root_url
        || info.url != lineage.child_url
        || info.latest_rev != lineage.child.child_revision
    {
        bail!("owned working copy identity differs from approved child");
    }
    let status = child
        .run_svn_in_dir_public(&pins.working_copy, &["status", "--xml", "."])
        .await?;
    super::svn_preparation::require_refresh_merge_status(&status)?;
    let xml = child
        .run_svn_in_dir_public(
            &pins.working_copy,
            &[
                "proplist",
                "--xml",
                "--verbose",
                "--recursive",
                "--show-inherited-props",
                path,
            ],
        )
        .await?;
    let mut properties = super::svn_preparation::parse_refresh_properties(&xml, path)?;
    properties.retain(|path, values| path.is_empty() || !values.is_empty());
    if properties != pins.result_properties {
        bail!("owned working copy properties changed after approval");
    }
    super::svn_preparation::verify_refresh_svn_tree(
        prepared.git().repository().path(),
        &pins.result_git_tree,
        &pins.working_copy,
        &properties,
        Some(&pins.mergeinfo),
        &lineage.child.parent_path,
    )?;
    Ok(())
}
