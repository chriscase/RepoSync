//! Version 3 enrollment requires opaque live actor and owned preparation.
//! A signed job is a durable hold, never permission to publish or adopt files.

use hmac::{Hmac, Mac};
use rand::RngCore;
use serde_json::Value;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use super::*;
use crate::pair_refresh::actor_policy::LiveRefreshActorPolicy;
use crate::pair_refresh::live_preparation::LivePreparedRefresh;
use crate::sync_engine::SyncEngine;

const KEY_TABLE: &str = "pair_refresh_journal_key_v3";
const KEY_SCHEMA: &str = "CREATE TABLE pair_refresh_journal_key_v3 (id INTEGER PRIMARY KEY CHECK(id=1), secret BLOB NOT NULL CHECK(length(secret)=32))";
const DOMAIN: &str = "reposync_pair_refresh_authority_v3";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatabaseBinding {
    path: PathBuf,
    device: u64,
    inode: u64,
    owner: u32,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    version: u8,
    database: DatabaseBinding,
    actor: Value,
    resources: Value,
    visibility: Value,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Authority {
    evidence: Evidence,
    mac: String,
}

fn evidence(
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
) -> Result<Evidence, DatabaseError> {
    let resources = prepared.resource_pin()?;
    let visibility = prepared
        .visibility_pins()
        .filter(|pins| pins.len() == 5)
        .ok_or_else(|| refused("complete owned SVN visibility is required"))?;
    let roles = visibility
        .iter()
        .map(|pin| pin.role.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if roles
        != [
            "baseline",
            "parent",
            "child",
            "current_parent",
            "current_child",
        ]
        .into()
        || resources.workspace.operation_id != prepared.pins().git.operation_id
        || resources.workspace.actor_policy != *actor.pin()
    {
        return Err(refused(
            "owned preparation differs from actual actor/resources",
        ));
    }
    let database = &resources.workspace.database;
    Ok(Evidence {
        version: 3,
        database: DatabaseBinding {
            path: database.path.clone(),
            device: database.device,
            inode: database.inode,
            owner: database.owner,
        },
        actor: serde_json::to_value(actor.pin()).map_err(|_| refused("actor encoding"))?,
        resources: serde_json::to_value(&resources).map_err(|_| refused("resource encoding"))?,
        visibility: serde_json::to_value(visibility).map_err(|_| refused("visibility encoding"))?,
    })
}

fn approval(pins: &RefreshJobPins, evidence: &Evidence) -> Result<String, DatabaseError> {
    digest(&(DOMAIN, "approval", pins, evidence))
}

pub(crate) fn approval_digest(
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
) -> Result<String, DatabaseError> {
    approval(prepared.pins(), &evidence(actor, prepared)?)
}

fn private_key(tx: &Connection) -> Result<Option<[u8; 32]>, DatabaseError> {
    let schema: Option<String> = tx
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name=?1 AND type='table'",
            [KEY_TABLE],
            |row| row.get(0),
        )
        .optional()?;
    let Some(schema) = schema else {
        let other: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
            [KEY_TABLE],
            |row| row.get(0),
        )?;
        return if other {
            Err(refused("unknown journal key storage"))
        } else {
            Ok(None)
        };
    };
    let triggers: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND tbl_name=?1)",
        [KEY_TABLE],
        |row| row.get(0),
    )?;
    if schema != KEY_SCHEMA || triggers {
        return Err(refused("unknown journal key schema"));
    }
    let mut query = tx.prepare("SELECT id,secret FROM pair_refresh_journal_key_v3")?;
    let keys = query
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if keys.len() != 1 || keys[0].0 != 1 {
        return Err(refused("journal key is missing or ambiguous"));
    }
    Ok(Some(
        keys[0]
            .1
            .clone()
            .try_into()
            .map_err(|_| refused("journal key is corrupt"))?,
    ))
}

pub(super) fn require_key_matches_journal(
    tx: &Connection,
    authenticated: bool,
) -> Result<(), DatabaseError> {
    if private_key(tx)?.is_some() != authenticated {
        return Err(refused("journal key and authenticated history differ"));
    }
    Ok(())
}

