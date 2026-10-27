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

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::task::Poll;

/// Association belongs to the invocation, never to ambient process state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BlockingAssoc {
    Ordinary,
    Case(crate::process::CaseContextId),
}

impl BlockingAssoc {
    pub(crate) fn require_admitted(self) -> Result<()> {
        if let Self::Case(id) = self {
            anyhow::ensure!(crate::process::case_registration_open(id),
                "case invocation is no longer admitted");
        }
        Ok(())
    }
}

type BlockingValue = Box<dyn std::any::Any + Send>;

enum BlockingPhase {
    Reserved,
    Live {
        handle: Option<tokio::task::JoinHandle<()>>,
        receiver: Option<tokio::sync::oneshot::Receiver<BlockingValue>>,
    },
    Settled { value: Option<BlockingValue> },
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockingOwner { Invocation, Unresolved, Observer(u64) }

struct BlockingRecord {
    assoc: BlockingAssoc,
    phase: BlockingPhase,
    owner: BlockingOwner,
    case_work: Option<crate::process::CaseWorkToken>,
}

struct BlockingTable {
    records: HashMap<u64, BlockingRecord>,
    collectors: HashMap<BlockingAssoc, u64>,
}

static BLOCKING_TABLE: OnceLock<Mutex<BlockingTable>> = OnceLock::new();
static BLOCKING_IDS: AtomicU64 = AtomicU64::new(1);
static BLOCKING_OBSERVERS: AtomicU64 = AtomicU64::new(1);

fn blocking_table() -> std::sync::MutexGuard<'static, BlockingTable> {
    BLOCKING_TABLE.get_or_init(|| Mutex::new(BlockingTable {
        records: HashMap::new(), collectors: HashMap::new(),
    })).lock().unwrap_or_else(|poison| poison.into_inner())
}

struct BlockingLease { id: u64, settled: bool }

impl BlockingLease {
    fn reserve(assoc: BlockingAssoc) -> Result<Self> {
        let case_work = match assoc {
            BlockingAssoc::Ordinary => None,
            BlockingAssoc::Case(id) => Some(
                crate::process::CaseWorkToken::reserve(id, false)
                    .map_err(anyhow::Error::msg)?,
            ),
        };
        let mut table = blocking_table();
        anyhow::ensure!(
            !table.collectors.contains_key(&assoc)
                && !table.records.values().any(|record| record.assoc == assoc
                    && (record.owner != BlockingOwner::Invocation
                        || matches!(record.phase, BlockingPhase::Failed(_)))),
            "blocking cleanup for this invocation association is unresolved"
        );
        let id = BLOCKING_IDS.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |old| old.checked_add(1)).map_err(|_| anyhow::anyhow!("blocking identity exhausted"))?;
        table.records.insert(id, BlockingRecord {
            assoc, phase: BlockingPhase::Reserved, owner: BlockingOwner::Invocation, case_work,
        });
        Ok(Self { id, settled: false })
    }
}

impl Drop for BlockingLease {
    fn drop(&mut self) {
        if self.settled { return; }
        let mut table = blocking_table();
        if let Some(record) = table.records.get_mut(&self.id) {
            if matches!(record.phase, BlockingPhase::Reserved) {
                record.phase = BlockingPhase::Failed("blocking startup did not complete".into());
                drop(record.case_work.take());
            } else if matches!(record.phase, BlockingPhase::Live { .. }) {
                record.owner = BlockingOwner::Unresolved;
            }
            // Settled/Failed are terminal. Neither a consumed handle nor its
            // receiver is returned to a poll set.
        }
    }
}

