//! Signed original evidence remains immutable across native owner transfers.
//! A transfer approves ownership only; an existing intent is never retry permission.

use super::*;
use crate::pair_refresh::native_owner::NativeDeadOwnerProof;
use crate::pair_refresh::workspace::RefreshResourcePin;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecoveryStep {
    version: u8,
    predecessor_mac: String,
    writer_epoch: u64,
    evidence: Evidence,
    approved_digest: String,
    transferred_at: String,
}

pub(crate) struct VerifiedRecoveryCandidate {
    operation: PairRefreshOperation,
    authority_raw: String,
    resources: RefreshResourcePin,
    old_resources: RefreshResourcePin,
    dead_owner: NativeDeadOwnerProof,
    epoch: u64,
    actor_pin: crate::pair_refresh::actor_policy::RefreshActorPolicyPin,
    configuration_pin: crate::pair_refresh::execution_configuration::ExecutionConfigurationPin,
    configuration: Option<crate::pair_refresh::execution_configuration::LiveExecutionConfiguration>,
}

impl VerifiedRecoveryCandidate {
    pub(crate) fn operation(&self) -> &PairRefreshOperation {
        &self.operation
    }
    pub(crate) fn resources(&self) -> &RefreshResourcePin {
        &self.resources
    }
    pub(crate) fn old_resources(&self) -> &RefreshResourcePin {
        &self.old_resources
    }
    pub(crate) fn require_native_proof(&self) -> Result<(), DatabaseError> {
        if self.old_resources.workspace.native_owner.as_ref() != Some(self.dead_owner.old()) {
            return Err(refused("native proof differs from signed predecessor"));
        }
        self.dead_owner.require_current()
    }
    pub(crate) fn take_configuration(
        &mut self,
    ) -> Result<
        crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
        DatabaseError,
    > {
        self.configuration
            .take()
            .ok_or_else(|| refused("recovery configuration was already consumed"))
    }
    pub(crate) fn original_visibility(
        &self,
    ) -> Result<Vec<crate::pair_refresh::svn_visibility::SvnPresencePin>, DatabaseError> {
        let authority: Authority = serde_json::from_str(&self.authority_raw)
            .map_err(|_| refused("recovery authority encoding"))?;
        serde_json::from_value(authority.evidence.visibility)
            .map_err(|_| refused("unsupported stored visibility profile"))
    }
    pub(crate) fn approval_digest(
        &self,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
    ) -> Result<String, DatabaseError> {
        self.require_native_proof()?;
        if self.actor_pin != *actor.pin()
            || prepared.resource_pin()? != self.resources
            || prepared.pins() != &self.operation.pins
        {
            return Err(refused(
                "recovery preparation differs from verified candidate",
            ));
        }
        let authority: Authority = serde_json::from_str(&self.authority_raw)
            .map_err(|_| refused("recovery authority encoding"))?;
        recovery_approval(
            &self.operation,
            &authority.mac,
            self.epoch,
            &evidence(actor, prepared)?,
        )
    }
}

fn recovery_approval(
    op: &PairRefreshOperation,
    predecessor: &str,
    epoch: u64,
    evidence: &Evidence,
) -> Result<String, DatabaseError> {
    digest(&(
        DOMAIN,
        "recovery_approval_v1",
        op.id,
        &op.approval_digest,
        predecessor,
        epoch,
        evidence,
    ))
}

pub(super) fn effective_owner<'a>(
    op: &PairRefreshOperation,
    authority: &'a Authority,
) -> (&'a Evidence, u64) {
    authority
        .recoveries
        .last()
        .map_or((&authority.evidence, op.admitted_writer_epoch), |step| {
            (&step.evidence, step.writer_epoch)
        })
}

pub(super) fn read_authority_unchecked_document(
    tx: &Connection,
    id: uuid::Uuid,
) -> Result<Authority, DatabaseError> {
    let raw = read(tx, &operation_key(3, "authority", &id.to_string()))?
        .ok_or_else(|| refused("recovery authority is missing"))?;
    serde_json::from_str(&raw).map_err(|_| refused("corrupt recovery authority"))
}

pub(super) fn read_authority(
    tx: &Connection,
    op: &PairRefreshOperation,
) -> Result<Authority, DatabaseError> {
    let raw = read(tx, &operation_key(3, "authority", &op.id.to_string()))?
        .ok_or_else(|| refused("recovery authority is missing"))?;
    verify(tx, op, &raw)?;
    serde_json::from_str(&raw).map_err(|_| refused("corrupt recovery authority"))
}

fn resources(evidence: &Evidence) -> Result<RefreshResourcePin, DatabaseError> {
    serde_json::from_value(evidence.resources.clone())
        .map_err(|_| refused("unsupported stored resource metadata"))
}

