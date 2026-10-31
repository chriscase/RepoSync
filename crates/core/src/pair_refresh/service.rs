//! Versioned request orchestration over the actual running engine. Live Git
//! handles and owned workspaces stay on one thread; request DTOs carry no proof.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use tokio::sync::oneshot;
use uuid::Uuid;

use super::actor_policy::LiveRefreshActorPolicy;
use super::live_preparation::LivePreparedRefresh;
use super::noop::LivePreparedNoopRefresh;
use crate::db::pair_refresh_operations::{PairRefreshOperation, PairRefreshPhase};
use crate::sync_engine::SyncEngine;

#[path = "service_recovery_owner.rs"]
mod recovery_owner;
#[path = "service_recovery.rs"]
mod recovery_worker;

const PREPARATION_LIFETIME: Duration = Duration::from_secs(300);
const MAX_WORKERS: usize = 4;

#[derive(Debug, Clone, Serialize)]
pub struct RefreshReceiptView {
    pub version: u8,
    pub operation_id: Uuid,
    pub request_id: String,
    pub child_id: String,
    pub parent_id: String,
    pub phase: String,
    pub svn_revision: Option<i64>,
    pub git_sha: Option<String>,
}

impl RefreshReceiptView {
    fn operation(op: PairRefreshOperation) -> Self {
        Self {
            version: 2,
            operation_id: op.id,
            request_id: op.request_id,
            child_id: op.pins.child.repo_id,
            parent_id: op.pins.parent.repo_id,
            phase: serde_json::to_value(op.phase)
                .expect("phase is serializable")
                .as_str()
                .expect("phase is a string")
                .to_owned(),
            svn_revision: op.svn_receipt.map(|receipt| receipt.revision),
            git_sha: op.git_receipt_sha,
        }
    }
    fn noop(receipt: super::noop::NoopRefreshReceipt) -> Self {
        Self {
            version: 2,
            operation_id: receipt.id,
            request_id: receipt.request_id,
            child_id: receipt.pins.child.repo_id,
            parent_id: receipt.pins.parent.repo_id,
            phase: "no_op".into(),
            svn_revision: None,
            git_sha: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RefreshApprovalView {
    pub version: u8,
    pub preparation_id: Uuid,
    pub operation_id: Uuid,
    pub request_id: String,
    pub child_id: String,
    pub parent_id: String,
    pub approval_digest: String,
    pub kind: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "result", content = "details", rename_all = "snake_case")]
pub enum RefreshResponse {
    Approval(RefreshApprovalView),
    Receipt(RefreshReceiptView),
}

#[derive(Debug, Clone, Copy)]
pub enum RefreshAction {
    Execute,
    Reconcile,
    Cancel,
}

struct Command {
    bearer: String,
    digest: String,
    action: RefreshAction,
    reply: oneshot::Sender<Result<RefreshReceiptView>>,
}

struct Worker {
    child: String,
    request: String,
    sender: mpsc::SyncSender<Command>,
}

/// One instance per actual server engine. The registry contains channels and
/// routing metadata, never deserialized native/resource capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownRecord {
    Idle,
    Pending,
    Completed,
    Failed,
}

pub struct RefreshService {
    engine: Arc<SyncEngine>,
    workers: Mutex<HashMap<Uuid, Worker>>,
    fixture_client: Option<reqwest::Client>,
    recovery_owner: Option<Arc<recovery_owner::RecoveryOwner>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    stopping: AtomicBool,
    shutdown: Arc<Mutex<ShutdownRecord>>,
    shutdown_watch: tokio::sync::watch::Sender<ShutdownRecord>,
    shutdown_join: Mutex<Option<std::thread::JoinHandle<()>>>,
    shutdown_joined: AtomicBool,
    worker_failures: Arc<AtomicBool>,
    review_workers: Mutex<std::collections::HashSet<Uuid>>,
}

impl RefreshService {
    pub fn new(engine: Arc<SyncEngine>) -> Arc<Self> {
        let (shutdown_watch, _) = tokio::sync::watch::channel(ShutdownRecord::Idle);
        Arc::new(Self {
            engine,
            workers: Mutex::new(HashMap::new()),
            review_workers: Mutex::new(std::collections::HashSet::new()),
            threads: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            shutdown: Arc::new(Mutex::new(ShutdownRecord::Idle)),
            shutdown_watch,
            shutdown_join: Mutex::new(None),
            shutdown_joined: AtomicBool::new(false),
            worker_failures: Arc::new(AtomicBool::new(false)),
            fixture_client: None,
            recovery_owner: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_fixture_client(
        engine: Arc<SyncEngine>,
        client: reqwest::Client,
    ) -> Arc<Self> {
        let (shutdown_watch, _) = tokio::sync::watch::channel(ShutdownRecord::Idle);
        Arc::new(Self {
            engine,
            workers: Mutex::new(HashMap::new()),
            review_workers: Mutex::new(std::collections::HashSet::new()),
            threads: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            shutdown: Arc::new(Mutex::new(ShutdownRecord::Idle)),
            shutdown_watch,
            shutdown_join: Mutex::new(None),
            shutdown_joined: AtomicBool::new(false),
            worker_failures: Arc::new(AtomicBool::new(false)),
            fixture_client: Some(client),
            recovery_owner: None,
        })
    }

    /// A dedicated recovery server holds the local data-directory lock but
    /// obtains a durable writer epoch only through an authenticated held job.
    pub fn recovery_only(
        engine: Arc<SyncEngine>,
        lock: Arc<crate::data_dir::LockGuard>,
    ) -> Result<Arc<Self>> {
        let owner = recovery_owner::RecoveryOwner::new(engine.clone(), lock)?;
        let (shutdown_watch, _) = tokio::sync::watch::channel(ShutdownRecord::Idle);
        Ok(Arc::new(Self {
            engine,
            workers: Mutex::new(HashMap::new()),
            review_workers: Mutex::new(std::collections::HashSet::new()),
            threads: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            shutdown: Arc::new(Mutex::new(ShutdownRecord::Idle)),
            shutdown_watch,
            shutdown_join: Mutex::new(None),
            shutdown_joined: AtomicBool::new(false),
            worker_failures: Arc::new(AtomicBool::new(false)),
            fixture_client: None,
            recovery_owner: Some(owner),
        }))
    }

    #[cfg(test)]
    pub(crate) fn with_fixture_recovery_client(
        engine: Arc<SyncEngine>,
        lock: Arc<crate::data_dir::LockGuard>,
        client: reqwest::Client,
    ) -> Result<Arc<Self>> {
        let owner = recovery_owner::RecoveryOwner::new(engine.clone(), lock)?;
        let (shutdown_watch, _) = tokio::sync::watch::channel(ShutdownRecord::Idle);
        Ok(Arc::new(Self {
            engine,
            workers: Mutex::new(HashMap::new()),
            review_workers: Mutex::new(std::collections::HashSet::new()),
            threads: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            shutdown: Arc::new(Mutex::new(ShutdownRecord::Idle)),
            shutdown_watch,
            shutdown_join: Mutex::new(None),
            shutdown_joined: AtomicBool::new(false),
            worker_failures: Arc::new(AtomicBool::new(false)),
            fixture_client: Some(client),
            recovery_owner: Some(owner),
        }))
    }

    /// Stop accepting commands and wait for supervised work to finish before
    /// the server releases its engine, native lease or local directory lock.
    pub async fn shutdown(&self) {
        {
            let mut join_slot = self
                .shutdown_join
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let state = *self
                .shutdown
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let already = join_slot.is_some()
                || matches!(
                    state,
                    ShutdownRecord::Pending | ShutdownRecord::Completed | ShutdownRecord::Failed
                );
            if !already {
                self.stopping.store(true, Ordering::Release);
                let threads = {
                    let mut threads = self
                        .threads
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    self.workers
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clear();
                    std::mem::take(&mut *threads)
                };
                let parked = std::sync::Arc::new(std::sync::Mutex::new(Some(threads)));
                let parked_for_thread = std::sync::Arc::clone(&parked);
                let record = Arc::clone(&self.shutdown);
                let worker_failures = Arc::clone(&self.worker_failures);
                let watch = self.shutdown_watch.clone();
                let publish = Arc::new(AtomicBool::new(false));
                let publish_for_thread = Arc::clone(&publish);
                let spawned = std::thread::Builder::new()
                    .name("refresh-shutdown".into())
                    .spawn(move || {
                        let threads = parked_for_thread
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .take()
                            .unwrap_or_default();
                        let failed =
                            join_workers(threads) || worker_failures.load(Ordering::Acquire);
                        while !publish_for_thread.load(Ordering::Acquire) {
                            std::thread::yield_now();
                        }
                        let next = {
                            let mut slot = record
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            if *slot == ShutdownRecord::Pending {
                                *slot = if failed {
                                    ShutdownRecord::Failed
                                } else {
                                    ShutdownRecord::Completed
                                };
                            }
                            *slot
                        };
                        let _ = watch.send(next);
                    });
                match spawned {
                    Ok(handle) => {
                        let mut slot = self
                            .shutdown
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if *slot == ShutdownRecord::Idle {
                            *slot = ShutdownRecord::Pending;
                            let _ = self.shutdown_watch.send(ShutdownRecord::Pending);
                        }
                        drop(slot);
                        *join_slot = Some(handle);
                        publish.store(true, Ordering::Release);
                    }
                    Err(_) => {
                        let restored = parked
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .take()
                            .unwrap_or_default();
                        self.threads
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .extend(restored);
                        *self
                            .shutdown
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                            ShutdownRecord::Failed;
                        let _ = self.shutdown_watch.send(ShutdownRecord::Failed);
                    }
                }
            }
        }
        self.wait_shutdown_record().await;
        loop {
            let observed = {
                let mut slot = self.shutdown_join.lock().unwrap_or_else(|p| p.into_inner());
                match slot.as_ref() {
                    Some(handle) if handle.is_finished() => {
                        let joined = slot.take().unwrap().join();
                        if joined.is_ok() {
                            self.shutdown_joined.store(true, Ordering::Release);
                            Some(true)
                        } else {
                            // Publish failure while still owning the join slot;
                            // another waiter cannot observe None as success.
                            *self.shutdown.lock().unwrap_or_else(|p| p.into_inner()) =
                                ShutdownRecord::Failed;
                            let _ = self.shutdown_watch.send(ShutdownRecord::Failed);
                            Some(false)
                        }
                    }
                    Some(_) => None,
                    None if self.shutdown_joined.load(Ordering::Acquire) => Some(true),
                    None => Some(false),
                }
            };
            match observed {
                Some(true) => return,
                Some(false) => std::future::pending::<()>().await,
                None => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    }

    async fn wait_shutdown_record(&self) {
        let mut rx = self.shutdown_watch.subscribe();
        loop {
            let state = *self
                .shutdown
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match state {
                ShutdownRecord::Completed => return,
                ShutdownRecord::Failed => {
                    std::future::pending::<()>().await;
                }
                ShutdownRecord::Pending | ShutdownRecord::Idle => {
                    if rx.changed().await.is_err() {
                        std::future::pending::<()>().await;
                    }
                }
            }
        }
    }

    pub fn require_engine(&self, engine: &Arc<SyncEngine>) -> Result<()> {
        anyhow::ensure!(
            Arc::ptr_eq(&self.engine, engine),
            "refresh service engine changed"
        );
        Ok(())
    }

    /// Durable authenticated lookup works independently of a live worker and
    /// returns metadata only. It does not renew a lease or repair any journal.
    pub fn receipt(
        &self,
        bearer: &str,
        child: &str,
        request: &str,
    ) -> Result<Option<RefreshReceiptView>> {
        validate_request(child, request)?;
        let actor = self.engine.capture_pair_refresh_actor_policy(bearer)?;
        let scope =
            crate::db::repo_scope_identity::repository_scope_uuid(&self.engine.db().conn(), child)?;
        if let Some(op) = actor.refresh_request_receipt(&self.engine, request, &scope)? {
            return Ok(Some(RefreshReceiptView::operation(op)));
        }
        Ok(actor
            .noop_request_receipt(&self.engine, request, &scope)?
            .map(RefreshReceiptView::noop))
    }

    pub async fn prepare(
        self: &Arc<Self>,
        bearer: String,
        child: String,
        request: String,
    ) -> Result<RefreshResponse> {
        anyhow::ensure!(
            self.recovery_owner.is_none(),
            "recovery server cannot prepare new work"
        );
        if let Some(receipt) = self.receipt(&bearer, &child, &request)? {
            return Ok(RefreshResponse::Receipt(receipt));
        }
        self.spawn_request(bearer, child, request, false).await
    }

    /// Recover only the exact original actor's existing durable request. This
    /// generates a new approval over reconstructed native resources; it does
    /// not transfer the job or replay an intent during preparation.
    pub async fn prepare_recovery(
        self: &Arc<Self>,
        bearer: String,
        child: String,
        request: String,
    ) -> Result<RefreshResponse> {
        let receipt = self
            .receipt(&bearer, &child, &request)?
            .context("held refresh request not found")?;
        if matches!(
            receipt.phase.as_str(),
            "completed" | "cancelled_before_publication" | "no_op"
        ) {
            return Ok(RefreshResponse::Receipt(receipt));
        }
        self.spawn_request(bearer, child, request, true).await
    }

    async fn spawn_request(
        self: &Arc<Self>,
        bearer: String,
        child: String,
        request: String,
        recovery: bool,
    ) -> Result<RefreshResponse> {
        let result = {
            let mut threads = self.threads.lock().unwrap_or_else(|p| p.into_inner());
            anyhow::ensure!(
                !self.stopping.load(Ordering::Acquire),
                "refresh service is stopping"
            );
            if collect_finished_workers(&mut threads) {
                self.worker_failures.store(true, Ordering::Release);
            }
            let handle = Uuid::new_v4();
            let (sender, receiver) = mpsc::sync_channel(1);
            let (reply, result) = oneshot::channel();
            {
                let mut workers = self.workers.lock().unwrap_or_else(|p| p.into_inner());
                // Recheck the durable result under the same lock that reserves a
                // live request, closing the prepare/worker-completion race.
                if let Some(receipt) = self.receipt(&bearer, &child, &request)? {
                    if !recovery
                        || matches!(
                            receipt.phase.as_str(),
                            "completed" | "cancelled_before_publication" | "no_op"
                        )
                    {
                        return Ok(RefreshResponse::Receipt(receipt));
                    }
                }
                anyhow::ensure!(
                    !workers
                        .values()
                        .any(|worker| worker.child == child && worker.request == request),
                    "refresh request already has a live preparation"
                );
                anyhow::ensure!(
                    workers.len() + self.review_workers.lock().unwrap_or_else(|p| p.into_inner()).len() < MAX_WORKERS,
                    "refresh preparation capacity reached"
                );
                workers.insert(
                    handle,
                    Worker {
                        child: child.clone(),
                        request: request.clone(),
                        sender,
                    },
                );
            }
            let service = Arc::downgrade(self);
            let engine = self.engine.clone();
            let client = self.fixture_client.clone();
            let recovery_owner = self.recovery_owner.clone();
            let cleanup_failures = Arc::clone(&self.worker_failures);
            let spawned = std::thread::Builder::new()
                .name("pair-refresh".into())
                .spawn(move || {
                    let _registration = Registration { service, handle };
                    let runtime = match tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            let _ = reply.send(Err(error.into()));
                            return;
                        }
                    };
                    let owner = RequestOwner {
                        bearer,
                        child,
                        request,
                        handle,
                        fixture_client: client,
                    };
                    let blocking = super::execution::BlockingCohort::for_worker(handle);
                    if recovery {
                        runtime.block_on(blocking.run(recovery_worker::own_recovery(
                            &engine,
                            owner,
                            recovery_owner,
                            receiver,
                            reply,
                        )));
                    } else {
                        runtime
                            .block_on(blocking.run(own_request(&engine, owner, receiver, reply)));
                    }
                    if runtime.block_on(blocking.observe()).is_err() {
                        cleanup_failures.store(true, Ordering::Release);
                    }
                });
            match spawned {
                Ok(thread) => threads.push(thread),
                Err(error) => {
                    self.workers
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&handle);
                    return Err(error.into());
                }
            }
            result
        };
        result.await.context("refresh preparation worker stopped")?
    }

    pub async fn act(
        &self,
        bearer: String,
        child: &str,
        request: &str,
        handle: Uuid,
        digest: String,
        action: RefreshAction,
    ) -> Result<RefreshReceiptView> {
        validate_request(child, request)?;
        // Authenticate before dispatch, including unknown/expired handles.
        let actor = self.engine.capture_pair_refresh_actor_policy(&bearer)?;
        let scope =
            crate::db::repo_scope_identity::repository_scope_uuid(&self.engine.db().conn(), child)?;
        if let Some(op) = self.engine.db().authenticated_terminal_refresh_request(
            &self.engine,
            &actor,
            request,
            &scope,
            &digest,
        )? {
            return Ok(RefreshReceiptView::operation(op));
        }
        if let Some(receipt) = actor.noop_request_receipt(&self.engine, request, &scope)? {
            anyhow::ensure!(
                receipt.approval_digest == digest,
                "terminal no-op belongs to another approval"
            );
            return Ok(RefreshReceiptView::noop(receipt));
        }
        anyhow::ensure!(
            !self.stopping.load(Ordering::Acquire),
            "refresh service is stopping"
        );
        let sender = {
            let workers = self.workers.lock().unwrap_or_else(|p| p.into_inner());
            workers
                .get(&handle)
                .filter(|worker| worker.child == child && worker.request == request)
                .map(|worker| worker.sender.clone())
        };
        let Some(sender) = sender else {
            bail!("refresh preparation expired or requires recovery");
        };
        let (reply, result) = oneshot::channel();
        sender
            .try_send(Command {
                bearer,
                digest,
                action,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("refresh worker is busy or stopped"))?;
        result
            .await
            .context("refresh request outcome requires durable lookup")?
    }
}

// A finished handle still owns a result. Observe it before removal so a
// panic is carried into shutdown rather than silently discarded at admission.
fn collect_finished_workers(threads: &mut Vec<std::thread::JoinHandle<()>>) -> bool {
    let mut failed = false;
    let mut index = 0;
    while index < threads.len() {
        if threads[index].is_finished() {
            failed |= threads.swap_remove(index).join().is_err();
        } else {
            index += 1;
        }
    }
    failed
}

fn join_workers(threads: Vec<std::thread::JoinHandle<()>>) -> bool {
    let current = std::thread::current().id();
    let mut failed = false;
    let mut skipped = false;
    for thread in threads {
        // A registration can temporarily hold the last service Arc from this
        // worker after all its SCM work has finished. Never join ourselves.
        if thread.thread().id() == current {
            skipped = true;
            continue;
        }
        if thread.join().is_err() {
            failed = true;
        }
    }
    failed || skipped
}

impl Drop for RefreshService {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.workers
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        if self
            .shutdown_join
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
        {
            return;
        }
        let threads = std::mem::take(
            self.threads
                .get_mut()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        if threads.is_empty() {
            return;
        }
        if join_workers(threads) {
            *self
                .shutdown
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = ShutdownRecord::Failed;
            let _ = self.shutdown_watch.send(ShutdownRecord::Failed);
        }
    }
}

fn validate_request(child: &str, request: &str) -> Result<()> {
    anyhow::ensure!(
        !child.is_empty() && child.len() <= 256 && !request.is_empty() && request.len() <= 256,
        "child and request IDs are required and bounded"
    );
    Ok(())
}

struct Registration {
    service: Weak<RefreshService>,
    handle: Uuid,
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(service) = self.service.upgrade() {
            service
                .workers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&self.handle);
        }
    }
}

enum Preparation {
    Integration(Box<LivePreparedRefresh>),
    Noop(Box<LivePreparedNoopRefresh>),
}

async fn prepare_sources(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    child: &str,
    client: &reqwest::Client,
) -> Result<Preparation> {
    let workspace = actor.create_workspace(engine)?;
    if let Ok(noop) = super::noop::prepare_registered_noop_with_client(
        engine.db(),
        child,
        workspace.clone(),
        client.clone(),
    )
    .await
    {
        return Ok(Preparation::Noop(Box::new(noop)));
    }
    // A separate owned workspace prevents an unsuccessful observation from
    // supplying files or proof to the integration preparation.
    drop(workspace);
    let workspace = actor.create_workspace(engine)?;
    Ok(Preparation::Integration(Box::new(
        super::live_preparation::prepare_registered_in_workspace_with_client(
            engine.db(),
            child,
            workspace,
            client.clone(),
        )
        .await?,
    )))
}

struct RequestOwner {
    bearer: String,
    child: String,
    request: String,
    handle: Uuid,
    fixture_client: Option<reqwest::Client>,
}

struct PanicRetain {
    busy: *mut Option<crate::busy::PairBusyGuard>,
    operation_id: Uuid,
    engine: *const SyncEngine,
}

impl Drop for PanicRetain {
    fn drop(&mut self) {
        if std::thread::panicking() {
            unsafe {
                let holds = (*self.engine)
                    .db()
                    .verified_refresh_job_holds(self.operation_id)
                    .map_err(|_| ());
                release_busy_on_receiver_close(&mut *self.busy, holds);
            }
        }
    }
}

fn release_busy_on_receiver_close(
    busy: &mut Option<crate::busy::PairBusyGuard>,
    holds: Result<bool, ()>,
) {
    match holds {
        Ok(false) => {
            *busy = None;
        }
        Ok(true) | Err(()) => {
            if let Some(guard) = busy.take() {
                std::mem::forget(guard);
            }
        }
    }
}

async fn own_request(
    engine: &SyncEngine,
    owner: RequestOwner,
    receiver: mpsc::Receiver<Command>,
    reply: oneshot::Sender<Result<RefreshResponse>>,
) {
    let RequestOwner {
        bearer,
        child,
        request,
        handle,
        fixture_client,
    } = owner;
    let prepared = async {
        let actor = engine.capture_pair_refresh_actor_policy(&bearer)?;
        let client = match fixture_client {
            Some(client) => client,
            None => super::git_sources::source_client()?,
        };
        let started = Instant::now();
        // Preserve supervised command cleanup instead of abandoning an active
        // SCM subprocess future. An overlong preparation grants no approval.
        let preparation = prepare_sources(engine, &actor, &child, &client).await?;
        anyhow::ensure!(
            started.elapsed() < PREPARATION_LIFETIME,
            "refresh preparation timed out"
        );
        let (operation_id, parent, digest, kind) = match &preparation {
            Preparation::Integration(value) => (
                value.pins().git.operation_id,
                value.pins().parent.repo_id.clone(),
                actor.prepared_approval_digest(engine, value)?,
                "integration",
            ),
            Preparation::Noop(value) => (
                value.pins().operation_id,
                value.pins().parent.repo_id.clone(),
                actor.noop_approval_digest(engine, value)?,
                "no_op",
            ),
        };
        Ok::<_, anyhow::Error>((
            actor,
            preparation,
            operation_id,
            parent,
            digest,
            kind,
            client,
        ))
    }
    .await;
    let (actor, preparation, operation_id, parent, digest, kind, client) = match prepared {
        Ok(value) => value,
        Err(error) => {
            let _ = reply.send(Err(error));
            return;
        }
    };
    let expiry = Instant::now() + PREPARATION_LIFETIME;
    let approval = RefreshApprovalView {
        version: 2,
        preparation_id: handle,
        operation_id,
        request_id: request.clone(),
        child_id: child.clone(),
        parent_id: parent.clone(),
        approval_digest: digest.clone(),
        kind: kind.into(),
        expires_at: (chrono::Utc::now()
            + chrono::Duration::seconds(PREPARATION_LIFETIME.as_secs() as i64))
        .to_rfc3339(),
    };
    if reply.send(Ok(RefreshResponse::Approval(approval))).is_err() {
        return;
    }
    let mut busy: Option<crate::busy::PairBusyGuard> = None;
    let _panic_retain = PanicRetain {
        busy: &mut busy as *mut Option<crate::busy::PairBusyGuard>,
        operation_id,
        engine: engine as *const SyncEngine,
    };
    loop {
        let command = if busy.is_some() {
            receiver.recv().ok()
        } else {
            receiver
                .recv_timeout(expiry.saturating_duration_since(Instant::now()))
                .ok()
        };
        let Some(command) = command else {
            release_busy_on_receiver_close(
                &mut busy,
                engine
                    .db()
                    .verified_refresh_job_holds(operation_id)
                    .map_err(|_| ()),
            );
            return;
        };
        let outcome = async {
            let current = engine.capture_pair_refresh_actor_policy(&command.bearer)?;
            anyhow::ensure!(
                current.pin() == actor.pin(),
                "preparation belongs to another actor/session"
            );
            actor.require_current(engine)?;
            anyhow::ensure!(command.digest == digest, "approval digest changed");
            anyhow::ensure!(
                !(matches!(preparation, Preparation::Noop(_))
                    && matches!(command.action, RefreshAction::Reconcile)),
                "no-op preparation has no publication intent"
            );
            if busy.is_none() {
                anyhow::ensure!(Instant::now() < expiry, "refresh approval expired");
                busy = Some(
                    crate::busy::try_acquire_pair(&child, &parent).context("refresh pair busy")?,
                );
            }
            let held = busy.as_ref().expect("busy guard was acquired");
            match &preparation {
                Preparation::Integration(prepared) => {
                    let scope = &prepared.pins().child.scope_uuid;
                    let result = match command.action {
                        RefreshAction::Execute => {
                            super::execution::complete_owned_refresh_with_client(
                                engine,
                                &actor,
                                prepared,
                                &request,
                                &digest,
                                held,
                                client.clone(),
                            )
                            .await?
                        }
                        RefreshAction::Reconcile => {
                            super::execution::reconcile_owned_refresh_intent_with_client(
                                engine,
                                &actor,
                                prepared,
                                held,
                                client.clone(),
                            )
                            .await?
                        }
                        RefreshAction::Cancel => {
                            if actor
                                .refresh_request_receipt(engine, &request, scope)?
                                .is_none()
                            {
                                return Ok((
                                    cancelled_view(operation_id, &request, &child, &parent),
                                    true,
                                ));
                            }
                            actor.cancel_prepared_enrollment(engine, prepared, held)?
                        }
                    };
                    let terminal = matches!(
                        result.phase,
                        PairRefreshPhase::Completed | PairRefreshPhase::CancelledBeforePublication
                    );
                    Ok((RefreshReceiptView::operation(result), terminal))
                }
                Preparation::Noop(prepared) => match command.action {
                    RefreshAction::Execute => Ok((
                        RefreshReceiptView::noop(
                            super::noop::complete_owned_noop_with_client(
                                engine,
                                &actor,
                                prepared,
                                &request,
                                &digest,
                                held,
                                client.clone(),
                            )
                            .await?,
                        ),
                        true,
                    )),
                    RefreshAction::Cancel => Ok((
                        cancelled_view(operation_id, &request, &child, &parent),
                        true,
                    )),
                    RefreshAction::Reconcile => {
                        bail!("no-op preparation has no publication intent")
                    }
                },
            }
        }
        .await;
        match outcome {
            Ok((receipt, terminal)) => {
                let _ = command.reply.send(Ok(receipt));
                if terminal {
                    return;
                }
            }
            Err(error) => {
                // Refused unadmitted work does not become an indefinite owner.
                // A complete read proves absence/terminal state; corrupt or
                // holding history retains the guard conservatively.
                if matches!(
                    engine.db().verified_refresh_job_holds(operation_id),
                    Ok(false)
                ) {
                    busy = None;
                }
                let _ = command.reply.send(Err(error));
            }
        }
    }
}

fn cancelled_view(id: Uuid, request: &str, child: &str, parent: &str) -> RefreshReceiptView {
    RefreshReceiptView {
        version: 2,
        operation_id: id,
        request_id: request.into(),
        child_id: child.into(),
        parent_id: parent.into(),
        phase: "preparation_cancelled".into(),
        svn_revision: None,
        git_sha: None,
    }
}

#[cfg(test)]
mod service_shutdown_and_receiver {
    use super::{release_busy_on_receiver_close, RefreshService, ShutdownRecord};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct WorkerCleanup {
        release: Arc<AtomicBool>,
        service: Option<Arc<RefreshService>>,
    }

