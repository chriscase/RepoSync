//! Operational canonical lineage identity and typed directional checkpoints (#63).
//!
//! These tables are independent of the fixture-only candidate `pair_lineages`
//! / `pair_frontiers` / `pair_outcomes` registry. `commit_map.git_sha` stays
//! NOT NULL so this story does not compete with #54's nullable rebuild.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::repo_scope_identity::repository_scope_uuid;
use super::Database;
use crate::echo_receipt_scope::repo_echo_generation_tx;
use crate::errors::{DatabaseError, SvnError, SyncError};
use crate::path_projection::svn_path_identity;
use crate::svn::{SvnClient, SvnInfo, SvnLogEntry};

/// Typed mapping / checkpoint outcome. NULL SHA, empty string, and a missing
/// row are never aliases of one another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingOutcome {
    AppliedAndVerified,
    IntentionallyFilteredNoTarget,
    Pending,
    LocallyPublishedNotRemote,
    UnknownReconciliationRequired,
}

impl MappingOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppliedAndVerified => "applied_and_verified",
            Self::IntentionallyFilteredNoTarget => "intentionally_filtered_no_target",
            Self::Pending => "pending",
            Self::LocallyPublishedNotRemote => "locally_published_not_remote",
            Self::UnknownReconciliationRequired => "unknown_reconciliation_required",
        }
    }

    fn parse(value: &str) -> Result<Self, DatabaseError> {
        match value {
            "applied_and_verified" => Ok(Self::AppliedAndVerified),
            "intentionally_filtered_no_target" => Ok(Self::IntentionallyFilteredNoTarget),
            "pending" => Ok(Self::Pending),
            "locally_published_not_remote" => Ok(Self::LocallyPublishedNotRemote),
            "unknown_reconciliation_required" => Ok(Self::UnknownReconciliationRequired),
            other => Err(DatabaseError::Other(format!(
                "unknown canonical mapping outcome {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointDirection {
    SvnToGit,
    GitToSvn,
}

impl CheckpointDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SvnToGit => "svn_to_git",
            Self::GitToSvn => "git_to_svn",
        }
    }

    fn parse(value: &str) -> Result<Self, DatabaseError> {
        match value {
            "svn_to_git" => Ok(Self::SvnToGit),
            "git_to_svn" => Ok(Self::GitToSvn),
            other => Err(DatabaseError::Other(format!(
                "unknown canonical checkpoint direction {other}"
            ))),
        }
    }
}

/// Verified SVN identity for one `(scope_uuid, generation)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalLineage {
    pub scope_uuid: String,
    pub generation: i64,
    pub repo_id: String,
    pub svn_uuid: String,
    pub svn_root_url: String,
    pub svn_url: String,
    pub svn_branch_path: String,
    pub incarnation_created_rev: i64,
    pub copy_from_path: Option<String>,
    pub copy_from_rev: Option<i64>,
    pub git_baseline_sha: Option<String>,
    pub git_ref: Option<String>,
    pub projection_version: String,
    pub policy_hash: String,
}

/// One authoritative directional checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalCheckpoint {
    pub scope_uuid: String,
    pub generation: i64,
    pub direction: CheckpointDirection,
    pub handled_source_key: String,
    pub outcome: MappingOutcome,
    pub target_git_sha: Option<String>,
    pub target_svn_rev: Option<i64>,
    pub reason: Option<String>,
    pub policy_snapshot: Option<String>,
}

/// Bundled mapping/checkpoint write so helpers stay under clippy's arity limit.
#[derive(Debug, Clone, Copy)]
pub struct MappingWrite<'a> {
    pub direction: CheckpointDirection,
    pub source_key: &'a str,
    pub outcome: MappingOutcome,
    pub target_git_sha: Option<&'a str>,
    pub target_svn_rev: Option<i64>,
    pub reason: Option<&'a str>,
    pub policy_snapshot: Option<&'a str>,
}

/// Loaded typed mapping without overloading NULL / empty / missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedMapping {
    pub outcome: MappingOutcome,
    pub target_git_sha: Option<String>,
    pub target_svn_rev: Option<i64>,
}

/// Observed live SVN UUID / URL / path incarnation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSvnIdentity {
    pub uuid: String,
    pub root_url: String,
    pub url: String,
    pub branch_path: String,
    pub incarnation_created_rev: i64,
    pub copy_from_path: Option<String>,
    pub copy_from_rev: Option<i64>,
}

pub fn svn_source_key(revision: i64) -> String {
    format!("svn:{revision}")
}

pub fn git_source_key(sha: &str) -> String {
    format!("git:{sha}")
}

fn tables_ready(conn: &Connection) -> Result<bool, DatabaseError> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='canonical_lineage')",
        [],
        |row| row.get(0),
    )?;
    Ok(exists)
}

fn scope_and_generation(conn: &Connection, repo_id: &str) -> Result<(String, i64), DatabaseError> {
    let scope = repository_scope_uuid(conn, repo_id)?;
    let generation = repo_echo_generation_tx(conn, repo_id)?;
    Ok((scope, generation))
}

