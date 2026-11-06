//! Terminal no-op receipts use a separate namespace and MAC domain. They cannot
//! be replayed as publication jobs or contribute a new integration baseline.

use super::*;
use crate::pair_refresh::noop::{
    LivePreparedNoopRefresh, NoopObservationProof, NoopRefreshReceipt,
};

const NOOP_FAMILY: &str = "pair_refresh_noop_";
const NOOP_PREFIX: &str = "pair_refresh_noop_v1:";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoopRecord {
    receipt: NoopRefreshReceipt,
    evidence: Evidence,
    mac: String,
}

fn request_key(actor: &str, request: &str, scope: &str) -> Result<String, DatabaseError> {
    Ok(format!(
        "{NOOP_PREFIX}request:{}",
        digest(&(actor, request, scope))?
    ))
}

fn evidence(
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedNoopRefresh,
) -> Result<Evidence, DatabaseError> {
    evidence_parts(
        actor,
        prepared.workspace().resource_pin()?,
        prepared.visibility(),
        prepared.pins().operation_id,
    )
}

pub(crate) fn approval_digest(
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedNoopRefresh,
) -> Result<String, DatabaseError> {
    digest(&(
        DOMAIN,
        "no_op_approval",
        prepared.pins(),
        evidence(actor, prepared)?,
    ))
}

fn receipt_mac(
    key: &[u8; 32],
    evidence: &Evidence,
    receipt: &NoopRefreshReceipt,
) -> Result<Hmac<Sha256>, DatabaseError> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).map_err(|_| refused("invalid no-op journal key"))?;
    mac.update(
        &serde_json::to_vec(&(DOMAIN, "no_op_receipt", evidence, receipt))
            .map_err(|_| refused("no-op authority encoding"))?,
    );
    Ok(mac)
}

fn validate(receipt: &NoopRefreshReceipt) -> Result<(), DatabaseError> {
    let pins = &receipt.pins;
    let lineage = &pins.lineage.pins;
    crate::pair_refresh::lineage::validate_stored_refresh_proof(&pins.lineage)
        .map_err(|_| refused("invalid no-op lineage"))?;
    if receipt.version != 1
        || receipt.id.is_nil()
        || receipt.id != pins.operation_id
        || receipt.initiator_id.is_empty()
        || receipt.request_id.is_empty()
        || receipt.request_id.len() > 256
        || receipt.admitted_writer_epoch == 0
        || !full_digest(&receipt.approval_digest)
        || chrono::DateTime::parse_from_rfc3339(&receipt.completed_at).is_err()
        || !canonical_uuid(&pins.child.scope_uuid)
        || !canonical_uuid(&pins.parent.scope_uuid)
        || pins.child.scope_uuid == pins.parent.scope_uuid
        || pins.child.repo_id == pins.parent.repo_id
        || pins.parent.last_git_sha != pins.parent_baseline_sha
        || !crate::history_inspect::is_full_git_oid(&pins.git_tree)
        || !crate::history_inspect::is_full_git_oid(&pins.child.last_git_sha)
        || !crate::history_inspect::is_full_git_oid(&pins.parent.last_git_sha)
        || pins.child.last_svn_rev != lineage.child.child_revision
        || pins.parent.last_svn_rev != lineage.parent_revision
        || lineage.parent_revision != lineage.child.parent_baseline_revision
        || pins.inspected_head_revision < pins.child.last_svn_rev
        || pins.inspected_head_revision < pins.parent.last_svn_rev
        || (pins.baseline_operation_id.is_none()
            && (pins.previous_mergeinfo.is_some()
                || pins.child.last_git_sha != pins.parent.last_git_sha
                || pins.child.last_svn_rev != pins.lineage.child_copy.created_revision
                || pins.parent.last_svn_rev != pins.lineage.child_copy.copy_from_revision))
        || (pins.baseline_operation_id.is_some() && pins.previous_mergeinfo.is_none())
    {
        return Err(refused("invalid no-op receipt"));
    }
    Ok(())
}