fn poll_blocking(record: &mut BlockingRecord, cx: &mut std::task::Context<'_>)
    -> Poll<std::result::Result<(), String>>
{
    let result = match &mut record.phase {
        BlockingPhase::Reserved => return Poll::Ready(Err("blocking startup incomplete".into())),
        BlockingPhase::Settled { .. } => return Poll::Ready(Ok(())),
        BlockingPhase::Failed(error) => return Poll::Ready(Err(error.clone())),
        BlockingPhase::Live { handle, receiver } => {
            if let Some(join) = handle.as_mut() {
                match Pin::new(join).poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(())) => { handle.take(); }
                    Poll::Ready(Err(error)) => {
                        let error = format!("blocking join failed: {error}");
                        handle.take();
                        receiver.take();
                        record.phase = BlockingPhase::Failed(error.clone());
                        drop(record.case_work.take());
                        return Poll::Ready(Err(error));
                    }
                }
            }
            match receiver.as_mut() {
                Some(rx) => match Pin::new(rx).poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(value)) => Ok(value),
                    Poll::Ready(Err(error)) => Err(format!("blocking result was lost: {error}")),
                },
                None => Err("blocking result receiver disappeared".into()),
            }
        }
    };
    match result {
        Ok(value) => {
            record.phase = BlockingPhase::Settled { value: Some(value) };
            if let Some(work) = record.case_work.take() { work.complete(); }
            Poll::Ready(Ok(()))
        }
        Err(error) => {
            record.phase = BlockingPhase::Failed(error.clone());
            drop(record.case_work.take());
            Poll::Ready(Err(error))
        }
    }
}

async fn join_owned_blocking<T: Send + 'static>(
    assoc: BlockingAssoc,
    work: impl FnOnce() -> Result<T, anyhow::Error> + Send + 'static,
) -> Result<Result<T, anyhow::Error>> {
    let mut lease = BlockingLease::reserve(assoc)?;
    let (tx, rx) = tokio::sync::oneshot::channel::<BlockingValue>();
    let handle = tokio::task::spawn_blocking(move || {
        // The inner work result, including an intentional publication error,
        // is preserved. A joined worker is not a publication-success receipt.
        let value: BlockingValue = Box::new(work());
        let _ = tx.send(value);
    });
    {
        let mut table = blocking_table();
        let record = table.records.get_mut(&lease.id)
            .expect("live blocking lease owns its reserved record");
        record.phase = BlockingPhase::Live { handle: Some(handle), receiver: Some(rx) };
    }
    let id = lease.id;
    let joined = std::future::poll_fn(|cx| {
        let mut table = blocking_table();
        match table.records.get_mut(&id) {
            Some(record) => poll_blocking(record, cx),
            None => Poll::Ready(Err("blocking record disappeared".into())),
        }
    }).await;
    lease.settled = true;
    joined.map_err(anyhow::Error::msg)?;
    let value = {
        let mut table = blocking_table();
        let record = table.records.get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("blocking result record disappeared"))?;
        let value = match &mut record.phase {
            BlockingPhase::Settled { value } => value.take(),
            _ => None,
        }.ok_or_else(|| anyhow::anyhow!("blocking result already consumed"))?;
        if assoc == BlockingAssoc::Ordinary { table.records.remove(&id); }
        value
    };
    value.downcast::<Result<T, anyhow::Error>>()
        .map(|value| *value)
        .map_err(|_| anyhow::anyhow!("blocking result type changed"))
}

struct BlockingObserver { assoc: BlockingAssoc, id: u64 }
impl Drop for BlockingObserver {
    fn drop(&mut self) {
        let mut table = blocking_table();
        if table.collectors.get(&self.assoc) != Some(&self.id) { return; }
        for record in table.records.values_mut() {
            if record.assoc == self.assoc && record.owner == BlockingOwner::Observer(self.id) {
                record.owner = if matches!(record.phase, BlockingPhase::Live { .. }) {
                    BlockingOwner::Unresolved
                } else { BlockingOwner::Invocation };
            }
        }
        table.collectors.remove(&self.assoc);
    }
}