/// Persist a proven lineage identity. Existing rows are immutable: a different
/// live identity is reconciliation, never a silent overwrite.
pub fn pin_canonical_lineage(
    conn: &Connection,
    lineage: &CanonicalLineage,
) -> Result<(), DatabaseError> {
    if !tables_ready(conn)? {
        return Err(DatabaseError::Other(
            "canonical lineage tables are not present".into(),
        ));
    }
    if let Some(existing) =
        load_canonical_lineage_tx(conn, &lineage.scope_uuid, lineage.generation)?
    {
        if existing.svn_uuid != lineage.svn_uuid
            || existing.svn_root_url != lineage.svn_root_url
            || existing.svn_url != lineage.svn_url
            || existing.svn_branch_path != lineage.svn_branch_path
            || existing.incarnation_created_rev != lineage.incarnation_created_rev
            || existing.copy_from_path != lineage.copy_from_path
            || existing.copy_from_rev != lineage.copy_from_rev
        {
            return Err(DatabaseError::Other(
                "canonical lineage identity changed; reconciliation required".into(),
            ));
        }
        return Ok(());
    }
    conn.execute(
        "INSERT INTO canonical_lineage (
            scope_uuid, generation, repo_id, svn_uuid, svn_root_url, svn_url,
            svn_branch_path, incarnation_created_rev, copy_from_path, copy_from_rev,
            git_baseline_sha, git_ref, projection_version, policy_hash, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            lineage.scope_uuid,
            lineage.generation,
            lineage.repo_id,
            lineage.svn_uuid,
            lineage.svn_root_url,
            lineage.svn_url,
            lineage.svn_branch_path,
            lineage.incarnation_created_rev,
            lineage.copy_from_path,
            lineage.copy_from_rev,
            lineage.git_baseline_sha,
            lineage.git_ref,
            lineage.projection_version,
            lineage.policy_hash,
            Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub fn load_canonical_lineage_tx(
    conn: &Connection,
    scope_uuid: &str,
    generation: i64,
) -> Result<Option<CanonicalLineage>, DatabaseError> {
    if !tables_ready(conn)? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT scope_uuid, generation, repo_id, svn_uuid, svn_root_url, svn_url,
                svn_branch_path, incarnation_created_rev, copy_from_path, copy_from_rev,
                git_baseline_sha, git_ref, projection_version, policy_hash
         FROM canonical_lineage WHERE scope_uuid = ?1 AND generation = ?2",
        params![scope_uuid, generation],
        |row| {
            Ok(CanonicalLineage {
                scope_uuid: row.get(0)?,
                generation: row.get(1)?,
                repo_id: row.get(2)?,
                svn_uuid: row.get(3)?,
                svn_root_url: row.get(4)?,
                svn_url: row.get(5)?,
                svn_branch_path: row.get(6)?,
                incarnation_created_rev: row.get(7)?,
                copy_from_path: row.get(8)?,
                copy_from_rev: row.get(9)?,
                git_baseline_sha: row.get(10)?,
                git_ref: row.get(11)?,
                projection_version: row.get(12)?,
                policy_hash: row.get(13)?,
            })
        },
    )
    .optional()
    .map_err(DatabaseError::from)
}

pub fn load_canonical_lineage(
    db: &Database,
    repo_id: &str,
) -> Result<Option<CanonicalLineage>, DatabaseError> {
    let conn = db.conn();
    let (scope, generation) = match scope_and_generation(&conn, repo_id) {
        Ok(pair) => pair,
        Err(DatabaseError::NotFound { .. }) => return Ok(None),
        Err(other) => return Err(other),
    };
    load_canonical_lineage_tx(&conn, &scope, generation)
}

/// Record a typed mapping outcome. Empty-string targets are refused so they
/// cannot stand in for NULL (no-target) or a missing row (pending).
pub fn record_mapping_outcome(
    conn: &Connection,
    scope_uuid: &str,
    generation: i64,
    write: MappingWrite<'_>,
) -> Result<(), DatabaseError> {
    if !tables_ready(conn)? {
        return Err(DatabaseError::Other(
            "canonical mapping outcome tables are not present".into(),
        ));
    }
    if write.source_key.is_empty() {
        return Err(DatabaseError::Other(
            "canonical mapping source_key must not be empty".into(),
        ));
    }
    if write.target_git_sha.is_some_and(str::is_empty) {
        return Err(DatabaseError::Other(
            "canonical mapping must not store an empty-string Git SHA".into(),
        ));
    }
    if matches!(write.outcome, MappingOutcome::IntentionallyFilteredNoTarget)
        && (write.target_git_sha.is_some() || write.target_svn_rev.is_some())
    {
        return Err(DatabaseError::Other(
            "filtered/no-target outcomes must not store a target".into(),
        ));
    }
    if matches!(write.outcome, MappingOutcome::AppliedAndVerified) {
        let ok = match write.direction {
            CheckpointDirection::SvnToGit => write.target_git_sha.is_some(),
            CheckpointDirection::GitToSvn => write.target_svn_rev.is_some(),
        };
        if !ok {
            return Err(DatabaseError::Other(
                "applied outcomes require the opposite-direction target".into(),
            ));
        }
    }
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO canonical_mapping_outcomes (
            scope_uuid, generation, direction, source_key, outcome,
            target_git_sha, target_svn_rev, reason, policy_snapshot, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(scope_uuid, generation, direction, source_key) DO UPDATE SET
            outcome = excluded.outcome,
            target_git_sha = excluded.target_git_sha,
            target_svn_rev = excluded.target_svn_rev,
            reason = excluded.reason,
            policy_snapshot = excluded.policy_snapshot",
        params![
            scope_uuid,
            generation,
            write.direction.as_str(),
            write.source_key,
            write.outcome.as_str(),
            write.target_git_sha,
            write.target_svn_rev,
            write.reason,
            write.policy_snapshot,
            now,
        ],
    )?;
    Ok(())
}

pub fn load_mapping_outcome(
    conn: &Connection,
    scope_uuid: &str,
    generation: i64,
    direction: CheckpointDirection,
    source_key: &str,
) -> Result<Option<LoadedMapping>, DatabaseError> {
    if !tables_ready(conn)? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT outcome, target_git_sha, target_svn_rev
         FROM canonical_mapping_outcomes
         WHERE scope_uuid = ?1 AND generation = ?2 AND direction = ?3 AND source_key = ?4",
        params![scope_uuid, generation, direction.as_str(), source_key],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        },
    )
    .optional()?
    .map(|(outcome, sha, rev)| {
        Ok(LoadedMapping {
            outcome: MappingOutcome::parse(&outcome)?,
            target_git_sha: sha,
            target_svn_rev: rev,
        })
    })
    .transpose()
}