    impl WorkerCleanup {
        fn arm(release: Arc<AtomicBool>, service: Arc<RefreshService>) -> Self {
            Self {
                release,
                service: Some(service),
            }
        }

        fn finish(&mut self) {
            self.release.store(true, Ordering::Release);
            let Some(service) = self.service.take() else {
                return;
            };
            let handle = service
                .shutdown_join
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            drop(service);
            if let Some(handle) = handle {
                let _ = handle.join();
            }
        }
    }

    impl Drop for WorkerCleanup {
        fn drop(&mut self) {
            self.finish();
        }
    }

    fn record(service: &RefreshService) -> ShutdownRecord {
        *service
            .shutdown
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn idle_service() -> (tempfile::TempDir, Arc<RefreshService>) {
        let temp = tempfile::tempdir().unwrap();
        let mut config: crate::config::AppConfig = toml::from_str(
            r#"
            [daemon]
            data_dir = "/unused"
            [svn]
            url = "file:///unused/svn"
            username = ""
            password_env = ""
            [github]
            repo = "fixture/repo"
            token_env = ""
            "#,
        )
        .unwrap();
        config.daemon.data_dir = temp.path().to_owned();
        let git_dir = temp.path().join("engine-git");
        git2::Repository::init(&git_dir).unwrap();
        let engine = crate::sync_engine::SyncEngine::new(
            config,
            crate::Database::new(temp.path().join("reposync.db")).unwrap(),
            crate::svn::SvnClient::new("file:///unused/svn", "", ""),
            crate::git::client::GitClient::new(&git_dir).unwrap(),
            Arc::new(
                crate::identity::mapper::IdentityMapper::new(
                    &crate::config::IdentityConfig::default(),
                )
                .unwrap(),
            ),
        );
        (temp, RefreshService::new(Arc::new(engine)))
    }

    #[tokio::test]
    async fn empty_service_shutdown_returns_only_for_completed() {
        let (_temp, service) = idle_service();
        assert!(matches!(record(&service), ShutdownRecord::Idle));
        tokio::time::timeout(Duration::from_secs(2), service.shutdown())
            .await
            .expect("an empty worker set reaches Completed");
        assert!(matches!(record(&service), ShutdownRecord::Completed));
        assert!(service.shutdown_joined.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn cancelled_shutdown_stays_pending() {
        let (_temp, service) = idle_service();
        let release = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let _cleanup = WorkerCleanup::arm(Arc::clone(&release), Arc::clone(&service));
        {
            let release = Arc::clone(&release);
            let finished = Arc::clone(&finished);
            service
                .threads
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(std::thread::spawn(move || {
                    while !release.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    finished.store(true, Ordering::Release);
                }));
        }
        let observed = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.shutdown().await })
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(record(&service), ShutdownRecord::Pending) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("shutdown entered Pending");
        observed.abort();
        let _ = observed.await;
        assert!(matches!(record(&service), ShutdownRecord::Pending));
        assert!(!finished.load(Ordering::Acquire));
        let mut second = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.shutdown().await })
        };
        let returned_early = tokio::select! {
            biased;
            joined = &mut second => Some(joined),
            _ = tokio::time::sleep(Duration::from_millis(200)) => None,
        };
        assert!(
            returned_early.is_none(),
            "shutdown join returned before the worker was released"
        );
        assert!(matches!(record(&service), ShutdownRecord::Pending));
        release.store(true, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .expect("the second await returns only for Completed")
            .unwrap();
        assert!(finished.load(Ordering::Acquire));
        assert!(matches!(record(&service), ShutdownRecord::Completed));
        assert!(service.shutdown_joined.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn panic_then_joining_the_other_worker_latches_failed() {
        let (_temp, service) = idle_service();
        let release = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let mut cleanup = WorkerCleanup::arm(Arc::clone(&release), Arc::clone(&service));
        {
            let release = Arc::clone(&release);
            let finished = Arc::clone(&finished);
            let mut threads = service
                .threads
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            threads.push(std::thread::spawn(|| panic!("worker")));
            threads.push(std::thread::spawn(move || {
                while !release.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                finished.store(true, Ordering::Release);
            }));
        }
        let first = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.shutdown().await })
        };
        let not_joined = tokio::time::timeout(Duration::from_millis(200), async {
            loop {
                if finished.load(Ordering::Acquire) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(not_joined.is_err());
        assert!(matches!(record(&service), ShutdownRecord::Pending));
        release.store(true, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if finished.load(Ordering::Acquire)
                    && matches!(record(&service), ShutdownRecord::Failed)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the other worker was joined and Failed latched");
        first.abort();
        let _ = first.await;
        assert!(matches!(record(&service), ShutdownRecord::Failed));
        let mut again = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.shutdown().await })
        };
        let returned_while_failed = tokio::select! {
            biased;
            joined = &mut again => Some(joined),
            _ = tokio::time::sleep(Duration::from_millis(200)) => None,
        };
        assert!(
            returned_while_failed.is_none(),
            "shutdown join returned while Failed"
        );
        assert!(matches!(record(&service), ShutdownRecord::Failed));
        again.abort();
        let _ = again.await;
        cleanup.finish();
        drop(cleanup);
        let service = match Arc::try_unwrap(service) {
            Ok(owned) => owned,
            Err(_shared) => panic!("no observer still holds the service"),
        };
        let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(service)));
        assert!(dropped.is_ok());
    }

    #[test]
    fn receiver_close_forgets_a_holding_guard_and_drops_a_proved_absence() {
        let held_a = format!("recv-hold-a-{}", std::process::id());
        let held_b = format!("recv-hold-b-{}", std::process::id());
        let mut busy = Some(crate::busy::try_acquire_pair(&held_a, &held_b).unwrap());
        release_busy_on_receiver_close(&mut busy, Ok(true));
        assert!(busy.is_none());
        assert!(crate::busy::is_busy(&held_a));
        assert!(crate::busy::is_busy(&held_b));

        let free_a = format!("recv-free-a-{}", std::process::id());
        let free_b = format!("recv-free-b-{}", std::process::id());
        let mut busy = Some(crate::busy::try_acquire_pair(&free_a, &free_b).unwrap());
        release_busy_on_receiver_close(&mut busy, Ok(false));
        assert!(busy.is_none());
        assert!(!crate::busy::is_busy(&free_a));
        assert!(!crate::busy::is_busy(&free_b));

        let bad_a = format!("recv-bad-a-{}", std::process::id());
        let bad_b = format!("recv-bad-b-{}", std::process::id());
        let mut busy = Some(crate::busy::try_acquire_pair(&bad_a, &bad_b).unwrap());
        release_busy_on_receiver_close(&mut busy, Err(()));
        assert!(busy.is_none());
        assert!(crate::busy::is_busy(&bad_a));
    }
    #[tokio::test]
    async fn finished_worker_panic_is_not_lost_before_shutdown() {
        let (_temp, service) = idle_service();
        let worker = std::thread::spawn(|| panic!("finished worker"));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !worker.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("worker finished");
        {
            let mut threads = service.threads.lock().unwrap_or_else(|p| p.into_inner());
            threads.push(worker);
            if super::collect_finished_workers(&mut threads) {
                service.worker_failures.store(true, Ordering::Release);
            }
            assert!(threads.is_empty());
        }
        assert!(service.worker_failures.load(Ordering::Acquire));
        let _cleanup = WorkerCleanup::arm(Arc::new(AtomicBool::new(true)), Arc::clone(&service));
        let stopped = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.shutdown().await })
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while !matches!(record(&service), ShutdownRecord::Failed) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("saved worker failure reached shutdown");
        assert!(!stopped.is_finished(), "Failed must not become Completed");
        stopped.abort();
        let _ = stopped.await;
        assert!(matches!(record(&service), ShutdownRecord::Failed));
    }
}
/// JSON intent for inspection, deliberately unrelated to RefreshAction and its
/// preparation/approval handles. A review pin is never an execution credential.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshReviewCandidateRequest {
    pub review_pin: String,
    pub resolution: super::preparation::ChangedChildResolutionCandidateRequest,
}

