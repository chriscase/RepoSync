//! #69 coordinated pair-refresh preview (no execute, no re-anchor).
//!
//! The default operation is **update pair from parent**: pin both sides and
//! report unsynced work without discarding it. Re-anchor and execution are
//! refused. Published Git commits and SVN revisions are not rewritten.

use std::path::Path;
use std::process::Output;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::history_inspect::is_full_git_oid;
use crate::late_pair::VerifiedMapping;

/// Policy identity pinned into every preview from this slice.
pub const POLICY_VERSION: &str = "pair_refresh_preview_v1";
/// Single existing registration until #63 generation tables exist.
pub const PAIR_GENERATION: i64 = 1;
pub const GENERATION_SOURCE: &str = "compatibility_single_registration";
pub const OPERATION_UPDATE: &str = "update_pair_from_parent";
pub const PARENT_INSPECT_REF: &str = "refs/reposync/pair-refresh/parent";
pub const PAIR_INSPECT_REF: &str = "refs/reposync/pair-refresh/pair";
const MAX_PENDING_COMMITS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOperation {
    UpdateFromParent,
    Reanchor,
}

pub fn parse_operation(raw: &str) -> Result<RefreshOperation, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "update_pair_from_parent" | "update-from-parent" | "update" => {
            Ok(RefreshOperation::UpdateFromParent)
        }
        "reanchor" | "re-anchor" | "re_anchor" | "recreate" | "re-create" | "recreate_pair" => {
            Ok(RefreshOperation::Reanchor)
        }
        other => Err(other.to_string()),
    }
}

pub fn execution_requested(execute: bool, dry_run: Option<bool>) -> bool {
    execute || dry_run == Some(false)
}

pub fn reanchor_refusal() -> (&'static str, &'static str) {
    (
        "reanchor_not_implemented",
        "recreate/re-anchor is a separate explicit mode and is NOT IMPLEMENTED. It is not an alias for update-from-parent, reset, force-push, or removal of the old SVN path.",
    )
}

pub fn execute_refusal(digest: Option<&str>) -> (String, String) {
    let detail = match digest {
        Some(digest) => format!(
            "update-pair-from-parent execution is a later slice and was not started. No durable job was created and no external Git or SVN write was performed. plan_digest={digest}"
        ),
        None => "update-pair-from-parent execution is a later slice and was not started. No durable job was created and no external Git or SVN write was performed.".into(),
    };
    ("refresh_execute_not_implemented".into(), detail)
}

pub fn format_refusal(reason: &str, detail: &str) -> String {
    format!("{reason}: {detail}")
}

pub fn svn_path_missing(error: &str) -> bool {
    error.contains("E170000")
        || error.contains("E160013")
        || error.contains("W160013")
        || error.contains("(not found)")
}