/// Joins retained handles of this association only. Cancellation leaves every
/// handle, receiver and id in the registry and releases only the observer lease.
pub(crate) async fn observe_retained(assoc: BlockingAssoc) -> Result<()> {
    let id = BLOCKING_OBSERVERS.fetch_update(Ordering::AcqRel, Ordering::Acquire,
        |old| old.checked_add(1)).map_err(|_| anyhow::anyhow!("observer identity exhausted"))?;
    let observer = {
        let mut table = blocking_table();
        anyhow::ensure!(!table.collectors.contains_key(&assoc), "blocking observer already active");
        let unresolved = table.records.values().any(|record| {
            record.assoc == assoc && record.owner == BlockingOwner::Unresolved
        });
        if !unresolved {
            if let Some(error) = table.records.values().find_map(|record| {
                if record.assoc != assoc { return None; }
                match &record.phase { BlockingPhase::Failed(error) => Some(error.clone()), _ => None }
            }) { return Err(anyhow::Error::msg(error)); }
            // A no-op observer must not briefly exclude otherwise independent
            // ordinary invocations. Active records belong to their invokers.
            return Ok(());
        }
        table.collectors.insert(assoc, id);
        for record in table.records.values_mut() {
            if record.assoc == assoc && record.owner == BlockingOwner::Unresolved {
                record.owner = BlockingOwner::Observer(id);
            }
        }
        BlockingObserver { assoc, id }
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(5),
        std::future::poll_fn(|cx| {
            let mut table = blocking_table();
            let mut pending = false;
            let mut failure = None;
            for record in table.records.values_mut().filter(|record| record.assoc == assoc) {
                if let BlockingPhase::Failed(error) = &record.phase {
                    failure = Some(error.clone());
                }
                if record.owner != BlockingOwner::Observer(id) { continue; }
                match poll_blocking(record, cx) {
                    Poll::Pending => pending = true,
                    Poll::Ready(Ok(())) => {},
                    Poll::Ready(Err(error)) => { failure = Some(error); }
                }
            }
            // Still join the other handles after one failure. Failure remains
            // latched even after the final live handle finishes.
            if pending { Poll::Pending }
            else { Poll::Ready(failure.map_or(Ok(()), Err)) }
        })
    ).await;
    if matches!(&result, Ok(Ok(()))) && assoc == BlockingAssoc::Ordinary {
        let retired = {
            let mut table = blocking_table();
            let ids: Vec<_> = table.records.iter().filter_map(|(key, record)| {
                (record.assoc == assoc && record.owner == BlockingOwner::Observer(id)
                    && matches!(record.phase, BlockingPhase::Settled { .. })).then_some(*key)
            }).collect();
            ids.into_iter().filter_map(|key| table.records.remove(&key)).collect::<Vec<_>>()
        };
        // Native/configuration destructors do not run under the registry lock.
        drop(retired);
    }
    drop(observer);
    match result {
        Ok(result) => result.map_err(anyhow::Error::msg),
        Err(_) => bail!("blocking observation timed out; work remains retained"),
    }
}

pub(crate) struct BlockingProof { session: crate::process::CaseContextId }
impl BlockingProof {
    pub(crate) fn require_session(&self, session: crate::process::CaseContextId) -> Result<(), String> {
        if self.session != session { return Err("blocking proof belongs to another session".into()); }
        Ok(())
    }
}

pub(crate) fn blocking_proof(session: crate::process::CaseContextId) -> Result<BlockingProof, String> {
    if !crate::process::case_blocking_ready(session) {
        return Err("case registration is open, pending or failed".into());
    }
    let table = blocking_table();
    let assoc = BlockingAssoc::Case(session);
    let mut settled = 0;
    for record in table.records.values().filter(|record| record.assoc == assoc) {
        if matches!(record.phase, BlockingPhase::Settled { .. }) { settled += 1; }
        else { return Err("this session still owns incomplete blocking work".into()); }
    }
    if settled == 0 || table.collectors.contains_key(&assoc) {
        return Err("no completed blocking cohort for this session".into());
    }
    Ok(BlockingProof { session })
}

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
    reconcile_owned_refresh_intent_with_client_assoc(
        engine, actor, prepared, busy, client, BlockingAssoc::Ordinary,
    ).await
}