#[derive(Serialize)]
pub struct RefreshReviewResponse {
    pub version: u8,
    pub purpose: &'static str,
    pub state: &'static str,
    pub pending: super::live_preparation::RegisteredReviewIntake,
    pub applicability: Option<serde_json::Value>,
    pub candidate: Option<serde_json::Value>,
    pub review_pin: Option<String>,
    pub blocked: Vec<String>,
    pub actor_approval: bool,
    pub execution_eligible: bool,
}

enum ReviewCommand {
    Read,
    Candidate(RefreshReviewCandidateRequest),
    #[cfg(test)]
    ReadAtBoundary(Arc<dyn Fn() + Send + Sync>),
}

/// Fixed inspection refusal codes. Transport/native errors are deliberately not
/// exposed in the read API, including private scratch paths or credentials.
#[derive(Debug, Clone, Copy)]
pub enum RefreshReviewRefusal {
    MalformedRequest,
    StaleReview,
    ChoicesOrSourcesRefused,
    AuthorizationChanged,
    SourcesChanged,
    Unavailable,
}

impl RefreshReviewRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::MalformedRequest => "inspection_malformed_request",
            Self::StaleReview => "inspection_stale_review",
            Self::ChoicesOrSourcesRefused => "inspection_choices_or_sources_refused",
            Self::AuthorizationChanged => "inspection_authorization_changed",
            Self::SourcesChanged => "inspection_sources_changed",
            Self::Unavailable => "inspection_unavailable",
        }
    }
}