fn establish_key(tx: &Connection) -> Result<[u8; 32], DatabaseError> {
    if let Some(key) = private_key(tx)? {
        return Ok(key);
    }
    let mut secret = [0_u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut secret)
        .map_err(|_| refused("journal key entropy unavailable"))?;
    tx.execute_batch(KEY_SCHEMA)?;
    tx.execute(
        "INSERT INTO pair_refresh_journal_key_v3(id,secret) VALUES(1,?1)",
        [secret.as_slice()],
    )?;
    Ok(secret)
}

fn mac(
    key: &[u8; 32],
    evidence: &Evidence,
    op: &PairRefreshOperation,
) -> Result<Hmac<Sha256>, DatabaseError> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).map_err(|_| refused("invalid journal key"))?;
    mac.update(
        &serde_json::to_vec(&(DOMAIN, "operation", evidence, op))
            .map_err(|_| refused("authority encoding"))?,
    );
    Ok(mac)
}

pub(super) fn verify(
    tx: &Connection,
    op: &PairRefreshOperation,
    raw: &str,
) -> Result<(), DatabaseError> {
    let authority: Authority =
        serde_json::from_str(raw).map_err(|_| refused("corrupt authenticated authority"))?;
    if authority.evidence.version != 3
        || serde_json::to_string(&authority).map_err(|_| refused("authority encoding"))? != raw
        || op.approval_digest != approval(&op.pins, &authority.evidence)?
    {
        return Err(refused(
            "noncanonical or mismatched authenticated authority",
        ));
    }
    let key = private_key(tx)?.ok_or_else(|| refused("historical journal key is missing"))?;
    let signature = hex::decode(&authority.mac).map_err(|_| refused("invalid authority MAC"))?;
    if !full_digest(&authority.mac)
        || mac(&key, &authority.evidence, op)?
            .verify_slice(&signature)
            .is_err()
    {
        return Err(refused("authenticated operation MAC differs"));
    }
    let expected = &authority.evidence.database;
    let actual = super::super::opened_file::OpenedDatabaseFile::attest(tx, &expected.path, None)
        .ok_or_else(|| refused("authenticated database identity is unknown"))?;
    let metadata = std::fs::symlink_metadata(&expected.path)
        .map_err(|_| refused("authenticated database metadata is unknown"))?;
    if actual.path != expected.path
        || actual.device != expected.device
        || actual.inode != expected.inode
        || metadata.uid() != expected.owner
        || metadata.dev() != actual.device
        || metadata.ino() != actual.inode
    {
        return Err(refused(
            "authenticated journal belongs to different storage",
        ));
    }
    Ok(())
}

impl Database {
    /// Original-owner observation only. This entrance never returns a first
    /// intent capability and cannot authorize an external publication retry.
    pub(crate) fn require_authenticated_reconciliation_inputs(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
        busy: &crate::busy::PairBusyGuard,
        phase: PairRefreshPhase,
    ) -> Result<
        crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
        DatabaseError,
    > {
        if !matches!(
            phase,
            PairRefreshPhase::SvnIntent | PairRefreshPhase::GitIntent
        ) {
            return Err(refused(
                "reconciliation requires an existing publication intent",
            ));
        }
        self.immediate_transaction(|tx| {
            let (_, _, configuration) =
                execution_inputs(tx, self, engine, actor, prepared, busy, phase)?;
            Ok(configuration)
        })
    }

    pub(crate) fn require_authenticated_git_inputs(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
        busy: &crate::busy::PairBusyGuard,
        phase: PairRefreshPhase,
    ) -> Result<
        crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
        DatabaseError,
    > {
        if !matches!(
            phase,
            PairRefreshPhase::SvnObserved | PairRefreshPhase::GitObserved
        ) {
            return Err(refused("unsupported authenticated Git entrance"));
        }
        self.immediate_transaction(|tx| {
            let (_, _, configuration) =
                execution_inputs(tx, self, engine, actor, prepared, busy, phase)?;
            Ok(configuration)
        })
    }

