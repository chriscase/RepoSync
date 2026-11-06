//! Durable v12 operation journal for #67 late-pair publish (SVN copy + replay).
//! Documents live in `kv_state`; schema stays v12.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Database;
use crate::errors::DatabaseError;

const PREFIX: &str = "late_pair_publish_v1:";
const MAX_RECONCILIATION_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatePairPublishState {
    Queued,
    /// SVN copy intent persisted; copy may be in flight or complete but not yet journaled.
    SvnCopyPending,
    SvnCopied,
    ChildRegistered,
    ReplayInProgress,
    Completed,
    Failed,
    ReconciliationRequired,
    /// V2 uncertainty still owns the parent and blocks ordinary scheduling.
    ReconciliationHeld,
    Cancelled,
}

impl LatePairPublishState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::ReconciliationRequired | Self::Cancelled
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatePairPublishOperation {
    pub version: u8,
    pub id: String,
    /// Parent repository that owns the pair request.
    pub parent_repo_id: String,
    pub child_repo_id: Option<String>,
    pub operation_type: String,
    pub initiator_id: String,
    pub request_id: String,
    pub target_fingerprint: String,
    pub created_at: String,
    pub updated_at: String,
    pub state: LatePairPublishState,
    pub policy_version: String,
    pub git_branch: String,
    pub svn_branch: String,
    pub pinned_git_tip: String,
    pub baseline_git_sha: String,
    pub baseline_svn_rev: i64,
    pub svn_copy_source_rev: i64,
    #[serde(default)]
    pub svn_copy_source_path: Option<String>,
    #[serde(default)]
    pub svn_branch_head_rev: Option<i64>,
    #[serde(default)]
    pub replayed_git_shas: Vec<String>,
    pub outcome_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation: Option<Box<ReconciliationProgress>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationPhase {
    Prepared,
    CopyPending,
    SourceReady,
    Applying,
    Publishing,
    Checkpointing,
    Completed,
    Cancelled,
    Held,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopyEvidence {
    pub revision: i64,
    pub uuid: String,
    pub source_path: String,
    pub source_revision: i64,
    pub target_path: String,
    pub tree_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationStepReceipt {
    pub index: usize,
    pub svn_revision: i64,
    pub tree_digest: String,
    /// An actual completed #64 producer document, not a caller completion flag.
    pub svn_operation: Option<super::svn_commit_operations::SvnCommitOperation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Cancel,
    Continue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationControl {
    pub kind: ControlKind,
    pub actor: String,
    pub review_pin: String,
    pub request_id: String,
    pub recorded_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationProgress {
    pub plan: crate::late_pair::reconciliation::PreparedPlan,
    pub reserved_bytes: usize,
    pub child_scope_uuid: Option<String>,
    pub phase: ReconciliationPhase,
    pub copy_evidence: Option<CopyEvidence>,
    pub svn_revision: Option<i64>,
    pub receipts: Vec<ReconciliationStepReceipt>,
    pub pending_svn_operation: Option<String>,
    pub git_push_operation: Option<String>,
    pub publication: Option<super::git_push_operations::GitPushOperation>,
    pub checkpoint_verified: bool,
    pub cancel_requested: bool,
    pub controls: Vec<ReconciliationControl>,
}

fn invalid(detail: &str) -> DatabaseError {
    DatabaseError::Other(format!(
        "invalid late-pair reconciliation evidence: {detail}"
    ))
}

fn reserve_reconciliation_bytes(
    plan: &crate::late_pair::reconciliation::PreparedPlan,
) -> Result<usize, DatabaseError> {
    // Precharge all copies of variable message/path/endpoint data plus a
    // bounded envelope for each producer and all 32 actor-control records.
    // Tree bytes are retained in the plan; producer receipts contain hashes.
    let encode = |value: &serde_json::Value| {
        serde_json::to_vec(value)
            .map(|v| v.len())
            .map_err(|_| invalid("capacity encoding"))
    };
    let mut total = serde_json::to_vec(plan)
        .map_err(|_| invalid("plan encoding"))?
        .len()
        .checked_add(512 * 1024)
        .ok_or_else(|| invalid("capacity overflow"))?;
    let endpoint = encode(&serde_json::json!([
        plan.svn_root_url,
        plan.target_url,
        plan.target_path,
        plan.git_remote_url,
        plan.actor
    ]))?;
    let mut before = &plan.initial_target_tree;
    for step in &plan.steps {
        let paths =
            crate::late_pair::reconciliation::producer_changed_paths(before, &step.intended_tree);
        let variable = serde_json::to_vec(&(paths, &step.message))
            .map_err(|_| invalid("producer capacity encoding"))?
            .len();
        total = total
            .checked_add(variable)
            .and_then(|n| n.checked_add(endpoint.checked_mul(2)?))
            .and_then(|n| n.checked_add(64 * 1024))
            .ok_or_else(|| invalid("producer capacity overflow"))?;
        before = &step.intended_tree;
    }
    total = total
        .checked_add(endpoint)
        .and_then(|n| n.checked_add(64 * 1024))
        .ok_or_else(|| invalid("publication capacity overflow"))?;
    if total > MAX_RECONCILIATION_DOCUMENT_BYTES {
        return Err(invalid(
            "complete plan and future receipts exceed durable record capacity",
        ));
    }
    Ok(total)
}

fn validate_reconciliation(op: &LatePairPublishOperation) -> Result<(), DatabaseError> {
    use crate::late_pair::reconciliation::{
        producer_changed_paths, producer_tree_digest, tree_digest, POLICY,
    };
    let progress = op
        .reconciliation
        .as_deref()
        .ok_or_else(|| invalid("v2 payload absent"))?;
    let plan = &progress.plan;
    if Uuid::parse_str(&op.id).is_err()
        || op.request_id.is_empty()
        || op.request_id.len() > 256
        || chrono::DateTime::parse_from_rfc3339(&op.created_at).is_err()
        || chrono::DateTime::parse_from_rfc3339(&op.updated_at).is_err()
        || progress
            .child_scope_uuid
            .as_ref()
            .is_some_and(|scope| Uuid::parse_str(scope).is_err())
    {
        return Err(invalid("operation or child incarnation identity malformed"));
    }
    if progress.reserved_bytes != reserve_reconciliation_bytes(plan)? {
        return Err(invalid("evidence capacity reservation changed"));
    }
    plan.validate()
        .map_err(|_| invalid("plan digest or source/result chain"))?;
    if progress.controls.len() > 32 {
        return Err(invalid("control history bound"));
    }
    let mut cancelled = false;
    let mut requests = std::collections::BTreeSet::new();
    for control in &progress.controls {
        if control.actor != op.initiator_id
            || control.review_pin != plan.review_pin
            || control.request_id.is_empty()
            || control.request_id.len() > 256
            || !requests.insert(&control.request_id)
            || chrono::DateTime::parse_from_rfc3339(&control.recorded_at).is_err()
        {
            return Err(invalid("control history identity or encoding"));
        }
        match control.kind {
            ControlKind::Cancel if !cancelled => cancelled = true,
            ControlKind::Continue if cancelled => cancelled = false,
            _ => {
                return Err(invalid(
                    "control history is not an ordered cancel/continue sequence",
                ))
            }
        }
    }
    if cancelled != progress.cancel_requested {
        return Err(invalid(
            "cancellation differs from retained control history",
        ));
    }
    if op.policy_version != POLICY
        || op.parent_repo_id != plan.parent_id
        || op.initiator_id != plan.actor
        || op.git_branch != plan.git_branch
        || op.svn_branch != plan.svn_branch
        || op.pinned_git_tip != plan.git_tip
        || op.baseline_git_sha != plan.baseline_git_sha
        || op.baseline_svn_rev != plan.baseline_svn_revision
        || op.target_fingerprint != plan.review_pin
    {
        return Err(invalid("operation and plan identities differ"));
    }
    let replayed = progress
        .receipts
        .iter()
        .filter_map(|receipt| plan.steps.get(receipt.index))
        .filter_map(|step| match &step.source {
            crate::late_pair::reconciliation::StepSource::Git { sha, .. } => Some(sha.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if op.svn_copy_source_rev != plan.baseline_svn_revision
        || op.svn_copy_source_path.as_deref() != Some(plan.source_path.as_str())
        || op.svn_branch_head_rev != progress.svn_revision
        || op.replayed_git_shas != replayed
        || (progress.child_scope_uuid.is_some() && op.child_repo_id.is_none())
    {
        return Err(invalid("outer frontier differs from retained v2 evidence"));
    }
    let initial_digest =
        tree_digest(&plan.initial_target_tree).map_err(|_| invalid("initial tree"))?;
    let initial_revision = match (&plan.target_revision, &progress.copy_evidence) {
        (Some(revision), None) => Some(*revision),
        (None, Some(copy))
            if copy.uuid == plan.svn_uuid
                && copy.source_path == plan.source_path
                && copy.source_revision == plan.baseline_svn_revision
                && copy.target_path == plan.target_path
                && copy.tree_digest == initial_digest
                && copy.revision > plan.baseline_svn_revision =>
        {
            Some(copy.revision)
        }
        (None, None) => None,
        _ => return Err(invalid("copy proof contradicts the planned target")),
    };
    if progress.receipts.len() > plan.steps.len() {
        return Err(invalid("too many replay receipts"));
    }
    if progress.publication.is_some()
        && (progress.receipts.len() != plan.steps.len() || progress.pending_svn_operation.is_some())
        || progress.git_push_operation.is_some() && progress.receipts.len() != plan.steps.len()
        || progress.pending_svn_operation.is_some()
            && (progress.receipts.len() == plan.steps.len() || progress.child_scope_uuid.is_none())
    {
        return Err(invalid(
            "publication or pending intent precedes its required frontier",
        ));
    }
    let mut previous = initial_revision;
    let mut previous_tree = &plan.initial_target_tree;
    for (index, receipt) in progress.receipts.iter().enumerate() {
        let step = &plan.steps[index];
        if receipt.index != index || receipt.tree_digest != step.intended_digest {
            return Err(invalid("replay receipt order or tree"));
        }
        match &receipt.svn_operation {
            Some(producer) => {
                if step.before_digest == step.intended_digest
                    || producer.version != 1
                    || producer.operation_type != "git_to_svn_commit"
                    || producer.state
                        != super::svn_commit_operations::SvnCommitOperationState::Completed
                    || Some(&producer.repo_id) != op.child_repo_id.as_ref()
                    || producer.source_git_sha != step.git_result.sha
                    || producer.source_git_parent != step.git_result.parent
                    || producer.source_git_tree != step.git_result.tree
                    || producer.projection != POLICY
                    || producer.initiator_id != op.initiator_id
                    || producer.request_id != format!("{}:{index}", op.id)
                    || producer.resume_authorized
                    || producer.pre_write_svn_tree != producer_tree_digest(previous_tree)
                    || producer.intended_svn_tree != producer_tree_digest(&step.intended_tree)
                    || producer.intended_changed_paths
                        != producer_changed_paths(previous_tree, &step.intended_tree)
                    || producer.source_message != step.message
                    || producer.target_fingerprint
                        != super::svn_commit_operations::svn_commit_target_fingerprint(
                            op.child_repo_id.as_deref().unwrap_or(""),
                            &plan.svn_uuid,
                            &plan.target_url,
                            &plan.target_path,
                            POLICY,
                        )
                    || producer.target_svn_uuid != plan.svn_uuid
                    || producer.target_svn_path != plan.target_url
                    || producer.target_svn_root_url != plan.svn_root_url
                    || producer.target_svn_branch_path != plan.target_path
                    || Some(producer.pre_write_svn_rev) != previous
                    || producer.last_confirmed_svn_rev != Some(receipt.svn_revision)
                    || producer.last_confirmed_svn_tree.as_deref()
                        != Some(producer.intended_svn_tree.as_str())
                    || previous.is_none_or(|revision| receipt.svn_revision <= revision)
                {
                    return Err(invalid("SVN producer does not prove the planned replay"));
                }
            }
            None if step.before_digest == step.intended_digest
                && Some(receipt.svn_revision) == previous => {}
            _ => return Err(invalid("missing producer for a changed replay step")),
        }
        previous = Some(receipt.svn_revision);
        previous_tree = &step.intended_tree;
    }
    if progress.svn_revision != previous {
        return Err(invalid("handled SVN revision differs from receipts"));
    }
    if let Some(publication) = &progress.publication {
        if publication.version != 1
            || publication.operation_type != "svn_to_git_push"
            || publication.state != super::git_push_operations::GitPushOperationState::Completed
            || Some(&publication.repo_id) != op.child_repo_id.as_ref()
            || Some(publication.id.as_str()) != progress.git_push_operation.as_deref()
            || Some(publication.source_svn_rev) != previous
            || publication.pre_push_git_sha != plan.git_tip
            || publication.pre_push_git_branch != plan.git_branch
            || publication.pre_push_git_remote != plan.git_remote_url
            || publication.initiator_id != op.initiator_id
            || publication.request_id != op.id
            || publication.resume_authorized
            || publication.target_fingerprint
                != super::git_push_operations::git_push_target_fingerprint(
                    op.child_repo_id.as_deref().unwrap_or(""),
                    &plan.git_remote_url,
                    &plan.git_branch,
                )
            || Some(publication.intended_local_git_sha.as_str()) != plan.intended_git_sha.as_deref()
            || publication.last_confirmed_git_sha != plan.intended_git_sha
            || Some(publication.intended_local_git_tree.as_str())
                != plan.intended_git_tree.as_deref()
            || publication.last_confirmed_git_tree != plan.intended_git_tree
        {
            return Err(invalid("Git producer does not prove intended publication"));
        }
    }
    let expected = match progress.phase {
        ReconciliationPhase::Prepared => LatePairPublishState::Queued,
        ReconciliationPhase::CopyPending => LatePairPublishState::SvnCopyPending,
        ReconciliationPhase::Completed => LatePairPublishState::Completed,
        ReconciliationPhase::Cancelled => LatePairPublishState::Cancelled,
        ReconciliationPhase::Held => LatePairPublishState::ReconciliationHeld,
        _ => LatePairPublishState::ReplayInProgress,
    };
    if op.state != expected {
        return Err(invalid("phase/state blending"));
    }
    match progress.phase {
        ReconciliationPhase::Prepared
        | ReconciliationPhase::CopyPending
        | ReconciliationPhase::Cancelled => {
            if progress.copy_evidence.is_some()
                || op.child_repo_id.is_some()
                || !progress.receipts.is_empty()
                || progress.pending_svn_operation.is_some()
                || progress.git_push_operation.is_some()
                || progress.publication.is_some()
                || (progress.phase == ReconciliationPhase::CopyPending
                    && plan.target_revision.is_some())
            {
                return Err(invalid("pre-effect phase contains effect evidence"));
            }
        }
        ReconciliationPhase::SourceReady => {
            if initial_revision.is_none()
                || !progress.receipts.is_empty()
                || progress.pending_svn_operation.is_some()
                || progress.git_push_operation.is_some()
                || progress.publication.is_some()
            {
                return Err(invalid("invalid initial source phase"));
            }
        }
        ReconciliationPhase::Applying
        | ReconciliationPhase::Publishing
        | ReconciliationPhase::Checkpointing
        | ReconciliationPhase::Completed => {
            if initial_revision.is_none()
                || op.child_repo_id.is_none()
                || progress.child_scope_uuid.is_none()
            {
                return Err(invalid(
                    "application lacks registered source and child custody",
                ));
            }
            if matches!(
                progress.phase,
                ReconciliationPhase::Publishing
                    | ReconciliationPhase::Checkpointing
                    | ReconciliationPhase::Completed
            ) && (progress.receipts.len() != plan.steps.len()
                || progress.pending_svn_operation.is_some())
            {
                return Err(invalid("publication phase lacks complete replay"));
            }
            if progress.phase == ReconciliationPhase::Checkpointing
                && progress.publication.is_none()
            {
                return Err(invalid("checkpointing precedes native publication proof"));
            }
        }
        ReconciliationPhase::Held => {}
    }
    if progress.phase == ReconciliationPhase::Completed {
        if !plan.conflicts.is_empty()
            || progress.receipts.len() != plan.steps.len()
            || initial_revision.is_none()
            || op.child_repo_id.is_none()
            || progress.child_scope_uuid.is_none()
            || progress.pending_svn_operation.is_some()
            || progress.publication.is_none()
            || !progress.checkpoint_verified
            || progress.cancel_requested
        {
            return Err(invalid("completion without all producer/checkpoint proof"));
        }
    } else if progress.checkpoint_verified {
        return Err(invalid("checkpoint claimed before completion"));
    }
    Ok(())
}

fn key(kind: &str, id: &str) -> String {
    format!("{PREFIX}{kind}:{id}")
}

fn read_value(tx: &Connection, name: &str) -> Result<Option<String>, DatabaseError> {
    Ok(tx
        .query_row("SELECT value FROM kv_state WHERE key=?1", [name], |r| {
            r.get(0)
        })
        .optional()?)
}

fn write_value(tx: &Connection, name: &str, value: &str) -> Result<(), DatabaseError> {
    tx.execute(
        "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        params![name, value, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn parse(raw: &str) -> Result<LatePairPublishOperation, DatabaseError> {
    let op: LatePairPublishOperation = serde_json::from_str(raw)
        .map_err(|e| DatabaseError::Other(format!("invalid late-pair publish operation: {e}")))?;
    if !matches!(op.version, 1 | 2) {
        return Err(DatabaseError::Other(
            "unsupported late-pair publish operation version".into(),
        ));
    }
    if op.operation_type != "late_pair_publish" {
        return Err(DatabaseError::Other(
            "unsupported late-pair publish operation type".into(),
        ));
    }
    if op.version == 1 {
        if op.reconciliation.is_some()
            || op.policy_version == crate::late_pair::reconciliation::POLICY
            || matches!(
                op.state,
                LatePairPublishState::Cancelled | LatePairPublishState::ReconciliationHeld
            )
        {
            return Err(invalid("v1/v2 policy blending"));
        }
    } else {
        if raw.len() > MAX_RECONCILIATION_DOCUMENT_BYTES {
            return Err(invalid("journal document exceeds capacity"));
        }
        if serde_json::to_string(&op).map_err(|_| invalid("encoding"))? != raw {
            return Err(invalid("noncanonical or unknown v2 fields"));
        }
        validate_reconciliation(&op)?;
    }
    Ok(op)
}

/// Exact legacy admission plus explicit, coherent v2 completion. The caller
/// must retain its separate active-pointer refusal before invoking this.
pub fn completed_for_foreign_journal(
    raw: &str,
    id: &str,
    parent_id: &str,
) -> Result<bool, DatabaseError> {
    let op = parse(raw)?;
    Ok(op.id == id
        && op.parent_repo_id == parent_id
        && op.operation_type == "late_pair_publish"
        && op.state == LatePairPublishState::Completed)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReconciliationFrontier {
    version: u8,
    operation_id: String,
    document_sha256: String,
    repo_id: String,
    scope_uuid: String,
    generation: i64,
    projection: String,
    git_sha: String,
    svn_revision: i64,
}

fn frontier_key(scope: &str, sha: &str) -> String {
    format!("late_pair_reconciliation_frontier_v2:{scope}:{sha}")
}

/// Read-only family exclusion for an existing ordinary mutation entry point.
/// It does not clear, renew or otherwise acquire any operation ownership.
pub fn require_no_reconciliation_hold(tx: &Connection, repo_id: &str) -> Result<(), String> {
    require_no_reconciliation_hold_inner(tx, repo_id)
        .map_err(|error| crate::errors::redact_vcs_error_detail(&error.to_string()))
}

fn require_no_reconciliation_hold_inner(
    tx: &Connection,
    repo_id: &str,
) -> Result<(), DatabaseError> {
    let parent: Option<String> = tx
        .query_row(
            "SELECT parent_id FROM repositories WHERE id=?1",
            [repo_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    for owner in std::iter::once(repo_id).chain(parent.as_deref()) {
        for pointer in ["active", "latest"] {
            if let Some(id) = read_value(tx, &key(pointer, owner))? {
                let raw = read_value(tx, &key("document", &id))?
                    .ok_or_else(|| invalid("late-pair pointer document absent"))?;
                let operation = parse(&raw)?;
                if operation.id != id || operation.parent_repo_id != owner {
                    return Err(invalid("late-pair pointer ownership mismatch"));
                }
                if operation.version == 2
                    && (owner == repo_id || operation.child_repo_id.as_deref() == Some(repo_id))
                    && (pointer == "active"
                        || !matches!(
                            operation.state,
                            LatePairPublishState::Completed | LatePairPublishState::Cancelled
                        ))
                {
                    return Err(invalid(
                        "late-pair reconciliation owns this scope; ordinary mutation is held",
                    ));
                }
            }
        }
    }
    Ok(())
}

// Validate every retained document before deciding whether this SHA requires
// reconciliation proof, including when an older repository has no scope UUID.
fn requires_reconciliation_frontier(
    tx: &Connection,
    repo_id: &str,
    sha: &str,
) -> Result<bool, DatabaseError> {
    let mut query = tx.prepare("SELECT value FROM kv_state WHERE substr(key,1,?1)=?2")?;
    let prefix = key("document", "");
    let documents =
        query.query_map(params![prefix.len(), prefix], |row| row.get::<_, String>(0))?;
    let mut required = false;
    for raw in documents {
        let op = parse(&raw?)?;
        if op.version == 2
            && op.child_repo_id.as_deref() == Some(repo_id)
            && op
                .reconciliation
                .as_deref()
                .is_some_and(|p| p.plan.intended_git_sha.as_deref() == Some(sha))
        {
            required = true;
        }
    }
    Ok(required)
}

/// Read-only authority for already-covered Git work. This grants no lease,
/// effect, approval or recovery permission. Missing/changed retained evidence
/// refuses rather than reclassifying a reconciliation as an old import.
pub fn verified_reconciliation_frontier(
    tx: &Connection,
    repo_id: &str,
    sha: &str,
    projection: &str,
) -> Result<bool, DatabaseError> {
    let scope = match super::repo_scope_identity::repository_scope_uuid(tx, repo_id) {
        Ok(scope) => scope,
        Err(_) if !requires_reconciliation_frontier(tx, repo_id, sha)? => return Ok(false),
        Err(_) => return Err(invalid("claimed reconciliation has no valid current scope")),
    };
    let raw = read_value(tx, &frontier_key(&scope, sha))?;
    let Some(raw) = raw else {
        if requires_reconciliation_frontier(tx, repo_id, sha)? {
            return Err(invalid("required reconciliation frontier receipt missing"));
        }
        return Ok(false);
    };
    let proof: ReconciliationFrontier =
        serde_json::from_str(&raw).map_err(|_| invalid("frontier receipt corrupt"))?;
    if serde_json::to_string(&proof).map_err(|_| invalid("frontier encoding"))? != raw
        || proof.version != 2
        || proof.repo_id != repo_id
        || proof.scope_uuid != scope
        || proof.git_sha != sha
        || proof.projection != projection
        || proof.generation != crate::echo_receipt_scope::repo_echo_generation_tx(tx, repo_id)?
    {
        return Err(invalid(
            "frontier receipt identity, generation or projection changed",
        ));
    }
    super::pair_refresh_operations::require_no_pair_refresh_hold(tx, Some(repo_id))?;
    let document = read_value(tx, &key("document", &proof.operation_id))?
        .ok_or_else(|| invalid("frontier journal missing"))?;
    if hex::encode(Sha256::digest(document.as_bytes())) != proof.document_sha256 {
        return Err(invalid("frontier journal changed"));
    }
    let op = parse(&document)?;
    let p = op
        .reconciliation
        .as_deref()
        .ok_or_else(|| invalid("frontier v2 progress absent"))?;
    if read_value(tx, &key("active", &op.parent_repo_id))?.as_deref() == Some(op.id.as_str()) {
        return Err(invalid("completed frontier still owns an active pointer"));
    }
    if op.version != 2
        || op.state != LatePairPublishState::Completed
        || op.id != proof.operation_id
        || op.child_repo_id.as_deref() != Some(repo_id)
        || p.child_scope_uuid.as_deref() != Some(scope.as_str())
        || p.plan.projection != projection
        || p.plan.intended_git_sha.as_deref() != Some(sha)
        || p.svn_revision != Some(proof.svn_revision)
    {
        return Err(invalid("frontier completion evidence differs"));
    }
    let (parent,branch,svn_branch,allowed,blocked,current_revision):(Option<String>,String,String,Option<String>,Option<String>,i64)=tx.query_row(
        "SELECT parent_id,git_branch,svn_branch,allowed_paths,blocked_patterns,last_svn_rev FROM repositories WHERE id=?1",[repo_id],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    let allowed: Vec<String> = allowed
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| invalid("current projection corrupt"))?
        .unwrap_or_default();
    let blocked: Vec<String> = blocked
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| invalid("current projection corrupt"))?
        .unwrap_or_default();
    let current_projection =
        serde_json::json!({"allowed_paths":allowed,"blocked_patterns":blocked}).to_string();
    if parent.as_deref() != Some(op.parent_repo_id.as_str())
        || branch != p.plan.git_branch
        || svn_branch != p.plan.svn_branch
        || super::repo_scope_identity::repository_scope_uuid(tx, &op.parent_repo_id)?
            != p.plan.parent_scope_uuid
        || current_projection != projection
        || current_revision < proof.svn_revision
    {
        return Err(invalid(
            "frontier registration or current SVN coordinate changed",
        ));
    }
    for receipt in &p.receipts {
        if let Some(producer) = &receipt.svn_operation {
            let raw = read_value(
                tx,
                &format!("git_to_svn_commit_v1:document:{}", producer.id),
            )?
            .ok_or_else(|| invalid("retained replay producer missing"))?;
            let actual: super::svn_commit_operations::SvnCommitOperation =
                serde_json::from_str(&raw)
                    .map_err(|_| invalid("retained replay producer corrupt"))?;
            if actual != *producer {
                return Err(invalid("retained replay producer changed"));
            }
            let (count,exact):(i64,i64)=tx.query_row(
                "SELECT count(*),coalesce(sum(repo_id=?1 AND svn_rev=?3 AND status='applied'),0) FROM sync_records WHERE (repo_id=?1 OR repo_id IS NULL OR repo_id='') AND git_sha=?2 AND direction='git_to_svn'",
                params![repo_id,producer.source_git_sha,receipt.svn_revision],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if count != 1 || exact != 1 {
                return Err(invalid(
                    "replay correspondence is missing, duplicate or contradictory",
                ));
            }
        }
    }
    let publication = p
        .publication
        .as_ref()
        .ok_or_else(|| invalid("publication absent"))?;
    let raw = read_value(
        tx,
        &format!("svn_to_git_push_v1:document:{}", publication.id),
    )?
    .ok_or_else(|| invalid("retained publication missing"))?;
    let actual: super::git_push_operations::GitPushOperation =
        serde_json::from_str(&raw).map_err(|_| invalid("retained publication corrupt"))?;
    if actual != *publication {
        return Err(invalid("retained publication changed"));
    }
    let (count,exact):(i64,i64)=tx.query_row(
        "SELECT count(*),coalesce(sum(svn_rev=?3 AND status='applied'),0) FROM sync_records WHERE repo_id=?1 AND git_sha=?2 AND direction='svn_to_git'",
        params![repo_id,sha,proof.svn_revision],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if count != 1 || exact != 1 {
        return Err(invalid(
            "publication correspondence is missing, duplicate or contradictory",
        ));
    }
    Ok(true)
}

fn write_op(tx: &Connection, op: &LatePairPublishOperation) -> Result<(), DatabaseError> {
    if op.version == 2 {
        validate_reconciliation(op)?;
    }
    if let Some(raw) = read_value(tx, &key("document", &op.id))? {
        let stored = parse(&raw)?;
        super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(
            tx,
            &stored.parent_repo_id,
        )?;
        if let Some(child) = &stored.child_repo_id {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, child)?;
        }
        if stored.id != op.id
            || stored.parent_repo_id != op.parent_repo_id
            || stored.version != op.version
        {
            return Err(DatabaseError::Other(
                "late-pair operation ownership changed".into(),
            ));
        }
    }
    let active_prefix = key("active", "");
    let mut query =
        tx.prepare("SELECT key FROM kv_state WHERE substr(key,1,?1)=?2 AND value=?3")?;
    let pointers = query
        .query_map(params![active_prefix.len(), active_prefix, op.id], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for pointer in pointers {
        let owner = pointer
            .strip_prefix(&active_prefix)
            .ok_or_else(|| DatabaseError::Other("invalid late-pair active pointer".into()))?;
        super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, owner)?;
        if owner != op.parent_repo_id {
            return Err(DatabaseError::Other(
                "late-pair active pointer has different ownership".into(),
            ));
        }
    }
    super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(
        tx,
        &op.parent_repo_id,
    )?;
    if let Some(child) = &op.child_repo_id {
        super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, child)?;
    }
    let encoded = serde_json::to_string(op).map_err(|e| DatabaseError::Other(e.to_string()))?;
    if let Some(progress) = op.reconciliation.as_deref() {
        if encoded.len() > progress.reserved_bytes {
            return Err(invalid(
                "producer evidence exceeded its precharged capacity",
            ));
        }
    }
    write_value(tx, &key("document", &op.id), &encoded)
}

pub fn late_pair_publish_fingerprint(
    parent_id: &str,
    git_branch: &str,
    svn_branch: &str,
    git_tip: &str,
    baseline_git_sha: &str,
    baseline_svn_rev: i64,
) -> String {
    let source = serde_json::json!({
        "parent_repo_id": parent_id,
        "git_branch": git_branch,
        "svn_branch": svn_branch,
        "pinned_git_tip": git_tip,
        "baseline_git_sha": baseline_git_sha,
        "baseline_svn_rev": baseline_svn_rev,
    })
    .to_string();
    hex::encode(Sha256::digest(source.as_bytes()))
}

impl Database {
    #[allow(clippy::too_many_arguments)]
    pub fn create_late_pair_publish_operation(
        &self,
        parent_repo_id: &str,
        initiator_id: &str,
        request_id: &str,
        fingerprint: &str,
        git_branch: &str,
        svn_branch: &str,
        pinned_git_tip: &str,
        baseline_git_sha: &str,
        baseline_svn_rev: i64,
        svn_copy_source_rev: i64,
        policy_version: &str,
    ) -> Result<LatePairPublishOperation, DatabaseError> {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(
                tx,
                parent_repo_id,
            )?;
            crate::writer_fence::require_current(tx)?;
            if super::managed_remove::new_work_blocked(tx, parent_repo_id)? {
                return Err(DatabaseError::Other(
                    "repository removal blocks a new late-pair publish".into(),
                ));
            }
            let active = key("active", parent_repo_id);
            if read_value(tx, &active)?.is_some() {
                return Err(DatabaseError::Other(
                    "parent repository has an active late-pair publish operation".into(),
                ));
            }
            let now = Utc::now().to_rfc3339();
            let op = LatePairPublishOperation {
                version: 1,
                id: Uuid::new_v4().to_string(),
                parent_repo_id: parent_repo_id.into(),
                child_repo_id: None,
                operation_type: "late_pair_publish".into(),
                initiator_id: initiator_id.into(),
                request_id: request_id.into(),
                target_fingerprint: fingerprint.into(),
                created_at: now.clone(),
                updated_at: now,
                state: LatePairPublishState::Queued,
                policy_version: policy_version.into(),
                git_branch: git_branch.into(),
                svn_branch: svn_branch.into(),
                pinned_git_tip: pinned_git_tip.into(),
                baseline_git_sha: baseline_git_sha.into(),
                baseline_svn_rev,
                svn_copy_source_rev,
                svn_copy_source_path: None,
                svn_branch_head_rev: None,
                replayed_git_shas: Vec::new(),
                outcome_detail: None,
                reconciliation: None,
            };
            write_op(tx, &op)?;
            write_value(tx, &active, &op.id)?;
            write_value(tx, &key("latest", parent_repo_id), &op.id)?;
            Ok(op)
        })
    }

    pub fn get_late_pair_publish_operation(
        &self,
        parent_repo_id: &str,
        op_id: &str,
    ) -> Result<Option<LatePairPublishOperation>, DatabaseError> {
        let conn = self.conn();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM kv_state WHERE key=?1",
                [key("document", op_id)],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|v| parse(&v)).transpose().map(|op| {
            if op
                .as_ref()
                .is_some_and(|o| o.parent_repo_id != parent_repo_id)
            {
                None
            } else {
                op
            }
        })
    }

    pub fn latest_late_pair_publish_operation(
        &self,
        parent_repo_id: &str,
    ) -> Result<Option<LatePairPublishOperation>, DatabaseError> {
        let op_id: Option<String> = {
            let conn = self.conn();
            conn.query_row(
                "SELECT value FROM kv_state WHERE key=?1",
                [key("latest", parent_repo_id)],
                |r| r.get(0),
            )
            .optional()?
        };
        match op_id {
            Some(id) => self.get_late_pair_publish_operation(parent_repo_id, &id),
            None => Ok(None),
        }
    }

    pub fn update_late_pair_publish_operation(
        &self,
        op: LatePairPublishOperation,
    ) -> Result<LatePairPublishOperation, DatabaseError> {
        self.transaction(|tx| {
            crate::writer_fence::require_current(tx)?;
            if op.version != 1 {
                return Err(invalid("v2 operation requires compare-and-swap transition"));
            }
            let mut op = op;
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            Ok(op)
        })
    }

    pub fn finalize_late_pair_publish_operation(
        &self,
        parent_repo_id: &str,
        op_id: &str,
    ) -> Result<LatePairPublishOperation, DatabaseError> {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(
                tx,
                parent_repo_id,
            )?;
            crate::writer_fence::require_current(tx)?;
            finalize_late_pair_publish_op_in_tx(tx, parent_repo_id, op_id, None)
        })
    }

    /// Atomically mark the journal completed and enable the child (crash-safe publish finish).
    pub fn finalize_late_pair_publish_enabling_child(
        &self,
        parent_repo_id: &str,
        op_id: &str,
        child_repo_id: &str,
        git_tip: &str,
    ) -> Result<LatePairPublishOperation, DatabaseError> {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(
                tx,
                parent_repo_id,
            )?;
            crate::writer_fence::require_current(tx)?;
            finalize_late_pair_publish_op_in_tx(
                tx,
                parent_repo_id,
                op_id,
                Some((child_repo_id, git_tip)),
            )
        })
    }
}

impl Database {
    /// Claim a freshly reviewed plan under the same IMMEDIATE transaction as
    /// the parent-generation comparison and active-family reservation.
    pub(crate) fn claim_late_pair_reconciliation(
        &self,
        plan: crate::late_pair::reconciliation::PreparedPlan,
        request_id: &str,
    ) -> Result<LatePairPublishOperation, DatabaseError> {
        plan.validate().map_err(|_| invalid("invalid claim plan"))?;
        let reserved_bytes = reserve_reconciliation_bytes(&plan)?;
        if !plan.conflicts.is_empty() || request_id.is_empty() || request_id.len() > 256 {
            return Err(invalid("blocked plan or invalid request identity"));
        }
        self.immediate_transaction(|tx| {
            crate::writer_fence::require_current(tx)?;
            let scope = super::repo_scope_identity::repository_scope_uuid(tx, &plan.parent_id)?;
            super::ordinary_writer_windows::require_quiet_for_refresh(tx, &plan.parent_id, &scope)?;
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(
                tx,
                &plan.parent_id,
            )?;
            if super::managed_remove::new_work_blocked(tx, &plan.parent_id)? {
                return Err(invalid("parent removal blocks new work"));
            }
            if crate::late_pair::reconciliation::parent_generation_in_connection(
                tx,
                &plan.parent_id,
            )
            .map_err(|_| invalid("parent generation unavailable"))?
                != plan.parent_generation
            {
                return Err(invalid("parent or credential generation changed"));
            }
            if read_value(tx, &key("active", &plan.parent_id))?.is_some() {
                return Err(invalid(
                    "an active late-pair operation already owns this parent",
                ));
            }
            let now = Utc::now().to_rfc3339();
            let op = LatePairPublishOperation {
                version: 2,
                id: Uuid::new_v4().to_string(),
                parent_repo_id: plan.parent_id.clone(),
                child_repo_id: None,
                operation_type: "late_pair_publish".into(),
                initiator_id: plan.actor.clone(),
                request_id: request_id.into(),
                target_fingerprint: plan.review_pin.clone(),
                created_at: now.clone(),
                updated_at: now,
                state: LatePairPublishState::Queued,
                policy_version: plan.policy.clone(),
                git_branch: plan.git_branch.clone(),
                svn_branch: plan.svn_branch.clone(),
                pinned_git_tip: plan.git_tip.clone(),
                baseline_git_sha: plan.baseline_git_sha.clone(),
                baseline_svn_rev: plan.baseline_svn_revision,
                svn_copy_source_rev: plan.baseline_svn_revision,
                svn_copy_source_path: Some(plan.source_path.clone()),
                svn_branch_head_rev: plan.target_revision,
                replayed_git_shas: Vec::new(),
                outcome_detail: None,
                reconciliation: Some(Box::new(ReconciliationProgress {
                    svn_revision: plan.target_revision,
                    plan,
                    reserved_bytes,
                    child_scope_uuid: None,
                    phase: ReconciliationPhase::Prepared,
                    copy_evidence: None,
                    receipts: Vec::new(),
                    pending_svn_operation: None,
                    git_push_operation: None,
                    publication: None,
                    checkpoint_verified: false,
                    cancel_requested: false,
                    controls: Vec::new(),
                })),
            };
            write_op(tx, &op)?;
            write_value(tx, &key("active", &op.parent_repo_id), &op.id)?;
            write_value(tx, &key("latest", &op.parent_repo_id), &op.id)?;
            Ok(op)
        })
    }

    /// Internal producer transition. The complete previous document is the
    /// CAS token; copied #64 receipts must equal their real durable documents.
    pub(crate) fn transition_late_pair_reconciliation(
        &self,
        expected: &LatePairPublishOperation,
        mut next: LatePairPublishOperation,
    ) -> Result<LatePairPublishOperation, DatabaseError> {
        self.immediate_transaction(|tx| {
            crate::writer_fence::require_current(tx)?;
            if expected.version!=2 || next.version!=2 {return Err(invalid("non-v2 transition"));}
            let raw=read_value(tx,&key("document",&expected.id))?.ok_or_else(||invalid("operation absent"))?;
            if parse(&raw)?!=*expected {return Err(invalid("stale operation transition"));}
            if read_value(tx,&key("active",&expected.parent_repo_id))?.as_deref()!=Some(expected.id.as_str()) {
                return Err(invalid("operation no longer owns the active reservation"));
            }
            let prior=expected.reconciliation.as_deref().ok_or_else(||invalid("missing prior progress"))?;
            let progress=next.reconciliation.as_deref().ok_or_else(||invalid("missing next progress"))?;
            let mut identity=next.clone();
            identity.updated_at=expected.updated_at.clone();identity.state=expected.state.clone();
            identity.child_repo_id=expected.child_repo_id.clone();identity.svn_branch_head_rev=expected.svn_branch_head_rev;
            identity.replayed_git_shas=expected.replayed_git_shas.clone();identity.outcome_detail=expected.outcome_detail.clone();
            identity.reconciliation=expected.reconciliation.clone();
            if identity!=*expected || progress.plan!=prior.plan
                || prior.phase==ReconciliationPhase::Completed || prior.phase==ReconciliationPhase::Cancelled
                || (expected.child_repo_id.is_some() && next.child_repo_id!=expected.child_repo_id)
                || (prior.child_scope_uuid.is_some() && progress.child_scope_uuid!=prior.child_scope_uuid)
                || (prior.copy_evidence.is_some() && progress.copy_evidence!=prior.copy_evidence)
                || !progress.receipts.starts_with(&prior.receipts)
                || (prior.publication.is_some() && progress.publication!=prior.publication)
                || !progress.controls.starts_with(&prior.controls) || progress.controls.len()>prior.controls.len()+1 {
                return Err(invalid("immutable identity or retained evidence changed"));
            }
            validate_reconciliation(&next)?;
            if let Some(scope)=&progress.child_scope_uuid {
                let child=next.child_repo_id.as_deref().ok_or_else(||invalid("scope without child"))?;
                if super::repo_scope_identity::repository_scope_uuid(tx,child)?!=*scope {
                    return Err(invalid("child registration incarnation changed"));
                }
            }
            for receipt in &progress.receipts {
                if let Some(producer)=&receipt.svn_operation {
                    let raw=read_value(tx,&format!("git_to_svn_commit_v1:document:{}",producer.id))?
                        .ok_or_else(||invalid("SVN producer document absent"))?;
                    let actual:super::svn_commit_operations::SvnCommitOperation=serde_json::from_str(&raw)
                        .map_err(|_|invalid("SVN producer document malformed"))?;
                    if &actual!=producer {return Err(invalid("SVN receipt differs from durable producer"));}
                }
            }
            if let Some(producer)=&progress.publication {
                let raw=read_value(tx,&format!("svn_to_git_push_v1:document:{}",producer.id))?
                    .ok_or_else(||invalid("Git producer document absent"))?;
                let actual:super::git_push_operations::GitPushOperation=serde_json::from_str(&raw)
                    .map_err(|_|invalid("Git producer document malformed"))?;
                if &actual!=producer {return Err(invalid("Git receipt differs from durable producer"));}
            }
            if next.state==LatePairPublishState::Cancelled
                && (prior.phase!=ReconciliationPhase::Prepared || prior.copy_evidence.is_some()
                    || prior.pending_svn_operation.is_some() || expected.child_repo_id.is_some()) {
                return Err(invalid("effects may have started; cancellation must retain ownership"));
            }
            if next.state==LatePairPublishState::Completed {
                let child=next.child_repo_id.as_deref().ok_or_else(||invalid("child absent"))?;
                super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx,child)?;
                if super::managed_remove::new_work_blocked(tx,child)? {return Err(invalid("child removal blocks finalization"));}
                let row:Option<(String,String,String,i64,String,i64)>=tx.query_row(
                    "SELECT parent_id,git_branch,svn_branch,last_svn_rev,last_git_sha,enabled FROM repositories WHERE id=?1",
                    [child],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
                let plan=&progress.plan;
                if crate::late_pair::reconciliation::parent_generation_in_connection(tx,&plan.parent_id)
                    .map_err(|_|invalid("final parent generation unavailable"))?!=plan.parent_generation {
                    return Err(invalid("parent changed before atomic completion"));
                }
                if !matches!(row,Some((ref parent,ref git,ref svn,revision,ref sha,0))
                    if parent==&plan.parent_id && git==&plan.git_branch && svn==&plan.svn_branch
                    && Some(revision)==progress.svn_revision && Some(sha)==plan.intended_git_sha.as_ref()) {
                    return Err(invalid("current child checkpoints do not prove completed publication"));
                }
                tx.execute("UPDATE repositories SET enabled=1,sync_status='idle',updated_at=?1 WHERE id=?2 AND enabled=0",
                    params![Utc::now().to_rfc3339(),child])?;
            }
            next.updated_at=Utc::now().to_rfc3339();
            write_op(tx,&next)?;
            if matches!(next.state,LatePairPublishState::Completed|LatePairPublishState::Cancelled) {
                tx.execute("DELETE FROM kv_state WHERE key=?1 AND value=?2",params![key("active",&next.parent_repo_id),next.id])?;
            }
            if next.state==LatePairPublishState::Completed {
                let p=next.reconciliation.as_deref().unwrap();
                let child=next.child_repo_id.as_deref().unwrap();
                let scope=p.child_scope_uuid.as_deref().unwrap();
                let sha=p.plan.intended_git_sha.as_deref().unwrap();
                let proof=ReconciliationFrontier {version:2,operation_id:next.id.clone(),
                    document_sha256:hex::encode(Sha256::digest(serde_json::to_vec(&next).map_err(|_|invalid("frontier document encoding"))?)),
                    repo_id:child.into(),scope_uuid:scope.into(),generation:crate::echo_receipt_scope::repo_echo_generation_tx(tx,child)?,
                    projection:p.plan.projection.clone(),git_sha:sha.into(),svn_revision:p.svn_revision.unwrap()};
                write_value(tx,&frontier_key(scope,sha),&serde_json::to_string(&proof).map_err(|_|invalid("frontier receipt encoding"))?)?;
                crate::echo_receipt_scope::write_scoped_last_git_sha_kv(tx,child,sha,&next.updated_at)?;
                if !verified_reconciliation_frontier(tx,child,sha,&p.plan.projection)? {return Err(invalid("new frontier is not consumable"));}
            }
            Ok(next)
        })
    }
}

fn finalize_late_pair_publish_op_in_tx(
    tx: &Connection,
    parent_repo_id: &str,
    op_id: &str,
    enable_child: Option<(&str, &str)>,
) -> Result<LatePairPublishOperation, DatabaseError> {
    let raw = read_value(tx, &key("document", op_id))?
        .ok_or_else(|| DatabaseError::Other("late-pair publish operation not found".into()))?;
    let mut op = parse(&raw)?;
    if op.version != 1 {
        return Err(invalid("legacy finalizer cannot complete v2 work"));
    }
    if op.parent_repo_id != parent_repo_id {
        return Err(DatabaseError::Other(
            "late-pair publish operation parent mismatch".into(),
        ));
    }
    if let Some((child_id, git_tip)) = enable_child {
        super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, child_id)?;
        if super::managed_remove::new_work_blocked(tx, child_id)? {
            op.state = LatePairPublishState::ReconciliationRequired;
            op.outcome_detail =
                Some("child repository removal blocks late-pair publish finalize".into());
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            tx.execute(
                "DELETE FROM kv_state WHERE key=?1 AND value=?2",
                params![key("active", parent_repo_id), op_id],
            )?;
            return Ok(op);
        }
        let changed = tx.execute(
            "UPDATE repositories SET enabled = 1, sync_status = 'idle', last_git_sha = ?1, updated_at = ?2 WHERE id = ?3",
            params![git_tip, Utc::now().to_rfc3339(), child_id],
        )?;
        if changed != 1 {
            op.state = LatePairPublishState::ReconciliationRequired;
            op.outcome_detail = Some(
                "child repository row missing or unchanged during late-pair publish finalize"
                    .into(),
            );
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            tx.execute(
                "DELETE FROM kv_state WHERE key=?1 AND value=?2",
                params![key("active", parent_repo_id), op_id],
            )?;
            return Ok(op);
        }
    }
    op.state = LatePairPublishState::Completed;
    op.updated_at = Utc::now().to_rfc3339();
    write_op(tx, &op)?;
    tx.execute(
        "DELETE FROM kv_state WHERE key=?1 AND value=?2",
        params![key("active", parent_repo_id), op_id],
    )?;
    Ok(op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn insert_parent_child(db: &Database, child_id: &str, enabled: bool) {
        let now = Utc::now().to_rfc3339();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors,parent_id)
                 VALUES ('parent','p','file:///svn','trunk','','local','','r','main','team',5,0,0,1,?1,?1,2,'base','idle',0,0,NULL),
                        (?2,'c','file:///svn','branches/f','','local','','r','feature','team',5,0,0,?3,?1,?1,2,'base','idle',0,0,'parent')",
                params![now, child_id, enabled as i32],
            )
            .unwrap();
    }

    fn replay_in_progress_op(child_id: &str) -> LatePairPublishOperation {
        let now = Utc::now().to_rfc3339();
        LatePairPublishOperation {
            version: 1,
            id: "op-finalize".into(),
            parent_repo_id: "parent".into(),
            child_repo_id: Some(child_id.into()),
            operation_type: "late_pair_publish".into(),
            initiator_id: "i".into(),
            request_id: "r".into(),
            target_fingerprint: "fp".into(),
            created_at: now.clone(),
            updated_at: now,
            state: LatePairPublishState::ReplayInProgress,
            policy_version: "late_pair_publish_v1".into(),
            git_branch: "feature".into(),
            svn_branch: "branches/f".into(),
            pinned_git_tip: "tipsha".into(),
            baseline_git_sha: "base".into(),
            baseline_svn_rev: 2,
            svn_copy_source_rev: 2,
            svn_copy_source_path: Some("trunk".into()),
            svn_branch_head_rev: Some(3),
            replayed_git_shas: vec!["sha1".into()],
            outcome_detail: None,
            reconciliation: None,
        }
    }

    fn seed_active_op(db: &Database, op: &LatePairPublishOperation) {
        let conn = db.conn();
        write_op(&conn, op).unwrap();
        write_value(&conn, &key("active", "parent"), &op.id).unwrap();
        write_value(&conn, &key("latest", "parent"), &op.id).unwrap();
    }

    fn child_enabled(db: &Database, child_id: &str) -> bool {
        db.conn()
            .query_row(
                "SELECT enabled FROM repositories WHERE id=?1",
                [child_id],
                |r| r.get::<_, i32>(0),
            )
            .unwrap()
            != 0
    }

    fn tombstone_child(db: &Database, child_id: &str) {
        let conn = db.conn();
        write_value(
            &conn,
            &format!("managed_remove_v1:tombstone:{child_id}"),
            "{}",
        )
        .unwrap();
    }

    #[test]
    fn finalize_enables_child_and_completes_journal() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let child_id = "child-ok";
        insert_parent_child(&db, child_id, false);
        let op = replay_in_progress_op(child_id);
        seed_active_op(&db, &op);

        let done = db
            .finalize_late_pair_publish_enabling_child("parent", &op.id, child_id, "tipsha")
            .unwrap();
        assert_eq!(done.state, LatePairPublishState::Completed);
        assert!(child_enabled(&db, child_id));
        assert!(db
            .latest_late_pair_publish_operation("parent")
            .unwrap()
            .is_some());
    }

    #[test]
    fn finalize_refuses_tombstoned_child_without_enabling() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let child_id = "child-tomb";
        insert_parent_child(&db, child_id, false);
        tombstone_child(&db, child_id);
        let op = replay_in_progress_op(child_id);
        seed_active_op(&db, &op);

        let held = db
            .finalize_late_pair_publish_enabling_child("parent", &op.id, child_id, "tipsha")
            .unwrap();
        assert_eq!(held.state, LatePairPublishState::ReconciliationRequired);
        assert!(!child_enabled(&db, child_id));
        let loaded = db
            .get_late_pair_publish_operation("parent", &op.id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.state, LatePairPublishState::ReconciliationRequired);
        assert_ne!(loaded.state, LatePairPublishState::Completed);
    }

    #[test]
    fn finalize_refuses_when_child_row_deleted() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let child_id = "child-gone";
        insert_parent_child(&db, child_id, false);
        let op = replay_in_progress_op(child_id);
        seed_active_op(&db, &op);
        db.conn()
            .execute("DELETE FROM repositories WHERE id=?1", [child_id])
            .unwrap();

        let held = db
            .finalize_late_pair_publish_enabling_child("parent", &op.id, child_id, "tipsha")
            .unwrap();
        assert_eq!(held.state, LatePairPublishState::ReconciliationRequired);
        let loaded = db
            .get_late_pair_publish_operation("parent", &op.id)
            .unwrap()
            .unwrap();
        assert_ne!(loaded.state, LatePairPublishState::Completed);
    }
}