impl std::fmt::Display for RefreshReviewRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for RefreshReviewRefusal {}

struct ReviewRegistration {
    service: Weak<RefreshService>,
    handle: Uuid,
}

impl Drop for ReviewRegistration {
    fn drop(&mut self) {
        if let Some(service) = self.service.upgrade() {
            service.review_workers.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.handle);
        }
    }
}

fn require_review_pin(pin: &str) -> Result<()> {
    anyhow::ensure!(
        pin.len() == 64 && pin.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        RefreshReviewRefusal::MalformedRequest
    );
    Ok(())
}

fn authenticated_review_pin(
    actor: &LiveRefreshActorPolicy<'_>,
    intake: &super::live_preparation::RegisteredReviewIntake,
    manifest: &super::live_preparation::ChangedChildApplicabilityManifest,
) -> Result<String> {
    use sha2::Digest;
    Ok(hex::encode(sha2::Sha256::digest(serde_json::to_vec(&(
        "authenticated_inspection_review_pin_v1",
        actor.pin(),
        &intake.observation_digest,
        &manifest.manifest_digest,
    ))?)))
}

impl RefreshService {
    /// A real configured-source read, not a preparation or approval request.
    pub async fn review(self: &Arc<Self>, bearer: String, child: String) -> Result<RefreshReviewResponse> {
        self.spawn_review(bearer, child, ReviewCommand::Read).await
    }

