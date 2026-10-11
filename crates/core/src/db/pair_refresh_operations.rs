//! Durable two-scope journal for the bounded refresh execution caller.
//!
//! This storage component does not authorize an API request or prove a remote
//! result. The execution caller must authenticate approval, prove live pins and
//! credentials, and use the independent publication observers. Admission must
//! also own the sorted process busy slots. Until every writer honors the guards
//! here, the execution API remains unavailable.
//!
//! Intent survives a lost remote response. An uncertain outcome retains both
//! holds; neither a dropped busy guard nor an absence observation releases them.

use std::collections::BTreeMap;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Database;
use crate::errors::DatabaseError;
use crate::history_inspect::is_full_git_oid;
use crate::pair_refresh::preparation::PreparedCleanChildGit;
use crate::pair_refresh::publication::PinnedPushPolicy;
use crate::pair_refresh::svn_observation::SvnRefreshPublicationReceipt;
use crate::pair_refresh::svn_preparation::PreparedCleanChildSvn;

const PREFIX: &str = "pair_refresh_execution_v2:";
const FAMILY: &str = "pair_refresh_execution_";

fn refused(detail: &str) -> DatabaseError {
    DatabaseError::Other(format!("pair refresh journal: {detail}"))
}

fn key(kind: &str, id: &str) -> String {
    format!("{PREFIX}{kind}:{id}")
}

fn read(tx: &Connection, key: &str) -> Result<Option<String>, DatabaseError> {
    Ok(tx
        .query_row("SELECT value FROM kv_state WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

fn write(tx: &Connection, key: &str, value: &str) -> Result<(), DatabaseError> {
    tx.execute(
        "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        params![key, value, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn digest(value: &impl Serialize) -> Result<String, DatabaseError> {
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(value).map_err(|error| refused(&error.to_string()))?,
    )))
}

fn full_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn canonical_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
}

/// A registration incarnation and its independently pinned echo counter.
/// The scope UUID, rather than the human ID or echo counter, identifies the
/// registration. Configuration and checkpoint drift invalidate this snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshRegistrationPin {
    pub repo_id: String,
    pub scope_uuid: String,
    pub configuration_digest: String,
    pub echo_generation: i64,
    pub last_git_sha: String,
    pub last_svn_rev: i64,
    pub scoped_git_checkpoint: Option<String>,
    pub scoped_svn_checkpoint: Option<String>,
}

/// Capture an already established durable identity. Never synthesize a UUID or
/// adopt a legacy human-ID checkpoint while admitting a refresh.
pub fn capture_refresh_registration(
    tx: &Connection,
    repo_id: &str,
) -> Result<RefreshRegistrationPin, DatabaseError> {
    let (scope, configuration, last_git_sha, last_svn_rev): (Option<String>, String, String, i64) =
        tx.query_row(
            "SELECT scope_uuid,json_object(
                'id',id,'parent_id',parent_id,'svn_url',svn_url,'svn_branch',svn_branch,
                'svn_username',svn_username,'git_provider',git_provider,'git_api_url',git_api_url,
                'git_repo',git_repo,'git_branch',git_branch,'sync_mode',sync_mode,
                'poll_interval_secs',poll_interval_secs,'lfs_threshold_mb',lfs_threshold_mb,
                'auto_merge',auto_merge,'enabled',enabled,'allowed_paths',allowed_paths,
                'blocked_patterns',blocked_patterns),last_git_sha,last_svn_rev
             FROM repositories WHERE id=?1",
            [repo_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| refused("registration is missing"))?;
    let scope_uuid = scope
        .filter(|id| canonical_uuid(id))
        .ok_or_else(|| refused("registration lacks a canonical durable scope UUID"))?;
    let echo_raw = read(
        tx,
        &super::repo_scope_identity::repo_echo_generation_kv_key(&scope_uuid),
    )?;
    let echo_generation = match echo_raw {
        None => 1,
        Some(raw) => raw
            .parse::<i64>()
            .ok()
            .filter(|value| *value > 0 && value.to_string() == raw)
            .ok_or_else(|| refused("echo generation is corrupt"))?,
    };
    Ok(RefreshRegistrationPin {
        repo_id: repo_id.into(),
        scoped_git_checkpoint: read(
            tx,
            &super::repo_scope_identity::last_git_sha_kv_key(&scope_uuid),
        )?,
        scoped_svn_checkpoint: read(tx, &format!("last_svn_rev_{repo_id}"))?,
        scope_uuid,
        configuration_digest: digest(&configuration)?,
        echo_generation,
        last_git_sha,
        last_svn_rev,
    })
}

