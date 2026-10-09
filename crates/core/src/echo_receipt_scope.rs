//! Generation-scoped echo receipt keys and validation (#63).
//!
//! Echo receipts, generations, and checkpoint KV keys are scoped by durable
//! `repositories.scope_uuid`, not the human repository id.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

use crate::db::repo_scope_identity::{
    attach_scope_uuid_to_receipt, last_git_sha_kv_key, legacy_last_git_sha_kv_key,
    legacy_repo_echo_generation_kv_key, legacy_repo_id_kv_authoritative,
    receipt_scope_uuid_matches, repo_echo_generation_kv_key, repository_scope_uuid,
};
use crate::db::Database;
use crate::echo_suppression::{verify_no_target_receipt, NoTargetReceiptVerdict};
use crate::errors::DatabaseError;
use crate::pair_refresh::PAIR_GENERATION;

/// True when the managed repository row carries a durable `scope_uuid` (v13+).
fn managed_repo_requires_scoped_receipts(
    tx: &Connection,
    repo_id: &str,
) -> Result<bool, DatabaseError> {
    if repo_id == crate::db::personal_scope::PERSONAL_SCOPE_KEY {
        return Ok(false);
    }
    Ok(repository_scope_uuid(tx, repo_id).is_ok())
}

fn receipt_admits_for_repo(
    tx: &Connection,
    repo_id: &str,
    scope: &str,
    record: &serde_json::Value,
    legacy_authoritative: bool,
) -> Result<bool, DatabaseError> {
    if record.get("repo_id").and_then(|v| v.as_str()) != Some(repo_id) {
        return Ok(false);
    }
    if managed_repo_requires_scoped_receipts(tx, repo_id)? {
        return Ok(record.get("scope_uuid").is_some() && receipt_scope_uuid_matches(record, scope));
    }
    if record.get("scope_uuid").is_some() {
        return Ok(receipt_scope_uuid_matches(record, scope));
    }
    Ok(legacy_authoritative)
}

fn scope_token_for_repo(tx: &Connection, repo_id: &str) -> Result<String, DatabaseError> {
    if repo_id == crate::db::personal_scope::PERSONAL_SCOPE_KEY {
        return Ok(repo_id.to_string());
    }
    match repository_scope_uuid(tx, repo_id) {
        Ok(scope) => Ok(scope),
        Err(DatabaseError::NotFound { entity, .. }) if entity == "repository" => {
            Ok(repo_id.to_string())
        }
        Err(other) => Err(other),
    }
}