pub(crate) async fn reconcile_owned_refresh_intent_with_client_assoc(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
    busy: &PairBusyGuard,
    client: reqwest::Client,
    blocking: BlockingAssoc,
) -> Result<PairRefreshOperation> {
    blocking.require_admitted()?;
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
    let (configuration, sha) = join_owned_blocking(blocking, move || {
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
    complete_owned_refresh_with_client_assoc(
        engine, actor, prepared, request, approved_digest, busy, client, BlockingAssoc::Ordinary,
    ).await
}

pub(crate) async fn complete_owned_refresh_with_client_assoc(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
    request: &str,
    approved_digest: &str,
    busy: &PairBusyGuard,
    client: reqwest::Client,
    blocking: BlockingAssoc,
) -> Result<PairRefreshOperation> {
    blocking.require_admitted()?;
    let op = publish_owned_svn_stage_with_client_assoc(
        engine,
        actor,
        prepared,
        request,
        approved_digest,
        busy,
        client.clone(),
        blocking,
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
        let (configuration, fresh_git) = join_owned_blocking(blocking, move || {
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
        let (configuration, _push_response) = join_owned_blocking(blocking, move || {
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
    let (configuration, sha) = join_owned_blocking(blocking, move || {
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
    publish_owned_svn_stage_with_client_assoc(
        engine, actor, prepared, request, approved_digest, busy, client, BlockingAssoc::Ordinary,
    ).await
}

pub(crate) async fn publish_owned_svn_stage_with_client_assoc(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
    request: &str,
    approved_digest: &str,
    busy: &PairBusyGuard,
    client: reqwest::Client,
    blocking: BlockingAssoc,
) -> Result<PairRefreshOperation> {
    blocking.require_admitted()?;
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
    let (configuration, fresh_git) = join_owned_blocking(blocking, move || {
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
            if let Err(error) = commit_result {
                // The outer sentence and the retained-intent decision stay.
                // The source chain is the discarded CLI error for diagnosis.
                return Err(error).context(
                    "SVN commit response was unsuccessful; retained intent requires independent reconciliation",
                );
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


#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    // Failure records intentionally have no automatic production clear. Use a
    // fresh libtest process for each real lifecycle regression, and fail closed
    // if --exact accidentally runs zero tests. These sources remain UNRUN.
    fn isolated(name: &str) -> bool {
        const CHILD: &str = "REPOSYNC_BLOCKING_LIFECYCLE_CHILD";
        if std::env::var(CHILD).ok().as_deref() == Some(name) { return false; }
        let full = format!("pair_refresh::execution::lifecycle_tests::{name}");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", full.as_str(), "--nocapture"])
            .env(CHILD, name).env_remove("REPOSYNC_LAUNCH_BINDING_V1")
            .output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "regression child failed: {}",
            String::from_utf8_lossy(&output.stderr));
        assert!(stdout.contains(&full) && stdout.contains("1 passed; 0 failed"),
            "exact regression did not execute: {stdout}");
        true
    }

    #[tokio::test]
    async fn ordinary_work_stays_independent_of_held_case() {
        if isolated("ordinary_work_stays_independent_of_held_case") { return; }
        let session = crate::process::admit_case_context().unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let first = tokio::spawn(join_owned_blocking(BlockingAssoc::Ordinary, move || {
            let _ = started.send(());
            wait.recv().unwrap();
            Ok::<_, anyhow::Error>(3u8)
        }));
        ready.await.unwrap();
        assert_eq!(join_owned_blocking(BlockingAssoc::Ordinary,
            || Ok::<_, anyhow::Error>(5u8)).await.unwrap().unwrap(), 5);
        observe_retained(BlockingAssoc::Ordinary).await.unwrap();
        crate::process::close_case_registration(session).unwrap();
        assert!(blocking_proof(session).is_err(), "ordinary work is not case proof");
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let in_work = ran.clone();
        assert!(join_owned_blocking(BlockingAssoc::Case(session), move || {
            in_work.store(true, Ordering::Release);
            Ok::<_, anyhow::Error>(())
        }).await.is_err());
        assert!(!ran.load(Ordering::Acquire), "late case work ran after closure");
        assert_eq!(join_owned_blocking(BlockingAssoc::Ordinary,
            || Ok::<_, anyhow::Error>(7u8)).await.unwrap().unwrap(), 7);
        release.send(()).unwrap();
        assert_eq!(first.await.unwrap().unwrap().unwrap(), 3);
        crate::process::release_case_context(session, false).unwrap();
    }

    #[tokio::test]
    async fn cancelled_invocation_and_observer_keep_handle_and_result() {
        if isolated("cancelled_invocation_and_observer_keep_handle_and_result") { return; }
        let session = crate::process::admit_case_context().unwrap();
        let assoc = BlockingAssoc::Case(session);
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let invocation = tokio::spawn(join_owned_blocking(assoc, move || {
            let _ = started.send(());
            wait.recv().unwrap();
            Ok::<_, anyhow::Error>(37u8)
        }));
        ready.await.unwrap();
        invocation.abort();
        assert!(invocation.await.unwrap_err().is_cancelled());
        crate::process::close_case_registration(session).unwrap();
        assert!(blocking_proof(session).is_err());
        assert!(crate::process::release_case_context(session, false).is_err());
        let observer = tokio::spawn(observe_retained(assoc));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if blocking_table().collectors.contains_key(&assoc) { break; }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        assert!(blocking_proof(session).is_err());
        release.send(()).unwrap();
        observe_retained(assoc).await.unwrap();
        blocking_proof(session).unwrap().require_session(session).unwrap();
        let preserved = {
            let table = blocking_table();
            table.records.values().find_map(|record| {
                if record.assoc != assoc { return None; }
                match &record.phase {
                    BlockingPhase::Settled { value: Some(value) } => {
                        value.downcast_ref::<Result<u8, anyhow::Error>>()
                            .and_then(|value| value.as_ref().ok()).copied()
                    }
                    _ => None,
                }
            })
        };
        assert_eq!(preserved, Some(37), "cancelled receiver lost the real result");
        crate::process::release_case_context(session, false).unwrap();
    }

    #[tokio::test]
    async fn panic_is_terminal_and_does_not_block_ordinary_work() {
        if isolated("panic_is_terminal_and_does_not_block_ordinary_work") { return; }
        let session = crate::process::admit_case_context().unwrap();
        let assoc = BlockingAssoc::Case(session);
        assert!(join_owned_blocking::<u8>(assoc, || panic!("owned regression panic"))
            .await.is_err());
        crate::process::close_case_registration(session).unwrap();
        assert!(observe_retained(assoc).await.is_err());
        assert!(observe_retained(assoc).await.is_err(), "consumed panic must not be repolled");
        assert!(blocking_proof(session).is_err());
        assert!(crate::process::admit_case_context().is_err());
        assert_eq!(join_owned_blocking(BlockingAssoc::Ordinary,
            || Ok::<_, anyhow::Error>(11u8)).await.unwrap().unwrap(), 11);
    }

    #[tokio::test]
    async fn intended_work_error_is_not_a_failed_join() {
        if isolated("intended_work_error_is_not_a_failed_join") { return; }
        let session = crate::process::admit_case_context().unwrap();
        let assoc = BlockingAssoc::Case(session);
        let result = join_owned_blocking::<u8>(assoc,
            || Err(anyhow::anyhow!("intended receiver rejection"))).await.unwrap();
        assert!(result.is_err());
        crate::process::close_case_registration(session).unwrap();
        observe_retained(assoc).await.unwrap();
        blocking_proof(session).unwrap().require_session(session).unwrap();
        crate::process::release_case_context(session, false).unwrap();
    }
}