/// Persist a typed checkpoint when canonical tables and a managed
/// `scope_uuid` exist. Deferred v12 and personal-scope-only databases skip.
pub(crate) fn try_persist_directional_checkpoint_tx(
    conn: &Connection,
    repo_id: &str,
    write: MappingWrite<'_>,
) -> Result<(), DatabaseError> {
    if !tables_ready(conn)? {
        return Ok(());
    }
    let (scope, generation) = match scope_and_generation(conn, repo_id) {
        Ok(pair) => pair,
        Err(DatabaseError::NotFound { .. }) => return Ok(()),
        Err(error) => return Err(error),
    };
    persist_directional_checkpoint_tx(conn, &scope, generation, write)
}

/// Write the directional checkpoint in the same SQLite transaction as the
/// resolved mapping outcome. Cross-repository I/O is not claimed atomic.
pub fn persist_directional_checkpoint_tx(
    conn: &Connection,
    scope_uuid: &str,
    generation: i64,
    write: MappingWrite<'_>,
) -> Result<(), DatabaseError> {
    record_mapping_outcome(conn, scope_uuid, generation, write)?;
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO canonical_checkpoints (
            scope_uuid, generation, direction, handled_source_key, outcome,
            target_git_sha, target_svn_rev, reason, policy_snapshot, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(scope_uuid, generation, direction) DO UPDATE SET
            handled_source_key = excluded.handled_source_key,
            outcome = excluded.outcome,
            target_git_sha = excluded.target_git_sha,
            target_svn_rev = excluded.target_svn_rev,
            reason = excluded.reason,
            policy_snapshot = excluded.policy_snapshot,
            updated_at = excluded.updated_at",
        params![
            scope_uuid,
            generation,
            write.direction.as_str(),
            write.source_key,
            write.outcome.as_str(),
            write.target_git_sha,
            write.target_svn_rev,
            write.reason,
            write.policy_snapshot,
            now,
        ],
    )?;
    Ok(())
}

pub fn load_directional_checkpoint(
    conn: &Connection,
    scope_uuid: &str,
    generation: i64,
    direction: CheckpointDirection,
) -> Result<Option<CanonicalCheckpoint>, DatabaseError> {
    if !tables_ready(conn)? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT scope_uuid, generation, direction, handled_source_key, outcome,
                target_git_sha, target_svn_rev, reason, policy_snapshot
         FROM canonical_checkpoints
         WHERE scope_uuid = ?1 AND generation = ?2 AND direction = ?3",
        params![scope_uuid, generation, direction.as_str()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        },
    )
    .optional()?
    .map(
        |(scope, generation, direction, key, outcome, sha, rev, reason, policy)| {
            Ok(CanonicalCheckpoint {
                scope_uuid: scope,
                generation,
                direction: CheckpointDirection::parse(&direction)?,
                handled_source_key: key,
                outcome: MappingOutcome::parse(&outcome)?,
                target_git_sha: sha,
                target_svn_rev: rev,
                reason,
                policy_snapshot: policy,
            })
        },
    )
    .transpose()
}

/// Classify a source without overloading NULL / empty / missing.
pub fn classify_mapping_outcome(
    conn: &Connection,
    scope_uuid: &str,
    generation: i64,
    direction: CheckpointDirection,
    source_key: &str,
) -> Result<MappingOutcome, DatabaseError> {
    if let Some(loaded) = load_mapping_outcome(conn, scope_uuid, generation, direction, source_key)?
    {
        return Ok(loaded.outcome);
    }
    Ok(MappingOutcome::Pending)
}

fn incarnation_from_history(
    history: &[SvnLogEntry],
    branch_path: &str,
) -> (i64, Option<String>, Option<i64>) {
    let oldest = history.last();
    let created = oldest
        .map(|entry| entry.revision)
        .filter(|rev| *rev > 0)
        .unwrap_or(1);
    let wanted = if branch_path.is_empty() {
        None
    } else {
        Some(format!("/{branch_path}"))
    };
    if let Some(entry) = oldest {
        for change in &entry.changed_paths {
            let matches_path = wanted
                .as_deref()
                .is_none_or(|path| change.path == path || change.path == *branch_path);
            if matches_path && change.copy_from_path.is_some() && change.copy_from_rev.is_some() {
                return (
                    entry.revision.max(1),
                    change.copy_from_path.clone(),
                    change.copy_from_rev,
                );
            }
        }
    }
    (created, None, None)
}

pub async fn observe_live_svn_identity(
    svn: &SvnClient,
    info: &SvnInfo,
) -> Result<LiveSvnIdentity, SvnError> {
    if info.uuid.trim().is_empty() {
        return Err(SvnError::LineageRefused {
            reason: "svn_uuid_missing".into(),
            detail: "live SVN info did not report a repository UUID".into(),
        });
    }
    let identity = svn_path_identity(&info.root_url, &info.url)?;
    let peg = info.latest_rev.max(1);
    let history = svn.copy_history_at_rev(peg).await?;
    if history.is_empty() {
        return Err(SvnError::LineageRefused {
            reason: "svn_incarnation_unproved".into(),
            detail: "stop-on-copy history is empty; refusing to guess path incarnation".into(),
        });
    }
    let (created, copy_from_path, copy_from_rev) =
        incarnation_from_history(&history, &identity.branch_path);
    Ok(LiveSvnIdentity {
        uuid: info.uuid.clone(),
        root_url: identity.root_url,
        url: info.url.trim_end_matches('/').to_string(),
        branch_path: identity.branch_path,
        incarnation_created_rev: created,
        copy_from_path,
        copy_from_rev,
    })
}

fn identities_match(stored: &CanonicalLineage, live: &LiveSvnIdentity) -> bool {
    stored.svn_uuid == live.uuid
        && stored.svn_root_url == live.root_url
        && stored.svn_url == live.url
        && stored.svn_branch_path == live.branch_path
        && stored.incarnation_created_rev == live.incarnation_created_rev
        && stored.copy_from_path == live.copy_from_path
        && stored.copy_from_rev == live.copy_from_rev
}