    pub(crate) fn record_authenticated_git_intent(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
        busy: &crate::busy::PairBusyGuard,
        proof: crate::pair_refresh::execution::GitIntentProof<'_>,
    ) -> Result<
        crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
        DatabaseError,
    > {
        self.immediate_transaction(|tx| {
            let (mut op, evidence, configuration) = execution_inputs(
                tx,
                self,
                engine,
                actor,
                prepared,
                busy,
                PairRefreshPhase::SvnObserved,
            )?;
            proof.require_binding(engine, prepared, &configuration)?;
            if op.svn_receipt.as_ref() != Some(proof.svn_receipt()) {
                return Err(refused("fresh SVN receipt differs before Git intent"));
            }
            op.phase = PairRefreshPhase::GitIntent;
            store_progress(tx, &mut op, evidence)?;
            Ok(configuration)
        })
    }

    pub(crate) fn record_authenticated_git_receipt(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
        busy: &crate::busy::PairBusyGuard,
        proof: &crate::pair_refresh::execution::GitReceiptProof<'_>,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.immediate_transaction(|tx| {
            let (mut op, evidence, configuration) = execution_inputs(
                tx,
                self,
                engine,
                actor,
                prepared,
                busy,
                PairRefreshPhase::GitIntent,
            )?;
            proof.require_binding(engine, prepared, &configuration)?;
            if op.svn_receipt.as_ref() != Some(proof.svn_receipt()) {
                return Err(refused("SVN receipt differs at Git confirmation"));
            }
            op.git_receipt_sha = Some(proof.sha().into());
            op.phase = PairRefreshPhase::GitObserved;
            op.reconciliation_detail = None;
            store_progress(tx, &mut op, evidence)?;
            Ok(op)
        })
    }

    pub(crate) fn finalize_authenticated_refresh(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
        busy: &crate::busy::PairBusyGuard,
        proof: crate::pair_refresh::execution::GitReceiptProof<'_>,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.immediate_transaction(|tx| {
            let (mut op, evidence, configuration) = execution_inputs(tx, self, engine, actor, prepared, busy, PairRefreshPhase::GitObserved)?;
            proof.require_binding(engine, prepared, &configuration)?;
            if op.svn_receipt.as_ref() != Some(proof.svn_receipt()) || op.git_receipt_sha.as_deref() != Some(proof.sha()) {
                return Err(refused("complete publication proof differs before finalization"));
            }
            let receipt = proof.svn_receipt();
            let child = &op.pins.child;
            let sha = proof.sha();
            let conflict: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_records WHERE (repo_id=?1 OR repo_id IS NULL OR repo_id='') AND (git_sha=?2 OR svn_rev=?3))", params![child.repo_id, sha, receipt.revision], |row| row.get(0))?;
            let conflicting_map: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM commit_map WHERE (git_sha=?2 OR svn_rev=?3) AND ((repo_id=?1 AND NOT (git_sha=?2 AND svn_rev=?3 AND direction='git_to_svn')) OR repo_id IS NULL OR repo_id=''))", params![child.repo_id, sha, receipt.revision], |row| row.get(0))?;
            if conflict || conflicting_map { return Err(refused("publication has conflicting correspondence")); }
            let now = Utc::now().to_rfc3339();
            tx.execute("INSERT INTO commit_map(repo_id,svn_rev,git_sha,direction,synced_at,svn_author,git_author) SELECT ?1,?2,?3,'git_to_svn',?4,'RepoSync','RepoSync' WHERE NOT EXISTS(SELECT 1 FROM commit_map WHERE repo_id=?1 AND svn_rev=?2 AND git_sha=?3 AND direction='git_to_svn')", params![child.repo_id, receipt.revision, sha, now])?;
            tx.execute("INSERT INTO sync_records(id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) VALUES(?1,?2,?3,?4,'git_to_svn','RepoSync','Update pair from parent',?5,?5,'applied')", params![op.id.to_string(), child.repo_id, receipt.revision, sha, now])?;
            if tx.execute("UPDATE repositories SET last_git_sha=?1,last_svn_rev=?2,last_sync_at=?3,total_syncs=total_syncs+1,updated_at=?3 WHERE id=?4 AND scope_uuid=?5", params![sha, receipt.revision, now, child.repo_id, child.scope_uuid])? != 1 { return Err(refused("child registration disappeared during finalization")); }
            crate::echo_receipt_scope::write_scoped_last_git_sha_kv(tx, &child.repo_id, sha, &now)?;
            write(tx, &format!("last_svn_rev_{}", child.repo_id), &receipt.revision.to_string())?;
            for scope in [&child.scope_uuid, &op.pins.parent.scope_uuid] {
                if tx.execute("DELETE FROM kv_state WHERE key=?1 AND value=?2", params![operation_key(3,"hold",scope),op.id.to_string()])? != 1 { return Err(refused("lost authenticated completion hold")); }
            }
            op.phase = PairRefreshPhase::Completed;
            store_progress(tx, &mut op, evidence)?;
            Ok(op)
        })
    }

    pub(crate) fn require_authenticated_svn_preparation(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<
        crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
        DatabaseError,
    > {
        self.immediate_transaction(|tx| {
            let (_, _, configuration) = execution_inputs(
                tx,
                self,
                engine,
                actor,
                prepared,
                busy,
                PairRefreshPhase::Prepared,
            )?;
            Ok(configuration)
        })
    }

    pub(crate) fn record_authenticated_svn_intent(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
        busy: &crate::busy::PairBusyGuard,
        proof: crate::pair_refresh::execution::SvnIntentProof<'_>,
    ) -> Result<
        crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
        DatabaseError,
    > {
        self.immediate_transaction(|tx| {
            let (mut op, evidence, configuration) = execution_inputs(
                tx,
                self,
                engine,
                actor,
                prepared,
                busy,
                PairRefreshPhase::Prepared,
            )?;
            proof.require_binding(engine, prepared, &configuration)?;
            op.phase = PairRefreshPhase::SvnIntent;
            store_progress(tx, &mut op, evidence)?;
            Ok(configuration)
        })
    }

    pub(crate) fn record_authenticated_svn_receipt(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
        busy: &crate::busy::PairBusyGuard,
        proof: crate::pair_refresh::execution::SvnReceiptProof<'_>,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.immediate_transaction(|tx| {
            let (mut op, evidence, configuration) = execution_inputs(
                tx,
                self,
                engine,
                actor,
                prepared,
                busy,
                PairRefreshPhase::SvnIntent,
            )?;
            proof.require_binding(engine, prepared, &configuration)?;
            op.svn_receipt = Some(proof.receipt().clone());
            op.phase = PairRefreshPhase::SvnObserved;
            op.reconciliation_detail = None;
            store_progress(tx, &mut op, evidence)?;
            Ok(op)
        })
    }

    pub(crate) fn retain_authenticated_svn_uncertainty(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<(), DatabaseError> {
        self.immediate_transaction(|tx| {
            let (mut op, evidence, _) = execution_inputs(
                tx,
                self,
                engine,
                actor,
                prepared,
                busy,
                PairRefreshPhase::SvnIntent,
            )?;
            op.reconciliation_detail =
                Some("SVN publication requires independent reconciliation".into());
            store_progress(tx, &mut op, evidence)
        })
    }

    pub(crate) fn cancel_authenticated_preparation(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        let evidence = evidence(actor, prepared)?;
        self.immediate_transaction(|tx| {
            actor.require_current_in_reserved_transaction(engine, tx)?;
            let epoch = crate::writer_fence::require_owned_current(tx)?;
            prepared.workspace()?.require_binding(self, actor.pin())?;
            let mut op = lookup_operation(tx, prepared.pins().git.operation_id)?;
            if op.version != 3
                || op.admitted_writer_epoch != epoch
                || op.phase != PairRefreshPhase::Prepared
                || op.reconciliation_detail.is_some()
                || op.pins != *prepared.pins()
                || op.approval_digest != approval(&op.pins, &evidence)?
                || !busy.owns_pair(&op.pins.child.repo_id, &op.pins.parent.repo_id)
            {
                return Err(refused(
                    "cancellation requires the exact owned pre-intent job",
                ));
            }
            let secret = private_key(tx)?.ok_or_else(|| refused("journal key is missing"))?;
            op.phase = PairRefreshPhase::CancelledBeforePublication;
            op.updated_at = Utc::now().to_rfc3339();
            validate(&op)?;
            let authority = Authority {
                mac: hex::encode(mac(&secret, &evidence, &op)?.finalize().into_bytes()),
                evidence,
            };
            write(
                tx,
                &operation_key(3, "document", &op.id.to_string()),
                &serde_json::to_string(&op).map_err(|_| refused("job encoding"))?,
            )?;
            write(
                tx,
                &operation_key(3, "authority", &op.id.to_string()),
                &serde_json::to_string(&authority).map_err(|_| refused("authority encoding"))?,
            )?;
            for scope in [&op.pins.child.scope_uuid, &op.pins.parent.scope_uuid] {
                if tx.execute(
                    "DELETE FROM kv_state WHERE key=?1 AND value=?2",
                    params![operation_key(3, "hold", scope), op.id.to_string()],
                )? != 1
                {
                    return Err(refused("lost authenticated hold during cancellation"));
                }
            }
            journal(tx)?;
            Ok(op)
        })
    }

    pub(crate) fn admit_authenticated_pair_refresh(
        &self,
        engine: &SyncEngine,
        actor: &LiveRefreshActorPolicy<'_>,
        prepared: &LivePreparedRefresh,
        request: &str,
        approved_digest: &str,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        let evidence = evidence(actor, prepared)?;
        self.immediate_transaction(|tx| {
            actor.require_current_in_reserved_transaction(engine, tx)?;
            let epoch = crate::writer_fence::require_owned_current(tx)?;
            let pins = prepared.pins();
            pins.validate()?;
            if request.is_empty()
                || request.len() > 256
                || approval(pins, &evidence)? != approved_digest
                || !busy.owns_pair(&pins.child.repo_id, &pins.parent.repo_id)
            {
                return Err(refused(
                    "authenticated approval, request or busy ownership differs",
                ));
            }
            let operations = journal(tx)?;
            if let Some(prior) = operations.iter().find(|op| {
                op.initiator_id == actor.pin().actor_id
                    && op.request_id == request
                    && op.pins.child.scope_uuid == pins.child.scope_uuid
            }) {
                if prior.version != 3
                    || prior.pins != *pins
                    || prior.approval_digest != approved_digest
                {
                    return Err(refused(
                        "request already belongs to another authenticated intent",
                    ));
                }
                return Ok(prior.clone());
            }
            if operations.iter().any(|op| op.id == pins.git.operation_id) {
                return Err(refused("authenticated operation identity was reused"));
            }
            require_no_pair_refresh_hold(tx, None)?;
            require_baseline_provenance(pins, &operations)?;
            require_pins_current(tx, pins)?;
            require_credentials_current(tx, pins)?;
            require_execution_conflicts_quiet(tx, pins)?;
            let configuration =
                crate::pair_refresh::execution_configuration::capture_execution_configuration(
                    tx,
                    &pins.child.repo_id,
                )?;
            configuration.require_job_configuration(pins)?;
            if configuration.pin() != prepared.configuration().pin() {
                return Err(refused("prepared registration context changed"));
            }
            super::super::ordinary_writer_windows::require_globally_quiet_for_refresh(tx)?;
            super::super::pair_refresh_competing_journals::require_globally_quiet(tx)?;
            prepared.workspace()?.require_binding(self, actor.pin())?;
            if evidence != self::evidence(actor, prepared)? {
                return Err(refused("owned resources changed before enrollment"));
            }
            // All refusal checks precede key establishment. Key, complete job,
            // signed authority, request and both holds share this transaction.
            let secret = establish_key(tx)?;
            let now = Utc::now().to_rfc3339();
            let op = PairRefreshOperation {
                version: 3,
                id: pins.git.operation_id,
                initiator_id: actor.pin().actor_id.clone(),
                request_id: request.into(),
                approval_digest: approved_digest.into(),
                admitted_writer_epoch: epoch,
                created_at: now.clone(),
                updated_at: now,
                pins: pins.clone(),
                phase: PairRefreshPhase::Prepared,
                svn_receipt: None,
                git_receipt_sha: None,
                reconciliation_detail: None,
            };
            validate(&op)?;
            let authority = Authority {
                mac: hex::encode(mac(&secret, &evidence, &op)?.finalize().into_bytes()),
                evidence,
            };
            write(
                tx,
                &operation_key(3, "document", &op.id.to_string()),
                &serde_json::to_string(&op).map_err(|_| refused("job encoding"))?,
            )?;
            write(
                tx,
                &operation_key(3, "authority", &op.id.to_string()),
                &serde_json::to_string(&authority).map_err(|_| refused("authority encoding"))?,
            )?;
            write(tx, &operation_request_key(&op)?, &op.id.to_string())?;
            for scope in [&pins.child.scope_uuid, &pins.parent.scope_uuid] {
                write(tx, &operation_key(3, "hold", scope), &op.id.to_string())?;
            }
            journal(tx)?;
            Ok(op)
        })
    }
}