pub(super) fn validate_history(
    op: &PairRefreshOperation,
    authority: &Authority,
) -> Result<(), DatabaseError> {
    if authority.recoveries.len() > 16 {
        return Err(refused("unsupported recovery chain length"));
    }
    let mut prior_evidence = &authority.evidence;
    let mut prior_epoch = op.admitted_writer_epoch;
    for step in &authority.recoveries {
        let old = resources(prior_evidence)?;
        let new = resources(&step.evidence)?;
        let old_owner = old
            .workspace
            .native_owner
            .as_ref()
            .ok_or_else(|| refused("transfer has no recorded native predecessor"))?;
        let new_owner = new
            .workspace
            .native_owner
            .as_ref()
            .ok_or_else(|| refused("transfer has no native current owner"))?;
        old_owner.validate()?;
        new_owner.validate()?;
        if step.version != 1
            || step.writer_epoch <= prior_epoch
            || !full_digest(&step.predecessor_mac)
            || chrono::DateTime::parse_from_rfc3339(&step.transferred_at).is_err()
            || step.approved_digest
                != recovery_approval(op, &step.predecessor_mac, step.writer_epoch, &step.evidence)?
            || step.evidence.version != 3
            || step.evidence.database != authority.evidence.database
            || step.evidence.visibility != authority.evidence.visibility
            || new.workspace.operation_id != op.id
            || !old_owner.same_domain(new_owner)
            || old_owner.pid() == new_owner.pid()
            || new.workspace.actor_policy.actor_id != op.initiator_id
            || serde_json::to_value(&new.workspace.actor_policy)
                .map_err(|_| refused("recovery actor encoding"))?
                != step.evidence.actor
        {
            return Err(refused("contradictory signed recovery history"));
        }
        let mut expected = old;
        expected.workspace.actor_policy = new.workspace.actor_policy.clone();
        expected.workspace.native_owner = new.workspace.native_owner.clone();
        if expected != new {
            return Err(refused("recovery changed retained resource identities"));
        }
        prior_evidence = &step.evidence;
        prior_epoch = step.writer_epoch;
    }
    Ok(())
}

fn current_inputs(
    tx: &Connection,
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    op: &PairRefreshOperation,
    busy: &crate::busy::PairBusyGuard,
) -> Result<crate::pair_refresh::execution_configuration::LiveExecutionConfiguration, DatabaseError>
{
    actor.require_current_in_reserved_transaction(engine, tx)?;
    if op.version != 3
        || !op.phase.holds_scopes()
        || op.initiator_id != actor.pin().actor_id
        || !busy.owns_pair(&op.pins.child.repo_id, &op.pins.parent.repo_id)
    {
        return Err(refused(
            "recovery requires the authenticated held job and original actor",
        ));
    }
    let operations = journal(tx)?;
    if operations
        .iter()
        .any(|other| other.id != op.id && other.phase.holds_scopes())
    {
        return Err(refused("another refresh holds recovery exclusion"));
    }
    require_pins_current(tx, &op.pins)?;
    require_credentials_current(tx, &op.pins)?;
    require_execution_conflicts_quiet(tx, &op.pins)?;
    let configuration =
        crate::pair_refresh::execution_configuration::capture_execution_configuration(
            tx,
            &op.pins.child.repo_id,
        )?;
    configuration.require_job_configuration(&op.pins)?;
    super::super::super::ordinary_writer_windows::require_globally_quiet_for_refresh(tx)?;
    super::super::super::pair_refresh_competing_journals::require_globally_quiet(tx)?;
    Ok(configuration)
}

impl Database {
    pub(crate) fn authenticated_terminal_refresh_request(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        request: &str,
        child_scope: &str,
        approved_digest: &str,
    ) -> Result<Option<PairRefreshOperation>, DatabaseError> {
        self.immediate_transaction(|tx| {
            actor.require_current_in_reserved_transaction(engine, tx)?;
            let Some(op) = journal(tx)?.into_iter().find(|op| {
                op.version == 3
                    && op.initiator_id == actor.pin().actor_id
                    && op.request_id == request
                    && op.pins.child.scope_uuid == child_scope
            }) else {
                return Ok(None);
            };
            if op.phase.holds_scopes() {
                return Ok(None);
            }
            let authority = read_authority(tx, &op)?;
            if op.approval_digest != approved_digest
                && !authority
                    .recoveries
                    .iter()
                    .any(|step| step.approved_digest == approved_digest)
            {
                return Err(refused("terminal request belongs to another approval"));
            }
            Ok(Some(op))
        })
    }

