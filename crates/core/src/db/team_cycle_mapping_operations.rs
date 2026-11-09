//! Bounded v12 operation journal for ordinary team-cycle mapping recordings
//! that do not go through the per-write SVN→Git push or Git→SVN commit journals.
//!
//! Covers metadata-only SVN revisions and intentional Git no-target outcomes
//! (filtered, empty, verified no-delta) in the bidirectional sync cycle.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Database;
use crate::errors::DatabaseError;

const PREFIX: &str = "team_cycle_mapping_v1:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamCycleMappingState {
    Queued,
    Running,
    Completed,
    Failed,
    ReconciliationRequired,
}

impl TeamCycleMappingState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::ReconciliationRequired
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamCycleMappingDirection {
    SvnToGit,
    GitToSvn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamCycleMappingOutcome {
    SvnNoTargetContent,
    /// #53: an SVN revision whose selected Git-content effect is zero under
    /// the active path policy. Only a verified SVN projection can begin or
    /// finalize it; the generic journal API refuses this outcome.
    SvnFiltered,
    GitFiltered,
    GitEmptyCommit,
    GitNoSvnDelta,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamCycleMappingOperation {
    pub version: u8,
    pub id: String,
    pub repo_id: String,
    pub operation_type: String,
    pub initiator_id: String,
    pub request_id: String,
    pub target_fingerprint: String,
    pub created_at: String,
    pub updated_at: String,
    pub state: TeamCycleMappingState,
    pub direction: TeamCycleMappingDirection,
    pub outcome: TeamCycleMappingOutcome,
    pub source_svn_rev: Option<i64>,
    pub source_git_sha: Option<String>,
    pub pre_write_svn_rev: i64,
    pub pre_write_git_sha: String,
    pub projection: String,
    pub intended_target_proof: Option<serde_json::Value>,
    pub observed_target_proof: Option<serde_json::Value>,
    #[serde(default)]
    pub resume_authorized: bool,
    pub outcome_detail: Option<String>,
}

pub struct TeamCycleMappingIntent<'a> {
    pub repo_id: &'a str,
    pub initiator_id: &'a str,
    pub request_id: &'a str,
    pub target_fingerprint: &'a str,
    pub direction: TeamCycleMappingDirection,
    pub outcome: TeamCycleMappingOutcome,
    pub source_svn_rev: Option<i64>,
    pub source_git_sha: Option<&'a str>,
    pub pre_write_svn_rev: i64,
    pub pre_write_git_sha: &'a str,
    pub projection: &'a str,
    pub intended_target_proof: Option<&'a serde_json::Value>,
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
         ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        params![name, value, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn parse(raw: &str) -> Result<TeamCycleMappingOperation, DatabaseError> {
    let op: TeamCycleMappingOperation = serde_json::from_str(raw)
        .map_err(|e| DatabaseError::Other(format!("invalid team cycle mapping operation: {e}")))?;
    if op.version != 1 {
        return Err(DatabaseError::Other(
            "unsupported team cycle mapping operation version".into(),
        ));
    }
    if op.operation_type != "team_cycle_mapping" {
        return Err(DatabaseError::Other(
            "unsupported team cycle mapping operation type".into(),
        ));
    }
    Ok(op)
}

pub fn team_cycle_mapping_fingerprint(repo_id: &str, projection: &str) -> String {
    let source = serde_json::json!({
        "repo_id": repo_id,
        "projection": projection,
    })
    .to_string();
    hex::encode(Sha256::digest(source.as_bytes()))
}

fn write_op(tx: &Connection, op: &TeamCycleMappingOperation) -> Result<(), DatabaseError> {
    write_value(
        tx,
        &key("document", &op.id),
        &serde_json::to_string(op).map_err(|e| DatabaseError::Other(e.to_string()))?,
    )
}

fn clear_active(tx: &Connection, repo_id: &str, op_id: &str) -> Result<(), DatabaseError> {
    tx.execute(
        "DELETE FROM kv_state WHERE key=?1 AND value=?2",
        params![key("active", repo_id), op_id],
    )?;
    Ok(())
}

fn advance_svn_only_tx(tx: &Connection, repo_id: &str, svn_rev: i64) -> Result<(), DatabaseError> {
    let updated = tx.execute(
        "UPDATE repositories SET last_svn_rev = MAX(last_svn_rev, ?1) WHERE id = ?2",
        params![svn_rev, repo_id],
    )?;
    if updated != 1 {
        return Err(DatabaseError::Other(
            "team cycle mapping lost its repository registration".into(),
        ));
    }
    write_value(
        tx,
        &format!("last_svn_rev_{}", repo_id),
        &svn_rev.to_string(),
    )?;
    Ok(())
}

fn advance_svn_with_receipt_tx(
    tx: &Connection,
    repo_id: &str,
    svn_rev: i64,
    receipt: serde_json::Value,
) -> Result<(), DatabaseError> {
    if svn_rev <= 0 {
        return Err(DatabaseError::Other(
            "team cycle mapping lacks a positive SVN revision".into(),
        ));
    }
    advance_svn_only_tx(tx, repo_id, svn_rev)?;
    let generation = crate::echo_receipt_scope::repo_echo_generation_tx(tx, repo_id)?;
    let scope = crate::db::repo_scope_identity::repository_scope_uuid(tx, repo_id)?;
    let mut receipt = receipt;
    crate::echo_receipt_scope::attach_generation_to_receipt(&mut receipt, generation);
    crate::db::repo_scope_identity::attach_scope_uuid_to_receipt(&mut receipt, &scope);
    let receipt_key =
        crate::echo_receipt_scope::handled_svn_no_target_state_key(&scope, generation, svn_rev);
    write_value(tx, &receipt_key, &receipt.to_string())?;
    if crate::db::repo_scope_identity::legacy_repo_id_kv_authoritative(tx)? {
        let legacy_key = crate::echo_receipt_scope::handled_svn_no_target_state_key(
            repo_id, generation, svn_rev,
        );
        write_value(tx, &legacy_key, &receipt.to_string())?;
    }
    Ok(())
}

fn advance_git_with_receipt_tx(
    tx: &Connection,
    repo_id: &str,
    git_sha: &str,
    receipt: serde_json::Value,
) -> Result<(), DatabaseError> {
    if git_sha.len() != 40 || !git_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DatabaseError::Other(
            "team cycle mapping lacks a full Git SHA".into(),
        ));
    }
    let updated = tx.execute(
        "UPDATE repositories SET last_git_sha = ?1 WHERE id = ?2",
        params![git_sha, repo_id],
    )?;
    if updated != 1 {
        return Err(DatabaseError::Other(
            "team cycle mapping lost its repository registration".into(),
        ));
    }
    let generation = crate::echo_receipt_scope::repo_echo_generation_tx(tx, repo_id)?;
    let scope = crate::db::repo_scope_identity::repository_scope_uuid(tx, repo_id)?;
    let mut receipt = receipt;
    crate::echo_receipt_scope::attach_generation_to_receipt(&mut receipt, generation);
    crate::db::repo_scope_identity::attach_scope_uuid_to_receipt(&mut receipt, &scope);
    let projection = receipt["projection"].as_str().unwrap_or("");
    let receipt_proves_kv = crate::echo_suppression::verify_no_target_receipt(
        &receipt,
        repo_id,
        git_sha,
        projection,
        generation,
        Some(&scope),
    ) == crate::echo_suppression::NoTargetReceiptVerdict::Accepted;
    if receipt_proves_kv {
        let now = chrono::Utc::now().to_rfc3339();
        crate::echo_receipt_scope::write_scoped_last_git_sha_kv(tx, repo_id, git_sha, &now)?;
    }
    crate::echo_receipt_scope::write_git_no_target_receipt_kv(
        tx,
        repo_id,
        generation,
        git_sha,
        &receipt.to_string(),
        &chrono::Utc::now().to_rfc3339(),
    )?;
    Ok(())
}

fn finalize_tx(
    tx: &Connection,
    mut op: TeamCycleMappingOperation,
    observed_proof: Option<serde_json::Value>,
    filtered: Option<&crate::sync_engine::svn_projection_53::VerifiedSvnProjection<'_, '_>>,
) -> Result<TeamCycleMappingOperation, DatabaseError> {
    if op.intended_target_proof.is_some() && observed_proof != op.intended_target_proof {
        return Err(DatabaseError::Other(
            "observed target proof differs from the recorded intent".into(),
        ));
    }
    match (&op.direction, &op.outcome) {
        (TeamCycleMappingDirection::SvnToGit, TeamCycleMappingOutcome::SvnFiltered) => {
            finalize_svn_filtered_tx(tx, &op, filtered)?;
        }
        (TeamCycleMappingDirection::SvnToGit, TeamCycleMappingOutcome::SvnNoTargetContent) => {
            let svn_rev = op.source_svn_rev.ok_or_else(|| {
                DatabaseError::Other("svn no-target mapping lacks source rev".into())
            })?;
            if svn_rev <= op.pre_write_svn_rev {
                return Err(DatabaseError::Other(
                    "observed SVN revision is not after the pre-write revision".into(),
                ));
            }
            let receipt = serde_json::json!({
                "version": 1,
                "repo_id": op.repo_id,
                "svn_revision": svn_rev,
                "outcome": "no_git_content",
                "projection": op.projection,
            });
            advance_svn_with_receipt_tx(tx, &op.repo_id, svn_rev, receipt)?;
            let source_key = super::lineage_checkpoints::svn_source_key(svn_rev);
            super::lineage_checkpoints::try_persist_directional_checkpoint_tx(
                tx,
                &op.repo_id,
                super::lineage_checkpoints::MappingWrite {
                    direction: super::lineage_checkpoints::CheckpointDirection::SvnToGit,
                    source_key: &source_key,
                    outcome:
                        super::lineage_checkpoints::MappingOutcome::IntentionallyFilteredNoTarget,
                    target_git_sha: None,
                    target_svn_rev: None,
                    reason: Some("no_git_content"),
                    policy_snapshot: Some(op.projection.as_str()),
                },
            )?;
        }
        (
            TeamCycleMappingDirection::GitToSvn,
            TeamCycleMappingOutcome::GitFiltered | TeamCycleMappingOutcome::GitEmptyCommit,
        ) => {
            let git_sha = op.source_git_sha.as_deref().ok_or_else(|| {
                DatabaseError::Other("git no-target mapping lacks source sha".into())
            })?;
            let outcome = match op.outcome {
                TeamCycleMappingOutcome::GitFiltered => "filtered",
                TeamCycleMappingOutcome::GitEmptyCommit => "empty_commit",
                _ => unreachable!(),
            };
            let receipt = serde_json::json!({
                "version": 1,
                "repo_id": op.repo_id,
                "git_sha": git_sha,
                "outcome": outcome,
                "projection": op.projection,
            });
            advance_git_with_receipt_tx(tx, &op.repo_id, git_sha, receipt)?;
            let source_key = super::lineage_checkpoints::git_source_key(git_sha);
            super::lineage_checkpoints::try_persist_directional_checkpoint_tx(
                tx,
                &op.repo_id,
                super::lineage_checkpoints::MappingWrite {
                    direction: super::lineage_checkpoints::CheckpointDirection::GitToSvn,
                    source_key: &source_key,
                    outcome:
                        super::lineage_checkpoints::MappingOutcome::IntentionallyFilteredNoTarget,
                    target_git_sha: None,
                    target_svn_rev: None,
                    reason: Some(outcome),
                    policy_snapshot: Some(op.projection.as_str()),
                },
            )?;
        }
        (TeamCycleMappingDirection::GitToSvn, TeamCycleMappingOutcome::GitNoSvnDelta) => {
            let git_sha = op.source_git_sha.as_deref().ok_or_else(|| {
                DatabaseError::Other("git no-delta mapping lacks source sha".into())
            })?;
            let target = observed_proof
                .clone()
                .or_else(|| op.intended_target_proof.clone())
                .ok_or_else(|| {
                    DatabaseError::Other("git no-delta mapping lacks target proof".into())
                })?;
            let receipt = serde_json::json!({
                "version": 3,
                "repo_id": op.repo_id,
                "git_sha": git_sha,
                "outcome": "no_svn_delta",
                "projection": op.projection,
                "target": target,
            });
            advance_git_with_receipt_tx(tx, &op.repo_id, git_sha, receipt)?;
            let source_key = super::lineage_checkpoints::git_source_key(git_sha);
            super::lineage_checkpoints::try_persist_directional_checkpoint_tx(
                tx,
                &op.repo_id,
                super::lineage_checkpoints::MappingWrite {
                    direction: super::lineage_checkpoints::CheckpointDirection::GitToSvn,
                    source_key: &source_key,
                    outcome:
                        super::lineage_checkpoints::MappingOutcome::IntentionallyFilteredNoTarget,
                    target_git_sha: None,
                    target_svn_rev: None,
                    reason: Some("no_svn_delta"),
                    policy_snapshot: Some(op.projection.as_str()),
                },
            )?;
        }
        _ => {
            return Err(DatabaseError::Other(
                "unsupported team cycle mapping direction/outcome pair".into(),
            ));
        }
    }
    let now = Utc::now().to_rfc3339();
    op.state = TeamCycleMappingState::Completed;
    op.observed_target_proof = observed_proof;
    op.resume_authorized = false;
    op.outcome_detail = Some("team cycle mapping verified and recorded".into());
    op.updated_at = now;
    write_op(tx, &op)?;
    clear_active(tx, &op.repo_id, &op.id)?;
    Ok(op)
}

impl Database {
    pub fn get_team_cycle_mapping_operation(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> Result<Option<TeamCycleMappingOperation>, DatabaseError> {
        let conn = self.conn();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM kv_state WHERE key=?1",
                [key("document", op_id)],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|v| parse(&v)).transpose().map(|op| {
            if op.as_ref().is_some_and(|o| o.repo_id != repo_id) {
                None
            } else {
                op
            }
        })
    }

    pub fn active_team_cycle_mapping_operation(
        &self,
        repo_id: &str,
    ) -> Result<Option<TeamCycleMappingOperation>, DatabaseError> {
        let id = self.get_state(&key("active", repo_id))?;
        id.map(|id| self.get_team_cycle_mapping_operation(repo_id, &id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn has_any_blocking_team_cycle_mapping_hold(&self) -> Result<bool, DatabaseError> {
        let conn = self.conn();
        let mut query = conn.prepare(
            "SELECT key,value FROM kv_state WHERE key LIKE 'team_cycle_mapping_v1:active:%'",
        )?;
        let rows = query
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(query);
        drop(conn);
        for (active_key, id) in rows {
            let repo_id = active_key.strip_prefix(&key("active", "")).ok_or_else(|| {
                DatabaseError::Other("invalid active team cycle mapping key".into())
            })?;
            if let Some(op) = self.get_team_cycle_mapping_operation(repo_id, &id)? {
                if op.state == TeamCycleMappingState::ReconciliationRequired
                    && !op.resume_authorized
                {
                    return Ok(true);
                }
                if !op.state.is_terminal() {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub fn begin_team_cycle_mapping(
        &self,
        intent: TeamCycleMappingIntent<'_>,
    ) -> Result<TeamCycleMappingOperation, DatabaseError> {
        if intent.outcome == TeamCycleMappingOutcome::SvnFiltered {
            return Err(DatabaseError::Other(
                "a filtered SVN team cycle mapping requires a verified SVN projection".into(),
            ));
        }
        self.transaction(|tx| begin_team_cycle_mapping_tx(tx, intent))
    }

    pub fn hold_team_cycle_mapping_reconciliation(
        &self,
        repo_id: &str,
        op_id: &str,
        detail: &str,
    ) -> Result<TeamCycleMappingOperation, DatabaseError> {
        self.update_team_cycle_mapping_operation(repo_id, op_id, |op| {
            if op.state == TeamCycleMappingState::Completed {
                return Err(DatabaseError::Other(
                    "terminal team cycle mapping cannot be rewritten".into(),
                ));
            }
            if op.state == TeamCycleMappingState::ReconciliationRequired {
                op.outcome_detail = Some(detail.into());
                return Ok(());
            }
            op.state = TeamCycleMappingState::ReconciliationRequired;
            op.resume_authorized = false;
            op.outcome_detail = Some(detail.into());
            Ok(())
        })
    }

    pub fn confirm_team_cycle_mapping(
        &self,
        repo_id: &str,
        op_id: &str,
        observed_proof: Option<serde_json::Value>,
    ) -> Result<TeamCycleMappingOperation, DatabaseError> {
        self.transaction(|tx| {
            confirm_team_cycle_mapping_tx(tx, repo_id, op_id, observed_proof, None)
        })
    }

    fn update_team_cycle_mapping_operation<F>(
        &self,
        repo_id: &str,
        op_id: &str,
        edit: F,
    ) -> Result<TeamCycleMappingOperation, DatabaseError>
    where
        F: FnOnce(&mut TeamCycleMappingOperation) -> Result<(), DatabaseError>,
    {
        self.transaction(|tx| {
            super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, repo_id)?;
            if read_value(tx, &key("active", repo_id))?.as_deref() != Some(op_id) {
                return Err(DatabaseError::Other(
                    "stale or inactive team cycle mapping operation".into(),
                ));
            }
            let document = key("document", op_id);
            let mut op = parse(&read_value(tx, &document)?.ok_or_else(|| {
                DatabaseError::Other("missing team cycle mapping document".into())
            })?)?;
            if op.repo_id != repo_id {
                return Err(DatabaseError::Other(
                    "team cycle mapping repository mismatch".into(),
                ));
            }
            let original = op.clone();
            edit(&mut op)?;
            if op == original {
                return Ok(op);
            }
            op.updated_at = Utc::now().to_rfc3339();
            write_op(tx, &op)?;
            if op.state == TeamCycleMappingState::Completed {
                clear_active(tx, repo_id, op_id)?;
            }
            Ok(op)
        })
    }

    pub fn hold_interrupted_team_cycle_mappings(&self) -> Result<(), DatabaseError> {
        let active = {
            let conn = self.conn();
            let mut query = conn.prepare(
                "SELECT key,value FROM kv_state WHERE key LIKE 'team_cycle_mapping_v1:active:%'",
            )?;
            let rows = query
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for (active_key, id) in active {
            let repo_id = active_key.strip_prefix(&key("active", "")).ok_or_else(|| {
                DatabaseError::Other("invalid active team cycle mapping key".into())
            })?;
            let op = self
                .get_team_cycle_mapping_operation(repo_id, &id)?
                .ok_or_else(|| {
                    DatabaseError::Other(
                        "active team cycle mapping document missing or misowned".into(),
                    )
                })?;
            if !op.state.is_terminal() {
                self.update_team_cycle_mapping_operation(repo_id, &op.id, |o| {
                    o.state = TeamCycleMappingState::ReconciliationRequired;
                    o.resume_authorized = false;
                    o.outcome_detail = Some(
                        "worker stopped before a verified team cycle mapping; inspect the exact target"
                            .into(),
                    );
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::echo_suppression::{
        classify_incoming_svn_revision, verify_svn_no_target_receipt, EchoDisposition,
        NoTargetReceiptVerdict, TeamEchoContext,
    };

    #[test]
    fn svn_no_target_confirm_advances_watermark() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO repositories (id, name, svn_url, svn_branch, svn_username, git_api_url, git_repo, git_branch, enabled, created_at, updated_at, last_svn_rev, last_git_sha)
                 VALUES ('pair', 'pair', 'file:///svn', '', 'alice', 'file:///git', 'pair', 'main', 1, 't', 't', 1, ?1)",
                ["a".repeat(40)],
            )
            .unwrap();
        let inbound_git_before =
            crate::echo_receipt_scope::read_scoped_last_git_sha_kv(&db, "pair").unwrap();
        let fingerprint = team_cycle_mapping_fingerprint("pair", "unfiltered");
        let op = db
            .begin_team_cycle_mapping(TeamCycleMappingIntent {
                repo_id: "pair",
                initiator_id: "team_worker",
                request_id: "svn-r2",
                target_fingerprint: &fingerprint,
                direction: TeamCycleMappingDirection::SvnToGit,
                outcome: TeamCycleMappingOutcome::SvnNoTargetContent,
                source_svn_rev: Some(2),
                source_git_sha: None,
                pre_write_svn_rev: 1,
                pre_write_git_sha: "a".repeat(40).as_str(),
                projection: "unfiltered",
                intended_target_proof: None,
            })
            .unwrap();
        db.confirm_team_cycle_mapping("pair", &op.id, None).unwrap();
        let (svn_rev, _) = db.get_repo_watermark("pair").unwrap();
        assert_eq!(svn_rev, 2);
        assert_eq!(
            db.get_state("last_svn_rev").unwrap(),
            None,
            "managed team svn no-target must not write global last_svn_rev"
        );
        assert_eq!(
            db.get_state("last_svn_rev_pair").unwrap().as_deref(),
            Some("2")
        );
        let receipt = crate::echo_receipt_scope::read_svn_no_target_receipt(&db, "pair", 2)
            .unwrap()
            .unwrap();
        let scope =
            crate::db::repo_scope_identity::repository_scope_uuid(&db.conn(), "pair").unwrap();
        assert_eq!(
            verify_svn_no_target_receipt(&receipt, "pair", 2, "unfiltered", 1, Some(&scope)),
            NoTargetReceiptVerdict::Accepted
        );
        let echo_ctx = TeamEchoContext {
            db: &db,
            repo_id: "pair",
            no_target_projection: "unfiltered",
        };
        assert_eq!(
            classify_incoming_svn_revision(&echo_ctx, 2, "no marker").unwrap(),
            EchoDisposition::SkipEcho
        );
        let emitted_tip: String = db
            .conn()
            .query_row(
                "SELECT last_git_sha FROM repositories WHERE id='pair'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(emitted_tip, "a".repeat(40));
        assert_eq!(
            crate::echo_receipt_scope::read_scoped_last_git_sha_kv(&db, "pair").unwrap(),
            inbound_git_before,
            "svn no-target must not move inbound git cursor"
        );
        assert!(db
            .active_team_cycle_mapping_operation("pair")
            .unwrap()
            .is_none());
    }

    #[test]
    fn git_filtered_confirm_writes_receipt() {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        let git_sha = "b".repeat(40);
        db.conn()
            .execute(
                "INSERT INTO repositories (id, name, svn_url, svn_branch, svn_username, git_api_url, git_repo, git_branch, enabled, created_at, updated_at, last_svn_rev, last_git_sha)
                 VALUES ('pair', 'pair', 'file:///svn', '', 'alice', 'file:///git', 'pair', 'main', 1, 't', 't', 1, ?1)",
                ["a".repeat(40)],
            )
            .unwrap();
        let fingerprint = team_cycle_mapping_fingerprint("pair", "unfiltered");
        let op = db
            .begin_team_cycle_mapping(TeamCycleMappingIntent {
                repo_id: "pair",
                initiator_id: "team_worker",
                request_id: &git_sha,
                target_fingerprint: &fingerprint,
                direction: TeamCycleMappingDirection::GitToSvn,
                outcome: TeamCycleMappingOutcome::GitFiltered,
                source_svn_rev: None,
                source_git_sha: Some(&git_sha),
                pre_write_svn_rev: 1,
                pre_write_git_sha: "a".repeat(40).as_str(),
                projection: "unfiltered",
                intended_target_proof: None,
            })
            .unwrap();
        db.confirm_team_cycle_mapping("pair", &op.id, None).unwrap();
        let (_, emitted) = db.get_repo_watermark("pair").unwrap();
        assert_eq!(emitted, git_sha);
        assert_eq!(
            db.get_state("last_git_hash").unwrap(),
            None,
            "managed team git no-target must not write global last_git_hash"
        );
        assert_eq!(
            crate::echo_receipt_scope::read_scoped_last_git_sha_kv(&db, "pair")
                .unwrap()
                .as_deref(),
            Some(git_sha.as_str())
        );
        let receipt = crate::echo_receipt_scope::read_git_no_target_receipt(&db, "pair", &git_sha)
            .unwrap()
            .unwrap();
        assert_eq!(receipt["outcome"], "filtered");
    }
}

/// Canonical reason recorded for a #53 filtered SVN revision. It is distinct
/// from the legacy `no_git_content` reason of metadata-only revisions.
pub(crate) const SVN_FILTERED_REASON: &str = "svn_filtered_no_git_content";

/// The one shared begin core: the original `begin_team_cycle_mapping`
/// transaction body, moved here unchanged. Both the generic API and the typed
/// filtered begin run it inside their own transaction.
// Kept byte-for-byte as moved from the original method (dedented only),
// so the moved body can be compared exactly with its preimage.
#[rustfmt::skip]
fn begin_team_cycle_mapping_tx(
    tx: &Connection,
    intent: TeamCycleMappingIntent<'_>,
) -> Result<TeamCycleMappingOperation, DatabaseError> {
    super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(
        tx,
        intent.repo_id,
    )?;
    let active = key("active", intent.repo_id);
    if read_value(tx, &active)?.is_some() {
        return Err(DatabaseError::Other(
            "repository has an active or held team cycle mapping".into(),
        ));
    }
    if super::managed_remove::new_work_blocked(tx, intent.repo_id)? {
        return Err(DatabaseError::Other(
            "repository removal blocks a new team cycle mapping".into(),
        ));
    }
    let now = Utc::now().to_rfc3339();
    let op = TeamCycleMappingOperation {
        version: 1,
        id: Uuid::new_v4().to_string(),
        repo_id: intent.repo_id.into(),
        operation_type: "team_cycle_mapping".into(),
        initiator_id: intent.initiator_id.into(),
        request_id: intent.request_id.into(),
        target_fingerprint: intent.target_fingerprint.into(),
        created_at: now.clone(),
        updated_at: now,
        state: TeamCycleMappingState::Running,
        direction: intent.direction,
        outcome: intent.outcome,
        source_svn_rev: intent.source_svn_rev,
        source_git_sha: intent.source_git_sha.map(str::to_owned),
        pre_write_svn_rev: intent.pre_write_svn_rev,
        pre_write_git_sha: intent.pre_write_git_sha.into(),
        projection: intent.projection.into(),
        intended_target_proof: intent.intended_target_proof.cloned(),
        observed_target_proof: None,
        resume_authorized: false,
        outcome_detail: None,
    };
    write_op(tx, &op)?;
    write_value(tx, &active, &op.id)?;
    write_value(tx, &key("latest", intent.repo_id), &op.id)?;
    Ok(op)
}

/// The one shared confirm core: the original `confirm_team_cycle_mapping`
/// guards and finalization, moved here unchanged except that the optional
/// verified projection reaches `finalize_tx`.
// Kept byte-for-byte as moved from the original method (dedented only),
// so the moved body can be compared exactly with its preimage.
#[rustfmt::skip]
fn confirm_team_cycle_mapping_tx(
    tx: &Connection,
    repo_id: &str,
    op_id: &str,
    observed_proof: Option<serde_json::Value>,
    filtered: Option<&crate::sync_engine::svn_projection_53::VerifiedSvnProjection<'_, '_>>,
) -> Result<TeamCycleMappingOperation, DatabaseError> {
    super::pair_refresh_operations::require_no_pair_refresh_hold_for_writer(tx, repo_id)?;
    if read_value(tx, &key("active", repo_id))?.as_deref() != Some(op_id) {
        return Err(DatabaseError::Other(
            "stale or inactive team cycle mapping operation".into(),
        ));
    }
    let op = parse(&read_value(tx, &key("document", op_id))?.ok_or_else(|| {
        DatabaseError::Other("missing team cycle mapping document".into())
    })?)?;
    if op.repo_id != repo_id || op.state != TeamCycleMappingState::Running {
        return Err(DatabaseError::Other(
            "team cycle mapping is not running".into(),
        ));
    }
    finalize_tx(tx, op, observed_proof, filtered)
}

/// Filtered SVN→Git tail of `finalize_tx`. In the caller's single journal
/// transaction it writes, together: the scoped SVN no-target receipt and
/// watermark, the canonical `intentionally_filtered_no_target` outcome and
/// checkpoint (policy, reason, no targets), and the proven NULL `commit_map`
/// projection. `finalize_tx` then completes the operation. It never moves
/// the inbound Git cursor, never records a Git SHA and reports no remote
/// effect. Any failure rolls the whole transaction back.
fn finalize_svn_filtered_tx(
    tx: &Connection,
    op: &TeamCycleMappingOperation,
    filtered: Option<&crate::sync_engine::svn_projection_53::VerifiedSvnProjection<'_, '_>>,
) -> Result<(), DatabaseError> {
    let Some(proof) = filtered else {
        return Err(DatabaseError::Other(
            "a filtered SVN team cycle mapping can only be finalized with a verified SVN projection"
                .into(),
        ));
    };
    let svn_rev = op
        .source_svn_rev
        .ok_or_else(|| DatabaseError::Other("svn filtered mapping lacks source rev".into()))?;
    if op.repo_id != proof.repo_id()
        || svn_rev != proof.revision()
        || op.projection != proof.policy_snapshot()
    {
        return Err(DatabaseError::Other(
            "svn filtered mapping does not match its verified SVN projection".into(),
        ));
    }
    if op.source_git_sha.is_some() || op.intended_target_proof.is_some() {
        return Err(DatabaseError::Other(
            "svn filtered mapping must not carry a Git source or target proof".into(),
        ));
    }
    if svn_rev <= op.pre_write_svn_rev {
        return Err(DatabaseError::Other(
            "observed SVN revision is not after the pre-write revision".into(),
        ));
    }
    proof.revalidate_tx(tx)?;
    // The projection proved that this revision has no Git content under the
    // active policy, so the existing scoped v1 `no_git_content` receipt holds.
    let receipt = serde_json::json!({
        "version": 1,
        "repo_id": op.repo_id,
        "svn_revision": svn_rev,
        "outcome": "no_git_content",
        "projection": op.projection,
    });
    advance_svn_with_receipt_tx(tx, &op.repo_id, svn_rev, receipt)?;
    // The receipt path resolves the current generation; refuse if that moved.
    proof.revalidate_tx(tx)?;
    let source_key = super::lineage_checkpoints::svn_source_key(svn_rev);
    super::lineage_checkpoints::persist_directional_checkpoint_tx(
        tx,
        proof.scope_uuid(),
        proof.generation(),
        super::lineage_checkpoints::MappingWrite {
            direction: super::lineage_checkpoints::CheckpointDirection::SvnToGit,
            source_key: &source_key,
            outcome: super::lineage_checkpoints::MappingOutcome::IntentionallyFilteredNoTarget,
            target_git_sha: None,
            target_svn_rev: None,
            reason: Some(SVN_FILTERED_REASON),
            policy_snapshot: Some(op.projection.as_str()),
        },
    )?;
    super::nullable_commit_map::insert_proven_no_target_row_tx(
        tx,
        svn_rev,
        "svn_to_git",
        proof.svn_author(),
        "",
        &op.repo_id,
    )?;
    Ok(())
}

impl Database {
    /// #53 typed begin for one filtered SVN revision. The owner check runs
    /// outside SQLite; the captured identity is revalidated inside the same
    /// transaction as the shared begin core.
    pub(crate) fn begin_svn_filtered_mapping(
        &self,
        proof: &crate::sync_engine::svn_projection_53::VerifiedSvnProjection<'_, '_>,
        pre_write_svn_rev: i64,
        pre_write_git_sha: &str,
    ) -> Result<TeamCycleMappingOperation, DatabaseError> {
        proof.require_owner(self)?;
        let repo_id = proof.repo_id();
        let projection = proof.policy_snapshot();
        let fingerprint = team_cycle_mapping_fingerprint(repo_id, projection);
        let request_id = format!("svn-r{}", proof.revision());
        self.transaction(|tx| {
            proof.revalidate_tx(tx)?;
            begin_team_cycle_mapping_tx(
                tx,
                TeamCycleMappingIntent {
                    repo_id,
                    initiator_id: "team_worker",
                    request_id: &request_id,
                    target_fingerprint: &fingerprint,
                    direction: TeamCycleMappingDirection::SvnToGit,
                    outcome: TeamCycleMappingOutcome::SvnFiltered,
                    source_svn_rev: Some(proof.revision()),
                    source_git_sha: None,
                    pre_write_svn_rev,
                    pre_write_git_sha,
                    projection,
                    intended_target_proof: None,
                },
            )
        })
    }

    /// #53 typed confirm: the shared confirm core with the verified
    /// projection, in one transaction after the owner check.
    pub(crate) fn confirm_svn_filtered_mapping(
        &self,
        proof: &crate::sync_engine::svn_projection_53::VerifiedSvnProjection<'_, '_>,
        op_id: &str,
    ) -> Result<TeamCycleMappingOperation, DatabaseError> {
        proof.require_owner(self)?;
        self.transaction(|tx| {
            proof.revalidate_tx(tx)?;
            confirm_team_cycle_mapping_tx(tx, proof.repo_id(), op_id, None, Some(proof))
        })
    }
}

#[cfg(test)]
mod svn_filtered_tests {
    use super::*;

    fn pair_db() -> Database {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db.conn()
            .execute(
                "INSERT INTO repositories (id, name, svn_url, svn_branch, svn_username, git_api_url, git_repo, git_branch, enabled, created_at, updated_at, last_svn_rev, last_git_sha)
                 VALUES ('pair', 'pair', 'file:///svn', '', 'alice', 'file:///git', 'pair', 'main', 1, 't', 't', 1, ?1)",
                ["a".repeat(40)],
            )
            .unwrap();
        db
    }

    fn filtered_intent<'a>(fingerprint: &'a str, pre_git: &'a str) -> TeamCycleMappingIntent<'a> {
        TeamCycleMappingIntent {
            repo_id: "pair",
            initiator_id: "team_worker",
            request_id: "svn-r2",
            target_fingerprint: fingerprint,
            direction: TeamCycleMappingDirection::SvnToGit,
            outcome: TeamCycleMappingOutcome::SvnFiltered,
            source_svn_rev: Some(2),
            source_git_sha: None,
            pre_write_svn_rev: 1,
            pre_write_git_sha: pre_git,
            projection: "unfiltered",
            intended_target_proof: None,
        }
    }

    fn no_effects(db: &Database) {
        let (svn_rev, _) = db.get_repo_watermark("pair").unwrap();
        assert_eq!(svn_rev, 1, "no SVN watermark movement");
        assert_eq!(db.get_state("last_svn_rev_pair").unwrap(), None);
        assert!(
            crate::echo_receipt_scope::read_svn_no_target_receipt(db, "pair", 2)
                .unwrap()
                .is_none()
        );
        let rows: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM commit_map", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0, "no commit_map row");
        let outcomes: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM canonical_mapping_outcomes",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(outcomes, 0, "no canonical outcome");
    }

    #[test]
    fn generic_begin_refuses_svn_filtered_without_projection() {
        let db = pair_db();
        let fingerprint = team_cycle_mapping_fingerprint("pair", "unfiltered");
        let pre_git = "a".repeat(40);
        let error = db
            .begin_team_cycle_mapping(filtered_intent(&fingerprint, &pre_git))
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("requires a verified SVN projection"));
        assert!(db
            .active_team_cycle_mapping_operation("pair")
            .unwrap()
            .is_none());
        no_effects(&db);
    }

    #[test]
    fn forged_svn_filtered_journal_json_cannot_be_confirmed_generically() {
        let db = pair_db();
        let now = Utc::now().to_rfc3339();
        let forged = TeamCycleMappingOperation {
            version: 1,
            id: "forged-op".into(),
            repo_id: "pair".into(),
            operation_type: "team_cycle_mapping".into(),
            initiator_id: "team_worker".into(),
            request_id: "svn-r2".into(),
            target_fingerprint: team_cycle_mapping_fingerprint("pair", "unfiltered"),
            created_at: now.clone(),
            updated_at: now,
            state: TeamCycleMappingState::Running,
            direction: TeamCycleMappingDirection::SvnToGit,
            outcome: TeamCycleMappingOutcome::SvnFiltered,
            source_svn_rev: Some(2),
            source_git_sha: None,
            pre_write_svn_rev: 1,
            pre_write_git_sha: "a".repeat(40),
            projection: "unfiltered".into(),
            intended_target_proof: None,
            observed_target_proof: None,
            resume_authorized: false,
            outcome_detail: None,
        };
        db.transaction(|tx| {
            write_op(tx, &forged)?;
            write_value(tx, &key("active", "pair"), &forged.id)
        })
        .unwrap();
        let error = db
            .confirm_team_cycle_mapping("pair", "forged-op", None)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("can only be finalized with a verified SVN projection"));
        let still = db
            .get_team_cycle_mapping_operation("pair", "forged-op")
            .unwrap()
            .unwrap();
        assert_eq!(still.state, TeamCycleMappingState::Running);
        no_effects(&db);
    }

    #[test]
    fn svn_filtered_outcome_keeps_existing_serialized_names() {
        for (outcome, name) in [
            (
                TeamCycleMappingOutcome::SvnNoTargetContent,
                "svn_no_target_content",
            ),
            (TeamCycleMappingOutcome::GitFiltered, "git_filtered"),
            (TeamCycleMappingOutcome::GitEmptyCommit, "git_empty_commit"),
            (TeamCycleMappingOutcome::GitNoSvnDelta, "git_no_svn_delta"),
            (TeamCycleMappingOutcome::SvnFiltered, "svn_filtered"),
        ] {
            assert_eq!(
                serde_json::to_value(&outcome).unwrap(),
                serde_json::Value::String(name.into())
            );
        }
    }
}