/// Complete durable preparation evidence. No raw credential is stored here.
/// `credential_context_digest` is an authenticated caller's revision/context
/// pin, not permission to use or revive a credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshJobPins {
    pub child: RefreshRegistrationPin,
    pub parent: RefreshRegistrationPin,
    pub git: PreparedCleanChildGit,
    pub svn: PreparedCleanChildSvn,
    pub git_remote_url: String,
    pub git_branch: String,
    pub transport_policy: PinnedPushPolicy,
    pub credential_context_digest: String,
}

impl RefreshJobPins {
    pub fn approval_digest(&self) -> Result<String, DatabaseError> {
        digest(&(2_u8, "update_pair_from_parent", self))
    }

    fn validate(&self) -> Result<(), DatabaseError> {
        let child = &self.child;
        let parent = &self.parent;
        let svn = &self.svn.pins.lineage.pins;
        if child.repo_id.is_empty()
            || parent.repo_id.is_empty()
            || child.repo_id == parent.repo_id
            || !canonical_uuid(&child.scope_uuid)
            || !canonical_uuid(&parent.scope_uuid)
            || child.scope_uuid == parent.scope_uuid
            || !full_digest(&child.configuration_digest)
            || !full_digest(&parent.configuration_digest)
            || !full_digest(&self.credential_context_digest)
            || child.echo_generation < 1
            || parent.echo_generation < 1
            || child.last_svn_rev < 1
            || parent.last_svn_rev < 1
            || !is_full_git_oid(&child.last_git_sha)
            || !is_full_git_oid(&parent.last_git_sha)
            || self.git.operation_id.is_nil()
            || self.git.operation_id != self.svn.operation_id
            || self.git.prepared_at < 0
            || self.git.parent_advance_commits == 0
            || !is_full_git_oid(&self.git.prepared_sha)
            || !is_full_git_oid(&self.git.tree_sha)
            || !is_full_git_oid(&self.git.pins.parent_baseline_tip)
            || self.git.prepared_sha == child.last_git_sha
            || self.git.pins.child_tip != child.last_git_sha
            || self.git.pins.parent_tip != parent.last_git_sha
            || self.git.tree_sha != self.svn.result_git_tree
            || self.git.tree_sha != self.svn.pins.parent_git_tree
            || svn.child.child_mapping_revision != child.last_svn_rev
            || svn.child.child_revision != child.last_svn_rev
            || svn.parent_revision != parent.last_svn_rev
            || svn.parent_revision <= svn.child.parent_baseline_revision
            || self.git_branch.is_empty()
            || !git2::Reference::is_valid_name(&format!("refs/heads/{}", self.git_branch))
            || self.svn.working_copy.as_os_str().is_empty()
        {
            return Err(refused("invalid or inconsistent preparation pins"));
        }
        let endpoint = crate::pair_refresh::publication::canonical_url(&self.git_remote_url)
            .map_err(|_| refused("invalid Git endpoint"))?;
        let scheme = match self.transport_policy {
            PinnedPushPolicy::GitProtocolV1 => "git",
            PinnedPushPolicy::SmartHttpV1 => "https",
        };
        if endpoint.scheme() != scheme {
            return Err(refused("endpoint differs from pinned transport policy"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairRefreshPhase {
    Prepared,
    SvnIntent,
    SvnObserved,
    GitIntent,
    GitObserved,
    Completed,
    CancelledBeforePublication,
}

impl PairRefreshPhase {
    fn holds_scopes(self) -> bool {
        !matches!(self, Self::Completed | Self::CancelledBeforePublication)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRefreshOperation {
    pub version: u8,
    pub id: Uuid,
    pub initiator_id: String,
    pub request_id: String,
    pub approval_digest: String,
    pub admitted_writer_epoch: u64,
    pub created_at: String,
    pub updated_at: String,
    pub pins: RefreshJobPins,
    pub phase: PairRefreshPhase,
    pub svn_receipt: Option<SvnRefreshPublicationReceipt>,
    pub git_receipt_sha: Option<String>,
    /// An uncertainty diagnostic preserves the publication phase and both holds.
    pub reconciliation_detail: Option<String>,
}

fn request_key(initiator: &str, request: &str, child_scope: &str) -> Result<String, DatabaseError> {
    Ok(key("request", &digest(&(initiator, request, child_scope))?))
}

fn validate_svn_receipt(
    op: &PairRefreshOperation,
    receipt: &SvnRefreshPublicationReceipt,
) -> Result<(), DatabaseError> {
    let prepared = &op.pins.svn;
    let mut expected_copy = prepared.pins.lineage.child_copy.clone();
    expected_copy.inspected_revision = receipt.revision;
    if receipt.operation_id != op.id
        || receipt.revision <= op.pins.child.last_svn_rev
        || receipt.child_url != prepared.pins.lineage.pins.child_url
        || receipt.child_copy != expected_copy
        || receipt.git_tree != op.pins.git.tree_sha
        || receipt.mergeinfo != prepared.mergeinfo
        || receipt.properties != prepared.result_properties
    {
        return Err(refused("SVN receipt differs from durable intent"));
    }
    Ok(())
}

fn validate(op: &PairRefreshOperation) -> Result<(), DatabaseError> {
    op.pins.validate()?;
    if op.version != 2
        || op.id != op.pins.git.operation_id
        || op.initiator_id.is_empty()
        || op.request_id.is_empty()
        || op.request_id.len() > 256
        || op.admitted_writer_epoch == 0
        || op.approval_digest != op.pins.approval_digest()?
        || chrono::DateTime::parse_from_rfc3339(&op.created_at).is_err()
        || chrono::DateTime::parse_from_rfc3339(&op.updated_at).is_err()
    {
        return Err(refused("invalid operation document"));
    }
    let needs_svn = matches!(
        op.phase,
        PairRefreshPhase::SvnObserved
            | PairRefreshPhase::GitIntent
            | PairRefreshPhase::GitObserved
            | PairRefreshPhase::Completed
    );
    let needs_git = matches!(
        op.phase,
        PairRefreshPhase::GitObserved | PairRefreshPhase::Completed
    );
    if needs_svn != op.svn_receipt.is_some()
        || needs_git != op.git_receipt_sha.is_some()
        || op
            .git_receipt_sha
            .as_ref()
            .is_some_and(|sha| sha != &op.pins.git.prepared_sha)
        || (!op.phase.holds_scopes() && op.reconciliation_detail.is_some())
        || op
            .reconciliation_detail
            .as_ref()
            .is_some_and(|detail| detail.is_empty())
    {
        return Err(refused(
            "receipt or hold state differs from publication phase",
        ));
    }
    if let Some(receipt) = &op.svn_receipt {
        validate_svn_receipt(op, receipt)?;
    }
    Ok(())
}

fn parse(raw: &str) -> Result<PairRefreshOperation, DatabaseError> {
    let op: PairRefreshOperation =
        serde_json::from_str(raw).map_err(|_| refused("corrupt operation document"))?;
    validate(&op)?;
    // Documents are emitted canonically. Refuse unknown nested fields, duplicate
    // keys or ambiguous alternate encodings rather than dropping evidence.
    if serde_json::to_string(&op).map_err(|error| refused(&error.to_string()))? != raw {
        return Err(refused("noncanonical operation document"));
    }
    Ok(op)
}

fn write_op(tx: &Connection, op: &PairRefreshOperation) -> Result<(), DatabaseError> {
    validate(op)?;
    write(
        tx,
        &key("document", &op.id.to_string()),
        &serde_json::to_string(op).map_err(|error| refused(&error.to_string()))?,
    )
}

/// Validate the complete journal/hold relationship, including orphan documents
/// or pointers. Unknown versions and corrupt evidence fail closed for every
/// writer, even when the damaged entry mentions another repository.
fn journal(tx: &Connection) -> Result<Vec<PairRefreshOperation>, DatabaseError> {
    let mut query =
        tx.prepare("SELECT key,value FROM kv_state WHERE substr(key,1,?1)=?2 ORDER BY key")?;
    let rows = query
        .query_map(params![FAMILY.len(), FAMILY], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut operations = Vec::new();
    let mut expected = BTreeMap::new();
    for (name, raw) in &rows {
        if name.starts_with(&key("document", "")) {
            let op = parse(raw)?;
            if name != &key("document", &op.id.to_string()) {
                return Err(refused("document key differs from operation identity"));
            }
            expected.insert(name.clone(), raw.clone());
            if expected
                .insert(
                    request_key(&op.initiator_id, &op.request_id, &op.pins.child.scope_uuid)?,
                    op.id.to_string(),
                )
                .is_some()
            {
                return Err(refused("duplicate request ownership"));
            }
            if op.phase.holds_scopes() {
                for scope in [&op.pins.child.scope_uuid, &op.pins.parent.scope_uuid] {
                    if expected
                        .insert(key("hold", scope), op.id.to_string())
                        .is_some()
                    {
                        return Err(refused("duplicate durable scope ownership"));
                    }
                }
            }
            operations.push(op);
        }
    }
    if rows != expected {
        return Err(refused(
            "unknown journal version, orphan or missing hold/request pointer",
        ));
    }
    Ok(operations)
}

/// Shared guard for all production writer entrances. A global writer without a
/// registered repository ID refuses while any refresh owns either scope.
pub fn require_no_pair_refresh_hold(
    tx: &Connection,
    repo_id: Option<&str>,
) -> Result<(), DatabaseError> {
    let held = journal(tx)?.into_iter().any(|op| {
        op.phase.holds_scopes()
            && repo_id.is_none_or(|id| id == op.pins.child.repo_id || id == op.pins.parent.repo_id)
    });
    if held {
        return Err(refused("a durable pair refresh owns this writer scope"));
    }
    Ok(())
}

fn require_pins_current(tx: &Connection, pins: &RefreshJobPins) -> Result<(), DatabaseError> {
    for pin in [&pins.child, &pins.parent] {
        if capture_refresh_registration(tx, &pin.repo_id)? != *pin {
            return Err(refused(
                "registration, configuration, echo generation or checkpoint drift",
            ));
        }
    }
    let parent: Option<String> = tx.query_row(
        "SELECT parent_id FROM repositories WHERE id=?1",
        [&pins.child.repo_id],
        |row| row.get(0),
    )?;
    if parent.as_deref() != Some(&pins.parent.repo_id) {
        return Err(refused("child no longer belongs to the pinned parent"));
    }
    Ok(())
}

fn require_other_journals_quiet(tx: &Connection, repo_id: &str) -> Result<(), DatabaseError> {
    for prefix in [
        "import_operation_v1",
        "git_to_svn_commit_v1",
        "svn_to_git_push_v1",
        "team_cycle_mapping_v1",
        "late_pair_publish_v1",
        "managed_remove_v1",
    ] {
        if read(tx, &format!("{prefix}:active:{repo_id}"))?.is_some() {
            return Err(refused("another operation owns the repository"));
        }
        if let Some(id) = read(tx, &format!("{prefix}:latest:{repo_id}"))? {
            let raw = read(tx, &format!("{prefix}:document:{id}"))?
                .ok_or_else(|| refused("another journal's latest document is missing"))?;
            macro_rules! completed {
                ($module:ident, $document:ident, $owner:ident, $kind:expr) => {{
                    let op: super::$module::$document = serde_json::from_str(&raw)
                        .map_err(|_| refused("another journal's latest document is corrupt"))?;
                    op.version == 1
                        && op.id == id
                        && op.$owner == repo_id
                        && $kind.contains(&op.operation_type.as_str())
                        && serde_json::to_value(&op.state)
                            .map_err(|_| refused("invalid prior operation state"))?
                            == "completed"
                }};
            }
            let completed = match prefix {
                "import_operation_v1" => completed!(
                    import_operations,
                    ImportOperation,
                    repo_id,
                    ["full_import", "snapshot_import"]
                ),
                "git_to_svn_commit_v1" => completed!(
                    svn_commit_operations,
                    SvnCommitOperation,
                    repo_id,
                    ["git_to_svn_commit"]
                ),
                "svn_to_git_push_v1" => completed!(
                    git_push_operations,
                    GitPushOperation,
                    repo_id,
                    ["svn_to_git_push"]
                ),
                "team_cycle_mapping_v1" => completed!(
                    team_cycle_mapping_operations,
                    TeamCycleMappingOperation,
                    repo_id,
                    ["team_cycle_mapping"]
                ),
                "late_pair_publish_v1" => completed!(
                    late_pair_publish_operations,
                    LatePairPublishOperation,
                    parent_repo_id,
                    ["late_pair_publish"]
                ),
                "managed_remove_v1" => completed!(
                    managed_remove,
                    ManagedRemoveOperation,
                    repo_id,
                    ["managed_remove"]
                ),
                _ => return Err(refused("unsupported competing journal")),
            };
            if !completed {
                return Err(refused(
                    "another journal has an unfinished or uncertain latest operation",
                ));
            }
        }
    }
    if super::managed_remove::new_work_blocked(tx, repo_id)? {
        return Err(refused("managed removal owns the repository"));
    }
    Ok(())
}

fn owned_operation(
    tx: &Connection,
    operation_id: Uuid,
) -> Result<PairRefreshOperation, DatabaseError> {
    journal(tx)?
        .into_iter()
        .find(|op| op.id == operation_id)
        .ok_or_else(|| refused("operation not found"))
}

fn release(tx: &Connection, op: &PairRefreshOperation) -> Result<(), DatabaseError> {
    for scope in [&op.pins.child.scope_uuid, &op.pins.parent.scope_uuid] {
        if tx.execute(
            "DELETE FROM kv_state WHERE key=?1 AND value=?2",
            params![key("hold", scope), op.id.to_string()],
        )? != 1
        {
            return Err(refused("lost durable hold during completion"));
        }
    }
    Ok(())
}

impl Database {
    pub fn pair_refresh_operation(
        &self,
        operation_id: Uuid,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        owned_operation(&self.conn(), operation_id)
    }

    pub fn pair_refresh_request(
        &self,
        initiator: &str,
        request: &str,
        child_scope: &str,
    ) -> Result<Option<PairRefreshOperation>, DatabaseError> {
        Ok(journal(&self.conn())?.into_iter().find(|op| {
            op.initiator_id == initiator
                && op.request_id == request
                && op.pins.child.scope_uuid == child_scope
        }))
    }

    /// Atomically enroll one request and both durable holds. A repeated request
    /// must present the identical durable intent; it never creates another job.
    pub fn admit_pair_refresh(
        &self,
        initiator: &str,
        request: &str,
        approved_digest: &str,
        pins: RefreshJobPins,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.transaction(|tx| {
            let epoch = crate::writer_fence::require_owned_current(tx)?;
            pins.validate()?;
            if !busy.owns_pair(&pins.child.repo_id, &pins.parent.repo_id) {
                return Err(refused("admission does not own both process busy slots"));
            }
            if pins.approval_digest()? != approved_digest
                || initiator.is_empty()
                || request.is_empty()
                || request.len() > 256
            {
                return Err(refused("approval or request identity mismatch"));
            }
            let operations = journal(tx)?;
            if let Some(op) = operations.iter().find(|op| {
                op.initiator_id == initiator
                    && op.request_id == request
                    && op.pins.child.scope_uuid == pins.child.scope_uuid
            }) {
                if op.pins != pins || op.approval_digest != approved_digest {
                    return Err(refused(
                        "request ID was already bound to a different intent",
                    ));
                }
                return Ok(op.clone());
            }
            if operations.iter().any(|op| op.id == pins.git.operation_id) {
                return Err(refused("operation identity was reused"));
            }
            require_pins_current(tx, &pins)?;
            for id in [&pins.child.repo_id, &pins.parent.repo_id] {
                require_no_pair_refresh_hold(tx, Some(id))?;
                require_other_journals_quiet(tx, id)?;
            }
            let now = Utc::now().to_rfc3339();
            let op = PairRefreshOperation {
                version: 2,
                id: pins.git.operation_id,
                initiator_id: initiator.into(),
                request_id: request.into(),
                approval_digest: approved_digest.into(),
                admitted_writer_epoch: epoch,
                created_at: now.clone(),
                updated_at: now,
                pins,
                phase: PairRefreshPhase::Prepared,
                svn_receipt: None,
                git_receipt_sha: None,
                reconciliation_detail: None,
            };
            write_op(tx, &op)?;
            write(
                tx,
                &request_key(initiator, request, &op.pins.child.scope_uuid)?,
                &op.id.to_string(),
            )?;
            for scope in [&op.pins.child.scope_uuid, &op.pins.parent.scope_uuid] {
                write(tx, &key("hold", scope), &op.id.to_string())?;
            }
            Ok(op)
        })
    }

    fn advance_refresh(
        &self,
        id: Uuid,
        expected: PairRefreshPhase,
        phase: PairRefreshPhase,
        receipt: Option<SvnRefreshPublicationReceipt>,
        git_sha: Option<&str>,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.transaction(|tx| {
            crate::writer_fence::require_owned_current(tx)?;
            let mut op = owned_operation(tx, id)?;
            if op.phase != expected
                || (op.reconciliation_detail.is_some() && receipt.is_none() && git_sha.is_none())
            {
                return Err(refused(
                    "publication phase changed or requires reconciliation",
                ));
            }
            require_pins_current(tx, &op.pins)?;
            op.phase = phase;
            if let Some(receipt) = receipt {
                op.svn_receipt = Some(receipt);
            }
            if let Some(sha) = git_sha {
                op.git_receipt_sha = Some(sha.into());
            }
            // An independently observed exact receipt can reconcile a lost
            // response at this intent phase. Invalid proof rolls this change
            // back; uncertainty alone never permits another publication.
            op.reconciliation_detail = None;
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            Ok(op)
        })
    }

    pub fn record_pair_refresh_svn_intent(
        &self,
        id: Uuid,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.advance_refresh(
            id,
            PairRefreshPhase::Prepared,
            PairRefreshPhase::SvnIntent,
            None,
            None,
        )
    }
    pub fn record_pair_refresh_svn_receipt(
        &self,
        id: Uuid,
        receipt: SvnRefreshPublicationReceipt,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.advance_refresh(
            id,
            PairRefreshPhase::SvnIntent,
            PairRefreshPhase::SvnObserved,
            Some(receipt),
            None,
        )
    }
    pub fn record_pair_refresh_git_intent(
        &self,
        id: Uuid,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.advance_refresh(
            id,
            PairRefreshPhase::SvnObserved,
            PairRefreshPhase::GitIntent,
            None,
            None,
        )
    }
    pub fn record_pair_refresh_git_receipt(
        &self,
        id: Uuid,
        sha: &str,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.advance_refresh(
            id,
            PairRefreshPhase::GitIntent,
            PairRefreshPhase::GitObserved,
            None,
            Some(sha),
        )
    }

    /// Retain the last intent/receipt phase and both holds, including after input
    /// drift. Only the current writer may record this diagnostic. There is no
    /// automatic retry authorization or hold-release API for uncertain work.
    pub fn hold_pair_refresh_reconciliation(
        &self,
        id: Uuid,
        detail: &str,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.transaction(|tx| {
            crate::writer_fence::require_owned_current(tx)?;
            let mut op = owned_operation(tx, id)?;
            if !op.phase.holds_scopes() || detail.is_empty() {
                return Err(refused("invalid reconciliation hold"));
            }
            op.reconciliation_detail = Some(detail.into());
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            Ok(op)
        })
    }

    pub fn cancel_unpublished_pair_refresh(
        &self,
        id: Uuid,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.transaction(|tx| {
            crate::writer_fence::require_owned_current(tx)?;
            let mut op = owned_operation(tx, id)?;
            if op.phase != PairRefreshPhase::Prepared || op.reconciliation_detail.is_some() {
                return Err(refused(
                    "publication intent or uncertainty prevents cancellation",
                ));
            }
            release(tx, &op)?;
            op.phase = PairRefreshPhase::CancelledBeforePublication;
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            Ok(op)
        })
    }

    /// Commit the child correspondence, both cursor copies, completion receipt
    /// and two hold releases in one SQLite transaction. Parent state and both
    /// echo generations remain unchanged. Caller supplies fresh remote proof.
    pub fn finalize_pair_refresh(&self, id: Uuid) -> Result<PairRefreshOperation, DatabaseError> {
        self.transaction(|tx| {
            crate::writer_fence::require_owned_current(tx)?;
            let mut op = owned_operation(tx, id)?;
            if op.phase == PairRefreshPhase::Completed { return Ok(op); }
            if op.phase != PairRefreshPhase::GitObserved || op.reconciliation_detail.is_some() { return Err(refused("both publications are not confirmed")); }
            require_pins_current(tx, &op.pins)?;
            let receipt = op.svn_receipt.as_ref().ok_or_else(|| refused("missing SVN receipt"))?;
            let child = &op.pins.child;
            let sha = &op.pins.git.prepared_sha;
            let conflict: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_records WHERE repo_id=?1 AND (git_sha=?2 OR svn_rev=?3))", params![child.repo_id, sha, receipt.revision], |row| row.get(0))?;
            if conflict { return Err(refused("publication already has correspondence outside this job")); }
            let now = Utc::now().to_rfc3339();
            tx.execute("INSERT INTO sync_records(id,repo_id,svn_rev,git_sha,direction,author,message,timestamp,synced_at,status) VALUES(?1,?2,?3,?4,'git_to_svn','RepoSync','Update pair from parent',?5,?5,'applied')", params![op.id.to_string(), child.repo_id, receipt.revision, sha, now])?;
            if tx.execute("UPDATE repositories SET last_git_sha=?1,last_svn_rev=?2,last_sync_at=?3,total_syncs=total_syncs+1,updated_at=?3 WHERE id=?4 AND scope_uuid=?5", params![sha, receipt.revision, now, child.repo_id, child.scope_uuid])? != 1 { return Err(refused("child registration disappeared during completion")); }
            crate::echo_receipt_scope::write_scoped_last_git_sha_kv(tx, &child.repo_id, sha, &now)?;
            write(tx, &format!("last_svn_rev_{}", child.repo_id), &receipt.revision.to_string())?;
            release(tx, &op)?;
            op.phase = PairRefreshPhase::Completed;
            op.updated_at = now;
            write_op(tx, &op)?;
            Ok(op)
        })
    }
}
