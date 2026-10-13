//! Worker for an existing signed request after positively proven native death.
use super::*;
use crate::pair_refresh::recovery::{prepare_owned_refresh_recovery, OwnedRecoveredRefresh};

pub(super) async fn own_recovery(
    engine: &SyncEngine,
    owner: RequestOwner,
    owner_lease: Option<Arc<recovery_owner::RecoveryOwner>>,
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
    let prepared = (|| {
        let actor = engine.capture_pair_refresh_actor_policy(&bearer)?;
        let scope =
            crate::db::repo_scope_identity::repository_scope_uuid(&engine.db().conn(), &child)?;
        let op = actor
            .refresh_request_receipt(engine, &request, &scope)?
            .context("held refresh request not found")?;
        anyhow::ensure!(
            !matches!(
                op.phase,
                PairRefreshPhase::Completed | PairRefreshPhase::CancelledBeforePublication
            ),
            "refresh request is terminal"
        );
        let busy = crate::busy::try_acquire_pair(&child, &op.pins.parent.repo_id)
            .context("refresh pair busy")?;
        // The ordinary daemon already holds its own current fence. A dedicated
        // recovery server may claim only through the authenticated job and its
        // signed native predecessor, never an ordinary foreign-host override.
        if let Some(owner) = owner_lease.as_ref() {
            owner.require_or_claim(engine, &actor, op.id, &busy)?;
        }
        // Ordinary capture checks its existing fence in the candidate's writer
        // transaction; refusals roll back renewal instead of publishing it.
        let candidate = prepare_owned_refresh_recovery(engine, &actor, op.id, &busy)?;
        let digest = candidate.approval_digest(engine, &actor)?;
        let client = match fixture_client {
            Some(client) => client,
            None => super::super::git_sources::source_client()?,
        };
        Ok::<_, anyhow::Error>((actor, op, busy, candidate, digest, client))
    })();
    let (actor, op, busy, candidate, digest, client) = match prepared {
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
        operation_id: op.id,
        request_id: request,
        child_id: child,
        parent_id: op.pins.parent.repo_id,
        approval_digest: digest.clone(),
        kind: "recovery".into(),
        expires_at: (chrono::Utc::now()
            + chrono::Duration::seconds(PREPARATION_LIFETIME.as_secs() as i64))
        .to_rfc3339(),
    };
    if reply.send(Ok(RefreshResponse::Approval(approval))).is_err() {
        return;
    }
    let mut candidate = Some(candidate);
    let mut owned: Option<OwnedRecoveredRefresh> = None;
    loop {
        let command = if owned.is_some() {
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
                "recovery belongs to another actor/session"
            );
            actor.require_current(engine)?;
            anyhow::ensure!(command.digest == digest, "recovery approval digest changed");
            if owned.is_none() {
                anyhow::ensure!(Instant::now() < expiry, "recovery approval expired");
                let value = candidate
                    .take()
                    .context("recovery preparation requires another observation")?;
                owned = Some(value.transfer(engine, &actor, &digest, &busy)?);
            }
            let owned = owned
                .as_ref()
                .expect("approved ownership transfer succeeded");
            match command.action {
                RefreshAction::Execute => {
                    owned
                        .complete_with_client(engine, &actor, &busy, client.clone())
                        .await
                }
                RefreshAction::Reconcile => {
                    owned
                        .reconcile_with_client(engine, &actor, &busy, client.clone())
                        .await
                }
                RefreshAction::Cancel => owned.cancel_before_intent(engine, &actor, &busy),
            }
        }
        .await;
        match outcome {
            Ok(operation) => {
                let terminal = matches!(
                    operation.phase,
                    PairRefreshPhase::Completed | PairRefreshPhase::CancelledBeforePublication
                );
                let _ = command
                    .reply
                    .send(Ok(RefreshReceiptView::operation(operation)));
                if terminal {
                    return;
                }
            }
            Err(error) => {
                let _ = command.reply.send(Err(error));
            }
        }
    }
}