fn execution_inputs(
    tx: &Connection,
    db: &Database,
    engine: &SyncEngine,
    actor: &LiveRefreshActorPolicy<'_>,
    prepared: &LivePreparedRefresh,
    busy: &crate::busy::PairBusyGuard,
    expected: PairRefreshPhase,
) -> Result<
    (
        PairRefreshOperation,
        Evidence,
        crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
    ),
    DatabaseError,
> {
    actor.require_current_in_reserved_transaction(engine, tx)?;
    let epoch = crate::writer_fence::require_owned_current(tx)?;
    prepared.workspace()?.require_binding(db, actor.pin())?;
    let evidence = evidence(actor, prepared)?;
    let operations = journal(tx)?;
    let op = operations
        .iter()
        .find(|op| op.id == prepared.pins().git.operation_id)
        .ok_or_else(|| refused("authenticated execution job is missing"))?
        .clone();
    if op.version != 3
        || op.phase != expected
        || (op.reconciliation_detail.is_some()
            && !matches!(
                expected,
                PairRefreshPhase::SvnIntent | PairRefreshPhase::GitIntent
            ))
        || op.admitted_writer_epoch != epoch
        || op.pins != *prepared.pins()
        || op.approval_digest != approval(&op.pins, &evidence)?
        || !busy.owns_pair(&op.pins.child.repo_id, &op.pins.parent.repo_id)
        || operations
            .iter()
            .any(|other| other.id != op.id && other.phase.holds_scopes())
    {
        return Err(refused(
            "authenticated phase, owner, inputs or global exclusion changed",
        ));
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
    if configuration.pin() != prepared.configuration().pin() {
        return Err(refused("authenticated prepared configuration changed"));
    }
    super::super::ordinary_writer_windows::require_globally_quiet_for_refresh(tx)?;
    super::super::pair_refresh_competing_journals::require_globally_quiet(tx)?;
    Ok((op, evidence, configuration))
}

fn store_progress(
    tx: &Connection,
    op: &mut PairRefreshOperation,
    evidence: Evidence,
) -> Result<(), DatabaseError> {
    let secret = private_key(tx)?.ok_or_else(|| refused("journal key is missing"))?;
    op.updated_at = Utc::now().to_rfc3339();
    validate(op)?;
    let authority = Authority {
        mac: hex::encode(mac(&secret, &evidence, op)?.finalize().into_bytes()),
        evidence,
    };
    write(
        tx,
        &operation_key(3, "document", &op.id.to_string()),
        &serde_json::to_string(op).map_err(|_| refused("job encoding"))?,
    )?;
    write(
        tx,
        &operation_key(3, "authority", &op.id.to_string()),
        &serde_json::to_string(&authority).map_err(|_| refused("authority encoding"))?,
    )?;
    journal(tx)?;
    Ok(())
}
