//! Current authenticated recovery of retained resources. Native death and a
//! signed ownership transfer never authorize replaying an existing intent.

use anyhow::Result;
use uuid::Uuid;

use super::actor_policy::LiveRefreshActorPolicy;
use super::live_preparation::LivePreparedRefresh;
use crate::busy::PairBusyGuard;
use crate::db::pair_refresh_operations::{PairRefreshOperation, VerifiedRecoveryCandidate};
use crate::sync_engine::SyncEngine;

pub struct LiveRefreshRecovery {
    candidate: VerifiedRecoveryCandidate,
    prepared: LivePreparedRefresh,
}

pub struct OwnedRecoveredRefresh {
    operation: PairRefreshOperation,
    prepared: LivePreparedRefresh,
}

/// Obtain a new durable writer epoch only for this authenticated retained job
/// after positive native death proof. Keep the supplied local lock held for
/// the returned fence's lifetime; publication still needs approved transfer.
pub fn claim_owned_refresh_recovery_writer(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    operation_id: Uuid,
    busy: &PairBusyGuard,
    lock: &crate::data_dir::LockGuard,
) -> Result<crate::writer_fence::WriterFenceGuard> {
    Ok(engine
        .db()
        .claim_authenticated_recovery_writer(engine, actor, operation_id, busy, lock)?)
}

impl LiveRefreshRecovery {
    pub fn operation(&self) -> &PairRefreshOperation {
        self.candidate.operation()
    }

    pub fn approval_digest(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
    ) -> Result<String> {
        actor.require_current(engine)?;
        self.prepared
            .workspace()?
            .require_binding(engine.db(), actor.pin())?;
        Ok(self.candidate.approval_digest(actor, &self.prepared)?)
    }

    pub fn transfer(
        self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        approved_digest: &str,
        busy: &PairBusyGuard,
    ) -> Result<OwnedRecoveredRefresh> {
        let operation = engine.db().transfer_authenticated_recovery(
            engine,
            actor,
            &self.candidate,
            &self.prepared,
            approved_digest,
            busy,
        )?;
        Ok(OwnedRecoveredRefresh {
            operation,
            prepared: self.prepared,
        })
    }
}

impl OwnedRecoveredRefresh {
    #[cfg(test)]
    pub(crate) fn prepared_for_test(&self) -> &LivePreparedRefresh {
        &self.prepared
    }

    pub fn operation(&self) -> &PairRefreshOperation {
        &self.operation
    }

    /// The existing pipeline revalidates actual current owner and remote inputs
    /// at every gate. It refuses SVN/Git intent replay without another write.
    pub async fn complete(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        busy: &PairBusyGuard,
    ) -> Result<PairRefreshOperation> {
        self.complete_with_client(engine, actor, busy, super::git_sources::source_client()?)
            .await
    }

    pub(crate) async fn complete_with_client(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        busy: &PairBusyGuard,
        client: reqwest::Client,
    ) -> Result<PairRefreshOperation> {
        super::execution::complete_owned_refresh_with_client(
            engine,
            actor,
            &self.prepared,
            &self.operation.request_id,
            &self.operation.approval_digest,
            busy,
            client,
        )
        .await
    }

    pub async fn reconcile_intent(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        busy: &PairBusyGuard,
    ) -> Result<PairRefreshOperation> {
        self.reconcile_with_client(engine, actor, busy, super::git_sources::source_client()?)
            .await
    }

    pub(crate) async fn reconcile_with_client(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        busy: &PairBusyGuard,
        client: reqwest::Client,
    ) -> Result<PairRefreshOperation> {
        super::execution::reconcile_owned_refresh_intent_with_client(
            engine,
            actor,
            &self.prepared,
            busy,
            client,
        )
        .await
    }

    pub fn cancel_before_intent(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        busy: &PairBusyGuard,
    ) -> Result<PairRefreshOperation> {
        Ok(actor.cancel_prepared_enrollment(engine, &self.prepared, busy)?)
    }
}

pub fn prepare_owned_refresh_recovery(
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    operation_id: Uuid,
    busy: &PairBusyGuard,
) -> Result<LiveRefreshRecovery> {
    let mut candidate =
        engine
            .db()
            .capture_authenticated_recovery(engine, actor, operation_id, busy)?;
    let workspace = super::workspace::RefreshWorkspace::reconstruct(
        engine.db(),
        &engine.config().daemon.data_dir,
        &candidate,
    )?;
    let prepared = LivePreparedRefresh::reconstruct(&mut candidate, workspace)?;
    actor.require_current(engine)?;
    candidate.require_native_proof()?;
    Ok(LiveRefreshRecovery {
        candidate,
        prepared,
    })
}