pub(super) fn verify_history(
    tx: &Connection,
    operations: &[PairRefreshOperation],
) -> Result<Vec<NoopRefreshReceipt>, DatabaseError> {
    let mut statement =
        tx.prepare("SELECT key,value FROM kv_state WHERE substr(key,1,?1)=?2 ORDER BY key")?;
    let rows = statement
        .query_map(params![NOOP_FAMILY.len() as i64, NOOP_FAMILY], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let secret = private_key(tx)?.ok_or_else(|| refused("no-op journal key is missing"))?;
    let mut expected = std::collections::BTreeMap::new();
    let mut ids = operations
        .iter()
        .map(|op| op.id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut requests = operations
        .iter()
        .map(|op| {
            (
                op.initiator_id.clone(),
                op.request_id.clone(),
                op.pins.child.scope_uuid.clone(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut receipts = Vec::new();
    for (name, raw) in &rows {
        let Some(id) = name.strip_prefix(&format!("{NOOP_PREFIX}document:")) else {
            continue;
        };
        let record: NoopRecord =
            serde_json::from_str(raw).map_err(|_| refused("corrupt no-op authority"))?;
        let receipt = &record.receipt;
        validate(receipt)?;
        if receipt.id.to_string() != id
            || !ids.insert(receipt.id)
            || !requests.insert((
                receipt.initiator_id.clone(),
                receipt.request_id.clone(),
                receipt.pins.child.scope_uuid.clone(),
            ))
            || serde_json::to_string(&record).map_err(|_| refused("no-op encoding"))? != *raw
            || receipt.approval_digest
                != digest(&(DOMAIN, "no_op_approval", &receipt.pins, &record.evidence))?
            || record.evidence.version != 3
            || !full_digest(&record.mac)
        {
            return Err(refused(
                "noncanonical, duplicate or contradictory no-op authority",
            ));
        }
        let signature = hex::decode(&record.mac).map_err(|_| refused("invalid no-op MAC"))?;
        receipt_mac(&secret, &record.evidence, receipt)?
            .verify_slice(&signature)
            .map_err(|_| refused("no-op authority MAC differs"))?;
        require_database_binding(tx, &record.evidence.database)?;
        expected.insert(name.clone(), raw.clone());
        expected.insert(
            request_key(
                &receipt.initiator_id,
                &receipt.request_id,
                &receipt.pins.child.scope_uuid,
            )?,
            receipt.id.to_string(),
        );
        receipts.push(receipt.clone());
    }
    if rows != expected {
        return Err(refused("unknown no-op version or missing/orphan request"));
    }
    Ok(receipts)
}

fn require_baseline(
    current: &crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
    prepared: &LivePreparedNoopRefresh,
    operations: &[PairRefreshOperation],
) -> Result<(), DatabaseError> {
    let child = &current.pin().child;
    let prior = operations
        .iter()
        .filter(|op| {
            op.phase == PairRefreshPhase::Completed && op.pins.child.scope_uuid == child.scope_uuid
        })
        .collect::<Vec<_>>();
    if let Some(expected) = prepared.baseline() {
        let matching = prior
            .into_iter()
            .filter(|op| {
                op.pins.git.prepared_sha == child.last_git_sha
                    || op
                        .svn_receipt
                        .as_ref()
                        .is_some_and(|receipt| receipt.revision == child.last_svn_rev)
            })
            .collect::<Vec<_>>();
        if matching.len() != 1 || matching[0] != expected {
            return Err(refused("no-op completed baseline changed"));
        }
    } else if !prior.is_empty() {
        return Err(refused(
            "no-op cannot downgrade completed history to first-copy evidence",
        ));
    }
    Ok(())
}

impl Database {
    pub(crate) fn authenticated_noop_request(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        request: &str,
        child_scope: &str,
    ) -> Result<Option<NoopRefreshReceipt>, DatabaseError> {
        self.immediate_transaction(|tx| {
            actor.require_current_in_reserved_transaction(engine, tx)?;
            let operations = journal(tx)?;
            Ok(verify_history(tx, &operations)?
                .into_iter()
                .find(|receipt| {
                    receipt.initiator_id == actor.pin().actor_id
                        && receipt.request_id == request
                        && receipt.pins.child.scope_uuid == child_scope
                }))
        })
    }

    pub(crate) fn complete_authenticated_noop(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedNoopRefresh,
        request_approval: (&str, &str),
        busy: &crate::busy::PairBusyGuard,
        proof: NoopObservationProof<'_>,
    ) -> Result<NoopRefreshReceipt, DatabaseError> {
        let (request, approved_digest) = request_approval;
        let evidence = evidence(actor, prepared)?;
        self.immediate_transaction(|tx| {
            actor.require_current_in_reserved_transaction(engine,tx)?;
            let epoch = crate::writer_fence::require_owned_current(tx)?;
            prepared.workspace().require_binding(self,actor.pin())?;
            if request.is_empty() || request.len() > 256 || approved_digest != approval_digest(actor,prepared)? || !busy.owns_pair(&prepared.pins().child.repo_id,&prepared.pins().parent.repo_id) { return Err(refused("no-op approval, request or busy ownership differs")); }
            let operations = journal(tx)?;
            if operations.iter().any(|op| op.phase.holds_scopes()) { return Err(refused("no-op cannot enter a held refresh")); }
            let current = crate::pair_refresh::execution_configuration::capture_execution_configuration(tx,&prepared.pins().child.repo_id)?;
            proof.require_binding(engine,prepared,&current)?;
            if current.pin() != prepared.configuration().pin() || serde_json::to_value(current.pin()).map_err(|_| refused("no-op configuration encoding"))? != prepared.pins().configuration { return Err(refused("no-op registered inputs changed")); }
            require_baseline(&current,prepared,&operations)?;
            let blocking: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM conflicts WHERE status NOT IN ('resolved','dismissed') AND (repo_id IS NULL OR repo_id='' OR repo_id=?1 OR repo_id=?2 OR repo_id=?3 OR repo_id=?4))",params![prepared.pins().child.repo_id,prepared.pins().parent.repo_id,super::super::super::personal_scope::PERSONAL_SCOPE_KEY,super::super::super::personal_scope::LEGACY_PERSONAL_REPO_ID],|row|row.get(0))?;
            if blocking { return Err(refused("unresolved conflict prevents no-op completion")); }
            super::super::super::ordinary_writer_windows::require_globally_quiet_for_refresh(tx)?;
            super::super::super::pair_refresh_competing_journals::require_globally_quiet(tx)?;
            for prior in verify_history(tx,&operations)? {
                if prior.initiator_id == actor.pin().actor_id && prior.request_id == request && prior.pins.child.scope_uuid == prepared.pins().child.scope_uuid {
                    if prior.pins != *prepared.pins() || prior.approval_digest != approved_digest { return Err(refused("no-op request belongs to different approved inputs")); }
                    return Ok(prior);
                }
            }
            if operations.iter().any(|op| op.id == prepared.pins().operation_id || (op.initiator_id == actor.pin().actor_id && op.request_id == request && op.pins.child.scope_uuid == prepared.pins().child.scope_uuid)) { return Err(refused("no-op request belongs to a publication job")); }
            let receipt = NoopRefreshReceipt { version:1,id:prepared.pins().operation_id,initiator_id:actor.pin().actor_id.clone(),request_id:request.into(),approval_digest:approved_digest.into(),admitted_writer_epoch:epoch,completed_at:Utc::now().to_rfc3339(),pins:prepared.pins().clone() };
            validate(&receipt)?;
            let secret = establish_key(tx)?;
            let record = NoopRecord { mac:hex::encode(receipt_mac(&secret,&evidence,&receipt)?.finalize().into_bytes()),receipt:receipt.clone(),evidence };
            write(tx,&format!("{NOOP_PREFIX}document:{}",receipt.id),&serde_json::to_string(&record).map_err(|_|refused("no-op encoding"))?)?;
            write(tx,&request_key(&receipt.initiator_id,request,&receipt.pins.child.scope_uuid)?,&receipt.id.to_string())?;
            journal(tx)?;
            Ok(receipt)
        })
    }
}