    pub(crate) fn claim_authenticated_recovery_writer(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        id: uuid::Uuid,
        busy: &crate::busy::PairBusyGuard,
        lock: &crate::data_dir::LockGuard,
    ) -> Result<crate::writer_fence::WriterFenceGuard, DatabaseError> {
        let data_dir = &engine.config().daemon.data_dir;
        lock.require_current(data_dir)?;
        let directory = data_dir
            .canonicalize()
            .map_err(|_| refused("recovery data directory is unavailable"))?;
        if self.opened_file()?.path.parent() != Some(directory.as_path()) {
            return Err(refused(
                "recovery lock belongs to different database storage",
            ));
        }
        let (epoch, host) = self.immediate_transaction(|tx| {
            let op = lookup_operation(tx, id)?;
            current_inputs(tx, engine, actor, &op, busy)?;
            let authority = read_authority(tx, &op)?;
            let (effective, previous_epoch) = effective_owner(&op, &authority);
            let old_resources = resources(effective)?;
            let proof =
                NativeDeadOwnerProof::capture(old_resources.workspace.native_owner.as_ref())?;
            lock.require_current(data_dir)?;
            crate::writer_fence::claim_authenticated_recovery_tx(tx, previous_epoch, &proof)
        })?;
        Ok(crate::writer_fence::finish_authenticated_recovery_claim(
            data_dir, epoch, host,
        ))
    }

    pub(crate) fn capture_authenticated_recovery(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        id: uuid::Uuid,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<VerifiedRecoveryCandidate, DatabaseError> {
        self.immediate_transaction(|tx| {
            let op = lookup_operation(tx, id)?;
            let configuration = current_inputs(tx, engine, actor, &op, busy)?;
            let epoch = crate::writer_fence::require_owned_current(tx)?;
            let raw = read(tx, &operation_key(3, "authority", &id.to_string()))?
                .ok_or_else(|| refused("recovery authority is missing"))?;
            let authority = read_authority(tx, &op)?;
            if authority.recoveries.len() >= 16 {
                return Err(refused("recovery chain retains its limit"));
            }
            let (effective, old_epoch) = effective_owner(&op, &authority);
            if epoch <= old_epoch {
                return Err(refused("recovery requires a new enrolled writer epoch"));
            }
            let old_resources = resources(effective)?;
            let dead_owner =
                NativeDeadOwnerProof::capture(old_resources.workspace.native_owner.as_ref())?;
            let mut resources = old_resources.clone();
            resources.workspace.actor_policy = actor.pin().clone();
            resources.workspace.native_owner = Some(dead_owner.current().clone());
            Ok(VerifiedRecoveryCandidate {
                operation: op,
                authority_raw: raw,
                resources,
                old_resources,
                dead_owner,
                epoch,
                actor_pin: actor.pin().clone(),
                configuration_pin: configuration.pin().clone(),
                configuration: Some(configuration),
            })
        })
    }

    pub(crate) fn transfer_authenticated_recovery(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        candidate: &VerifiedRecoveryCandidate,
        prepared: &LivePreparedRefresh,
        approved_digest: &str,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.immediate_transaction(|tx| {
            candidate.require_native_proof()?;
            let op = lookup_operation(tx, candidate.operation.id)?;
            let current = current_inputs(tx, engine, actor, &op, busy)?;
            let epoch = crate::writer_fence::require_owned_current(tx)?;
            let raw = read(tx, &operation_key(3, "authority", &op.id.to_string()))?
                .ok_or_else(|| refused("recovery authority is missing"))?;
            if op != candidate.operation
                || raw != candidate.authority_raw
                || epoch != candidate.epoch
                || current.pin() != &candidate.configuration_pin
                || actor.pin() != &candidate.actor_pin
                || prepared.pins() != &op.pins
                || prepared.resource_pin()? != candidate.resources
                || approved_digest != candidate.approval_digest(actor, prepared)?
            {
                return Err(refused("recovery owner, approval or inputs changed"));
            }
            prepared.workspace()?.require_binding(self, actor.pin())?;
            let mut authority = read_authority(tx, &op)?;
            let effective = evidence(actor, prepared)?;
            authority.recoveries.push(RecoveryStep {
                version: 1,
                predecessor_mac: authority.mac.clone(),
                writer_epoch: epoch,
                evidence: effective,
                approved_digest: approved_digest.into(),
                transferred_at: Utc::now().to_rfc3339(),
            });
            validate_history(&op, &authority)?;
            let secret =
                private_key(tx)?.ok_or_else(|| refused("recovery journal key is missing"))?;
            authority.mac = hex::encode(
                mac(&secret, &authority.evidence, &op, &authority.recoveries)?
                    .finalize()
                    .into_bytes(),
            );
            candidate.require_native_proof()?;
            write(
                tx,
                &operation_key(3, "authority", &op.id.to_string()),
                &serde_json::to_string(&authority)
                    .map_err(|_| refused("recovery authority encoding"))?,
            )?;
            journal(tx)?;
            Ok(op)
        })
    }
}
