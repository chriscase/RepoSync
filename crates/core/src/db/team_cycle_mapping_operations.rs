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
    write_value(tx, "last_svn_rev", &svn_rev.to_string())?;
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
    write_value(tx, &format!("last_git_sha_{}", repo_id), git_sha)?;
    write_value(tx, "last_git_hash", git_sha)?;
    let receipt_key = format!("handled_git_no_target_{}_{}", repo_id, git_sha);
    write_value(tx, &receipt_key, &receipt.to_string())?;
    Ok(())
}

fn finalize_tx(
    tx: &Connection,
    mut op: TeamCycleMappingOperation,
    observed_proof: Option<serde_json::Value>,
) -> Result<TeamCycleMappingOperation, DatabaseError> {
    if op.intended_target_proof.is_some() && observed_proof != op.intended_target_proof {
        return Err(DatabaseError::Other(
            "observed target proof differs from the recorded intent".into(),
        ));
    }
    match (&op.direction, &op.outcome) {
        (TeamCycleMappingDirection::SvnToGit, TeamCycleMappingOutcome::SvnNoTargetContent) => {
            let svn_rev = op.source_svn_rev.ok_or_else(|| {
                DatabaseError::Other("svn no-target mapping lacks source rev".into())
            })?;
            if svn_rev <= op.pre_write_svn_rev {
                return Err(DatabaseError::Other(
                    "observed SVN revision is not after the pre-write revision".into(),
                ));
            }
            advance_svn_only_tx(tx, &op.repo_id, svn_rev)?;
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
        self.transaction(|tx| {
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
        })
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
            finalize_tx(tx, op, observed_proof)
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
        let receipt = db
            .get_state(&format!("handled_git_no_target_pair_{}", git_sha))
            .unwrap()
            .unwrap();
        assert!(receipt.contains("filtered"));
    }
}
