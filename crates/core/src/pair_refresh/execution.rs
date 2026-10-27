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

/// Ordinary is a selector for the explicitly scoped caller, never a global
/// cohort. A worker scope is installed around its request future; direct API
/// callers get a stable operation scope from the actual prepared operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BlockingAssoc {
    Ordinary,
    Owned(OrdinaryCohortId),
    #[cfg(test)]
    Case(crate::process::CaseContextId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum OrdinaryCohortId {
    Worker(uuid::Uuid),
    Operation(uuid::Uuid),
}

tokio::task_local! {
    // Future-scoped transport of an explicit owner, not a process/thread-global
    // admission latch. Tokio-spawned child tasks do not inherit this binding.
    static ORDINARY_COHORT: OrdinaryCohortId;
}

#[derive(Clone, Copy)]
pub(crate) struct BlockingCohort {
    id: OrdinaryCohortId,
}

impl BlockingCohort {
    pub(crate) fn for_worker(worker: uuid::Uuid) -> Self {
        Self {
            id: OrdinaryCohortId::Worker(worker),
        }
    }

    fn scope<F: Future>(&self, future: Pin<Box<F>>) -> impl Future<Output = F::Output> {
        ORDINARY_COHORT.scope(self.id, future)
    }

    pub(crate) fn run<F: Future>(&self, future: F) -> impl Future<Output = F::Output> {
        let cohort = *self;
        let future = Box::pin(future);
        async move { cohort.scope(future).await }
    }

    pub(crate) async fn observe(&self) -> Result<()> {
        observe_retained(BlockingAssoc::Owned(self.id)).await
    }
}

impl BlockingAssoc {
    fn resolve(self) -> Result<Self> {
        match self {
            Self::Ordinary => ORDINARY_COHORT
                .try_with(|id| Self::Owned(*id))
                .map_err(|_| anyhow::anyhow!("ordinary blocking work requires its caller cohort")),
            associated => Ok(associated),
        }
    }

    fn is_ordinary(self) -> bool {
        matches!(self, Self::Owned(_))
    }
}

fn in_operation_cohort<T>(
    operation: uuid::Uuid,
    future: impl Future<Output = Result<T>>,
) -> impl Future<Output = Result<T>> {
    // Box before constructing the cohort state machine. Boxing inside an
    // async fn would still embed the original payload in its unpolled state.
    let future = Box::pin(future);
    async move {
        if let Ok(id) = ORDINARY_COHORT.try_with(|id| *id) {
            // A real service worker already owns the complete request/recovery
            // future. Do not replace that owner with an inner operation scope.
            observe_retained(BlockingAssoc::Owned(id)).await?;
            return future.await;
        }
        let cohort = BlockingCohort {
            id: OrdinaryCohortId::Operation(operation),
        };
        // A cancelled direct caller leaves a known operation owner. Re-entry must
        // observe its actual retained cohort before doing any more operation work;
        // a joined failure stays latched to this operation alone.
        cohort.observe().await?;
        cohort.scope(future).await
    }
}

type BlockingValue = Box<dyn std::any::Any + Send>;

enum BlockingPhase {
    Reserved,
    Live {
        handle: Option<tokio::task::JoinHandle<()>>,
        receiver: Option<tokio::sync::oneshot::Receiver<BlockingValue>>,
    },
    Settled {
        value: Option<BlockingValue>,
    },
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockingOwner {
    Invocation,
    Unresolved,
    Observer(u64),
}

struct BlockingRecord {
    assoc: BlockingAssoc,
    phase: BlockingPhase,
    owner: BlockingOwner,
    #[cfg(test)]
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
    BLOCKING_TABLE
        .get_or_init(|| {
            Mutex::new(BlockingTable {
                records: HashMap::new(),
                collectors: HashMap::new(),
            })
        })
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

struct BlockingLease {
    id: u64,
    assoc: BlockingAssoc,
    settled: bool,
}

impl BlockingLease {
    fn reserve(assoc: BlockingAssoc) -> Result<Self> {
        let assoc = assoc.resolve()?;
        #[cfg(test)]
        let case_work = match assoc {
            BlockingAssoc::Owned(_) => None,
            BlockingAssoc::Ordinary => unreachable!("resolved before reservation"),
            BlockingAssoc::Case(id) => {
                Some(crate::process::CaseWorkToken::reserve(id).map_err(anyhow::Error::msg)?)
            }
        };
        let mut table = blocking_table();
        anyhow::ensure!(
            !table.collectors.contains_key(&assoc)
                && !table.records.values().any(|record| record.assoc == assoc
                    && (record.owner != BlockingOwner::Invocation
                        || matches!(record.phase, BlockingPhase::Failed(_)))),
            "blocking cleanup for this invocation association is unresolved"
        );
        let mut observed = BLOCKING_IDS.load(Ordering::Acquire);
        let id = loop {
            let next = observed
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("blocking identity exhausted"))?;
            match BLOCKING_IDS.compare_exchange_weak(
                observed,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(previous) => break previous,
                Err(current) => observed = current,
            }
        };
        table.records.insert(
            id,
            BlockingRecord {
                assoc,
                phase: BlockingPhase::Reserved,
                owner: BlockingOwner::Invocation,
                #[cfg(test)]
                case_work,
            },
        );
        Ok(Self {
            id,
            assoc,
            settled: false,
        })
    }
}

impl Drop for BlockingLease {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let mut table = blocking_table();
        if let Some(record) = table.records.get_mut(&self.id) {
            if matches!(record.phase, BlockingPhase::Reserved) {
                record.phase = BlockingPhase::Failed("blocking startup did not complete".into());
                #[cfg(test)]
                drop(record.case_work.take());
            }
            // The invocation is gone even if an earlier poll settled its
            // result or joined a failure. Keep every phase reclaimable by the
            // actual owner's collector; terminal handles are never repolled.
            record.owner = BlockingOwner::Unresolved;
        }
    }
}

fn poll_blocking(
    record: &mut BlockingRecord,
    cx: &mut std::task::Context<'_>,
) -> Poll<std::result::Result<(), String>> {
    let result = match &mut record.phase {
        BlockingPhase::Reserved => return Poll::Ready(Err("blocking startup incomplete".into())),
        BlockingPhase::Settled { .. } => return Poll::Ready(Ok(())),
        BlockingPhase::Failed(error) => return Poll::Ready(Err(error.clone())),
        BlockingPhase::Live { handle, receiver } => {
            if let Some(join) = handle.as_mut() {
                match Pin::new(join).poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(())) => {
                        handle.take();
                    }
                    Poll::Ready(Err(error)) => {
                        let error = format!("blocking join failed: {error}");
                        handle.take();
                        receiver.take();
                        record.phase = BlockingPhase::Failed(error.clone());
                        #[cfg(test)]
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
            #[cfg(test)]
            if let Some(work) = record.case_work.take() {
                work.complete();
            }
            Poll::Ready(Ok(()))
        }
        Err(error) => {
            record.phase = BlockingPhase::Failed(error.clone());
            #[cfg(test)]
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
    let assoc = lease.assoc;
    let (tx, rx) = tokio::sync::oneshot::channel::<BlockingValue>();
    let handle = tokio::task::spawn_blocking(move || {
        // The inner work result, including an intentional publication error,
        // is preserved. A joined worker is not a publication-success receipt.
        let value: BlockingValue = Box::new(work());
        let _ = tx.send(value);
    });
    {
        let mut table = blocking_table();
        let record = table
            .records
            .get_mut(&lease.id)
            .expect("live blocking lease owns its reserved record");
        record.phase = BlockingPhase::Live {
            handle: Some(handle),
            receiver: Some(rx),
        };
    }
    let id = lease.id;
    let joined = std::future::poll_fn(|cx| {
        let mut table = blocking_table();
        match table.records.get_mut(&id) {
            Some(record) => poll_blocking(record, cx),
            None => Poll::Ready(Err("blocking record disappeared".into())),
        }
    })
    .await;
    joined.map_err(anyhow::Error::msg)?;
    lease.settled = true;
    let value = {
        let mut table = blocking_table();
        let record = table
            .records
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("blocking result record disappeared"))?;
        let value = match &mut record.phase {
            BlockingPhase::Settled { value } => value.take(),
            _ => None,
        }
        .ok_or_else(|| anyhow::anyhow!("blocking result already consumed"))?;
        if assoc.is_ordinary() {
            table.records.remove(&id);
        }
        value
    };
    value
        .downcast::<Result<T, anyhow::Error>>()
        .map(|value| *value)
        .map_err(|_| anyhow::anyhow!("blocking result type changed"))
}

struct BlockingObserver {
    assoc: BlockingAssoc,
    id: u64,
}
impl Drop for BlockingObserver {
    fn drop(&mut self) {
        let mut table = blocking_table();
        if table.collectors.get(&self.assoc) != Some(&self.id) {
            return;
        }
        for record in table.records.values_mut() {
            if record.assoc == self.assoc && record.owner == BlockingOwner::Observer(self.id) {
                // Only records from a vanished invocation were claimed.
                // Settled results and Failed evidence have no invoker to
                // return to: keep them collectible with their real cohort.
                record.owner = BlockingOwner::Unresolved;
            }
        }
        table.collectors.remove(&self.assoc);
    }
}

/// Joins retained handles of this association only. Cancellation leaves every
/// handle, receiver and id in the registry and releases only the observer lease.
pub(crate) async fn observe_retained(assoc: BlockingAssoc) -> Result<()> {
    let assoc = assoc.resolve()?;
    let mut observed = BLOCKING_OBSERVERS.load(Ordering::Acquire);
    let id = loop {
        let next = observed
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("observer identity exhausted"))?;
        match BLOCKING_OBSERVERS.compare_exchange_weak(
            observed,
            next,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(previous) => break previous,
            Err(current) => observed = current,
        }
    };
    let observer = {
        let mut table = blocking_table();
        anyhow::ensure!(
            !table.collectors.contains_key(&assoc),
            "blocking observer already active"
        );
        let unresolved = table
            .records
            .values()
            .any(|record| record.assoc == assoc && record.owner == BlockingOwner::Unresolved);
        if !unresolved {
            if let Some(error) = table.records.values().find_map(|record| {
                if record.assoc != assoc {
                    return None;
                }
                match &record.phase {
                    BlockingPhase::Failed(error) => Some(error.clone()),
                    _ => None,
                }
            }) {
                return Err(anyhow::Error::msg(error));
            }
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
    // Move only joined values out under the final registry lock. Their
    // destructors run after that lock is released, never inside the poll.
    let mut retired = Vec::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        std::future::poll_fn(|cx| {
            let mut table = blocking_table();
            let mut pending = false;
            let mut failure = None;
            for record in table
                .records
                .values_mut()
                .filter(|record| record.assoc == assoc)
            {
                // An invocation can be cancelled after the initial claim.
                // Adopt newly abandoned work on every poll while preserving
                // genuinely live invocation owners and unrelated cohorts.
                if record.owner == BlockingOwner::Unresolved {
                    record.owner = BlockingOwner::Observer(id);
                }
                if let BlockingPhase::Failed(error) = &record.phase {
                    failure = Some(error.clone());
                }
                if record.owner != BlockingOwner::Observer(id) {
                    continue;
                }
                match poll_blocking(record, cx) {
                    Poll::Pending => pending = true,
                    Poll::Ready(Ok(())) => {}
                    Poll::Ready(Err(error)) => {
                        failure = Some(error);
                    }
                }
            }
            // Still join the other handles after one failure. Failure remains
            // latched even after the final live handle finishes.
            if pending {
                return Poll::Pending;
            }

            // Reconciliation, terminal decision, retirement and collector
            // release form one critical section. A cancellation cannot insert
            // Unresolved work between the final poll and retirement/release.
            // A still-live invoker is not taken over; a cancellation after
            // this linearization point remains owned for the next observation.
            if assoc.is_ordinary() {
                let ids: Vec<_> = table
                    .records
                    .iter()
                    .filter_map(|(key, record)| {
                        (record.assoc == assoc
                            && record.owner == BlockingOwner::Observer(id)
                            && matches!(record.phase, BlockingPhase::Settled { .. }))
                        .then_some(*key)
                    })
                    .collect();
                retired.extend(ids.into_iter().filter_map(|key| table.records.remove(&key)));
            }
            for record in table.records.values_mut() {
                if record.assoc == assoc && record.owner == BlockingOwner::Observer(id) {
                    // Keep Failed evidence and separate Case proof values.
                    record.owner = BlockingOwner::Unresolved;
                }
            }
            table.collectors.remove(&assoc);
            Poll::Ready(failure.map_or(Ok(()), Err))
        }),
    )
    .await;
    drop(observer);
    drop(retired);
    match result {
        Ok(result) => result.map_err(anyhow::Error::msg),
        Err(_) => bail!("blocking observation timed out; work remains retained"),
    }
}

#[cfg(test)]
pub(crate) struct BlockingProof {
    session: crate::process::CaseContextId,
}
#[cfg(test)]
impl BlockingProof {
    pub(crate) fn require_session(
        &self,
        session: crate::process::CaseContextId,
    ) -> Result<(), String> {
        if self.session != session {
            return Err("blocking proof belongs to another session".into());
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn blocking_proof(
    session: crate::process::CaseContextId,
) -> Result<BlockingProof, String> {
    if !crate::process::case_blocking_ready(session) {
        return Err("case registration is open, pending or failed".into());
    }
    let table = blocking_table();
    let assoc = BlockingAssoc::Case(session);
    let mut settled = 0;
    for record in table
        .records
        .values()
        .filter(|record| record.assoc == assoc)
    {
        if matches!(record.phase, BlockingPhase::Settled { .. }) {
            settled += 1;
        } else {
            return Err("this session still owns incomplete blocking work".into());
        }
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
    in_operation_cohort(
        prepared.pins().git.operation_id,
        reconcile_owned_refresh_intent_in_cohort(engine, actor, prepared, busy, client),
    )
    .await
}

async fn reconcile_owned_refresh_intent_in_cohort(
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
    let (configuration, sha) = join_owned_blocking(BlockingAssoc::Ordinary, move || {
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
    in_operation_cohort(
        prepared.pins().git.operation_id,
        complete_owned_refresh_in_cohort(
            engine,
            actor,
            prepared,
            request,
            approved_digest,
            busy,
            client,
        ),
    )
    .await
}

async fn complete_owned_refresh_in_cohort(
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
        let (configuration, fresh_git) = join_owned_blocking(BlockingAssoc::Ordinary, move || {
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
        let (configuration, _push_response) =
            join_owned_blocking(BlockingAssoc::Ordinary, move || {
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
    let (configuration, sha) = join_owned_blocking(BlockingAssoc::Ordinary, move || {
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
    in_operation_cohort(
        prepared.pins().git.operation_id,
        publish_owned_svn_stage_in_cohort(
            engine,
            actor,
            prepared,
            request,
            approved_digest,
            busy,
            client,
        ),
    )
    .await
}

async fn publish_owned_svn_stage_in_cohort(
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
    let (configuration, fresh_git) = join_owned_blocking(BlockingAssoc::Ordinary, move || {
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
        if std::env::var(CHILD).ok().as_deref() == Some(name) {
            return false;
        }
        let full = format!("pair_refresh::execution::lifecycle_tests::{name}");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", full.as_str(), "--nocapture"])
            .env(CHILD, name)
            .env_remove("REPOSYNC_LAUNCH_BINDING_V1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "regression child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains(&full) && stdout.contains("1 passed; 0 failed"),
            "exact regression did not execute: {stdout}"
        );
        true
    }

    #[tokio::test]
    async fn ordinary_work_stays_independent_of_held_case() {
        if isolated("ordinary_work_stays_independent_of_held_case") {
            return;
        }
        let session = crate::process::admit_case_context().unwrap();
        let ordinary = BlockingAssoc::Owned(OrdinaryCohortId::Worker(uuid::Uuid::new_v4()));
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let first = tokio::spawn(join_owned_blocking(ordinary, move || {
            let _ = started.send(());
            wait.recv().unwrap();
            Ok::<_, anyhow::Error>(3u8)
        }));
        ready.await.unwrap();
        assert_eq!(
            join_owned_blocking(ordinary, || Ok::<_, anyhow::Error>(5u8))
                .await
                .unwrap()
                .unwrap(),
            5
        );
        observe_retained(ordinary).await.unwrap();
        crate::process::close_case_registration(session).unwrap();
        assert!(
            blocking_proof(session).is_err(),
            "ordinary work is not case proof"
        );
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let in_work = ran.clone();
        assert!(join_owned_blocking(BlockingAssoc::Case(session), move || {
            in_work.store(true, Ordering::Release);
            Ok::<_, anyhow::Error>(())
        })
        .await
        .is_err());
        assert!(
            !ran.load(Ordering::Acquire),
            "late case work ran after closure"
        );
        assert_eq!(
            join_owned_blocking(ordinary, || Ok::<_, anyhow::Error>(7u8))
                .await
                .unwrap()
                .unwrap(),
            7
        );
        release.send(()).unwrap();
        assert_eq!(first.await.unwrap().unwrap().unwrap(), 3);
        crate::process::release_case_context(session, false).unwrap();
    }

    #[tokio::test]
    async fn cancelled_invocation_and_observer_keep_handle_and_result() {
        if isolated("cancelled_invocation_and_observer_keep_handle_and_result") {
            return;
        }
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
                if blocking_table().collectors.contains_key(&assoc) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        assert!(blocking_proof(session).is_err());
        release.send(()).unwrap();
        observe_retained(assoc).await.unwrap();
        blocking_proof(session)
            .unwrap()
            .require_session(session)
            .unwrap();
        let preserved = {
            let table = blocking_table();
            table.records.values().find_map(|record| {
                if record.assoc != assoc {
                    return None;
                }
                match &record.phase {
                    BlockingPhase::Settled { value: Some(value) } => value
                        .downcast_ref::<Result<u8, anyhow::Error>>()
                        .and_then(|value| value.as_ref().ok())
                        .copied(),
                    _ => None,
                }
            })
        };
        assert_eq!(
            preserved,
            Some(37),
            "cancelled receiver lost the real result"
        );
        crate::process::release_case_context(session, false).unwrap();
    }

    #[tokio::test]
    async fn panic_is_terminal_and_does_not_block_ordinary_work() {
        if isolated("panic_is_terminal_and_does_not_block_ordinary_work") {
            return;
        }
        let session = crate::process::admit_case_context().unwrap();
        let assoc = BlockingAssoc::Case(session);
        assert!(
            join_owned_blocking::<u8>(assoc, || panic!("owned regression panic"))
                .await
                .is_err()
        );
        crate::process::close_case_registration(session).unwrap();
        assert!(observe_retained(assoc).await.is_err());
        assert!(
            observe_retained(assoc).await.is_err(),
            "consumed panic must not be repolled"
        );
        assert!(blocking_proof(session).is_err());
        assert!(crate::process::admit_case_context().is_err());
        let ordinary = BlockingAssoc::Owned(OrdinaryCohortId::Worker(uuid::Uuid::new_v4()));
        assert_eq!(
            join_owned_blocking(ordinary, || Ok::<_, anyhow::Error>(11u8))
                .await
                .unwrap()
                .unwrap(),
            11
        );
    }

    #[tokio::test]
    async fn intended_work_error_is_not_a_failed_join() {
        if isolated("intended_work_error_is_not_a_failed_join") {
            return;
        }
        let session = crate::process::admit_case_context().unwrap();
        let assoc = BlockingAssoc::Case(session);
        let result = join_owned_blocking::<u8>(assoc, || {
            Err(anyhow::anyhow!("intended receiver rejection"))
        })
        .await
        .unwrap();
        assert!(result.is_err());
        crate::process::close_case_registration(session).unwrap();
        observe_retained(assoc).await.unwrap();
        blocking_proof(session)
            .unwrap()
            .require_session(session)
            .unwrap();
        crate::process::release_case_context(session, false).unwrap();
    }

    #[tokio::test]
    async fn ordinary_panic_stays_with_its_worker_cohort() {
        if isolated("ordinary_panic_stays_with_its_worker_cohort") {
            return;
        }
        let failed = BlockingCohort::for_worker(uuid::Uuid::new_v4());
        let other = BlockingCohort::for_worker(uuid::Uuid::new_v4());
        assert!(failed
            .run(join_owned_blocking::<u8>(BlockingAssoc::Ordinary, || {
                panic!("ordinary owner panic")
            }))
            .await
            .is_err());
        assert!(failed.observe().await.is_err());
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let in_work = ran.clone();
        assert!(failed
            .run(join_owned_blocking(BlockingAssoc::Ordinary, move || {
                in_work.store(true, Ordering::Release);
                Ok::<_, anyhow::Error>(())
            }))
            .await
            .is_err());
        assert!(
            !ran.load(Ordering::Acquire),
            "failed owner was silently reset"
        );
        assert_eq!(
            other
                .run(join_owned_blocking(BlockingAssoc::Ordinary, || {
                    Ok::<_, anyhow::Error>(19u8)
                }))
                .await
                .unwrap()
                .unwrap(),
            19
        );
        other.observe().await.unwrap();
        assert!(
            failed.observe().await.is_err(),
            "another owner cleared the failure"
        );
        let table = blocking_table();
        assert!(table.records.values().any(|record| {
            record.assoc == BlockingAssoc::Owned(failed.id)
                && matches!(record.phase, BlockingPhase::Failed(_))
                && record.owner == BlockingOwner::Unresolved
        }));
        assert!(!table
            .records
            .values()
            .any(|record| record.assoc == BlockingAssoc::Owned(other.id)));
    }

    #[tokio::test]
    async fn ordinary_cancelled_work_does_not_fence_another_worker() {
        if isolated("ordinary_cancelled_work_does_not_fence_another_worker") {
            return;
        }
        let cancelled = BlockingCohort::for_worker(uuid::Uuid::new_v4());
        let other = BlockingCohort::for_worker(uuid::Uuid::new_v4());
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let invocation = tokio::spawn(async move {
            cancelled
                .run(join_owned_blocking(BlockingAssoc::Ordinary, move || {
                    let _ = started.send(());
                    // Dropping the only sender during assertion unwinding also
                    // releases this worker; no held barrier survives test exit.
                    let _ = wait.recv();
                    Ok::<_, anyhow::Error>(23u8)
                }))
                .await
        });
        ready.await.unwrap();
        invocation.abort();
        assert!(invocation.await.unwrap_err().is_cancelled());
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let in_work = ran.clone();
        assert!(cancelled
            .run(join_owned_blocking(BlockingAssoc::Ordinary, move || {
                in_work.store(true, Ordering::Release);
                Ok::<_, anyhow::Error>(())
            }))
            .await
            .is_err());
        assert!(!ran.load(Ordering::Acquire));
        assert_eq!(
            other
                .run(join_owned_blocking(BlockingAssoc::Ordinary, || {
                    Ok::<_, anyhow::Error>(29u8)
                }))
                .await
                .unwrap()
                .unwrap(),
            29
        );
        other.observe().await.unwrap();
        assert!(blocking_table().records.values().any(|record| {
            record.assoc == BlockingAssoc::Owned(cancelled.id)
                && record.owner == BlockingOwner::Unresolved
                && matches!(record.phase, BlockingPhase::Live { .. })
        }));
        release.send(()).unwrap();
        cancelled.observe().await.unwrap();
        assert!(!blocking_table()
            .records
            .values()
            .any(|record| { record.assoc == BlockingAssoc::Owned(cancelled.id) }));
    }

    #[tokio::test]
    async fn direct_operation_failure_refuses_only_its_own_reentry() {
        if isolated("direct_operation_failure_refuses_only_its_own_reentry") {
            return;
        }
        let failed = uuid::Uuid::new_v4();
        let other = uuid::Uuid::new_v4();
        assert!(in_operation_cohort(failed, async {
            join_owned_blocking::<u8>(BlockingAssoc::Ordinary, || panic!("direct operation panic"))
                .await??;
            Ok(())
        })
        .await
        .is_err());
        let entered = std::sync::atomic::AtomicBool::new(false);
        assert!(in_operation_cohort(failed, async {
            entered.store(true, Ordering::Release);
            Ok(())
        })
        .await
        .is_err());
        assert!(
            !entered.load(Ordering::Acquire),
            "failed re-entry polled its operation body"
        );
        assert_eq!(
            in_operation_cohort(other, async {
                join_owned_blocking(BlockingAssoc::Ordinary, || Ok::<_, anyhow::Error>(41u8))
                    .await?
            })
            .await
            .unwrap(),
            41
        );
        assert!(
            observe_retained(BlockingAssoc::Owned(OrdinaryCohortId::Operation(failed)))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn cancelled_partial_observer_reclaims_settled_and_pending_values() {
        if isolated("cancelled_partial_observer_reclaims_settled_and_pending_values") {
            return;
        }
        struct Tracked {
            marker: u8,
            drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::AcqRel);
            }
        }
        let cohort = BlockingCohort::for_worker(uuid::Uuid::new_v4());
        let assoc = BlockingAssoc::Owned(cohort.id);
        let first_drops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let second_drops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (release_first, wait_first) = std::sync::mpsc::channel();
        let (release_second, wait_second) = std::sync::mpsc::channel();
        let (first_started, first_ready) = tokio::sync::oneshot::channel();
        let (second_started, second_ready) = tokio::sync::oneshot::channel();
        let first = tokio::spawn({
            let drops = first_drops.clone();
            async move {
                cohort
                    .run(join_owned_blocking(BlockingAssoc::Ordinary, move || {
                        let _ = first_started.send(());
                        let _ = wait_first.recv();
                        Ok::<_, anyhow::Error>(Tracked { marker: 31, drops })
                    }))
                    .await
            }
        });
        let second = tokio::spawn({
            let drops = second_drops.clone();
            async move {
                cohort
                    .run(join_owned_blocking(BlockingAssoc::Ordinary, move || {
                        let _ = second_started.send(());
                        let _ = wait_second.recv();
                        Ok::<_, anyhow::Error>(Tracked { marker: 37, drops })
                    }))
                    .await
            }
        });
        first_ready.await.unwrap();
        second_ready.await.unwrap();
        first.abort();
        second.abort();
        // Do not require Debug on the deliberately opaque result payload.
        assert!(matches!(first.await, Err(error) if error.is_cancelled()));
        assert!(matches!(second.await, Err(error) if error.is_cancelled()));
        release_first.send(()).unwrap();
        let observer = tokio::spawn(async move { cohort.observe().await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let partial = {
                    let table = blocking_table();
                    let records: Vec<_> = table
                        .records
                        .values()
                        .filter(|r| r.assoc == assoc)
                        .collect();
                    records.len() == 2
                        && records
                            .iter()
                            .any(|r| matches!(r.phase, BlockingPhase::Settled { .. }))
                        && records
                            .iter()
                            .any(|r| matches!(r.phase, BlockingPhase::Live { .. }))
                        && records
                            .iter()
                            .all(|r| matches!(r.owner, BlockingOwner::Observer(_)))
                };
                if partial {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("one settled result and one live handle belong to the collector");
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        {
            let table = blocking_table();
            let records: Vec<_> = table
                .records
                .values()
                .filter(|r| r.assoc == assoc)
                .collect();
            assert_eq!(records.len(), 2);
            assert!(records.iter().all(|r| r.owner == BlockingOwner::Unresolved));
            assert!(!table.collectors.contains_key(&assoc));
            let marker = records.iter().find_map(|record| match &record.phase {
                BlockingPhase::Settled { value: Some(value) } => value
                    .downcast_ref::<Result<Tracked, anyhow::Error>>()
                    .and_then(|result| result.as_ref().ok())
                    .map(|tracked| tracked.marker),
                _ => None,
            });
            assert_eq!(marker, Some(31), "settled value lost its actual custodian");
        }
        assert_eq!(first_drops.load(Ordering::Acquire), 0);
        assert_eq!(second_drops.load(Ordering::Acquire), 0);
        release_second.send(()).unwrap();
        cohort.observe().await.unwrap();
        assert_eq!(first_drops.load(Ordering::Acquire), 1);
        assert_eq!(second_drops.load(Ordering::Acquire), 1);
        let table = blocking_table();
        assert!(!table.records.values().any(|record| record.assoc == assoc));
        assert!(!table.collectors.contains_key(&assoc));
    }

    #[tokio::test]
    async fn late_cancellation_blocks_reentry_until_both_results_retire() {
        if isolated("late_cancellation_blocks_reentry_until_both_results_retire") {
            return;
        }
        struct Tracked {
            marker: u8,
            drops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::AcqRel);
            }
        }
        let operation = uuid::Uuid::new_v4();
        let assoc = BlockingAssoc::Owned(OrdinaryCohortId::Operation(operation));
        let first_drops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let second_drops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let effects = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (release_first, wait_first) = std::sync::mpsc::channel();
        let (release_second, wait_second) = std::sync::mpsc::channel();
        let (first_started, first_ready) = tokio::sync::oneshot::channel();
        let (second_started, second_ready) = tokio::sync::oneshot::channel();
        let first = tokio::spawn({
            let drops = first_drops.clone();
            async move {
                in_operation_cohort(operation, async move {
                    join_owned_blocking(BlockingAssoc::Ordinary, move || {
                        let _ = first_started.send(());
                        // Sole senders are on the test stack; assertion
                        // unwinding also releases both held blocking workers.
                        let _ = wait_first.recv();
                        Ok::<_, anyhow::Error>(Tracked { marker: 43, drops })
                    })
                    .await?
                })
                .await
            }
        });
        first_ready.await.unwrap();
        let first_id = blocking_table()
            .records
            .iter()
            .find_map(|(id, r)| (r.assoc == assoc).then_some(*id))
            .unwrap();
        let second = tokio::spawn({
            let drops = second_drops.clone();
            async move {
                in_operation_cohort(operation, async move {
                    join_owned_blocking(BlockingAssoc::Ordinary, move || {
                        let _ = second_started.send(());
                        let _ = wait_second.recv();
                        Ok::<_, anyhow::Error>(Tracked { marker: 47, drops })
                    })
                    .await?
                })
                .await
            }
        });
        second_ready.await.unwrap();
        let second_id = blocking_table()
            .records
            .iter()
            .find_map(|(id, r)| (r.assoc == assoc && *id != first_id).then_some(*id))
            .unwrap();
        first.abort();
        assert!(matches!(first.await, Err(error) if error.is_cancelled()));
        let reentry = tokio::spawn({
            let effects = effects.clone();
            let first_drops = first_drops.clone();
            let second_drops = second_drops.clone();
            async move {
                in_operation_cohort(operation, async move {
                    effects.store(true, Ordering::Release);
                    assert_eq!(first_drops.load(Ordering::Acquire), 1);
                    assert_eq!(second_drops.load(Ordering::Acquire), 1);
                    Ok(())
                })
                .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let initially_claimed = {
                    let table = blocking_table();
                    table.collectors.get(&assoc).is_some_and(|id| {
                        table.records[&first_id].owner == BlockingOwner::Observer(*id)
                            && table.records[&second_id].owner == BlockingOwner::Invocation
                    })
                };
                if initially_claimed {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("collector owns cancelled A while B still has its live invoker");
        assert!(!effects.load(Ordering::Acquire));
        second.abort();
        assert!(matches!(second.await, Err(error) if error.is_cancelled()));
        {
            let table = blocking_table();
            let b_owner = table.records[&second_id].owner;
            assert!(
                b_owner == BlockingOwner::Unresolved
                    || table
                        .collectors
                        .get(&assoc)
                        .is_some_and(|id| { b_owner == BlockingOwner::Observer(*id) })
            );
            assert!(matches!(
                table.records[&second_id].phase,
                BlockingPhase::Live { .. }
            ));
        }
        release_first.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                assert!(
                    !effects.load(Ordering::Acquire),
                    "re-entry body ran while B was held"
                );
                assert!(!reentry.is_finished(), "collector skipped late-abandoned B");
                let adopted = {
                    let table = blocking_table();
                    table.collectors.get(&assoc).is_some_and(|id| {
                        table.records.get(&first_id).is_some_and(|a| {
                            a.owner == BlockingOwner::Observer(*id)
                                && matches!(a.phase, BlockingPhase::Settled { .. })
                        }) && table.records.get(&second_id).is_some_and(|b| {
                            b.owner == BlockingOwner::Observer(*id)
                                && matches!(b.phase, BlockingPhase::Live { .. })
                        })
                    })
                };
                if adopted {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the same collector adopted B before its final success/retirement decision");
        {
            let table = blocking_table();
            let marker = match &table.records[&first_id].phase {
                BlockingPhase::Settled { value: Some(value) } => value
                    .downcast_ref::<Result<Tracked, anyhow::Error>>()
                    .and_then(|result| result.as_ref().ok())
                    .map(|tracked| tracked.marker),
                _ => None,
            };
            assert_eq!(
                marker,
                Some(43),
                "A's completed result lost its actual custodian"
            );
        }
        assert_eq!(first_drops.load(Ordering::Acquire), 0);
        assert_eq!(second_drops.load(Ordering::Acquire), 0);
        assert_eq!(
            in_operation_cohort(uuid::Uuid::new_v4(), async {
                join_owned_blocking(BlockingAssoc::Ordinary, || Ok::<_, anyhow::Error>(53u8))
                    .await?
            })
            .await
            .unwrap(),
            53,
            "an unrelated owner must stay independent"
        );
        assert!(!effects.load(Ordering::Acquire));
        release_second.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), reentry)
            .await
            .expect("both owned results were joined and retired")
            .unwrap()
            .unwrap();
        assert!(effects.load(Ordering::Acquire));
        assert_eq!(first_drops.load(Ordering::Acquire), 1);
        assert_eq!(second_drops.load(Ordering::Acquire), 1);
        let table = blocking_table();
        assert!(!table.records.values().any(|r| r.assoc == assoc));
        assert!(!table.collectors.contains_key(&assoc));
    }

    #[test]
    fn cohort_wrapper_storage_is_independent_of_operation_payload() {
        struct Payload<const N: usize>([u8; N]);
        impl<const N: usize> Future for Payload<N> {
            type Output = Result<()>;

            fn poll(self: Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
                assert_eq!(self.0.len(), N);
                Poll::Ready(Ok(()))
            }
        }
        fn future_size<F: Future>(_factory: impl FnOnce() -> F) -> usize {
            // Never invoke the factory: no operation body is constructed or
            // polled, and no large payload or blocking work runs in this test.
            std::mem::size_of::<F>()
        }
        let operation = uuid::Uuid::nil();
        let worker = BlockingCohort::for_worker(operation);
        assert_eq!(
            future_size(|| in_operation_cohort(operation, Payload([0u8; 1]))),
            future_size(|| in_operation_cohort(operation, Payload([0u8; 64 * 1024]))),
            "the direct-call cohort state must not embed its operation payload"
        );
        assert_eq!(
            future_size(|| worker.run(Payload([0u8; 1]))),
            future_size(|| worker.run(Payload([0u8; 64 * 1024]))),
            "the service cohort state must not embed its worker payload"
        );
    }
}
