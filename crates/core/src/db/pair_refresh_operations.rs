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

#[path = "pair_refresh_authenticated.rs"]
mod authenticated;

pub(crate) use authenticated::approval_digest as authenticated_approval_digest;
pub(crate) use authenticated::noop_approval_digest as authenticated_noop_approval_digest;

#[derive(PartialEq, Eq)]
enum AdmissionValidation {
    Storage,
    Credentials,
    RegisteredSources,
}

fn refused(detail: &str) -> DatabaseError {
    DatabaseError::Other(format!("pair refresh journal: {detail}"))
}

fn key(kind: &str, id: &str) -> String {
    format!("{PREFIX}{kind}:{id}")
}

fn operation_key(version: u8, kind: &str, id: &str) -> String {
    format!("pair_refresh_execution_v{version}:{kind}:{id}")
}

fn operation_request_key(op: &PairRefreshOperation) -> Result<String, DatabaseError> {
    Ok(operation_key(
        op.version,
        "request",
        &digest(&(&op.initiator_id, &op.request_id, &op.pins.child.scope_uuid))?,
    ))
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
    let scoped_git_checkpoint = read(
        tx,
        &super::repo_scope_identity::last_git_sha_kv_key(&scope_uuid),
    )?;
    let scoped_svn_checkpoint = read(tx, &format!("last_svn_rev_{repo_id}"))?;
    if scoped_git_checkpoint
        .as_deref()
        .is_some_and(|value| value != last_git_sha)
        || (!last_git_sha.is_empty() && scoped_git_checkpoint.is_none())
        || scoped_svn_checkpoint
            .as_deref()
            .is_some_and(|value| value != last_svn_rev.to_string())
        || (last_svn_rev > 0 && scoped_svn_checkpoint.is_none())
    {
        return Err(refused(
            "column and scoped checkpoint copies differ or are missing",
        ));
    }
    Ok(RefreshRegistrationPin {
        repo_id: repo_id.into(),
        scoped_git_checkpoint,
        scoped_svn_checkpoint,
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
        crate::pair_refresh::lineage::validate_stored_refresh_proof(&self.svn.pins.lineage)
            .map_err(|_| refused("missing or contradictory stored lineage proof"))?;
        let lineage = &self.svn.pins.lineage.pins;
        let expected_mergeinfo = crate::pair_refresh::svn_preparation::expected_refresh_mergeinfo(
            &lineage.child.parent_path,
            lineage.child.parent_baseline_revision,
            lineage.parent_revision,
            self.svn.pins.previous_mergeinfo.as_deref(),
        )
        .map_err(|_| refused("invalid stored mergeinfo range"))?;
        if self.svn.mergeinfo != expected_mergeinfo {
            return Err(refused("stored mergeinfo differs from the approved range"));
        }
        let child = &self.child;
        let parent = &self.parent;
        let svn = &self.svn.pins.lineage.pins;
        let copy = &self.svn.pins.lineage.child_copy;
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
            || child.scoped_git_checkpoint.as_deref() != Some(child.last_git_sha.as_str())
            || parent.scoped_git_checkpoint.as_deref() != Some(parent.last_git_sha.as_str())
            || child.scoped_svn_checkpoint.as_deref()
                != Some(child.last_svn_rev.to_string().as_str())
            || parent.scoped_svn_checkpoint.as_deref()
                != Some(parent.last_svn_rev.to_string().as_str())
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
            || copy.repository_uuid != svn.child.repository_uuid
            || copy.child_path != svn.child.child_path
            || copy.parent_path != svn.child.parent_path
            || copy.inspected_revision != svn.child.child_revision
            || copy.created_revision < 1
            || copy.created_revision > svn.child.child_revision
            || self
                .svn
                .result_properties
                .get("")
                .and_then(|props| props.get("svn:mergeinfo"))
                != Some(&self.svn.mergeinfo)
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
    if !matches!(op.version, 2 | 3)
        || op.id != op.pins.git.operation_id
        || op.initiator_id.is_empty()
        || op.request_id.is_empty()
        || op.request_id.len() > 256
        || op.admitted_writer_epoch == 0
        || (op.version == 2 && op.approval_digest != op.pins.approval_digest()?)
        || !full_digest(&op.approval_digest)
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
    if op.version != 2 {
        return Err(refused(
            "legacy mutation cannot write authenticated operation",
        ));
    }
    validate(op)?;
    write(
        tx,
        &key("document", &op.id.to_string()),
        &serde_json::to_string(op).map_err(|error| refused(&error.to_string()))?,
    )
}

/// The initial copy is the first baseline. A later baseline needs the exact
/// completed child receipt, not caller-supplied mergeinfo or equal file bytes.
fn require_baseline_provenance(
    pins: &RefreshJobPins,
    operations: &[PairRefreshOperation],
) -> Result<(), DatabaseError> {
    let lineage = &pins.svn.pins.lineage;
    let baseline = lineage.pins.child.parent_baseline_revision;
    let mut candidates = operations.iter().filter(|op| {
        op.phase == PairRefreshPhase::Completed
            && op.pins.child.scope_uuid == pins.child.scope_uuid
            && (op.pins.git.prepared_sha == pins.child.last_git_sha
                || op
                    .svn_receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.revision == pins.child.last_svn_rev))
    });
    let Some(prior) = candidates.next() else {
        return if pins.svn.pins.previous_mergeinfo.is_none()
            && lineage.child_copy.copy_from_revision == baseline
        {
            Ok(())
        } else {
            Err(refused(
                "advanced baseline lacks a completed refresh receipt",
            ))
        };
    };
    if candidates.next().is_some() {
        return Err(refused("ambiguous completed child receipt"));
    }
    let receipt = prior
        .svn_receipt
        .as_ref()
        .ok_or_else(|| refused("completed child receipt is missing"))?;
    if prior.pins.git.prepared_sha != pins.child.last_git_sha
        || receipt.revision != pins.child.last_svn_rev
    {
        return Err(refused(
            "child checkpoints partially match a completed receipt",
        ));
    }
    let same_registration = |a: &RefreshRegistrationPin, b: &RefreshRegistrationPin| {
        a.repo_id == b.repo_id
            && a.scope_uuid == b.scope_uuid
            && a.configuration_digest == b.configuration_digest
            && a.echo_generation == b.echo_generation
    };
    let mut expected_copy = receipt.child_copy.clone();
    expected_copy.inspected_revision = lineage.child_copy.inspected_revision;
    let prior_lineage = &prior.pins.svn.pins.lineage;
    if prior.id == pins.git.operation_id
        || !same_registration(&prior.pins.child, &pins.child)
        || !same_registration(&prior.pins.parent, &pins.parent)
        || pins.git.pins.parent_baseline_tip != prior.pins.parent.last_git_sha
        || baseline != prior.pins.parent.last_svn_rev
        || pins.svn.pins.baseline_git_tree != receipt.git_tree
        || pins.svn.pins.previous_mergeinfo.as_ref() != Some(&receipt.mergeinfo)
        || lineage.child_copy != expected_copy
        || lineage.parent_originating_revision != prior_lineage.parent_originating_revision
        || lineage.pins.repository_root_url != prior_lineage.pins.repository_root_url
        || lineage.pins.child_url != prior_lineage.pins.child_url
        || lineage.pins.parent_url != prior_lineage.pins.parent_url
        || pins.git_remote_url != prior.pins.git_remote_url
        || pins.git_branch != prior.pins.git_branch
        || pins.transport_policy != prior.pins.transport_policy
    {
        return Err(refused(
            "advanced baseline differs from completed refresh evidence",
        ));
    }
    Ok(())
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
    let mut requests = std::collections::BTreeSet::new();
    let mut held_scopes = std::collections::BTreeSet::new();
    for (name, raw) in &rows {
        if [2, 3]
            .iter()
            .any(|version| name.starts_with(&operation_key(*version, "document", "")))
        {
            let op = parse(raw)?;
            if name != &operation_key(op.version, "document", &op.id.to_string())
                || operations
                    .iter()
                    .any(|prior: &PairRefreshOperation| prior.id == op.id)
            {
                return Err(refused("document key differs from operation identity"));
            }
            expected.insert(name.clone(), raw.clone());
            if !requests.insert((
                op.initiator_id.clone(),
                op.request_id.clone(),
                op.pins.child.scope_uuid.clone(),
            )) {
                return Err(refused("duplicate request ownership"));
            }
            expected.insert(operation_request_key(&op)?, op.id.to_string());
            if op.version == 3 {
                let name = operation_key(3, "authority", &op.id.to_string());
                let raw = rows
                    .get(&name)
                    .ok_or_else(|| refused("authenticated authority is missing"))?;
                authenticated::verify(tx, &op, raw)?;
                expected.insert(name, raw.clone());
            }
            if op.phase.holds_scopes() {
                for scope in [&op.pins.child.scope_uuid, &op.pins.parent.scope_uuid] {
                    if !held_scopes.insert(scope.to_owned()) {
                        return Err(refused("duplicate durable scope ownership"));
                    }
                    expected.insert(operation_key(op.version, "hold", scope), op.id.to_string());
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
    authenticated::require_key_matches_journal(tx, &operations)?;
    let mut completed_receipts = std::collections::BTreeSet::new();
    for op in &operations {
        if op.phase == PairRefreshPhase::Completed {
            let receipt = op
                .svn_receipt
                .as_ref()
                .ok_or_else(|| refused("completed child receipt is missing"))?;
            if !completed_receipts.insert((
                &op.pins.child.scope_uuid,
                &op.pins.git.prepared_sha,
                receipt.revision,
            )) {
                return Err(refused("duplicate completed child receipt"));
            }
        }
        require_baseline_provenance(&op.pins, &operations)?;
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
            && (op.version == 3
                || repo_id
                    .is_none_or(|id| id == op.pins.child.repo_id || id == op.pins.parent.repo_id))
    });
    if held {
        return Err(refused("a durable pair refresh owns this writer scope"));
    }
    Ok(())
}

/// Existing personal-mode journals have no managed registration identity and
/// must be treated as global writers for refresh exclusion.
pub fn require_no_pair_refresh_hold_for_writer(
    tx: &Connection,
    repo_id: &str,
) -> Result<(), DatabaseError> {
    let scope = (repo_id != super::personal_scope::PERSONAL_SCOPE_KEY).then_some(repo_id);
    require_no_pair_refresh_hold(tx, scope)
}

/// Registration and credential mutations must also exclude ancestors of either
/// held registration. Changing an ancestor can change inherited credentials or
/// their resolution chain even when it has no refresh hold of its own.
pub fn require_no_pair_refresh_registration_mutation(
    tx: &Connection,
    repo_id: &str,
) -> Result<(), DatabaseError> {
    for op in journal(tx)? {
        if !op.phase.holds_scopes() {
            continue;
        }
        if op.version == 3 {
            return Err(refused(
                "an authenticated pair refresh owns the global writer barrier",
            ));
        }
        for pin in [&op.pins.child, &op.pins.parent] {
            let mut current = Some(pin.repo_id.clone());
            let mut seen = std::collections::BTreeSet::new();
            while let Some(id) = current {
                if !seen.insert(id.clone()) {
                    return Err(refused("held registration has a cyclic ancestry"));
                }
                if id == repo_id {
                    return Err(refused(
                        "a durable pair refresh owns this registration chain",
                    ));
                }
                current = tx
                    .query_row(
                        "SELECT parent_id FROM repositories WHERE id=?1",
                        [&id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()?
                    .ok_or_else(|| refused("held registration ancestry is missing"))?;
            }
        }
    }
    Ok(())
}

/// Classify credential state writes. Generic KV is also used by internal
/// diagnostics and fault injection; typed checkpoint writers have their own
/// transaction guards. The caller must retain the transaction
/// containing this read through its mutation, so admission cannot race it.
pub fn require_no_pair_refresh_state_mutation(
    tx: &Connection,
    state_key: &str,
) -> Result<(), DatabaseError> {
    for prefix in [
        "secret_git_token",
        "secret_svn_password",
        "git_token",
        "svn_password",
    ] {
        if state_key == prefix {
            return require_no_pair_refresh_hold(tx, None);
        }
        if let Some(id) = state_key.strip_prefix(&format!("{prefix}_")) {
            return require_no_pair_refresh_registration_mutation(tx, id);
        }
    }
    if matches!(
        state_key,
        "encryption_key" | "last_git_hash" | "last_svn_rev"
    ) || state_key == super::repo_scope_identity::LEGACY_REPO_ID_KV_READS_KEY
    {
        return require_no_pair_refresh_hold(tx, None);
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

fn require_credentials_current(
    tx: &Connection,
    pins: &RefreshJobPins,
) -> Result<super::pair_refresh_credentials::RefreshCredentials, DatabaseError> {
    use subtle::ConstantTimeEq;
    let credentials = super::pair_refresh_credentials::resolve_refresh_credentials(
        tx,
        &pins.child.repo_id,
        &pins.parent.repo_id,
    )?;
    if !bool::from(
        credentials
            .context_digest()
            .as_bytes()
            .ct_eq(pins.credential_context_digest.as_bytes()),
    ) {
        return Err(refused("credential context differs from approval"));
    }
    Ok(credentials)
}

fn require_execution_conflicts_quiet(
    tx: &Connection,
    pins: &RefreshJobPins,
) -> Result<(), DatabaseError> {
    let blocking: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM conflicts WHERE status NOT IN ('resolved','dismissed') AND
         (repo_id IS NULL OR repo_id='' OR repo_id=?1 OR repo_id=?2 OR repo_id=?3 OR repo_id=?4))",
        params![
            pins.child.repo_id,
            pins.parent.repo_id,
            super::personal_scope::PERSONAL_SCOPE_KEY,
            super::personal_scope::LEGACY_PERSONAL_REPO_ID
        ],
        |row| row.get(0),
    )?;
    if blocking {
        return Err(refused("unresolved conflict prevents execution"));
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

fn lookup_operation(
    tx: &Connection,
    operation_id: Uuid,
) -> Result<PairRefreshOperation, DatabaseError> {
    journal(tx)?
        .into_iter()
        .find(|op| op.id == operation_id)
        .ok_or_else(|| refused("operation not found"))
}

fn owned_operation(
    tx: &Connection,
    operation_id: Uuid,
) -> Result<PairRefreshOperation, DatabaseError> {
    let op = lookup_operation(tx, operation_id)?;
    if op.version != 2 {
        return Err(refused(
            "authenticated operation requires its actor capability",
        ));
    }
    Ok(op)
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

fn require_held_execution_inputs(
    tx: &Connection,
    id: Uuid,
    busy: &crate::busy::PairBusyGuard,
) -> Result<PairRefreshOperation, DatabaseError> {
    crate::writer_fence::require_owned_current(tx)?;
    let op = owned_operation(tx, id)?;
    if !op.phase.holds_scopes() || !busy.owns_pair(&op.pins.child.repo_id, &op.pins.parent.repo_id)
    {
        return Err(refused("execution does not own both held process scopes"));
    }
    require_pins_current(tx, &op.pins)?;
    require_execution_conflicts_quiet(tx, &op.pins)?;
    for pin in [&op.pins.child, &op.pins.parent] {
        require_other_journals_quiet(tx, &pin.repo_id)?;
        super::ordinary_writer_windows::require_quiet_for_refresh(
            tx,
            &pin.repo_id,
            &pin.scope_uuid,
        )?;
    }
    for id in super::personal_scope::personal_repo_ids_for_journal_read() {
        require_other_journals_quiet(tx, id)?;
    }
    Ok(op)
}

impl Database {
    /// Validate newly prepared local evidence without enrolling a request.
    /// Remote proof and actor authorization come from the separate caller.
    pub fn require_registered_pair_refresh_preparation(
        &self,
        configuration: &crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
        pins: &RefreshJobPins,
    ) -> Result<(), DatabaseError> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let current =
            crate::pair_refresh::execution_configuration::capture_execution_configuration(
                &tx,
                &configuration.pin().child.repo_id,
            )?;
        if current.pin() != configuration.pin() {
            return Err(refused("registered inputs changed during preparation"));
        }
        current.require_job_configuration(pins)?;
        pins.validate()?;
        let operations = journal(&tx)?;
        require_no_pair_refresh_hold(&tx, Some(&pins.child.repo_id))?;
        require_no_pair_refresh_hold(&tx, Some(&pins.parent.repo_id))?;
        require_baseline_provenance(pins, &operations)?;
        require_execution_conflicts_quiet(&tx, pins)?;
        for pin in [&pins.child, &pins.parent] {
            require_other_journals_quiet(&tx, &pin.repo_id)?;
            super::ordinary_writer_windows::require_quiet_for_refresh(
                &tx,
                &pin.repo_id,
                &pin.scope_uuid,
            )?;
        }
        for id in super::personal_scope::personal_repo_ids_for_journal_read() {
            require_other_journals_quiet(&tx, id)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Read the unique completed receipt for the current child checkpoints.
    /// None means that the first-copy baseline still needs independent proof.
    /// A changed child after an earlier refresh cannot fall back to that case.
    /// This read-only storage evidence does not prove current remote state or
    /// authorize a new request; admission must revalidate the preparation.
    pub fn capture_completed_pair_refresh_baseline(
        &self,
        configuration: &crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
    ) -> Result<Option<PairRefreshOperation>, DatabaseError> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        let current =
            crate::pair_refresh::execution_configuration::capture_execution_configuration(
                &tx,
                &configuration.pin().child.repo_id,
            )?;
        if current.pin() != configuration.pin() {
            return Err(refused(
                "registered inputs changed before baseline selection",
            ));
        }
        let pin = current.pin();
        let operations = journal(&tx)?;
        if operations.iter().any(|op| {
            op.phase.holds_scopes()
                && [&op.pins.child.repo_id, &op.pins.parent.repo_id]
                    .iter()
                    .any(|id| **id == pin.child.repo_id || **id == pin.parent.repo_id)
        }) {
            return Err(refused("a held refresh cannot select a new baseline"));
        }
        let prior = operations
            .iter()
            .filter(|op| {
                op.phase == PairRefreshPhase::Completed
                    && op.pins.child.scope_uuid == pin.child.scope_uuid
            })
            .collect::<Vec<_>>();
        if prior.is_empty() {
            tx.commit()?;
            return Ok(None);
        }
        let mut matches = prior.into_iter().filter(|op| {
            op.pins.git.prepared_sha == pin.child.last_git_sha
                || op
                    .svn_receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.revision == pin.child.last_svn_rev)
        });
        let completed = matches
            .next()
            .ok_or_else(|| refused("current child has no matching completed refresh baseline"))?;
        if matches.next().is_some() {
            return Err(refused("ambiguous completed refresh baseline"));
        }
        let receipt = completed
            .svn_receipt
            .as_ref()
            .ok_or_else(|| refused("completed refresh baseline has no SVN receipt"))?;
        let same_registration = |a: &RefreshRegistrationPin, b: &RefreshRegistrationPin| {
            a.repo_id == b.repo_id
                && a.scope_uuid == b.scope_uuid
                && a.configuration_digest == b.configuration_digest
                && a.echo_generation == b.echo_generation
        };
        if completed.pins.git.prepared_sha != pin.child.last_git_sha
            || receipt.revision != pin.child.last_svn_rev
            || !same_registration(&completed.pins.child, &pin.child)
            || !same_registration(&completed.pins.parent, &pin.parent)
            || completed.pins.git_remote_url != pin.git_remote_url
            || completed.pins.git_branch != pin.child_git_branch
            || completed.pins.transport_policy != pin.transport_policy
            || completed.pins.svn.pins.lineage.pins.child_url != pin.child_svn_url
            || completed.pins.svn.pins.lineage.pins.parent_url != pin.parent_svn_url
        {
            return Err(refused(
                "completed refresh baseline differs from current registration",
            ));
        }
        let result = completed.clone();
        tx.commit()?;
        Ok(Some(result))
    }

    pub fn pair_refresh_operation(
        &self,
        operation_id: Uuid,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        lookup_operation(&self.conn(), operation_id)
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
        self.admit_pair_refresh_guarded(
            initiator,
            request,
            approved_digest,
            pins,
            busy,
            AdmissionValidation::Storage,
        )
    }

    /// The execution caller must authenticate the initiator and independently
    /// prove live remote pins. This variant also resolves revocation and binds
    /// current credential context inside the transaction creating both holds.
    pub fn admit_pair_refresh_with_current_credentials(
        &self,
        initiator: &str,
        request: &str,
        approved_digest: &str,
        pins: RefreshJobPins,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.admit_pair_refresh_guarded(
            initiator,
            request,
            approved_digest,
            pins,
            busy,
            AdmissionValidation::Credentials,
        )
    }

    /// Bind registered endpoints/branches and strict credentials in the same
    /// transaction creating both holds. The caller must still authenticate the
    /// actor and independently prove live remotes, correspondence and approval.
    pub fn admit_pair_refresh_with_registered_sources(
        &self,
        initiator: &str,
        request: &str,
        approved_digest: &str,
        pins: RefreshJobPins,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.admit_pair_refresh_guarded(
            initiator,
            request,
            approved_digest,
            pins,
            busy,
            AdmissionValidation::RegisteredSources,
        )
    }

    fn admit_pair_refresh_guarded(
        &self,
        initiator: &str,
        request: &str,
        approved_digest: &str,
        pins: RefreshJobPins,
        busy: &crate::busy::PairBusyGuard,
        validation: AdmissionValidation,
    ) -> Result<PairRefreshOperation, DatabaseError> {
        self.transaction(|tx| {
            let epoch = crate::writer_fence::require_owned_current(tx)?;
            pins.validate()?;
            if validation != AdmissionValidation::Storage {
                require_credentials_current(tx, &pins)?;
                require_execution_conflicts_quiet(tx, &pins)?;
            }
            if validation == AdmissionValidation::RegisteredSources {
                crate::pair_refresh::execution_configuration::capture_execution_configuration(
                    tx,
                    &pins.child.repo_id,
                )?
                .require_job_configuration(&pins)?;
            }
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
                if op.version != 2 {
                    return Err(refused("authenticated request cannot use legacy admission"));
                }
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
            require_baseline_provenance(&pins, &operations)?;
            require_pins_current(tx, &pins)?;
            for id in [&pins.child.repo_id, &pins.parent.repo_id] {
                require_no_pair_refresh_hold(tx, Some(id))?;
                require_other_journals_quiet(tx, id)?;
            }
            // Legacy config imports share global checkpoints and prepare a
            // global Git workdir before their engine worker starts.
            for id in super::personal_scope::personal_repo_ids_for_journal_read() {
                require_other_journals_quiet(tx, id)?;
            }
            for pin in [&pins.child, &pins.parent] {
                super::ordinary_writer_windows::require_quiet_for_refresh(
                    tx,
                    &pin.repo_id,
                    &pin.scope_uuid,
                )?;
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

    /// Revalidate local inputs and resolve fresh credentials before each remote
    /// observation/publication. New daemon epochs can observe a retained job;
    /// its intent phase never becomes permission to repeat an uncertain write.
    /// The caller still must check live SVN/Git evidence before the next intent.
    pub fn require_current_pair_refresh_execution(
        &self,
        id: Uuid,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<super::pair_refresh_credentials::RefreshCredentials, DatabaseError> {
        self.transaction(|tx| {
            let op = require_held_execution_inputs(tx, id, busy)?;
            require_credentials_current(tx, &op.pins)
        })
    }

    /// Resolve explicit clients only from current held registrations. This is
    /// local input proof; fresh independent remote evidence remains required.
    pub fn require_registered_pair_refresh_execution(
        &self,
        id: Uuid,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<
        crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
        DatabaseError,
    > {
        self.transaction(|tx| {
            let op = require_held_execution_inputs(tx, id, busy)?;
            let configuration =
                crate::pair_refresh::execution_configuration::capture_execution_configuration(
                    tx,
                    &op.pins.child.repo_id,
                )?;
            configuration.require_job_configuration(&op.pins)?;
            Ok(configuration)
        })
    }

    fn record_registered_refresh_intent(
        &self,
        id: Uuid,
        expected: PairRefreshPhase,
        next: PairRefreshPhase,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<
        (
            PairRefreshOperation,
            crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
        ),
        DatabaseError,
    > {
        self.transaction(|tx| {
            let mut op = require_held_execution_inputs(tx, id, busy)?;
            if op.phase != expected || op.reconciliation_detail.is_some() {
                return Err(refused(
                    "publication phase changed or requires reconciliation",
                ));
            }
            let configuration =
                crate::pair_refresh::execution_configuration::capture_execution_configuration(
                    tx,
                    &op.pins.child.repo_id,
                )?;
            configuration.require_job_configuration(&op.pins)?;
            op.phase = next;
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            Ok((op, configuration))
        })
    }

    /// Persist the first SVN intent with current local inputs and busy ownership.
    /// The caller must prove fresh remote pins before calling this method.
    pub fn record_pair_refresh_svn_intent_with_registered_sources(
        &self,
        id: Uuid,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<
        (
            PairRefreshOperation,
            crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
        ),
        DatabaseError,
    > {
        self.record_registered_refresh_intent(
            id,
            PairRefreshPhase::Prepared,
            PairRefreshPhase::SvnIntent,
            busy,
        )
    }

    /// Persist the first Git intent after independent SVN receipt observation.
    /// An existing intent or reconciliation hold never authorizes a retry.
    pub fn record_pair_refresh_git_intent_with_registered_sources(
        &self,
        id: Uuid,
        busy: &crate::busy::PairBusyGuard,
    ) -> Result<
        (
            PairRefreshOperation,
            crate::pair_refresh::execution_configuration::LiveExecutionConfiguration,
        ),
        DatabaseError,
    > {
        self.record_registered_refresh_intent(
            id,
            PairRefreshPhase::SvnObserved,
            PairRefreshPhase::GitIntent,
            busy,
        )
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
            let conflict: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_records WHERE (repo_id=?1 OR repo_id IS NULL OR repo_id='') AND (git_sha=?2 OR svn_rev=?3))", params![child.repo_id, sha, receipt.revision], |row| row.get(0))?;
            if conflict { return Err(refused("publication already has correspondence outside this job")); }
            let conflicting_map: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM commit_map WHERE (git_sha=?2 OR svn_rev=?3) AND ((repo_id=?1 AND NOT (git_sha=?2 AND svn_rev=?3 AND direction='git_to_svn')) OR repo_id IS NULL OR repo_id=''))", params![child.repo_id, sha, receipt.revision], |row| row.get(0))?;
            if conflicting_map { return Err(refused("publication has contradictory commit correspondence")); }
            let now = Utc::now().to_rfc3339();
            tx.execute("INSERT INTO commit_map(repo_id,svn_rev,git_sha,direction,synced_at,svn_author,git_author) SELECT ?1,?2,?3,'git_to_svn',?4,'RepoSync','RepoSync' WHERE NOT EXISTS(SELECT 1 FROM commit_map WHERE repo_id=?1 AND svn_rev=?2 AND git_sha=?3 AND direction='git_to_svn')", params![child.repo_id, receipt.revision, sha, now])?;
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