fn blocked(reason: &str, detail: impl Into<String>) -> SyncError {
    SyncError::HistoryBlocked {
        reason: reason.into(),
        detail: detail.into(),
    }
}

fn retained_legacy_baseline(conn: &Connection, repo_id: &str) -> Result<bool, DatabaseError> {
    let (last_svn, last_git): (i64, String) = conn.query_row(
        "SELECT last_svn_rev, last_git_sha FROM repositories WHERE id = ?1",
        [repo_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if last_svn > 0 || !last_git.is_empty() {
        return Ok(true);
    }
    let maps: i64 = conn.query_row(
        "SELECT COUNT(*) FROM commit_map WHERE repo_id = ?1",
        [repo_id],
        |row| row.get(0),
    )?;
    if maps > 0 {
        return Ok(true);
    }
    let records: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_records WHERE repo_id = ?1",
        [repo_id],
        |row| row.get(0),
    )?;
    Ok(records > 0)
}

pub fn independently_established_svn_uuid_kv_key(repo_id: &str) -> String {
    format!("independently_established_svn_uuid:{repo_id}")
}

fn snapshot_pin_svn_uuid(
    conn: &Connection,
    repo_id: &str,
) -> Result<Option<String>, DatabaseError> {
    let latest_key = format!("import_operation_v1:latest:{repo_id}");
    let op_id: Option<String> = conn
        .query_row(
            "SELECT value FROM kv_state WHERE key = ?1",
            [latest_key],
            |row| row.get(0),
        )
        .optional()?;
    let Some(op_id) = op_id else {
        return Ok(None);
    };
    let document_key = format!("import_operation_v1:document:{op_id}");
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM kv_state WHERE key = ?1",
            [document_key],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let op: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| DatabaseError::Other(error.to_string()))?;
    Ok(op
        .get("snapshot_pin")
        .and_then(|pin| pin.get("svn_uuid"))
        .and_then(|uuid| uuid.as_str())
        .filter(|uuid| !uuid.is_empty())
        .map(str::to_string))
}

/// Independently established SVN UUID: import snapshot pin, or a producer that
/// already observed live identity (completed history import / late-pair copy /
/// fixture that created the SVN). Retained watermarks alone are not proof.
pub fn independently_established_svn_uuid(
    conn: &Connection,
    repo_id: &str,
) -> Result<Option<String>, DatabaseError> {
    let pin = snapshot_pin_svn_uuid(conn, repo_id)?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM kv_state WHERE key = ?1",
            [independently_established_svn_uuid_kv_key(repo_id)],
            |row| row.get(0),
        )
        .optional()?;
    let recorded = raw
        .map(|uuid| uuid.trim().to_string())
        .filter(|uuid| !uuid.is_empty());
    match (pin, recorded) {
        (Some(pin), Some(recorded)) if pin != recorded => Err(DatabaseError::Other(format!(
            "import snapshot pin UUID {pin} does not match recorded UUID {recorded}"
        ))),
        (Some(pin), _) => Ok(Some(pin)),
        (None, Some(recorded)) => Ok(Some(recorded)),
        (None, None) => Ok(None),
    }
}

/// Record a live SVN UUID the producer already observed. Empty UUID is refused.
/// A different existing snapshot pin or recorded UUID is refused.
pub fn record_independently_established_svn_uuid(
    db: &Database,
    repo_id: &str,
    uuid: &str,
) -> Result<(), DatabaseError> {
    let uuid = uuid.trim();
    if uuid.is_empty() {
        return Err(DatabaseError::Other(
            "independently established SVN UUID is empty".into(),
        ));
    }
    {
        let conn = db.conn();
        if let Some(existing) = independently_established_svn_uuid(&conn, repo_id)? {
            if existing != uuid {
                return Err(DatabaseError::Other(format!(
                    "independently established SVN UUID {existing} does not match observed {uuid}"
                )));
            }
            return Ok(());
        }
    }
    db.set_state(&independently_established_svn_uuid_kv_key(repo_id), uuid)
}

/// Record a live UUID only as independently established initialization.
/// Retained import effects or mappings without prior proof hold instead of
/// minting today's observation as historical authority.
pub fn record_live_svn_uuid_or_hold_unproved(
    db: &Database,
    repo_id: &str,
    live: &LiveSvnIdentity,
    retained_producer_effects: bool,
) -> Result<(), SyncError> {
    if live.uuid.trim().is_empty() {
        return Err(blocked(
            "svn_uuid_missing",
            "live SVN info did not report a repository UUID",
        ));
    }
    let (proved, retained_baseline) = {
        let conn = db.conn();
        (
            independently_established_svn_uuid(&conn, repo_id).map_err(SyncError::DatabaseError)?,
            retained_legacy_baseline(&conn, repo_id).map_err(SyncError::DatabaseError)?,
        )
    };
    if let Some(expected) = proved {
        if expected != live.uuid {
            return hold_unproved_identity(
                db,
                repo_id,
                live,
                "svn_identity_changed",
                format!(
                    "live SVN uuid={} does not match independently established uuid={}",
                    live.uuid, expected
                ),
            );
        }
        return Ok(());
    }
    if retained_producer_effects || retained_baseline {
        return hold_unproved_identity(
            db,
            repo_id,
            live,
            "svn_identity_unproved",
            format!(
                "live SVN uuid={} url={} has no independently established identity for retained import effects or mappings/checkpoints",
                live.uuid, live.url
            ),
        );
    }
    record_independently_established_svn_uuid(db, repo_id, &live.uuid)
        .map_err(SyncError::DatabaseError)
}