/// Active echo-suppression generation for a managed repository scope.
pub fn repo_echo_generation_tx(tx: &Connection, repo_id: &str) -> Result<i64, DatabaseError> {
    let scope = scope_token_for_repo(tx, repo_id)?;
    let key = repo_echo_generation_kv_key(&scope);
    let raw: Option<String> = tx
        .query_row("SELECT value FROM kv_state WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?;
    if let Some(value) = raw {
        return value
            .parse::<i64>()
            .map_err(|_| DatabaseError::Other("invalid repo echo generation".into()));
    }
    if legacy_repo_id_kv_authoritative(tx)? {
        let legacy_key = legacy_repo_echo_generation_kv_key(repo_id);
        let legacy: Option<String> = tx
            .query_row(
                "SELECT value FROM kv_state WHERE key = ?1",
                [legacy_key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(value) = legacy {
            return value
                .parse::<i64>()
                .map_err(|_| DatabaseError::Other("invalid repo echo generation".into()));
        }
    }
    Ok(PAIR_GENERATION)
}

pub fn repo_echo_generation(db: &Database, repo_id: &str) -> Result<i64, DatabaseError> {
    repo_echo_generation_tx(&db.conn(), repo_id)
}

#[cfg(test)]
pub(crate) fn test_force_next_split_fail(db: &Database) {
    db.test_force_next_echo_split_fail();
}

/// When a generation bump follows a unified no-target cursor, restore the scoped
/// KV copy to the last applied outbound Git SHA so stale receipts cannot admit P.
fn split_unified_git_cursor_to_outbound_kv_tx(
    #[allow(unused_variables)] db: Option<&Database>,
    tx: &Connection,
    repo_id: &str,
) -> Result<(), DatabaseError> {
    #[cfg(test)]
    if db.is_some_and(|database| database.take_test_force_echo_split_fail()) {
        return Err(DatabaseError::Other("test forced split failure".into()));
    }
    let scope = scope_token_for_repo(tx, repo_id)?;
    let column: Option<String> = tx
        .query_row(
            "SELECT last_git_sha FROM repositories WHERE id = ?1",
            [repo_id],
            |row| row.get(0),
        )
        .optional()?;
    let column = column.filter(|value| !value.is_empty());
    if column.is_none() {
        return Ok(());
    }
    let column = column.unwrap();
    let kv_key = last_git_sha_kv_key(&scope);
    let kv: Option<String> = tx
        .query_row(
            "SELECT value FROM kv_state WHERE key = ?1",
            [&kv_key],
            |row| row.get(0),
        )
        .optional()?;
    if kv.as_deref() != Some(column.as_str()) {
        return Ok(());
    }
    let handled: Option<String> = tx
        .query_row(
            "SELECT git_sha FROM sync_records WHERE repo_id = ?1 AND direction = 'git_to_svn' AND status = 'applied' ORDER BY rowid DESC LIMIT 1",
            [repo_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(handled) = handled else {
        return Ok(());
    };
    if handled == column {
        return Ok(());
    }
    let now = Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![kv_key, handled, now],
    )?;
    if legacy_repo_id_kv_authoritative(tx)? {
        let legacy_key = legacy_last_git_sha_kv_key(repo_id);
        tx.execute(
            "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![legacy_key, handled, now],
        )?;
    }
    Ok(())
}

/// Bump the echo generation on an open connection (same transaction as reset).
pub fn bump_repo_echo_generation_tx(
    db: Option<&Database>,
    tx: &Connection,
    repo_id: &str,
) -> Result<i64, DatabaseError> {
    let next = repo_echo_generation_tx(tx, repo_id)? + 1;
    let scope = scope_token_for_repo(tx, repo_id)?;
    let key = repo_echo_generation_kv_key(&scope);
    let now = Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, next.to_string(), now],
    )?;
    split_unified_git_cursor_to_outbound_kv_tx(db, tx, repo_id)?;
    Ok(next)
}

/// Bump the echo generation after a reset/re-anchor so stale receipts fail closed.
pub fn bump_repo_echo_generation(db: &Database, repo_id: &str) -> Result<i64, DatabaseError> {
    let conn = db.conn();
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = bump_repo_echo_generation_tx(Some(db), &conn, repo_id);
    match result {
        Ok(next) => match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(next),
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(DatabaseError::from(error))
            }
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

pub fn receipt_generation_accepted(record: &serde_json::Value, current_generation: i64) -> bool {
    match record.get("generation") {
        Some(value) => value.as_i64() == Some(current_generation),
        None => current_generation == PAIR_GENERATION,
    }
}

pub fn attach_generation_to_receipt(receipt: &mut serde_json::Value, generation: i64) {
    if let Some(obj) = receipt.as_object_mut() {
        obj.insert("generation".into(), generation.into());
    }
}

pub fn handled_git_no_target_state_key(
    scope_token: &str,
    generation: i64,
    git_sha: &str,
) -> String {
    if generation == PAIR_GENERATION {
        format!("handled_git_no_target_{}_{}", scope_token, git_sha)
    } else {
        format!(
            "handled_git_no_target_{}_g{}_{}",
            scope_token, generation, git_sha
        )
    }
}

pub fn handled_svn_no_target_state_key(scope_token: &str, generation: i64, svn_rev: i64) -> String {
    if generation == PAIR_GENERATION {
        format!("handled_svn_no_target_{}_{}", scope_token, svn_rev)
    } else {
        format!(
            "handled_svn_no_target_{}_g{}_{}",
            scope_token, generation, svn_rev
        )
    }
}

fn legacy_handled_git_no_target_state_key(repo_id: &str, generation: i64, git_sha: &str) -> String {
    handled_git_no_target_state_key(repo_id, generation, git_sha)
}

/// Resolve a stored Git no-target receipt for the active generation, including
/// the legacy generation-1 key shape.
pub fn read_git_no_target_receipt(
    db: &Database,
    repo_id: &str,
    git_sha: &str,
) -> Result<Option<serde_json::Value>, DatabaseError> {
    let (scope, generation, legacy_authoritative) = {
        let conn = db.conn();
        let scope = scope_token_for_repo(&conn, repo_id)?;
        let generation = repo_echo_generation_tx(&conn, repo_id)?;
        let legacy_authoritative = legacy_repo_id_kv_authoritative(&conn)?;
        (scope, generation, legacy_authoritative)
    };
    let keys = if legacy_authoritative {
        [
            format!("handled_git_no_target_{}_{}", repo_id, git_sha),
            legacy_handled_git_no_target_state_key(repo_id, generation, git_sha),
            handled_git_no_target_state_key(&scope, generation, git_sha),
        ]
    } else {
        [
            handled_git_no_target_state_key(&scope, generation, git_sha),
            legacy_handled_git_no_target_state_key(repo_id, generation, git_sha),
            format!("handled_git_no_target_{}_{}", repo_id, git_sha),
        ]
    };
    for key in keys {
        let Some(raw) = db.get_state(&key)? else {
            continue;
        };
        let receipt = serde_json::from_str::<serde_json::Value>(&raw).ok();
        let Some(record) = receipt else {
            continue;
        };
        if !receipt_admits_for_repo(&db.conn(), repo_id, &scope, &record, legacy_authoritative)? {
            continue;
        }
        if receipt_generation_accepted(&record, generation) {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

/// Load a stored Git no-target receipt for `git_sha` without requiring the active generation.
pub fn read_git_no_target_receipt_any_generation(
    db: &Database,
    repo_id: &str,
    git_sha: &str,
) -> Result<Option<serde_json::Value>, DatabaseError> {
    Ok(
        collect_git_no_target_receipts_for_sha(db, repo_id, git_sha)?
            .into_iter()
            .next(),
    )
}

/// Every stored Git no-target receipt for `git_sha`, including legacy key shapes.
pub fn collect_git_no_target_receipts_for_sha(
    db: &Database,
    repo_id: &str,
    git_sha: &str,
) -> Result<Vec<serde_json::Value>, DatabaseError> {
    let (scope, generation, legacy_authoritative) = {
        let conn = db.conn();
        let scope = scope_token_for_repo(&conn, repo_id)?;
        let generation = repo_echo_generation_tx(&conn, repo_id)?;
        let legacy_authoritative = legacy_repo_id_kv_authoritative(&conn)?;
        (scope, generation, legacy_authoritative)
    };
    let mut keys = Vec::new();
    keys.push(legacy_handled_git_no_target_state_key(
        repo_id,
        PAIR_GENERATION,
        git_sha,
    ));
    keys.push(format!("handled_git_no_target_{}_{}", repo_id, git_sha));
    for gen in 1..=generation {
        keys.push(handled_git_no_target_state_key(&scope, gen, git_sha));
        keys.push(legacy_handled_git_no_target_state_key(
            repo_id, gen, git_sha,
        ));
    }
    let mut records = Vec::new();
    for key in keys {
        let Some(raw) = db.get_state(&key)? else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if record["repo_id"] != repo_id || record["git_sha"] != git_sha {
            continue;
        }
        if !receipt_admits_for_repo(&db.conn(), repo_id, &scope, &record, legacy_authoritative)? {
            continue;
        }
        records.push(record);
    }
    Ok(records)
}

/// Whether a generation-accepted, admission-scoped Git no-target receipt exists for `git_sha`.
pub fn stored_git_no_target_receipt_exists(
    db: &Database,
    repo_id: &str,
    git_sha: &str,
    projection: &str,
) -> Result<bool, DatabaseError> {
    let Some(record) = read_git_no_target_receipt(db, repo_id, git_sha)? else {
        return Ok(false);
    };
    let generation = repo_echo_generation(db, repo_id)?;
    let scope = repository_scope_uuid(&db.conn(), repo_id).ok();
    Ok(verify_no_target_receipt(
        &record,
        repo_id,
        git_sha,
        projection,
        generation,
        scope.as_deref(),
    ) == NoTargetReceiptVerdict::Accepted)
}

/// Resolve a stored SVN no-target receipt for the active generation.
pub fn read_svn_no_target_receipt(
    db: &Database,
    repo_id: &str,
    svn_rev: i64,
) -> Result<Option<serde_json::Value>, DatabaseError> {
    let (scope, generation, legacy_authoritative) = {
        let conn = db.conn();
        let scope = scope_token_for_repo(&conn, repo_id)?;
        let generation = repo_echo_generation_tx(&conn, repo_id)?;
        let legacy_authoritative = legacy_repo_id_kv_authoritative(&conn)?;
        (scope, generation, legacy_authoritative)
    };
    let keys = [
        handled_svn_no_target_state_key(&scope, generation, svn_rev),
        handled_svn_no_target_state_key(repo_id, generation, svn_rev),
        format!("handled_svn_no_target_{}_{}", repo_id, svn_rev),
    ];
    for key in keys {
        let Some(raw) = db.get_state(&key)? else {
            continue;
        };
        let receipt = serde_json::from_str::<serde_json::Value>(&raw).ok();
        let Some(record) = receipt else {
            continue;
        };
        if !receipt_admits_for_repo(&db.conn(), repo_id, &scope, &record, legacy_authoritative)? {
            continue;
        }
        if receipt_generation_accepted(&record, generation) {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

/// When a repo-id mirror receipt exists for a scoped repository, it must verify
/// for the same generation or checkpoint logic treats the mirror as tampered.
pub fn legacy_git_no_target_mirror_blocks(
    db: &Database,
    repo_id: &str,
    git_sha: &str,
    projection: &str,
    generation: i64,
) -> Result<bool, DatabaseError> {
    if !managed_repo_requires_scoped_receipts(&db.conn(), repo_id)? {
        return Ok(false);
    }
    let scope = scope_token_for_repo(&db.conn(), repo_id)?;
    let canonical_key = handled_git_no_target_state_key(&scope, generation, git_sha);
    if db.get_state(&canonical_key)?.is_none() {
        return Ok(false);
    }
    let legacy_key = format!("handled_git_no_target_{}_{}", repo_id, git_sha);
    let Some(raw) = db.get_state(&legacy_key)? else {
        return Ok(false);
    };
    let Ok(record) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Ok(true);
    };
    if record.get("scope_uuid").is_none() {
        return Ok(true);
    }
    if !receipt_scope_uuid_matches(&record, &scope) {
        return Ok(true);
    }
    Ok(verify_no_target_receipt(
        &record,
        repo_id,
        git_sha,
        projection,
        generation,
        Some(&scope),
    ) != NoTargetReceiptVerdict::Accepted)
}

/// True when UUID-scoped and repo-id inbound checkpoint mirrors disagree.
pub fn inbound_git_checkpoint_mirror_conflict(
    db: &Database,
    repo_id: &str,
) -> Result<bool, DatabaseError> {
    let scope = {
        let conn = db.conn();
        scope_token_for_repo(&conn, repo_id)?
    };
    let scoped = db
        .get_state(&last_git_sha_kv_key(&scope))?
        .filter(|value| !value.is_empty());
    let legacy = db
        .get_state(&legacy_last_git_sha_kv_key(repo_id))?
        .filter(|value| !value.is_empty());
    Ok(scoped.is_some() && legacy.is_some() && scoped != legacy)
}

/// Read the Git→SVN inbound handled checkpoint (UUID-scoped KV).
pub fn read_scoped_last_git_sha_kv(
    db: &Database,
    repo_id: &str,
) -> Result<Option<String>, DatabaseError> {
    let (scope, legacy_authoritative) = {
        let conn = db.conn();
        let scope = scope_token_for_repo(&conn, repo_id)?;
        let legacy_authoritative = legacy_repo_id_kv_authoritative(&conn)?;
        (scope, legacy_authoritative)
    };
    let scoped = db
        .get_state(&last_git_sha_kv_key(&scope))?
        .filter(|value| !value.is_empty());
    if scoped.is_some() {
        return Ok(scoped);
    }
    if legacy_authoritative {
        return Ok(db
            .get_state(&legacy_last_git_sha_kv_key(repo_id))?
            .filter(|value| !value.is_empty()));
    }
    Ok(None)
}

/// Persist the Git→SVN inbound handled checkpoint (UUID-scoped KV + repo-id mirror).
pub fn write_scoped_last_git_sha_kv(
    tx: &Connection,
    repo_id: &str,
    git_sha: &str,
    updated_at: &str,
) -> Result<(), DatabaseError> {
    let scope = scope_token_for_repo(tx, repo_id)?;
    let kv_key = last_git_sha_kv_key(&scope);
    tx.execute(
        "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![kv_key, git_sha, updated_at],
    )?;
    let legacy_key = legacy_last_git_sha_kv_key(repo_id);
    tx.execute(
        "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![legacy_key, git_sha, updated_at],
    )?;
    Ok(())
}

/// Persist a Git no-target receipt under scoped (and optional legacy) keys.
pub fn write_git_no_target_receipt_kv(
    tx: &Connection,
    repo_id: &str,
    generation: i64,
    git_sha: &str,
    receipt: &str,
    updated_at: &str,
) -> Result<(), DatabaseError> {
    let scope = scope_token_for_repo(tx, repo_id)?;
    let scoped_key = handled_git_no_target_state_key(&scope, generation, git_sha);
    tx.execute(
        "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![scoped_key, receipt, updated_at],
    )?;
    if legacy_repo_id_kv_authoritative(tx)? {
        let legacy_key = handled_git_no_target_state_key(repo_id, generation, git_sha);
        tx.execute(
            "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![legacy_key, receipt, updated_at],
        )?;
    }
    Ok(())
}

pub fn attach_scope_to_new_receipt(
    db: &Database,
    repo_id: &str,
    receipt: &mut serde_json::Value,
) -> Result<(), DatabaseError> {
    if repo_id == crate::db::personal_scope::PERSONAL_SCOPE_KEY {
        return Ok(());
    }
    let scope = repository_scope_uuid(&db.conn(), repo_id)?;
    attach_scope_uuid_to_receipt(receipt, &scope);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::echo_suppression::{
        classify_incoming_git_commit, verify_no_target_receipt, EchoDisposition,
        NoTargetReceiptVerdict, TeamEchoContext,
    };

    fn setup_db() -> Database {
        let db = Database::in_memory().unwrap();
        db.initialize().unwrap();
        db
    }

    fn insert_pair_repo(db: &Database) {
        let scope = crate::db::repo_scope_identity::new_scope_uuid();
        db.conn()
            .execute(
                "INSERT INTO repositories (id, name, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, sync_mode, poll_interval_secs, lfs_threshold_mb, auto_merge, enabled, created_at, updated_at, last_svn_rev, last_git_sha, sync_status, total_syncs, total_errors, consecutive_errors, scope_uuid)
                 VALUES ('pair', 'pair', '', '', '', 'local', '', '', 'main', 'team', 5, 0, 0, 1, 't', 't', 0, '', 'idle', 0, 0, 0, ?1)",
                [scope],
            )
            .unwrap();
    }

    #[test]
    fn stale_generation_receipt_does_not_suppress() {
        let db = setup_db();
        insert_pair_repo(&db);
        let scope = repository_scope_uuid(&db.conn(), "pair").unwrap();
        let sha = "a".repeat(40);
        db.set_state(
            &handled_git_no_target_state_key(&scope, 1, &sha),
            &serde_json::json!({
                "version": 1,
                "repo_id": "pair",
                "scope_uuid": scope,
                "git_sha": sha,
                "outcome": "filtered",
                "projection": "{}",
                "generation": 1,
            })
            .to_string(),
        )
        .unwrap();
        bump_repo_echo_generation(&db, "pair").unwrap();
        let ctx = TeamEchoContext {
            db: &db,
            repo_id: "pair",
            no_target_projection: "{}",
        };
        assert_eq!(
            classify_incoming_git_commit(&ctx, &sha, "no marker")
                .unwrap()
                .unwrap(),
            EchoDisposition::ApplyGenuine
        );
    }

    #[test]
    fn fieldless_legacy_receipt_rejected_at_generation_two() {
        let db = setup_db();
        insert_pair_repo(&db);
        let sha = "c".repeat(40);
        db.set_state(
            &format!("handled_git_no_target_pair_{sha}"),
            &serde_json::json!({
                "version": 1,
                "repo_id": "pair",
                "git_sha": sha,
                "outcome": "filtered",
                "projection": "{}",
            })
            .to_string(),
        )
        .unwrap();
        bump_repo_echo_generation(&db, "pair").unwrap();
        assert_eq!(repo_echo_generation(&db, "pair").unwrap(), 2);
        assert!(read_git_no_target_receipt(&db, "pair", &sha)
            .unwrap()
            .is_none());
        let ctx = TeamEchoContext {
            db: &db,
            repo_id: "pair",
            no_target_projection: "{}",
        };
        assert_eq!(
            classify_incoming_git_commit(&ctx, &sha, "no marker")
                .unwrap()
                .unwrap(),
            EchoDisposition::ApplyGenuine
        );
    }

    #[test]
    fn malformed_legacy_bytes_are_not_valid_stored_receipt() {
        let db = setup_db();
        insert_pair_repo(&db);
        let scope = repository_scope_uuid(&db.conn(), "pair").unwrap();
        let sha = "d".repeat(40);
        db.set_state(
            &handled_git_no_target_state_key(&scope, 1, &sha),
            "not-json",
        )
        .unwrap();
        assert!(
            !stored_git_no_target_receipt_exists(&db, "pair", &sha, "{}").unwrap(),
            "arbitrary legacy kv bytes must not count as a verified no-target receipt"
        );
    }

    #[test]
    fn stale_generation_svn_receipt_is_not_read() {
        let db = setup_db();
        insert_pair_repo(&db);
        let scope = repository_scope_uuid(&db.conn(), "pair").unwrap();
        bump_repo_echo_generation(&db, "pair").unwrap();
        db.set_state(
            &handled_svn_no_target_state_key(&scope, 1, 3),
            &serde_json::json!({
                "version": 1,
                "repo_id": "pair",
                "scope_uuid": scope,
                "svn_revision": 3,
                "outcome": "no_git_content",
                "projection": "{}",
                "generation": 1,
            })
            .to_string(),
        )
        .unwrap();
        assert!(read_svn_no_target_receipt(&db, "pair", 3)
            .unwrap()
            .is_none());
    }

    #[test]
    fn bump_repo_echo_generation_rolls_back_when_split_fails() {
        let db = setup_db();
        db.conn()
            .execute(
                "INSERT INTO repositories (id, name, svn_url, svn_branch, svn_username, git_api_url, git_repo, git_branch, enabled, created_at, updated_at, last_svn_rev, last_git_sha, scope_uuid)
                 VALUES ('pair', 'pair', '', '', '', '', '', '', 1, 't', 't', 0, '', ?1)",
                [crate::db::repo_scope_identity::new_scope_uuid()],
            )
            .unwrap();
        let handled = "b".repeat(40);
        let filtered = "c".repeat(40);
        let now = chrono::Utc::now().to_rfc3339();
        db.conn()
            .execute(
                "INSERT INTO sync_records (id, repo_id, svn_rev, git_sha, direction, author, message, timestamp, synced_at, status)
                 VALUES ('out', 'pair', NULL, ?1, 'git_to_svn', '', '', ?2, ?2, 'applied')",
                rusqlite::params![handled, now],
            )
            .unwrap();
        db.conn()
            .execute(
                "UPDATE repositories SET last_git_sha = ?1 WHERE id = 'pair'",
                [&filtered],
            )
            .unwrap();
        write_scoped_last_git_sha_kv(&db.conn(), "pair", &filtered, &now).unwrap();
        assert_eq!(repo_echo_generation(&db, "pair").unwrap(), 1);
        test_force_next_split_fail(&db);
        assert!(bump_repo_echo_generation(&db, "pair").is_err());
        assert_eq!(repo_echo_generation(&db, "pair").unwrap(), 1);
    }

    #[test]
    fn active_generation_receipt_suppresses() {
        let db = setup_db();
        insert_pair_repo(&db);
        let scope = repository_scope_uuid(&db.conn(), "pair").unwrap();
        let sha = "b".repeat(40);
        let generation = repo_echo_generation(&db, "pair").unwrap();
        let mut receipt = serde_json::json!({
            "version": 1,
            "repo_id": "pair",
            "scope_uuid": scope,
            "git_sha": sha,
            "outcome": "filtered",
            "projection": "{}",
        });
        attach_generation_to_receipt(&mut receipt, generation);
        db.set_state(
            &handled_git_no_target_state_key(&scope, generation, &sha),
            &receipt.to_string(),
        )
        .unwrap();
        let loaded = read_git_no_target_receipt(&db, "pair", &sha)
            .unwrap()
            .unwrap();
        assert_eq!(
            verify_no_target_receipt(&loaded, "pair", &sha, "{}", generation, Some(&scope),),
            NoTargetReceiptVerdict::Accepted
        );
    }
}
