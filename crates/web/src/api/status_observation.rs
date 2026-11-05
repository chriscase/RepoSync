//! #116 read-only projections. These observations never authorize an effect or release a hold.
use chrono::Utc;
use reposync_core::errors::redact_vcs_error_detail;
use reposync_core::models::Repository;
use serde::Serialize;
use serde_json::Value;

use crate::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Idle,
    Running,
    Initializing,
    Busy,
    Disabled,
    Paused,
    Conflict,
    Held,
    Error,
    Unknown,
}
#[derive(Clone, Debug, Serialize)]
pub struct Lifecycle {
    pub repository_id: String,
    pub kind: String,
    pub state: String,
    pub operation_id: Option<String>,
    pub updated_at: Option<String>,
    pub reason: Option<String>,
    pub target_ref: Option<String>,
    pub intended_git_sha: Option<String>,
    pub confirmed_svn_revision: Option<i64>,
    pub history_evidence: Option<Value>,
    pub target_evidence: Option<Value>,
    pub held: bool,
    pub details_visible: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Observation {
    pub version: u8,
    pub scope: String,
    pub repository_id: Option<String>,
    pub configured: String,
    pub health: Health,
    pub observed_at: String,
    pub data_freshness: String,
    pub last_successful_sync_at: Option<String>,
    pub remote_freshness: String,
    pub registry_busy: Option<bool>,
    pub evidence_complete: bool,
    pub operations: Vec<Lifecycle>,
    pub safe_next_action: String,
    pub uptime_available: bool,
    #[serde(skip)]
    seen_operations: std::collections::BTreeSet<(String, String)>,
}
impl Observation {
    fn unknown(scope: &str) -> Self {
        Self {
            version: 1,
            scope: scope.into(),
            repository_id: None,
            configured: "unknown".into(),
            health: Health::Unknown,
            observed_at: Utc::now().to_rfc3339(),
            data_freshness: "unknown".into(),
            last_successful_sync_at: None,
            remote_freshness: "not_verified_by_status".into(),
            registry_busy: None,
            evidence_complete: false,
            operations: vec![],
            safe_next_action: "Inspect current scoped evidence; unavailable status is not permission to retry, skip or release a hold.".into(),
            uptime_available: false,
            seen_operations:std::collections::BTreeSet::new(),
        }
    }
}
fn text(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}
fn target_evidence(kind: &str, value: &Value) -> Option<Value> {
    match kind {
        "late_pair" => Some(serde_json::json!({
            "kind":"late_pair", "target_git_branch":value.get("git_branch"),"target_svn_branch":value.get("svn_branch"),
            "pinned_git_tip":value.get("pinned_git_tip"),"baseline_git_sha":value.get("baseline_git_sha"),
            "baseline_svn_revision":value.get("baseline_svn_rev"),"copy_source_svn_revision":value.get("svn_copy_source_rev"),
            "copy_source_svn_path":value.get("svn_copy_source_path"),"observed_svn_branch_head_revision":value.get("svn_branch_head_rev"),
            "replayed_git_shas":value.get("replayed_git_shas"),
        })),
        "svn_commit" => Some(serde_json::json!({
            "kind":"svn_commit","target_svn_uuid":value.get("target_svn_uuid"),
            "target_svn_branch_path":value.get("target_svn_branch_path").and_then(Value::as_str).filter(|s|!s.is_empty()),
            "source_git_sha":value.get("source_git_sha"),"source_git_parent":value.get("source_git_parent"),"source_git_tree":value.get("source_git_tree"),
            "pre_write_svn_revision":value.get("pre_write_svn_rev"),"pre_write_svn_tree":value.get("pre_write_svn_tree"),
            "intended_svn_tree":value.get("intended_svn_tree"),
        })),
        _ => None,
    }
}

fn valid_history_record(block: &Value, repo_id: &str) -> bool {
    block.is_object()
        && block.get("state").and_then(Value::as_str) == Some("reconciliation_required")
        && block.get("repo_id").and_then(Value::as_str) == Some(repo_id)
        && block
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty())
        && block.get("detail").is_some_and(Value::is_string)
        && block.get("durable").is_some_and(Value::is_boolean)
        && block
            .get("observed_at")
            .and_then(Value::as_str)
            .is_some_and(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
        && [
            "p_handled",
            "o_prior_observed",
            "r_fresh_remote",
            "l_bridge",
        ]
        .iter()
        .all(|field| match block.get(field) {
            None | Some(Value::Null) => true,
            Some(Value::String(value)) => reposync_core::history_inspect::is_full_git_oid(value),
            _ => false,
        })
}

fn add_operation<T: Serialize>(
    observation: &mut Observation,
    kind: &str,
    result: Result<Option<T>, reposync_core::errors::DatabaseError>,
    actor: &str,
    admin: bool,
) {
    match result.and_then(|op| {
        op.map(|op| {
            serde_json::to_value(op)
                .map_err(|e| reposync_core::errors::DatabaseError::Other(e.to_string()))
        })
        .transpose()
    }) {
        Ok(Some(value)) => {
            let id = text(&value, "id");
            if let Some(id) = &id {
                if !observation
                    .seen_operations
                    .insert((kind.to_owned(), id.clone()))
                {
                    return;
                }
            }
            let state = text(&value, "state").unwrap_or_else(|| "unknown".into());
            let visible = admin || text(&value, "initiator_id").as_deref() == Some(actor);
            let held = matches!(
                state.as_str(),
                "reconciliation_required" | "reconciliation_held"
            ) || (kind == "removal" && state != "completed");
            observation.operations.push(Lifecycle {
                repository_id: observation.repository_id.clone().unwrap_or_default(),
                kind: kind.into(),
                state,
                operation_id: if visible { id } else { None },
                updated_at: text(&value, "updated_at"),
                reason: if visible {
                    text(&value, "outcome_detail").map(|s| redact_vcs_error_detail(&s))
                } else {
                    None
                },
                target_ref: if visible {
                    match kind {
                        "late_pair" => text(&value, "git_branch"),
                        "svn_commit" => {
                            text(&value, "target_svn_branch_path").filter(|s| !s.is_empty())
                        }
                        _ => text(&value, "intended_ref")
                            .or_else(|| text(&value, "pre_push_git_branch")),
                    }
                } else {
                    None
                },
                intended_git_sha: if visible {
                    text(&value, "intended_git_sha")
                        .or_else(|| text(&value, "intended_local_git_sha"))
                } else {
                    None
                },
                confirmed_svn_revision: if visible {
                    value.get("last_confirmed_svn_rev").and_then(Value::as_i64)
                } else {
                    None
                },
                history_evidence: None,
                target_evidence: if visible {
                    target_evidence(kind, &value)
                } else {
                    None
                },
                held,
                details_visible: visible,
            });
        }
        Ok(None) => {}
        Err(_) => {
            observation.evidence_complete = false;
            observation.operations.push(Lifecycle {
                repository_id: observation.repository_id.clone().unwrap_or_default(),kind:kind.into(),state:"unknown".into(),operation_id:None,updated_at:None,
                reason:Some("Durable operation evidence could not be validated; its outcome and hold remain unknown.".into()),
                target_ref:None,intended_git_sha:None,confirmed_svn_revision:None,history_evidence:None,target_evidence:None,held:false,details_visible:false,
            });
        }
    }
}

pub fn repository(state: &AppState, repo: &Repository, actor: &str, admin: bool) -> Observation {
    let db = &state.db;
    let mut out = Observation::unknown("repository");
    out.repository_id = Some(repo.id.clone());
    out.configured = if repo.enabled { "enabled" } else { "disabled" }.into();
    out.evidence_complete = true;
    out.registry_busy = Some(reposync_core::busy::is_busy(&repo.id));
    out.last_successful_sync_at = repo
        .last_sync_at
        .as_ref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .filter(|d| *d <= Utc::now())
        .map(|d| d.to_rfc3339());
    out.data_freshness = match (&repo.last_sync_at, &out.last_successful_sync_at) {
        (None, _) => "never_recorded",
        (Some(_), Some(_)) => "recorded_success",
        _ => "unknown",
    }
    .into();
    add_operation(
        &mut out,
        "import",
        db.latest_import_operation(&repo.id),
        actor,
        admin,
    );
    add_operation(
        &mut out,
        "import",
        db.active_import_operation(&repo.id),
        actor,
        admin,
    );
    add_operation(
        &mut out,
        "svn_commit",
        db.latest_svn_commit_operation(&repo.id),
        actor,
        admin,
    );
    add_operation(
        &mut out,
        "svn_commit",
        db.active_svn_commit_operation(&repo.id),
        actor,
        admin,
    );
    add_operation(
        &mut out,
        "git_push",
        db.latest_git_push_operation(&repo.id),
        actor,
        admin,
    );
    add_operation(
        &mut out,
        "git_push",
        db.active_git_push_operation(&repo.id),
        actor,
        admin,
    );
    add_operation(
        &mut out,
        "removal",
        db.managed_removal(&repo.id),
        actor,
        admin,
    );
    add_operation(
        &mut out,
        "late_pair",
        db.latest_late_pair_publish_operation(&repo.id),
        actor,
        admin,
    );
    match db.get_state(&format!("late_pair_publish_v1:active:{}", repo.id)) {
        Ok(Some(id)) => add_operation(
            &mut out,
            "late_pair",
            db.get_late_pair_publish_operation(&repo.id, &id),
            actor,
            admin,
        ),
        Ok(None) => {}
        Err(_) => out.evidence_complete = false,
    }
    // Public validated readers can return None for a dangling/foreign pointer. Do not call that quiet.
    for (prefix, kind) in [
        ("import_operation_v1", "import"),
        ("git_to_svn_commit_v1", "svn_commit"),
        ("svn_to_git_push_v1", "git_push"),
        ("late_pair_publish_v1", "late_pair"),
        ("managed_remove_v1", "removal"),
    ] {
        for pointer in ["active", "latest"] {
            match db.get_state(&format!("{prefix}:{pointer}:{}", repo.id)) {
                Ok(Some(id)) if !out.seen_operations.contains(&(kind.to_owned(), id.clone())) => {
                    out.evidence_complete = false
                }
                Err(_) => out.evidence_complete = false,
                Ok(Some(_)) if kind == "import" && pointer == "active" => {
                    for op in &mut out.operations {
                        if op.kind == kind
                            && matches!(op.state.as_str(), "cancelled" | "failed" | "completed")
                        {
                            op.held = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    match db.managed_remove_blocks_new_work(&repo.id) {
        Ok(true) => {
            let mut found = false;
            for op in &mut out.operations {
                if op.kind == "removal" {
                    op.held = true;
                    found = true;
                }
            }
            if !found {
                out.evidence_complete = false;
                out.operations.push(Lifecycle {repository_id:repo.id.clone(),kind:"removal".into(),state:"unknown_or_held".into(),operation_id:None,updated_at:None,reason:Some("The existing removal guard blocks new work, but a valid scoped operation document is unavailable. The pointer/hold was preserved.".into()),target_ref:None,intended_git_sha:None,confirmed_svn_revision:None,history_evidence:None,target_evidence:None,held:true,details_visible:false});
            }
        }
        Ok(false) => {}
        Err(_) => out.evidence_complete = false,
    }
    // Reuse the complete authenticated journal's existing refusal; do not parse its private schema,
    // turn a validation error into healthy, or invent an operation ID/ref that the reader cannot expose.
    if reposync_core::db::pair_refresh_operations::require_no_pair_refresh_hold(
        &db.conn(),
        Some(&repo.id),
    )
    .is_err()
    {
        out.evidence_complete = false;
        out.operations.push(Lifecycle {repository_id:repo.id.clone(),kind:"pair_refresh".into(),state:"unknown_or_held".into(),operation_id:None,updated_at:None,
            reason:Some("The authenticated pair-refresh reader refused quiet status: a durable hold or invalid history requires its existing recovery inspection.".into()),target_ref:None,intended_git_sha:None,confirmed_svn_revision:None,history_evidence:None,target_evidence:None,held:true,details_visible:false});
    }
    // Do not call resolve_repo_import_baseline here: its legacy path can backfill KV.
    // Read matching stored facts only; never repair or manufacture provenance in a status read.
    let scoped_svn = db.get_state(&format!("last_svn_rev_{}", repo.id));
    let scoped_git = reposync_core::echo_receipt_scope::read_scoped_last_git_sha_kv(db, &repo.id);
    let baseline_pending = repo.last_svn_rev == 0
        && repo.last_git_sha.is_empty()
        && matches!(&scoped_svn, Ok(None))
        && matches!(&scoped_git, Ok(None));
    let stored_consistent = matches!(&scoped_svn,Ok(Some(value)) if value.parse::<i64>().ok()==Some(repo.last_svn_rev) && repo.last_svn_rev>0)
        && reposync_core::history_inspect::is_full_git_oid(&repo.last_git_sha);
    if !baseline_pending && !stored_consistent {
        out.evidence_complete = false;
    }
    if scoped_git.is_err()
        || scoped_svn.is_err()
        || (repo.last_sync_at.is_some() && out.last_successful_sync_at.is_none())
    {
        out.evidence_complete = false;
    }
    match reposync_core::history_inspect::load_history_block_for_repo(db, &repo.id) {
        Ok(Some(block)) => {
            let valid = valid_history_record(&block, &repo.id);
            let durable = reposync_core::history_inspect::is_durable_history_record(&block);
            if !valid {
                out.evidence_complete = false;
                out.operations.push(Lifecycle {repository_id:repo.id.clone(),kind:"history".into(),state:if durable {"unknown_or_held"} else {"unknown"}.into(),operation_id:None,updated_at:None,reason:Some("History evidence is malformed, unsupported or foreign. No tips or completion were inferred and no record was changed.".into()),target_ref:None,intended_git_sha:None,confirmed_svn_revision:None,history_evidence:None,target_evidence:None,held:durable,details_visible:false});
            } else {
                let visible = admin;
                let history = serde_json::json!({
                    "handled_checkpoint":block.get("p_handled").and_then(Value::as_str).filter(|v|reposync_core::history_inspect::is_full_git_oid(v)),
                    "prior_observed_tip":block.get("o_prior_observed").and_then(Value::as_str).filter(|v|reposync_core::history_inspect::is_full_git_oid(v)),
                    "fresh_observed_remote_tip":block.get("r_fresh_remote").and_then(Value::as_str).filter(|v|reposync_core::history_inspect::is_full_git_oid(v)),
                    "local_bridge_tip":block.get("l_bridge").and_then(Value::as_str).filter(|v|reposync_core::history_inspect::is_full_git_oid(v)),
                });
                out.operations.push(Lifecycle {
                    repository_id: repo.id.clone(),
                    kind: "history".into(),
                    state: if durable {
                        "reconciliation_required"
                    } else {
                        "transient_refusal"
                    }
                    .into(),
                    operation_id: None,
                    updated_at: text(&block, "observed_at"),
                    reason: Some(if visible {
                        redact_vcs_error_detail(&format!(
                            "{}: {}",
                            block
                                .get("reason")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown_history_reason"),
                            block
                                .get("detail")
                                .and_then(Value::as_str)
                                .unwrap_or("No detail recorded")
                        ))
                    } else {
                        "History evidence requires an administrator or contains a scope mismatch."
                            .into()
                    }),
                    target_ref: None,
                    intended_git_sha: None,
                    confirmed_svn_revision: None,
                    history_evidence: visible.then_some(history),
                    target_evidence: None,
                    held: durable,
                    details_visible: visible,
                });
            }
        }
        Ok(None) => {}
        Err(_) => out.evidence_complete = false,
    }
    let held =
        out.operations.iter().any(|op| op.held) || repo.sync_status == "reconciliation_required";
    let running = out.operations.iter().any(|op| {
        matches!(
            op.state.as_str(),
            "queued"
                | "running"
                | "cancel_requested"
                | "cancelling"
                | "svn_copy_pending"
                | "svn_copied"
                | "child_registered"
                | "replay_in_progress"
        )
    });
    let failed = out.operations.iter().any(|op| op.state == "failed");
    out.health = if held {
        Health::Held
    } else if !out.evidence_complete
        || !matches!(
            repo.sync_status.as_str(),
            "idle"
                | "initializing"
                | "detecting"
                | "applying"
                | "syncing"
                | "error_paused"
                | "conflict_found"
                | "error"
                | "failed"
        )
    {
        Health::Unknown
    } else if !state.scheduler_service_state().is_running() {
        Health::Error
    } else if repo.sync_status == "error_paused" {
        Health::Paused
    } else if out.registry_busy == Some(true) {
        Health::Busy
    } else if running {
        Health::Running
    } else if matches!(repo.sync_status.as_str(), "error" | "failed") || failed {
        Health::Error
    } else if repo.sync_status == "conflict_found"
        || out
            .operations
            .iter()
            .any(|op| op.kind == "history" && op.state == "transient_refusal")
    {
        Health::Conflict
    } else if !repo.enabled {
        Health::Disabled
    } else if matches!(
        repo.sync_status.as_str(),
        "detecting" | "applying" | "syncing"
    ) {
        Health::Running
    } else if repo.sync_status == "initializing" || baseline_pending {
        Health::Initializing
    } else if repo.sync_status == "idle" {
        Health::Idle
    } else {
        Health::Unknown
    };
    out.safe_next_action=if held {"Inspect the retained operation, intended ref and confirmed checkpoints with the existing reconciliation workflow. Skip, Retry and error acknowledgement do not prove reconciliation or release the hold."}
        else if out.health==Health::Unknown {"Refresh status and inspect scoped evidence. Unknown is not idle and does not authorize a retry."}
        else if out.health==Health::Paused {"Inspect the recorded failure and pending work. A retry or skip is not proof of reconciliation."}
        else {"Inspect the latest observation and operation evidence. Registry idle and terminal outcomes are not descendant-cleanup or remote-freshness proof."}.into();
    out
}

pub fn aggregate(mut observations: Vec<Observation>) -> Observation {
    let mut out = Observation::unknown("aggregate");
    if observations.is_empty() {
        return out;
    }
    fn priority(h: Health) -> u8 {
        match h {
            Health::Held => 10,
            Health::Unknown => 9,
            Health::Error => 8,
            Health::Paused => 7,
            Health::Conflict => 6,
            Health::Busy => 5,
            Health::Running => 4,
            Health::Initializing => 3,
            Health::Idle => 2,
            Health::Disabled => 1,
        }
    }
    out.health = observations
        .iter()
        .map(|o| o.health)
        .max_by_key(|h| priority(*h))
        .unwrap_or(Health::Unknown);
    out.evidence_complete = observations.iter().all(|o| o.evidence_complete);
    out.configured = if observations
        .iter()
        .all(|o| o.configured == observations[0].configured)
    {
        observations[0].configured.clone()
    } else {
        "mixed".into()
    };
    out.data_freshness = "multiple_scoped_records".into();
    for o in &mut observations {
        out.operations.append(&mut o.operations);
    }
    out
}