fn git_baseline_from_repo(
    conn: &Connection,
    repo_id: &str,
) -> Result<(Option<String>, Option<String>), DatabaseError> {
    let (sha, branch): (String, String) = conn.query_row(
        "SELECT last_git_sha, git_branch FROM repositories WHERE id = ?1",
        [repo_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((
        (!sha.is_empty()).then_some(sha),
        (!branch.is_empty()).then_some(branch),
    ))
}

fn hold_unproved_identity(
    db: &Database,
    repo_id: &str,
    live: &LiveSvnIdentity,
    reason: &str,
    detail: String,
) -> Result<(), SyncError> {
    let _ = db.update_repo_sync_status(repo_id, "reconciliation_required");
    let identity_key = format!("svn-identity:{}", live.uuid);
    let _ = db.persist_canonical_checkpoint(
        repo_id,
        MappingWrite {
            direction: CheckpointDirection::SvnToGit,
            source_key: &identity_key,
            outcome: MappingOutcome::UnknownReconciliationRequired,
            target_git_sha: None,
            target_svn_rev: None,
            reason: Some(reason),
            policy_snapshot: None,
        },
    );
    Err(blocked(reason, detail))
}

/// Adopt a live SVN identity only when independently established, or when this
/// is a fresh pair with no retained baseline. Legacy upgrades without a proved
/// UUID hold rather than pinning a replacement at the same URL.
pub fn adopt_or_hold_live_svn_identity(
    db: &Database,
    repo_id: &str,
    live: &LiveSvnIdentity,
) -> Result<(), SyncError> {
    let conn = db.conn();
    if !tables_ready(&conn).map_err(SyncError::DatabaseError)? {
        return Ok(());
    }
    let (scope, generation) = match scope_and_generation(&conn, repo_id) {
        Ok(pair) => pair,
        Err(DatabaseError::NotFound { .. }) => return Ok(()),
        Err(error) => return Err(SyncError::DatabaseError(error)),
    };
    if let Some(stored) =
        load_canonical_lineage_tx(&conn, &scope, generation).map_err(SyncError::DatabaseError)?
    {
        if identities_match(&stored, live) {
            return Ok(());
        }
        let detail = format!(
            "live SVN uuid={} url={} created_rev={} copy_from={:?}@{:?} does not match pinned uuid={} url={} created_rev={} copy_from={:?}@{:?}",
            live.uuid,
            live.url,
            live.incarnation_created_rev,
            live.copy_from_path,
            live.copy_from_rev,
            stored.svn_uuid,
            stored.svn_url,
            stored.incarnation_created_rev,
            stored.copy_from_path,
            stored.copy_from_rev
        );
        drop(conn);
        return hold_unproved_identity(db, repo_id, live, "svn_identity_changed", detail);
    }
    let proved =
        independently_established_svn_uuid(&conn, repo_id).map_err(SyncError::DatabaseError)?;
    if let Some(expected) = proved {
        if expected != live.uuid {
            let detail = format!(
                "live SVN uuid={} does not match independently established uuid={}",
                live.uuid, expected
            );
            drop(conn);
            return hold_unproved_identity(db, repo_id, live, "svn_identity_changed", detail);
        }
    } else if retained_legacy_baseline(&conn, repo_id).map_err(SyncError::DatabaseError)? {
        let detail = format!(
            "live SVN uuid={} url={} created_rev={} has no independently established identity for retained mappings/checkpoints",
            live.uuid, live.url, live.incarnation_created_rev
        );
        drop(conn);
        return hold_unproved_identity(db, repo_id, live, "svn_identity_unproved", detail);
    }
    let (git_baseline_sha, git_ref) =
        git_baseline_from_repo(&conn, repo_id).map_err(SyncError::DatabaseError)?;
    let lineage = CanonicalLineage {
        scope_uuid: scope,
        generation,
        repo_id: repo_id.to_string(),
        svn_uuid: live.uuid.clone(),
        svn_root_url: live.root_url.clone(),
        svn_url: live.url.clone(),
        svn_branch_path: live.branch_path.clone(),
        incarnation_created_rev: live.incarnation_created_rev,
        copy_from_path: live.copy_from_path.clone(),
        copy_from_rev: live.copy_from_rev,
        git_baseline_sha,
        git_ref,
        projection_version: String::new(),
        policy_hash: String::new(),
    };
    pin_canonical_lineage(&conn, &lineage).map_err(SyncError::DatabaseError)
}

/// Compare live SVN UUID/path incarnation with the pinned lineage. A first
/// independently established observation pins identity; a later mismatch or
/// unproved legacy adoption holds only this pair.
pub async fn verify_or_pin_live_svn_identity(
    db: &Database,
    repo_id: &str,
    svn: &SvnClient,
    info: &SvnInfo,
) -> Result<(), SyncError> {
    let live = observe_live_svn_identity(svn, info)
        .await
        .map_err(SyncError::SvnError)?;
    adopt_or_hold_live_svn_identity(db, repo_id, &live)
}

impl Database {
    pub fn persist_canonical_checkpoint(
        &self,
        repo_id: &str,
        write: MappingWrite<'_>,
    ) -> Result<(), DatabaseError> {
        self.immediate_transaction(|tx| {
            let (scope, generation) = scope_and_generation(tx, repo_id)?;
            persist_directional_checkpoint_tx(tx, &scope, generation, write)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema;

    fn setup() -> Database {
        let db = Database::in_memory().unwrap();
        schema::run_migrations(&db.conn()).unwrap();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,scope_uuid)
                 VALUES ('pair','pair','file:///repo','trunk','','','r','main',1,'t','t',?1)",
                [crate::db::repo_scope_identity::new_scope_uuid()],
            )
            .unwrap();
        db
    }

    #[test]
    fn typed_outcomes_do_not_overload_null_empty_or_missing() {
        let db = setup();
        let conn = db.conn();
        let (scope, generation) = scope_and_generation(&conn, "pair").unwrap();
        assert_eq!(
            classify_mapping_outcome(
                &conn,
                &scope,
                generation,
                CheckpointDirection::SvnToGit,
                &svn_source_key(2)
            )
            .unwrap(),
            MappingOutcome::Pending
        );
        let svn_r2 = svn_source_key(2);
        assert!(record_mapping_outcome(
            &conn,
            &scope,
            generation,
            MappingWrite {
                direction: CheckpointDirection::SvnToGit,
                source_key: &svn_r2,
                outcome: MappingOutcome::AppliedAndVerified,
                target_git_sha: Some(""),
                target_svn_rev: None,
                reason: None,
                policy_snapshot: None,
            },
        )
        .is_err());
        let applied_sha = "a".repeat(40);
        record_mapping_outcome(
            &conn,
            &scope,
            generation,
            MappingWrite {
                direction: CheckpointDirection::SvnToGit,
                source_key: &svn_r2,
                outcome: MappingOutcome::AppliedAndVerified,
                target_git_sha: Some(applied_sha.as_str()),
                target_svn_rev: None,
                reason: None,
                policy_snapshot: None,
            },
        )
        .unwrap();
        let filtered_sha = "b".repeat(40);
        let filtered_key = git_source_key(&filtered_sha);
        record_mapping_outcome(
            &conn,
            &scope,
            generation,
            MappingWrite {
                direction: CheckpointDirection::GitToSvn,
                source_key: &filtered_key,
                outcome: MappingOutcome::IntentionallyFilteredNoTarget,
                target_git_sha: None,
                target_svn_rev: None,
                reason: Some("filtered"),
                policy_snapshot: Some("{}"),
            },
        )
        .unwrap();
        let local_sha = "c".repeat(40);
        let local_key = git_source_key(&local_sha);
        persist_directional_checkpoint_tx(
            &conn,
            &scope,
            generation,
            MappingWrite {
                direction: CheckpointDirection::GitToSvn,
                source_key: &local_key,
                outcome: MappingOutcome::LocallyPublishedNotRemote,
                target_git_sha: None,
                target_svn_rev: Some(4),
                reason: Some("published locally"),
                policy_snapshot: None,
            },
        )
        .unwrap();
        let unknown_key = svn_source_key(9);
        persist_directional_checkpoint_tx(
            &conn,
            &scope,
            generation,
            MappingWrite {
                direction: CheckpointDirection::SvnToGit,
                source_key: &unknown_key,
                outcome: MappingOutcome::UnknownReconciliationRequired,
                target_git_sha: None,
                target_svn_rev: None,
                reason: Some("conflicting cursors"),
                policy_snapshot: None,
            },
        )
        .unwrap();
        assert_eq!(
            classify_mapping_outcome(
                &conn,
                &scope,
                generation,
                CheckpointDirection::SvnToGit,
                &svn_source_key(2)
            )
            .unwrap(),
            MappingOutcome::AppliedAndVerified
        );
        let filtered = load_mapping_outcome(
            &conn,
            &scope,
            generation,
            CheckpointDirection::GitToSvn,
            &git_source_key(&filtered_sha),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            filtered.outcome,
            MappingOutcome::IntentionallyFilteredNoTarget
        );
        assert!(filtered.target_git_sha.is_none());
        assert!(filtered.target_svn_rev.is_none());
        let checkpoint =
            load_directional_checkpoint(&conn, &scope, generation, CheckpointDirection::SvnToGit)
                .unwrap()
                .unwrap();
        assert_eq!(
            checkpoint.outcome,
            MappingOutcome::UnknownReconciliationRequired
        );
    }

    #[test]
    fn lineage_pin_is_immutable_for_a_scope_generation() {
        let db = setup();
        let conn = db.conn();
        let (scope, generation) = scope_and_generation(&conn, "pair").unwrap();
        let lineage = CanonicalLineage {
            scope_uuid: scope.clone(),
            generation,
            repo_id: "pair".into(),
            svn_uuid: "uuid-a".into(),
            svn_root_url: "file:///repo".into(),
            svn_url: "file:///repo/trunk".into(),
            svn_branch_path: "trunk".into(),
            incarnation_created_rev: 1,
            copy_from_path: None,
            copy_from_rev: None,
            git_baseline_sha: None,
            git_ref: Some("refs/heads/main".into()),
            projection_version: String::new(),
            policy_hash: String::new(),
        };
        pin_canonical_lineage(&conn, &lineage).unwrap();
        pin_canonical_lineage(&conn, &lineage).unwrap();
        let mut changed = lineage.clone();
        changed.svn_uuid = "uuid-b".into();
        assert!(pin_canonical_lineage(&conn, &changed).is_err());
        let loaded = load_canonical_lineage_tx(&conn, &scope, generation)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.svn_uuid, "uuid-a");
    }

    #[test]
    fn two_repos_with_equal_revision_keep_separate_lineage() {
        let db = setup();
        db.conn()
            .execute(
                "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,scope_uuid)
                 VALUES ('other','other','file:///other','trunk','','','r2','main',1,'t','t',?1)",
                [crate::db::repo_scope_identity::new_scope_uuid()],
            )
            .unwrap();
        let conn = db.conn();
        let (scope_a, gen_a) = scope_and_generation(&conn, "pair").unwrap();
        let (scope_b, gen_b) = scope_and_generation(&conn, "other").unwrap();
        for (scope, gen, repo, uuid) in [
            (scope_a.clone(), gen_a, "pair", "uuid-a"),
            (scope_b.clone(), gen_b, "other", "uuid-b"),
        ] {
            pin_canonical_lineage(
                &conn,
                &CanonicalLineage {
                    scope_uuid: scope,
                    generation: gen,
                    repo_id: repo.into(),
                    svn_uuid: uuid.into(),
                    svn_root_url: format!("file:///{repo}"),
                    svn_url: format!("file:///{repo}/trunk"),
                    svn_branch_path: "trunk".into(),
                    incarnation_created_rev: 2,
                    copy_from_path: None,
                    copy_from_rev: None,
                    git_baseline_sha: None,
                    git_ref: None,
                    projection_version: String::new(),
                    policy_hash: String::new(),
                },
            )
            .unwrap();
        }
        assert_ne!(scope_a, scope_b);
        assert_eq!(
            load_canonical_lineage_tx(&conn, &scope_a, gen_a)
                .unwrap()
                .unwrap()
                .svn_uuid,
            "uuid-a"
        );
        assert_eq!(
            load_canonical_lineage_tx(&conn, &scope_b, gen_b)
                .unwrap()
                .unwrap()
                .svn_uuid,
            "uuid-b"
        );
    }

    #[test]
    fn applied_commit_map_row_writes_typed_checkpoint() {
        let db = setup();
        let sha = "d".repeat(40);
        db.insert_commit_map_with_repo(7, &sha, "svn_to_git", "a", "A", Some("pair"))
            .unwrap();
        let conn = db.conn();
        let (scope, generation) = scope_and_generation(&conn, "pair").unwrap();
        let checkpoint =
            load_directional_checkpoint(&conn, &scope, generation, CheckpointDirection::SvnToGit)
                .unwrap()
                .unwrap();
        assert_eq!(checkpoint.outcome, MappingOutcome::AppliedAndVerified);
        assert_eq!(checkpoint.handled_source_key, svn_source_key(7));
        assert_eq!(checkpoint.target_git_sha.as_deref(), Some(sha.as_str()));
        assert!(checkpoint.target_svn_rev.is_none());
        assert_eq!(
            classify_mapping_outcome(
                &conn,
                &scope,
                generation,
                CheckpointDirection::SvnToGit,
                &svn_source_key(7)
            )
            .unwrap(),
            MappingOutcome::AppliedAndVerified
        );
        assert_eq!(
            classify_mapping_outcome(
                &conn,
                &scope,
                generation,
                CheckpointDirection::SvnToGit,
                &svn_source_key(8)
            )
            .unwrap(),
            MappingOutcome::Pending
        );
    }

    fn live_identity(uuid: &str) -> LiveSvnIdentity {
        LiveSvnIdentity {
            uuid: uuid.into(),
            root_url: "file:///repo".into(),
            url: "file:///repo/trunk".into(),
            branch_path: "trunk".into(),
            incarnation_created_rev: 1,
            copy_from_path: None,
            copy_from_rev: None,
        }
    }

    #[test]
    fn fresh_pair_pins_as_independently_established_initialization() {
        let db = setup();
        adopt_or_hold_live_svn_identity(&db, "pair", &live_identity("uuid-fresh")).unwrap();
        let loaded = load_canonical_lineage(&db, "pair").unwrap().unwrap();
        assert_eq!(loaded.svn_uuid, "uuid-fresh");
        assert_eq!(loaded.git_ref.as_deref(), Some("main"));
    }

    #[test]
    fn upgraded_engine_holds_unproved_legacy_identity_before_any_pin() {
        let db = setup();
        db.conn()
            .execute(
                "UPDATE repositories SET last_svn_rev = 5, last_git_sha = ?1 WHERE id = 'pair'",
                ["a".repeat(40)],
            )
            .unwrap();
        let err = adopt_or_hold_live_svn_identity(&db, "pair", &live_identity("uuid-replacement"))
            .unwrap_err();
        match err {
            SyncError::HistoryBlocked { reason, .. } => {
                assert_eq!(reason, "svn_identity_unproved")
            }
            other => panic!("expected HistoryBlocked, got {other:?}"),
        }
        assert!(load_canonical_lineage(&db, "pair").unwrap().is_none());
        assert_eq!(
            db.get_repository("pair").unwrap().unwrap().sync_status,
            "reconciliation_required"
        );
        let again =
            adopt_or_hold_live_svn_identity(&db, "pair", &live_identity("uuid-replacement"));
        assert!(matches!(
            again,
            Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "svn_identity_unproved"
        ));
        assert!(load_canonical_lineage(&db, "pair").unwrap().is_none());
    }

    #[test]
    fn retained_baseline_with_matching_recorded_uuid_pins() {
        let db = setup();
        db.conn()
            .execute(
                "UPDATE repositories SET last_svn_rev = 5, last_git_sha = ?1 WHERE id = 'pair'",
                ["c".repeat(40)],
            )
            .unwrap();
        record_independently_established_svn_uuid(&db, "pair", "uuid-live").unwrap();
        adopt_or_hold_live_svn_identity(&db, "pair", &live_identity("uuid-live")).unwrap();
        let loaded = load_canonical_lineage(&db, "pair").unwrap().unwrap();
        assert_eq!(loaded.svn_uuid, "uuid-live");
    }

    #[test]
    fn retained_baseline_with_matching_snapshot_pin_pins() {
        let db = setup();
        db.conn()
            .execute(
                "UPDATE repositories SET last_svn_rev = 4, last_git_sha = ?1 WHERE id = 'pair'",
                ["d".repeat(40)],
            )
            .unwrap();
        db.set_state("import_operation_v1:latest:pair", "op-pin")
            .unwrap();
        db.set_state(
            "import_operation_v1:document:op-pin",
            &serde_json::json!({
                "snapshot_pin": {
                    "svn_uuid": "uuid-pin",
                    "canonical_url": "file:///repo",
                    "operative_rev": 4,
                    "peg_rev": 4,
                    "requested": "HEAD"
                }
            })
            .to_string(),
        )
        .unwrap();
        adopt_or_hold_live_svn_identity(&db, "pair", &live_identity("uuid-pin")).unwrap();
        assert_eq!(
            load_canonical_lineage(&db, "pair")
                .unwrap()
                .unwrap()
                .svn_uuid,
            "uuid-pin"
        );
    }

    #[test]
    fn retained_baseline_with_mismatched_recorded_uuid_holds_changed() {
        let db = setup();
        db.conn()
            .execute(
                "UPDATE repositories SET last_svn_rev = 5, last_git_sha = ?1 WHERE id = 'pair'",
                ["e".repeat(40)],
            )
            .unwrap();
        record_independently_established_svn_uuid(&db, "pair", "uuid-proved").unwrap();
        let err =
            adopt_or_hold_live_svn_identity(&db, "pair", &live_identity("uuid-other")).unwrap_err();
        match err {
            SyncError::HistoryBlocked { reason, .. } => {
                assert_eq!(reason, "svn_identity_changed")
            }
            other => panic!("expected HistoryBlocked, got {other:?}"),
        }
        assert!(load_canonical_lineage(&db, "pair").unwrap().is_none());
    }

    #[test]
    fn empty_recorded_uuid_is_refused() {
        let db = setup();
        assert!(record_independently_established_svn_uuid(&db, "pair", "  ").is_err());
        assert!(independently_established_svn_uuid(&db.conn(), "pair")
            .unwrap()
            .is_none());
    }

    #[test]
    fn retained_import_effects_without_uuid_hold_and_do_not_record_live() {
        let db = setup();
        let err = record_live_svn_uuid_or_hold_unproved(
            &db,
            "pair",
            &live_identity("uuid-replacement"),
            true,
        )
        .unwrap_err();
        match err {
            SyncError::HistoryBlocked { reason, .. } => {
                assert_eq!(reason, "svn_identity_unproved")
            }
            other => panic!("expected HistoryBlocked, got {other:?}"),
        }
        assert!(independently_established_svn_uuid(&db.conn(), "pair")
            .unwrap()
            .is_none());
        assert!(load_canonical_lineage(&db, "pair").unwrap().is_none());
        let again = record_live_svn_uuid_or_hold_unproved(
            &db,
            "pair",
            &live_identity("uuid-replacement"),
            true,
        );
        assert!(matches!(
            again,
            Err(SyncError::HistoryBlocked { ref reason, .. }) if reason == "svn_identity_unproved"
        ));
        assert!(independently_established_svn_uuid(&db.conn(), "pair")
            .unwrap()
            .is_none());
    }

    #[test]
    fn fresh_import_without_retained_effects_records_live_uuid() {
        let db = setup();
        record_live_svn_uuid_or_hold_unproved(&db, "pair", &live_identity("uuid-fresh"), false)
            .unwrap();
        assert_eq!(
            independently_established_svn_uuid(&db.conn(), "pair")
                .unwrap()
                .as_deref(),
            Some("uuid-fresh")
        );
    }

    #[test]
    fn matching_recorded_uuid_allows_resume_with_retained_effects() {
        let db = setup();
        record_independently_established_svn_uuid(&db, "pair", "uuid-live").unwrap();
        record_live_svn_uuid_or_hold_unproved(&db, "pair", &live_identity("uuid-live"), true)
            .unwrap();
        assert_eq!(
            independently_established_svn_uuid(&db.conn(), "pair")
                .unwrap()
                .as_deref(),
            Some("uuid-live")
        );
    }

    #[test]
    fn snapshot_pin_and_recorded_uuid_mismatch_is_refused() {
        let db = setup();
        db.set_state("import_operation_v1:latest:pair", "op-pin")
            .unwrap();
        db.set_state(
            "import_operation_v1:document:op-pin",
            &serde_json::json!({
                "snapshot_pin": {
                    "svn_uuid": "uuid-pin",
                    "canonical_url": "file:///repo",
                    "operative_rev": 1,
                    "peg_rev": 1,
                    "requested": "HEAD"
                }
            })
            .to_string(),
        )
        .unwrap();
        db.set_state(
            &independently_established_svn_uuid_kv_key("pair"),
            "uuid-other",
        )
        .unwrap();
        assert!(independently_established_svn_uuid(&db.conn(), "pair").is_err());
    }

    #[test]
    fn malformed_generation_refuses_checkpoint_without_generation_one_write() {
        let db = setup();
        let conn = db.conn();
        let scope = repository_scope_uuid(&conn, "pair").unwrap();
        drop(conn);
        db.set_state(
            &crate::db::repo_scope_identity::repo_echo_generation_kv_key(&scope),
            "not-a-number",
        )
        .unwrap();
        let sha = "b".repeat(40);
        let source = svn_source_key(2);
        let err = db.persist_canonical_checkpoint(
            "pair",
            MappingWrite {
                direction: CheckpointDirection::SvnToGit,
                source_key: &source,
                outcome: MappingOutcome::AppliedAndVerified,
                target_git_sha: Some(sha.as_str()),
                target_svn_rev: None,
                reason: None,
                policy_snapshot: None,
            },
        );
        assert!(err.is_err());
        let conn = db.conn();
        assert!(
            load_directional_checkpoint(&conn, &scope, 1, CheckpointDirection::SvnToGit)
                .unwrap()
                .is_none()
        );
        assert!(load_mapping_outcome(
            &conn,
            &scope,
            1,
            CheckpointDirection::SvnToGit,
            &svn_source_key(2)
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn generation_read_failure_refuses_checkpoint_without_generation_one_write() {
        let db = setup();
        let conn = db.conn();
        let scope = repository_scope_uuid(&conn, "pair").unwrap();
        drop(conn);
        db.conn()
            .execute("ALTER TABLE kv_state RENAME TO kv_state_hidden", [])
            .unwrap();
        let sha = "e".repeat(40);
        let source = svn_source_key(4);
        let err = db.persist_canonical_checkpoint(
            "pair",
            MappingWrite {
                direction: CheckpointDirection::SvnToGit,
                source_key: &source,
                outcome: MappingOutcome::AppliedAndVerified,
                target_git_sha: Some(sha.as_str()),
                target_svn_rev: None,
                reason: None,
                policy_snapshot: None,
            },
        );
        assert!(err.is_err());
        db.conn()
            .execute("ALTER TABLE kv_state_hidden RENAME TO kv_state", [])
            .unwrap();
        let conn = db.conn();
        assert!(
            load_directional_checkpoint(&conn, &scope, 1, CheckpointDirection::SvnToGit)
                .unwrap()
                .is_none()
        );
        assert!(load_mapping_outcome(
            &conn,
            &scope,
            1,
            CheckpointDirection::SvnToGit,
            &svn_source_key(4)
        )
        .unwrap()
        .is_none());
    }
}