pub fn branch_svn_url(root: &str, branch: &str) -> String {
    if branch.is_empty() {
        root.trim_end_matches('/').to_string()
    } else {
        format!(
            "{}/{}",
            root.trim_end_matches('/'),
            branch.trim_start_matches('/')
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BaselinePin {
    pub git_sha: String,
    pub svn_revision: i64,
    pub evidence: String,
    pub descends: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClassifiedPending {
    pub knowable: bool,
    pub rewritten: bool,
    pub complete: bool,
    pub count: usize,
    pub shas: Vec<String>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingSvn {
    pub knowable: bool,
    pub count: usize,
    pub mapped_revision: Option<i64>,
    pub head_revision: Option<i64>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingLocal {
    pub count: usize,
    pub complete: bool,
    pub diverged: bool,
    pub shas: Vec<String>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntendedResult {
    pub operation: String,
    pub executed: bool,
    pub preserves_published_git_commits: bool,
    pub preserves_published_svn_revisions: bool,
    pub discards_unsynced_work: bool,
    pub rewrites_svn_revisions: bool,
    pub force_push_auto_rebase: bool,
    pub graphs_must_be_identical: bool,
    pub summary: String,
    pub merge_echo_treatment: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefreshPlan {
    pub mode: String,
    pub operation: String,
    pub executed: bool,
    pub published: bool,
    pub durable_job_started: bool,
    pub policy_version: String,
    pub pair_id: String,
    pub parent_id: String,
    pub pair_generation: i64,
    pub generation_source: String,
    pub generation_note: String,
    pub pins_complete: bool,
    pub plan_id: String,
    pub plan_digest: String,
    pub approval: Approval,
    pub git: GitPins,
    pub svn: SvnPins,
    pub baseline: BaselinePins,
    pub pending: PendingReport,
    pub conflicts: Vec<String>,
    pub unknowns: Vec<String>,
    pub intended_result: IntendedResult,
    pub reanchor_status: String,
    pub execute_status: String,
    pub inspection: Inspection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Approval {
    pub eligible: bool,
    pub reason: String,
    pub binds_to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitPins {
    pub pair_branch: String,
    pub parent_branch: String,
    pub pair_tip: Option<String>,
    pub parent_tip: Option<String>,
    pub pair_local_tip: Option<String>,
    pub parent_local_tip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SvnPins {
    pub uuid: Option<String>,
    pub pair_uuid: Option<String>,
    pub pair_path: String,
    pub parent_path: String,
    pub pair_revision: Option<i64>,
    pub parent_revision: Option<i64>,
    pub pair_url: String,
    pub parent_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BaselinePins {
    pub pair: Option<BaselinePin>,
    pub parent: Option<BaselinePin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingReport {
    pub pair_git: ClassifiedPending,
    pub parent_git: ClassifiedPending,
    pub pair_svn: PendingSvn,
    pub parent_svn: PendingSvn,
    pub pair_local: PendingLocal,
    pub parent_local: PendingLocal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Inspection {
    pub namespace: String,
    pub external_git_writes: bool,
    pub external_svn_writes: bool,
    pub checkpoint_mutation: bool,
    pub durable_job_started: bool,
}

#[derive(Debug, Clone)]
pub struct RefreshObservations {
    pub pair_id: String,
    pub parent_id: String,
    pub pair_git_branch: String,
    pub parent_git_branch: String,
    pub pair_git_tip: Option<String>,
    pub parent_git_tip: Option<String>,
    pub pair_local_tip: Option<String>,
    pub parent_local_tip: Option<String>,
    pub pair_baseline: Option<BaselinePin>,
    pub parent_baseline: Option<BaselinePin>,
    pub pair_pending_git: ClassifiedPending,
    pub parent_pending_git: ClassifiedPending,
    pub svn_uuid: Option<String>,
    pub pair_svn_uuid: Option<String>,
    pub pair_svn_path: String,
    pub parent_svn_path: String,
    pub pair_svn_url: String,
    pub parent_svn_url: String,
    pub pair_svn_revision: Option<i64>,
    pub parent_svn_revision: Option<i64>,
    pub pair_svn_missing: bool,
    pub parent_svn_missing: bool,
    pub pair_local_ahead: Vec<String>,
    pub parent_local_ahead: Vec<String>,
    pub pair_local_ahead_complete: bool,
    pub parent_local_ahead_complete: bool,
    pub pair_local_diverged: bool,
    pub parent_local_diverged: bool,
    pub notes: Vec<String>,
}

pub fn select_pair_baseline(
    pair_mappings: &[VerifiedMapping],
    parent_mappings: &[VerifiedMapping],
    mut descends: impl FnMut(&str) -> Option<bool>,
) -> Option<BaselinePin> {
    if let Some(mapping) = latest_mapping(pair_mappings) {
        let descends = descends(&mapping.git_sha);
        return Some(pin_from(mapping, mapping.evidence.clone(), descends));
    }
    if parent_mappings.is_empty() {
        return None;
    }
    let judged: Vec<(&VerifiedMapping, Option<bool>)> = parent_mappings
        .iter()
        .map(|mapping| {
            let descent = descends(&mapping.git_sha);
            (mapping, descent)
        })
        .collect();
    if let Some(mapping) = judged
        .iter()
        .filter(|(_, descent)| *descent == Some(true))
        .map(|(mapping, _)| *mapping)
        .max_by_key(|mapping| (mapping.svn_revision, mapping.git_sha.as_str()))
    {
        return Some(pin_from(
            mapping,
            "inherited_parent_mapping".into(),
            Some(true),
        ));
    }
    if judged.iter().all(|(_, descent)| *descent == Some(false)) {
        let mapping = latest_mapping(parent_mappings)?;
        return Some(pin_from(
            mapping,
            "rejected_parent_mapping".into(),
            Some(false),
        ));
    }
    let mapping = latest_mapping(parent_mappings)?;
    Some(pin_from(mapping, "parent_mapping_unproven".into(), None))
}

pub fn select_parent_baseline(
    parent_mappings: &[VerifiedMapping],
    mut descends: impl FnMut(&str) -> Option<bool>,
) -> Option<BaselinePin> {
    let mapping = latest_mapping(parent_mappings)?;
    let descends = descends(&mapping.git_sha);
    Some(pin_from(mapping, mapping.evidence.clone(), descends))
}

fn latest_mapping(mappings: &[VerifiedMapping]) -> Option<&VerifiedMapping> {
    mappings
        .iter()
        .max_by_key(|mapping| (mapping.svn_revision, mapping.git_sha.as_str()))
}

fn pin_from(mapping: &VerifiedMapping, evidence: String, descends: Option<bool>) -> BaselinePin {
    BaselinePin {
        git_sha: mapping.git_sha.clone(),
        svn_revision: mapping.svn_revision,
        evidence,
        descends,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitObservation {
    pub remote_tip: Option<String>,
    pub local_tip: Option<String>,
    pub local_ahead: Vec<String>,
    pub local_ahead_complete: bool,
    pub local_diverged: bool,
    pub note: String,
}

impl GitObservation {
    fn missing(note: impl Into<String>) -> Self {
        Self {
            remote_tip: None,
            local_tip: None,
            local_ahead: Vec::new(),
            local_ahead_complete: true,
            local_diverged: false,
            note: note.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GitPreviewFacts {
    pub pair: GitObservation,
    pub parent: GitObservation,
    pub pair_baseline: Option<BaselinePin>,
    pub parent_baseline: Option<BaselinePin>,
    pub pair_pending: ClassifiedPending,
    pub parent_pending: ClassifiedPending,
    pub notes: Vec<String>,
}

pub struct GitLayout<'a> {
    pub parent_dir: Option<&'a Path>,
    pub pair_dir: Option<&'a Path>,
    pub same_remote: bool,
}

/// Optional HTTP(S) tokens for Git CLI preview fetches (never embedded in argv).
pub struct GitPreviewAuth<'a> {
    pub parent_token: Option<&'a str>,
    pub pair_token: Option<&'a str>,
}

impl<'a> GitPreviewAuth<'a> {
    pub fn none() -> Self {
        Self {
            parent_token: None,
            pair_token: None,
        }
    }
}

pub fn analyze_git_preview(
    layout: GitLayout<'_>,
    pair_mappings: &[VerifiedMapping],
    parent_mappings: &[VerifiedMapping],
    pair_branch: &str,
    parent_branch: &str,
    auth: GitPreviewAuth<'_>,
) -> GitPreviewFacts {
    let mut notes = Vec::new();
    let (parent_dir, pair_dir) = if layout.same_remote {
        let shared = layout.parent_dir.or(layout.pair_dir);
        (shared, shared)
    } else {
        notes.push(
            "pair and parent Git remotes differ; inherited commits are not guessed across clones"
                .into(),
        );
        (layout.parent_dir, layout.pair_dir)
    };
    let parent = observe_dir(
        parent_dir,
        parent_branch,
        PARENT_INSPECT_REF,
        auth.parent_token,
    );
    let pair = observe_dir(pair_dir, pair_branch, PAIR_INSPECT_REF, auth.pair_token);
    if !parent.note.is_empty() {
        notes.push(parent.note.clone());
    }
    if !pair.note.is_empty() {
        notes.push(pair.note.clone());
    }
    let pair_baseline = select_pair_baseline(pair_mappings, parent_mappings, |sha| {
        descent(pair_dir, sha, pair.remote_tip.as_deref())
    });
    let parent_baseline = select_parent_baseline(parent_mappings, |sha| {
        descent(parent_dir, sha, parent.remote_tip.as_deref())
    });
    let graph = if layout.same_remote { parent_dir } else { None };
    let pair_pending = classify_side_pending(
        graph,
        pair_baseline.as_ref(),
        pair.remote_tip.as_deref(),
        parent.remote_tip.as_deref(),
    );
    let parent_pending = classify_side_pending(
        graph,
        parent_baseline.as_ref(),
        parent.remote_tip.as_deref(),
        pair.remote_tip.as_deref(),
    );
    GitPreviewFacts {
        pair,
        parent,
        pair_baseline,
        parent_baseline,
        pair_pending,
        parent_pending,
        notes,
    }
}

fn observe_dir(
    dir: Option<&Path>,
    branch: &str,
    inspect_ref: &str,
    http_auth_token: Option<&str>,
) -> GitObservation {
    match dir {
        Some(dir) if git_dir_exists(dir) => {
            observe_branch(dir, branch, inspect_ref, http_auth_token)
        }
        _ => GitObservation::missing(format!("no Git clone to inspect '{branch}'")),
    }
}

fn descent(dir: Option<&Path>, sha: &str, tip: Option<&str>) -> Option<bool> {
    match (dir, tip) {
        (Some(dir), Some(tip)) => is_ancestor(dir, sha, tip),
        _ => None,
    }
}

pub fn classify_side_pending(
    graph: Option<&Path>,
    baseline: Option<&BaselinePin>,
    tip: Option<&str>,
    other_tip: Option<&str>,
) -> ClassifiedPending {
    let Some(baseline) = baseline else {
        return pending_unknown("no verified baseline for this side");
    };
    if baseline.descends == Some(false) {
        return ClassifiedPending {
            knowable: true,
            rewritten: true,
            complete: true,
            count: 0,
            shas: Vec::new(),
            note: "tip does not descend from the verified mapping; rewritten commits are not counted as new work and are not discarded"
                .into(),
        };
    }
    if baseline.descends != Some(true) {
        return pending_unknown("ancestry of the verified mapping could not be proved");
    }
    let (Some(graph), Some(tip), Some(other_tip)) = (graph, tip, other_tip) else {
        return pending_unknown(
            "tips are unpinned, so inherited commits cannot be excluded from new work",
        );
    };
    let after = match commits_ahead(graph, &baseline.git_sha, tip) {
        Some(span) if span.complete => span.shas,
        Some(_) => {
            return ClassifiedPending {
                knowable: false,
                rewritten: false,
                complete: false,
                count: 0,
                shas: Vec::new(),
                note: format!(
                    "more than {MAX_PENDING_COMMITS} commits after the mapping; the pending list was not truncated into an approval"
                ),
            };
        }
        None => return pending_unknown("git rev-list of commits after the mapping failed"),
    };
    let Some(unique) = exclude_inherited(graph, &after, other_tip) else {
        return pending_unknown(
            "inherited commits could not be proved, so new work was not guessed",
        );
    };
    let dropped = after.len().saturating_sub(unique.len());
    let note = if dropped > 0 {
        format!(
            "{dropped} inherited commit(s) already on the other tip were not counted as new work"
        )
    } else if unique.is_empty() {
        "no Git commits after the verified mapping are unique to this side".into()
    } else {
        "Git commits after the verified mapping that are not on the other tip".into()
    };
    let count = unique.len();
    ClassifiedPending {
        knowable: true,
        rewritten: false,
        complete: true,
        count,
        shas: unique,
        note,
    }
}

fn pending_unknown(note: impl Into<String>) -> ClassifiedPending {
    ClassifiedPending {
        knowable: false,
        rewritten: false,
        complete: false,
        count: 0,
        shas: Vec::new(),
        note: note.into(),
    }
}

pub fn build_preview(obs: &RefreshObservations) -> RefreshPlan {
    let pair_svn = classify_svn(
        obs.pair_baseline.as_ref().map(|pin| pin.svn_revision),
        obs.pair_svn_revision,
        obs.pair_svn_missing,
        "pair",
    );
    let parent_svn = classify_svn(
        obs.parent_baseline.as_ref().map(|pin| pin.svn_revision),
        obs.parent_svn_revision,
        obs.parent_svn_missing,
        "parent",
    );
    let pair_local = local_report(
        &obs.pair_local_ahead,
        obs.pair_local_ahead_complete,
        obs.pair_local_diverged,
        "pair",
    );
    let parent_local = local_report(
        &obs.parent_local_ahead,
        obs.parent_local_ahead_complete,
        obs.parent_local_diverged,
        "parent",
    );
    let mut conflicts = Vec::new();
    let mut unknowns = obs.notes.clone();
    if obs.pair_baseline.is_none() {
        conflicts.push("missing_pair_baseline".into());
    }
    if obs.parent_baseline.is_none() {
        conflicts.push("missing_parent_baseline".into());
    }
    if obs.pair_pending_git.rewritten {
        conflicts.push("rewritten_pair_lineage".into());
    }
    if obs.parent_pending_git.rewritten {
        conflicts.push("rewritten_parent_lineage".into());
    }
    if obs.pair_git_tip.is_none() || obs.parent_git_tip.is_none() {
        conflicts.push("unpinned_git_tip".into());
    }
    if obs.pair_svn_missing {
        conflicts.push("pair_svn_missing".into());
    }
    if obs.parent_svn_missing {
        conflicts.push("parent_svn_missing".into());
    }
    if !obs.pair_svn_missing
        && !obs.parent_svn_missing
        && (obs.pair_svn_revision.is_none() || obs.parent_svn_revision.is_none())
    {
        conflicts.push("unpinned_svn".into());
    }
    if let (Some(parent_uuid), Some(pair_uuid)) = (&obs.svn_uuid, &obs.pair_svn_uuid) {
        if parent_uuid != pair_uuid {
            conflicts.push("uuid_mismatch".into());
        }
    }
    if obs.pair_local_diverged {
        conflicts.push("local_pair_diverged".into());
    }
    if obs.parent_local_diverged {
        conflicts.push("local_parent_diverged".into());
    }
    if !obs.pair_pending_git.complete || !obs.parent_pending_git.complete {
        conflicts.push("pending_overflow".into());
    }
    let pair_unsynced = side_unsynced(&obs.pair_pending_git, &pair_svn, &pair_local);
    let parent_unsynced = side_unsynced(&obs.parent_pending_git, &parent_svn, &parent_local);
    if pair_unsynced && parent_unsynced {
        conflicts.push("both_advanced".into());
    }
    if !obs.pair_pending_git.knowable {
        unknowns.push("pair Git pending work is not fully knowable".into());
    }
    if !obs.parent_pending_git.knowable {
        unknowns.push("parent Git pending work is not fully knowable".into());
    }
    let uuid_ok = match (&obs.svn_uuid, &obs.pair_svn_uuid) {
        (Some(parent_uuid), Some(pair_uuid)) => parent_uuid == pair_uuid,
        (Some(_), None) if obs.pair_svn_missing => false,
        (Some(_), None) => true,
        _ => false,
    };
    let pins_complete = obs.pair_git_tip.as_deref().is_some_and(is_full_git_oid)
        && obs.parent_git_tip.as_deref().is_some_and(is_full_git_oid)
        && uuid_ok
        && obs.pair_svn_revision.is_some()
        && obs.parent_svn_revision.is_some()
        && obs
            .pair_baseline
            .as_ref()
            .is_some_and(|pin| pin.descends == Some(true))
        && obs
            .parent_baseline
            .as_ref()
            .is_some_and(|pin| pin.descends == Some(true))
        && !obs.pair_svn_missing
        && !obs.parent_svn_missing
        && obs.pair_pending_git.knowable
        && obs.parent_pending_git.knowable
        && obs.pair_pending_git.complete
        && obs.parent_pending_git.complete
        && pair_svn.knowable
        && parent_svn.knowable
        && !obs.pair_local_diverged
        && !obs.parent_local_diverged
        && obs.pair_local_ahead_complete
        && obs.parent_local_ahead_complete;
    let intended = intended_result(
        pair_unsynced,
        parent_unsynced,
        &obs.pair_pending_git,
        &obs.parent_pending_git,
    );
    let identity = PlanIdentity {
        operation: OPERATION_UPDATE,
        policy_version: POLICY_VERSION,
        pair_id: &obs.pair_id,
        parent_id: &obs.parent_id,
        pair_generation: PAIR_GENERATION,
        generation_source: GENERATION_SOURCE,
        pair_git_branch: &obs.pair_git_branch,
        parent_git_branch: &obs.parent_git_branch,
        pair_git_tip: &obs.pair_git_tip,
        parent_git_tip: &obs.parent_git_tip,
        pair_local_tip: &obs.pair_local_tip,
        parent_local_tip: &obs.parent_local_tip,
        svn_uuid: &obs.svn_uuid,
        pair_svn_path: &obs.pair_svn_path,
        parent_svn_path: &obs.parent_svn_path,
        pair_svn_revision: obs.pair_svn_revision,
        parent_svn_revision: obs.parent_svn_revision,
        pair_baseline_sha: obs.pair_baseline.as_ref().map(|pin| pin.git_sha.as_str()),
        pair_baseline_rev: obs.pair_baseline.as_ref().map(|pin| pin.svn_revision),
        parent_baseline_sha: obs.parent_baseline.as_ref().map(|pin| pin.git_sha.as_str()),
        parent_baseline_rev: obs.parent_baseline.as_ref().map(|pin| pin.svn_revision),
        pair_pending_git: &obs.pair_pending_git.shas,
        parent_pending_git: &obs.parent_pending_git.shas,
        pair_local_ahead: &obs.pair_local_ahead,
        parent_local_ahead: &obs.parent_local_ahead,
        pair_pending_svn_count: pair_svn.count,
        parent_pending_svn_count: parent_svn.count,
        pair_pending_svn_knowable: pair_svn.knowable,
        parent_pending_svn_knowable: parent_svn.knowable,
        conflicts: &conflicts,
        pair_rewritten: obs.pair_pending_git.rewritten,
        parent_rewritten: obs.parent_pending_git.rewritten,
        pins_complete,
    };
    let plan_digest = identity_digest(&identity);
    RefreshPlan {
        mode: "preview".into(),
        operation: OPERATION_UPDATE.into(),
        executed: false,
        published: false,
        durable_job_started: false,
        policy_version: POLICY_VERSION.into(),
        pair_id: obs.pair_id.clone(),
        parent_id: obs.parent_id.clone(),
        pair_generation: PAIR_GENERATION,
        generation_source: GENERATION_SOURCE.into(),
        generation_note: "#63 generation tables are not activated. This pin is the single existing registration, not a re-anchor.".into(),
        pins_complete,
        plan_id: plan_digest.clone(),
        plan_digest,
        approval: Approval {
            eligible: false,
            reason: "execution_not_implemented".into(),
            binds_to: "plan_digest".into(),
        },
        git: GitPins {
            pair_branch: obs.pair_git_branch.clone(),
            parent_branch: obs.parent_git_branch.clone(),
            pair_tip: obs.pair_git_tip.clone(),
            parent_tip: obs.parent_git_tip.clone(),
            pair_local_tip: obs.pair_local_tip.clone(),
            parent_local_tip: obs.parent_local_tip.clone(),
        },
        svn: SvnPins {
            uuid: obs.svn_uuid.clone(),
            pair_uuid: obs.pair_svn_uuid.clone(),
            pair_path: obs.pair_svn_path.clone(),
            parent_path: obs.parent_svn_path.clone(),
            pair_revision: obs.pair_svn_revision,
            parent_revision: obs.parent_svn_revision,
            pair_url: obs.pair_svn_url.clone(),
            parent_url: obs.parent_svn_url.clone(),
        },
        baseline: BaselinePins {
            pair: obs.pair_baseline.clone(),
            parent: obs.parent_baseline.clone(),
        },
        pending: PendingReport {
            pair_git: obs.pair_pending_git.clone(),
            parent_git: obs.parent_pending_git.clone(),
            pair_svn,
            parent_svn,
            pair_local,
            parent_local,
        },
        conflicts,
        unknowns,
        intended_result: intended,
        reanchor_status: "NOT_IMPLEMENTED".into(),
        execute_status: "NOT_IMPLEMENTED".into(),
        inspection: Inspection {
            namespace: "refs/reposync/pair-refresh/".into(),
            external_git_writes: false,
            external_svn_writes: false,
            checkpoint_mutation: false,
            durable_job_started: false,
        },
    }
}

fn side_unsynced(git: &ClassifiedPending, svn: &PendingSvn, local: &PendingLocal) -> bool {
    (git.knowable && !git.rewritten && git.count > 0)
        || (svn.knowable && svn.count > 0)
        || local.count > 0
        || local.diverged
}

fn intended_result(
    pair_unsynced: bool,
    parent_unsynced: bool,
    pair_git: &ClassifiedPending,
    parent_git: &ClassifiedPending,
) -> IntendedResult {
    let summary = if pair_git.rewritten || parent_git.rewritten {
        "Read-only preview of update pair from parent. Published lineage does not match a verified mapping. Rewritten commits are not counted as new work and are not discarded. This slice does not execute the refresh.".into()
    } else if pair_unsynced && parent_unsynced {
        "Read-only preview of update pair from parent. Parent and pair both have unsynced work. It is preserved and is not discarded or silently rewritten. This slice does not execute the refresh.".into()
    } else if pair_unsynced {
        "Read-only preview of update pair from parent. The pair has unsynced work. It is preserved and is not discarded. This slice does not execute the refresh.".into()
    } else if parent_unsynced {
        "Read-only preview of update pair from parent. The parent has changes the pair does not contain. A later update would integrate them as new history without rewriting published revisions. This slice does not execute the refresh.".into()
    } else {
        "Read-only preview of update pair from parent. Pinned inputs show no unsynced work on either side. A later execution with these inputs would be a no-op. This slice does not execute the refresh.".into()
    };
    IntendedResult {
        operation: OPERATION_UPDATE.into(),
        executed: false,
        preserves_published_git_commits: true,
        preserves_published_svn_revisions: true,
        discards_unsynced_work: false,
        rewrites_svn_revisions: false,
        force_push_auto_rebase: false,
        graphs_must_be_identical: false,
        summary,
        merge_echo_treatment: "Git and SVN graphs need not match. A later update would record integration as a new Git reconciliation commit and a normal SVN merge revision. Commits already contained in the other side's ancestry, or already named by a verified mapping, are inherited and must not be replayed. Equal trees or commit messages are not that proof. Committed SVN revisions are not rewritten. Force-push auto-rebase is not authorized.".into(),
    }
}

fn classify_svn(mapped: Option<i64>, head: Option<i64>, missing: bool, side: &str) -> PendingSvn {
    if missing {
        return PendingSvn {
            knowable: false,
            count: 0,
            mapped_revision: mapped,
            head_revision: None,
            note: format!("{side} SVN path is missing; revisions were not invented"),
        };
    }
    match (mapped, head) {
        (Some(mapped), Some(head)) if head >= mapped => PendingSvn {
            knowable: true,
            count: usize::try_from(head - mapped).unwrap_or(usize::MAX),
            mapped_revision: Some(mapped),
            head_revision: Some(head),
            note: format!("{side} SVN revisions after r{mapped} through r{head} are unsynced when the count is greater than zero"),
        },
        (Some(mapped), Some(head)) => PendingSvn {
            knowable: true,
            count: 0,
            mapped_revision: Some(mapped),
            head_revision: Some(head),
            note: format!(
                "{side} SVN head r{head} is behind verified mapping r{mapped}; revisions are not rewritten to catch up"
            ),
        },
        _ => PendingSvn {
            knowable: false,
            count: 0,
            mapped_revision: mapped,
            head_revision: head,
            note: format!("{side} SVN pending range is unpinned"),
        },
    }
}

fn local_report(shas: &[String], complete: bool, diverged: bool, side: &str) -> PendingLocal {
    let note = if diverged {
        format!("{side} bridge commits diverge from the fetched tip and are preserved; this preview does not reset or discard them")
    } else if shas.is_empty() {
        format!("{side} bridge tip matches the fetched tip or has no local-only commits")
    } else {
        format!("{side} bridge commits ahead of the fetched tip are preserved")
    };
    PendingLocal {
        count: shas.len(),
        complete,
        diverged,
        shas: shas.to_vec(),
        note,
    }
}

#[derive(Serialize)]
struct PlanIdentity<'a> {
    operation: &'a str,
    policy_version: &'a str,
    pair_id: &'a str,
    parent_id: &'a str,
    pair_generation: i64,
    generation_source: &'a str,
    pair_git_branch: &'a str,
    parent_git_branch: &'a str,
    pair_git_tip: &'a Option<String>,
    parent_git_tip: &'a Option<String>,
    pair_local_tip: &'a Option<String>,
    parent_local_tip: &'a Option<String>,
    svn_uuid: &'a Option<String>,
    pair_svn_path: &'a str,
    parent_svn_path: &'a str,
    pair_svn_revision: Option<i64>,
    parent_svn_revision: Option<i64>,
    pair_baseline_sha: Option<&'a str>,
    pair_baseline_rev: Option<i64>,
    parent_baseline_sha: Option<&'a str>,
    parent_baseline_rev: Option<i64>,
    pair_pending_git: &'a [String],
    parent_pending_git: &'a [String],
    pair_local_ahead: &'a [String],
    parent_local_ahead: &'a [String],
    pair_pending_svn_count: usize,
    parent_pending_svn_count: usize,
    pair_pending_svn_knowable: bool,
    parent_pending_svn_knowable: bool,
    conflicts: &'a [String],
    pair_rewritten: bool,
    parent_rewritten: bool,
    pins_complete: bool,
}

fn identity_digest(identity: &PlanIdentity<'_>) -> String {
    let bytes = serde_json::to_vec(identity).expect("plan identity serializes");
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn git_dir_exists(path: &Path) -> bool {
    path.join(".git").exists() || path.join("HEAD").exists()
}

fn git(workdir: &Path, args: &[&str], http_auth_token: Option<&str>) -> std::io::Result<Output> {
    crate::git::subprocess_auth::git_cli_output(workdir, args, http_auth_token)
}

fn stdout_trim(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr_trim(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

pub fn is_ancestor(workdir: &Path, ancestor: &str, descendant: &str) -> Option<bool> {
    let output = git(
        workdir,
        &["merge-base", "--is-ancestor", ancestor, descendant],
        None,
    )
    .ok()?;
    match output.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

struct CommitSpan {
    complete: bool,
    shas: Vec<String>,
}

fn commits_ahead(workdir: &Path, ancestor: &str, descendant: &str) -> Option<CommitSpan> {
    let output = git(
        workdir,
        &[
            "rev-list",
            "--reverse",
            &format!("{ancestor}..{descendant}"),
        ],
        None,
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let shas: Vec<String> = stdout_trim(&output)
        .lines()
        .filter(|line| is_full_git_oid(line))
        .map(str::to_string)
        .collect();
    if shas.len() > MAX_PENDING_COMMITS {
        Some(CommitSpan {
            complete: false,
            shas: Vec::new(),
        })
    } else {
        Some(CommitSpan {
            complete: true,
            shas,
        })
    }
}

fn exclude_inherited(workdir: &Path, shas: &[String], other_tip: &str) -> Option<Vec<String>> {
    let mut kept = Vec::new();
    for sha in shas {
        match is_ancestor(workdir, sha, other_tip) {
            Some(true) => {}
            Some(false) => kept.push(sha.clone()),
            None => return None,
        }
    }
    Some(kept)
}

fn rev_parse(workdir: &Path, rev: &str) -> Option<String> {
    let output = git(workdir, &["rev-parse", "--verify", rev], None).ok()?;
    if !output.status.success() {
        return None;
    }
    let text = stdout_trim(&output);
    is_full_git_oid(&text).then_some(text)
}

pub fn observe_branch(
    workdir: &Path,
    branch: &str,
    inspect_ref: &str,
    http_auth_token: Option<&str>,
) -> GitObservation {
    let mut obs = GitObservation::missing(String::new());
    let valid = git(workdir, &["check-ref-format", "--branch", branch], None)
        .ok()
        .is_some_and(|output| output.status.success());
    if !valid {
        obs.note = format!("git branch '{branch}' is not a valid ref name");
        return obs;
    }
    let spec = format!("+refs/heads/{branch}:{inspect_ref}");
    match git(
        workdir,
        &[
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--",
            "origin",
            &spec,
        ],
        http_auth_token,
    ) {
        Ok(output) if output.status.success() => {
            obs.remote_tip = rev_parse(workdir, &format!("{inspect_ref}^{{commit}}"));
            obs.note.clear();
        }
        Ok(output) => {
            let err = stderr_trim(&output);
            obs.note = if err.contains("couldn't find remote ref") {
                format!("remote Git branch '{branch}' is absent")
            } else {
                format!("fetch of '{branch}' failed: {err}")
            };
        }
        Err(err) => {
            obs.note = format!("git fetch could not start: {err}");
        }
    }
    obs.local_tip = rev_parse(workdir, &format!("refs/heads/{branch}^{{commit}}"));
    classify_local(&mut obs, workdir);
    obs
}

fn classify_local(obs: &mut GitObservation, workdir: &Path) {
    let (Some(remote), Some(local)) = (&obs.remote_tip.clone(), &obs.local_tip.clone()) else {
        if obs.local_tip.is_some() && obs.remote_tip.is_none() {
            obs.note = format!(
                "{}local tip is retained and is not discarded",
                if obs.note.is_empty() {
                    String::new()
                } else {
                    format!("{} ", obs.note)
                }
            );
        }
        return;
    };
    if remote == local {
        return;
    }
    match is_ancestor(workdir, remote, local) {
        Some(true) => match commits_ahead(workdir, remote, local) {
            Some(span) if span.complete => obs.local_ahead = span.shas,
            Some(_) => obs.local_ahead_complete = false,
            None => {
                obs.local_ahead_complete = false;
                obs.note = "local commits ahead of the fetched tip could not be listed".into();
            }
        },
        Some(false) => match is_ancestor(workdir, local, remote) {
            Some(true) => {}
            Some(false) => obs.local_diverged = true,
            None => {
                obs.local_ahead_complete = false;
                obs.note = "local and fetched tips could not be compared".into();
            }
        },
        None => {
            obs.local_ahead_complete = false;
            obs.note = "local and fetched tips could not be compared".into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{git, rev_parse, stdout_trim, *};
    use std::fs;
    use std::path::Path;
    use std::process::{Command, Output};

    fn sha(n: u8) -> String {
        format!("{n:040x}")
    }

    fn baseline(sha: &str, rev: i64, descends: Option<bool>) -> BaselinePin {
        BaselinePin {
            git_sha: sha.into(),
            svn_revision: rev,
            evidence: "applied_sync_record".into(),
            descends,
        }
    }

    fn quiet_pending() -> ClassifiedPending {
        ClassifiedPending {
            knowable: true,
            rewritten: false,
            complete: true,
            count: 0,
            shas: Vec::new(),
            note: "none".into(),
        }
    }

    fn sample() -> RefreshObservations {
        RefreshObservations {
            pair_id: "pair".into(),
            parent_id: "parent".into(),
            pair_git_branch: "feature".into(),
            parent_git_branch: "main".into(),
            pair_git_tip: Some(sha(2)),
            parent_git_tip: Some(sha(1)),
            pair_local_tip: None,
            parent_local_tip: Some(sha(1)),
            pair_baseline: Some(baseline(&sha(1), 2, Some(true))),
            parent_baseline: Some(baseline(&sha(1), 2, Some(true))),
            pair_pending_git: quiet_pending(),
            parent_pending_git: quiet_pending(),
            svn_uuid: Some("uuid-1".into()),
            pair_svn_uuid: Some("uuid-1".into()),
            pair_svn_path: "branches/feature".into(),
            parent_svn_path: "trunk".into(),
            pair_svn_url: "file:///svn/branches/feature".into(),
            parent_svn_url: "file:///svn/trunk".into(),
            pair_svn_revision: Some(2),
            parent_svn_revision: Some(2),
            pair_svn_missing: false,
            parent_svn_missing: false,
            pair_local_ahead: Vec::new(),
            parent_local_ahead: Vec::new(),
            pair_local_ahead_complete: true,
            parent_local_ahead_complete: true,
            pair_local_diverged: false,
            parent_local_diverged: false,
            notes: Vec::new(),
        }
    }

    #[test]
    fn digest_is_stable_and_changes_when_parent_tip_changes() {
        let plan = build_preview(&sample());
        let again = build_preview(&sample());
        assert_eq!(plan.plan_digest, again.plan_digest);
        assert_eq!(plan.plan_id, plan.plan_digest);
        assert_eq!(plan.plan_digest.len(), 64);
        assert!(!plan.approval.eligible);
        assert!(!plan.executed);
        assert_eq!(plan.reanchor_status, "NOT_IMPLEMENTED");
        let mut moved = sample();
        moved.parent_git_tip = Some(sha(3));
        assert_ne!(build_preview(&moved).plan_digest, plan.plan_digest);
    }

    #[test]
    fn both_sides_pending_are_preserved_and_not_discarded() {
        let mut obs = sample();
        obs.pair_pending_git = ClassifiedPending {
            knowable: true,
            rewritten: false,
            complete: true,
            count: 1,
            shas: vec![sha(2)],
            note: "pair work".into(),
        };
        obs.parent_pending_git = ClassifiedPending {
            knowable: true,
            rewritten: false,
            complete: true,
            count: 1,
            shas: vec![sha(4)],
            note: "parent work".into(),
        };
        obs.pair_svn_revision = Some(5);
        obs.parent_svn_revision = Some(4);
        let plan = build_preview(&obs);
        assert!(plan.conflicts.iter().any(|c| c == "both_advanced"));
        assert!(!plan.intended_result.discards_unsynced_work);
        assert!(plan.intended_result.preserves_published_git_commits);
        assert!(plan.intended_result.preserves_published_svn_revisions);
        assert!(!plan.intended_result.rewrites_svn_revisions);
        assert!(!plan.intended_result.force_push_auto_rebase);
        assert!(!plan.intended_result.graphs_must_be_identical);
        assert!(plan.intended_result.summary.contains("preserved"));
        assert!(plan.intended_result.summary.contains("not discarded"));
        assert_eq!(plan.pending.pair_svn.count, 3);
        assert_eq!(plan.pending.parent_svn.count, 2);
        assert!(!plan.inspection.external_git_writes);
        assert!(!plan.inspection.checkpoint_mutation);
        assert!(!plan.durable_job_started);
    }

    #[test]
    fn rewritten_lineage_is_not_counted_as_new_work() {
        let mut obs = sample();
        obs.pair_pending_git = ClassifiedPending {
            knowable: true,
            rewritten: true,
            complete: true,
            count: 0,
            shas: Vec::new(),
            note: "not counted".into(),
        };
        obs.pair_baseline.as_mut().unwrap().descends = Some(false);
        obs.parent_svn_revision = Some(4);
        let plan = build_preview(&obs);
        assert!(plan.conflicts.iter().any(|c| c == "rewritten_pair_lineage"));
        assert!(!plan.conflicts.iter().any(|c| c == "both_advanced"));
        assert!(plan.pending.pair_git.shas.is_empty());
        assert_eq!(plan.pending.pair_git.count, 0);
        assert!(plan.pending.pair_git.rewritten);
        assert!(!plan.intended_result.discards_unsynced_work);
        assert!(plan.intended_result.summary.contains("not discarded"));
    }

    #[test]
    fn unchanged_inputs_describe_a_noop_without_execution() {
        let plan = build_preview(&sample());
        assert!(plan.intended_result.summary.contains("no-op"));
        assert!(!plan.executed);
        assert!(plan.conflicts.is_empty());
        assert!(plan.pins_complete);
        assert_eq!(plan.pair_generation, 1);
        assert_eq!(plan.policy_version, POLICY_VERSION);
    }

    #[test]
    fn reanchor_and_execute_refusals_are_explicit() {
        let (reason, detail) = reanchor_refusal();
        let message = format_refusal(reason, detail);
        assert!(message.starts_with("reanchor_not_implemented:"));
        assert!(message.contains("NOT IMPLEMENTED"));
        let (reason, detail) = execute_refusal(Some("abc"));
        let message = format_refusal(&reason, &detail);
        assert!(message.starts_with("refresh_execute_not_implemented:"));
        assert!(message.contains("plan_digest=abc"));
        assert!(message.contains("No durable job"));
        assert_eq!(parse_operation("recreate"), Ok(RefreshOperation::Reanchor));
        assert_eq!(
            parse_operation("update_pair_from_parent"),
            Ok(RefreshOperation::UpdateFromParent)
        );
        assert!(execution_requested(false, Some(false)));
        assert!(!execution_requested(false, None));
    }

    #[test]
    fn pair_mapping_wins_over_inherited_parent_mapping() {
        let pair = vec![VerifiedMapping {
            git_sha: sha(2),
            svn_revision: 4,
            evidence: "applied_sync_record".into(),
        }];
        let parent = vec![VerifiedMapping {
            git_sha: sha(1),
            svn_revision: 2,
            evidence: "snapshot_import_confirmed".into(),
        }];
        let selected =
            select_pair_baseline(&pair, &parent, |sha| Some(sha.ends_with('1'))).unwrap();
        assert_eq!(selected.git_sha, sha(2));
        assert_eq!(selected.evidence, "applied_sync_record");
        assert_eq!(selected.descends, Some(false));
        let inherited = select_pair_baseline(&[], &parent, |_| Some(true)).unwrap();
        assert_eq!(inherited.evidence, "inherited_parent_mapping");
        assert_eq!(inherited.descends, Some(true));
    }

    fn git_cmd(dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap()
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let output = git_cmd(dir, args);
        assert!(
            output.status.success(),
            "{} {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let output = git_cmd(dir, args);
        assert!(
            output.status.success(),
            "{} {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn observe_branch_fetches_tip_without_moving_head() {
        let tmp = tempfile::tempdir().unwrap();
        let bare = tmp.path().join("bare.git");
        let work = tmp.path().join("work");
        git_ok(
            tmp.path(),
            &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
        );
        git_ok(tmp.path(), &["init", "-b", "main", work.to_str().unwrap()]);
        git_ok(&work, &["config", "user.email", "fixture@example.invalid"]);
        git_ok(&work, &["config", "user.name", "Fixture"]);
        fs::write(work.join("f.txt"), "base\n").unwrap();
        git_ok(&work, &["add", "f.txt"]);
        git_ok(&work, &["commit", "-m", "base"]);
        git_ok(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git_ok(&work, &["push", "-u", "origin", "main"]);
        git_ok(&work, &["checkout", "-b", "feature"]);
        fs::write(work.join("f.txt"), "feature\n").unwrap();
        git_ok(&work, &["commit", "-am", "feature"]);
        git_ok(&work, &["push", "-u", "origin", "feature"]);
        git_ok(&work, &["checkout", "main"]);
        git_ok(&work, &["branch", "-D", "feature"]);
        let head = rev_parse(&work, "HEAD").unwrap();
        let obs = observe_branch(&work, "feature", PAIR_INSPECT_REF, None);
        assert_eq!(obs.remote_tip.as_deref().map(is_full_git_oid), Some(true));
        assert!(obs.local_tip.is_none());
        assert_eq!(rev_parse(&work, "HEAD").unwrap(), head);
        let status = git(&work, &["status", "--porcelain"], None).unwrap();
        assert!(stdout_trim(&status).is_empty());
        assert!(rev_parse(&work, "refs/heads/feature").is_none());
    }

    #[test]
    fn classified_pending_excludes_inherited_and_rejects_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        git_ok(tmp.path(), &["init", "-b", "main", work.to_str().unwrap()]);
        git_ok(&work, &["config", "user.email", "fixture@example.invalid"]);
        git_ok(&work, &["config", "user.name", "Fixture"]);
        fs::write(work.join("f.txt"), "base\n").unwrap();
        git_ok(&work, &["add", "f.txt"]);
        git_ok(&work, &["commit", "-m", "base"]);
        let base = rev_parse(&work, "HEAD").unwrap();
        git_ok(&work, &["checkout", "-b", "feature"]);
        fs::write(work.join("f.txt"), "feature\n").unwrap();
        git_ok(&work, &["commit", "-am", "feature"]);
        let feature = rev_parse(&work, "HEAD").unwrap();
        git_ok(&work, &["checkout", "main"]);
        fs::write(work.join("f.txt"), "parent\n").unwrap();
        git_ok(&work, &["commit", "-am", "parent"]);
        let parent = rev_parse(&work, "HEAD").unwrap();

        let inherited = classify_side_pending(
            Some(&work),
            Some(&baseline(&base, 2, Some(true))),
            Some(&feature),
            Some(&base),
        );
        assert!(inherited.knowable);
        assert_eq!(inherited.shas, vec![feature.clone()]);

        let mapped = classify_side_pending(
            Some(&work),
            Some(&baseline(&feature, 4, Some(true))),
            Some(&feature),
            Some(&base),
        );
        assert!(mapped.knowable);
        assert!(mapped.shas.is_empty());

        let parent_pending = classify_side_pending(
            Some(&work),
            Some(&baseline(&base, 2, Some(true))),
            Some(&parent),
            Some(&feature),
        );
        assert_eq!(parent_pending.shas, vec![parent.clone()]);

        let tree = git_out(&work, &["rev-parse", &format!("{feature}^{{tree}}")]);
        let sibling = git_out(
            &work,
            &[
                "commit-tree",
                &tree,
                "-p",
                &base,
                "-m",
                "rebased equivalent",
            ],
        );
        assert_eq!(is_ancestor(&work, &feature, &sibling), Some(false));
        assert_eq!(is_ancestor(&work, &base, &sibling), Some(true));
        let rewritten = classify_side_pending(
            Some(&work),
            Some(&baseline(&feature, 4, Some(false))),
            Some(&sibling),
            Some(&parent),
        );
        assert!(rewritten.rewritten);
        assert!(rewritten.shas.is_empty());
        assert_eq!(rewritten.count, 0);
    }
}
