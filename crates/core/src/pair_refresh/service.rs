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
pub struct RefreshService {
    engine: Arc<SyncEngine>,
    workers: Mutex<HashMap<Uuid, Worker>>,
    fixture_client: Option<reqwest::Client>,
    recovery_owner: Option<Arc<recovery_owner::RecoveryOwner>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    stopping: AtomicBool,
}

impl RefreshService {
    pub fn new(engine: Arc<SyncEngine>) -> Arc<Self> {
        Arc::new(Self {
            engine,
            workers: Mutex::new(HashMap::new()),
            threads: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            fixture_client: None,
            recovery_owner: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_fixture_client(
        engine: Arc<SyncEngine>,
        client: reqwest::Client,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine,
            workers: Mutex::new(HashMap::new()),
            threads: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
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
        Ok(Arc::new(Self {
            engine,
            workers: Mutex::new(HashMap::new()),
            threads: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
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
        Ok(Arc::new(Self {
            engine,
            workers: Mutex::new(HashMap::new()),
            threads: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            fixture_client: Some(client),
            recovery_owner: Some(owner),
        }))
    }

    /// Stop accepting commands and wait for supervised work to finish before
    /// the server releases its engine, native lease or local directory lock.
    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        let threads = {
            let mut threads = self.threads.lock().unwrap_or_else(|p| p.into_inner());
            self.workers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
            std::mem::take(&mut *threads)
        };
        let _ = tokio::task::spawn_blocking(move || join_workers(threads)).await;
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
            threads.retain(|thread| !thread.is_finished());
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
                    workers.len() < MAX_WORKERS,
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
                    if recovery {
                        runtime.block_on(recovery_worker::own_recovery(
                            &engine,
                            owner,
                            recovery_owner,
                            receiver,
                            reply,
                        ));
                    } else {
                        runtime.block_on(own_request(&engine, owner, receiver, reply));
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

fn join_workers(threads: Vec<std::thread::JoinHandle<()>>) {
    let current = std::thread::current().id();
    for thread in threads {
        // A registration can temporarily hold the last service Arc from this
        // worker after all its SCM work has finished. Never join ourselves.
        if thread.thread().id() != current {
            let _ = thread.join();
        }
    }
}

impl Drop for RefreshService {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.workers
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        join_workers(std::mem::take(
            self.threads.get_mut().unwrap_or_else(|p| p.into_inner()),
        ));
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
    let mut busy = None;
    loop {
        let command = if busy.is_some() {
            receiver.recv().ok()
        } else {
            receiver
                .recv_timeout(expiry.saturating_duration_since(Instant::now()))
                .ok()
        };
        let Some(command) = command else {
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