    /// A source-test fault boundary only: runs after real observation, never
    /// replaces an OS call, source snapshot, actor or registered configuration.
    #[cfg(test)]
    pub(super) async fn review_at_boundary(
        self: &Arc<Self>,
        bearer: String,
        child: String,
        boundary: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<RefreshReviewResponse> {
        self.spawn_review(bearer, child, ReviewCommand::ReadAtBoundary(boundary)).await
    }

    #[cfg(test)]
    pub(super) fn review_shutdown_record(&self) -> ShutdownRecord {
        *self.shutdown.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub async fn review_candidate(
        self: &Arc<Self>,
        bearer: String,
        child: String,
        request: RefreshReviewCandidateRequest,
    ) -> Result<RefreshReviewResponse> {
        require_review_pin(&request.review_pin)?;
        super::preparation::require_changed_child_resolution_request_header(&request.resolution).map_err(|_| RefreshReviewRefusal::MalformedRequest)?;
        anyhow::ensure!(request.resolution.choices.len() <= 1000, RefreshReviewRefusal::MalformedRequest);
        self.spawn_review(bearer, child, ReviewCommand::Candidate(request)).await
    }

    /// Confined to the existing explicit reliability-fixture build. Production
    /// construction still uses the normal pinned HTTPS client and trust store.
    /// This supplies only a transport client, never a source/actor/mapping proof.
    #[cfg(feature = "reliability-fixture")]
    pub fn with_review_fixture_client(engine: Arc<SyncEngine>, client: reqwest::Client) -> Arc<Self> {
        let mut service = Self::new(engine);
        Arc::get_mut(&mut service).expect("new service has one owner").fixture_client = Some(client);
        service
    }

    async fn spawn_review(
        self: &Arc<Self>,
        bearer: String,
        child: String,
        command: ReviewCommand,
    ) -> Result<RefreshReviewResponse> {
        anyhow::ensure!(!child.is_empty() && child.len() <= 256, "bounded repository ID required");
        anyhow::ensure!(self.recovery_owner.is_none(), "inspection unavailable on recovery-only service");
        // Admission precedes configured endpoint/scratch reads, also for a
        // direct service caller that did not traverse the HTTP bearer helper.
        self.engine.capture_pair_refresh_actor_policy(&bearer)?;
        let result = {
            let mut threads = self.threads.lock().unwrap_or_else(|p| p.into_inner());
            anyhow::ensure!(!self.stopping.load(Ordering::Acquire), "refresh service is stopping");
            if collect_finished_workers(&mut threads) {
                self.worker_failures.store(true, Ordering::Release);
            }
            // Same lock order/admission budget as spawn_request. No detached
            // work or extra independent read-worker budget.
            let workers = self.workers.lock().unwrap_or_else(|p| p.into_inner());
            let mut readers = self.review_workers.lock().unwrap_or_else(|p| p.into_inner());
            anyhow::ensure!(workers.len() + readers.len() < MAX_WORKERS, "refresh preparation capacity reached");
            let handle = Uuid::new_v4();
            readers.insert(handle);
            drop(readers);
            drop(workers);
            let (reply, result) = oneshot::channel();
            let registration = ReviewRegistration { service: Arc::downgrade(self), handle };
            let engine = self.engine.clone();
            let client = self.fixture_client.clone();
            let spawned = std::thread::Builder::new().name("pair-refresh-review".into()).spawn(move || {
                let _registration = registration;
                let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        let _ = reply.send(Err(anyhow::anyhow!("inspection runtime unavailable")));
                        return;
                    }
                };
                // HTTP cancellation drops only the receiver. This thread owns
                // its complete future/resources until it finishes; its actual
                // JoinHandle is retained by the existing shutdown collector.
                let value = runtime.block_on(Box::pin(authenticated_review(&engine, &bearer, &child, client, command)));
                drop(runtime);
                let _ = reply.send(value);
            });
            match spawned {
                Ok(thread) => threads.push(thread),
                Err(_) => {
                    self.review_workers.lock().unwrap_or_else(|p| p.into_inner()).remove(&handle);
                    bail!("inspection worker unavailable");
                }
            }
            result
        };
        result.await.context("inspection worker ended without a result")?
    }
}

async fn authenticated_review(
    engine: &SyncEngine,
    bearer: &str,
    child: &str,
    client: Option<reqwest::Client>,
    command: ReviewCommand,
) -> Result<RefreshReviewResponse> {
    use super::live_preparation::{
        capture_registered_review_intake, capture_review_applicability, capture_review_resolution,
    };
    let actor = engine.capture_pair_refresh_actor_policy(bearer).map_err(|_| RefreshReviewRefusal::AuthorizationChanged)?;
    let intake = capture_registered_review_intake(engine.db(), child, &engine.config().daemon.data_dir, client.clone()).await.map_err(|_| RefreshReviewRefusal::Unavailable)?;
    actor.require_current(engine).map_err(|_| RefreshReviewRefusal::AuthorizationChanged)?;
    let raw_intake = intake.clone();
    let mut blocked = intake.blocked_reasons();
    let mut expected_context = None;
    #[cfg(test)]
    let boundary = match &command {
        ReviewCommand::ReadAtBoundary(boundary) => Some(boundary.clone()),
        _ => None,
    };
    let mut response = RefreshReviewResponse {
        version: 2,
        purpose: "inspection_only",
        state: "blocked",
        pending: intake,
        applicability: None,
        candidate: None,
        review_pin: None,
        blocked: Vec::new(),
        actor_approval: false,
        execution_eligible: false,
    };
    if response.pending.may_attempt_correspondence() {
        match capture_review_applicability(engine.db(), child, client.clone()).await {
            Ok(review) => {
                response.pending.bind_correspondence_presence(&review).map_err(|_| RefreshReviewRefusal::SourcesChanged)?;
                blocked = response.pending.blocked_reasons();
                anyhow::ensure!(blocked.is_empty(), RefreshReviewRefusal::Unavailable);
                let pin = authenticated_review_pin(&actor, &response.pending, review.manifest())?;
                expected_context = Some((
                    review.manifest().configuration.clone(),
                    review.manifest().candidate_set_digest.clone(),
                    review.manifest().mapping_snapshot_digest.clone(),
                ));
                response.applicability = Some(serde_json::to_value(review.manifest())?);
                match command {
                    ReviewCommand::Read => {
                        response.state = "ready";
                    }
                    #[cfg(test)]
                    ReviewCommand::ReadAtBoundary(_) => {
                        response.state = "ready";
                    }
                    ReviewCommand::Candidate(request) => {
                        anyhow::ensure!(request.review_pin == pin, RefreshReviewRefusal::StaleReview);
                        review.require_manifest_digest(&request.resolution.manifest_digest).map_err(|_| RefreshReviewRefusal::StaleReview)?;
                        let candidate = capture_review_resolution(engine.db(), child, &request.resolution, client.clone()).await.map_err(|_| RefreshReviewRefusal::ChoicesOrSourcesRefused)?;
                        anyhow::ensure!(
                            authenticated_review_pin(&actor, &response.pending, candidate.applicability_review().manifest())? == pin,
                            RefreshReviewRefusal::SourcesChanged
                        );
                        response.candidate = Some(serde_json::to_value(candidate.manifest())?);
                        response.state = "candidate";
                    }
                }
                response.review_pin = Some(pin);
            }
            Err(_) => blocked.push("registered_history_correspondence_visibility_or_topology_refused".into()),
        }
    }
    #[cfg(test)]
    if let Some(boundary) = boundary {
        boundary();
    }
    let repeated = capture_registered_review_intake(engine.db(), child, &engine.config().daemon.data_dir, client).await.map_err(|_| RefreshReviewRefusal::Unavailable)?;
    // Compare like-for-like raw observations. The separately promoted complete
    // flags/pins came from the actual opaque correspondence factory above and
    // are bound to exactly this configuration, role and observed peg interval.
    anyhow::ensure!(repeated == raw_intake, RefreshReviewRefusal::SourcesChanged);
    // The extra intake extends the observation interval. Recheck the native
    // candidate/mapping closure again rather than merely comparing tips/rows.
    if let Some((pin, candidates, mappings)) = expected_context {
        let current = engine.db().capture_pair_refresh_execution_configuration(child).map_err(|_| RefreshReviewRefusal::SourcesChanged)?;
        let candidate = engine.db().capture_changed_child_baseline_candidate_review(&current).map_err(|_| RefreshReviewRefusal::SourcesChanged)?;
        let mapping = engine.db().capture_changed_child_mapping_review(&current, &candidate).map_err(|_| RefreshReviewRefusal::SourcesChanged)?;
        let final_configuration = engine.db().capture_pair_refresh_execution_configuration(child).map_err(|_| RefreshReviewRefusal::SourcesChanged)?;
        anyhow::ensure!(
            current.pin() == &pin
                && candidate.candidate_set_digest() == candidates.as_str()
                && mapping.snapshot_digest() == mappings.as_str()
                && final_configuration.pin() == &pin,
            RefreshReviewRefusal::SourcesChanged
        );
    }
    actor.require_current(engine).map_err(|_| RefreshReviewRefusal::AuthorizationChanged)?;
    response.blocked = blocked;
    Ok(response)
}
