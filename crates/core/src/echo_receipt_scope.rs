//! Generation-scoped echo receipt keys and validation (#63).
//!
//! Until pair-generation tables are activated, each repository uses a monotonic
//! `repo_echo_generation_<repo_id>` kv counter (defaulting to 1). Receipts
//! written after a bump carry the active generation; older receipts cannot
//! suppress or satisfy a later generation.

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

use crate::db::Database;
use crate::echo_suppression::{verify_no_target_receipt, NoTargetReceiptVerdict};
use crate::errors::DatabaseError;
use crate::pair_refresh::PAIR_GENERATION;

const GENERATION_KV_PREFIX: &str = "repo_echo_generation_";

/// Active echo-suppression generation for a managed repository scope.
pub fn repo_echo_generation_tx(tx: &Connection, repo_id: &str) -> Result<i64, DatabaseError> {
    let key = format!("{GENERATION_KV_PREFIX}{repo_id}");
    let raw: Option<String> = tx
        .query_row("SELECT value FROM kv_state WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .optional()?;
    match raw {
        Some(value) => value
            .parse::<i64>()
            .map_err(|_| DatabaseError::Other("invalid repo echo generation".into())),
        None => Ok(PAIR_GENERATION),
    }
}

pub fn repo_echo_generation(db: &Database, repo_id: &str) -> Result<i64, DatabaseError> {
    let key = format!("{GENERATION_KV_PREFIX}{repo_id}");
    match db.get_state(&key)? {
        Some(raw) => raw
            .parse::<i64>()
            .map_err(|_| DatabaseError::Other("invalid repo echo generation".into())),
        None => Ok(PAIR_GENERATION),
    }
}

/// When a generation bump follows a unified no-target cursor, restore the scoped
/// KV copy to the last applied outbound Git SHA so stale receipts cannot admit P.
fn split_unified_git_cursor_to_outbound_kv_tx(
    tx: &Connection,
    repo_id: &str,
) -> Result<(), DatabaseError> {
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
    let kv_key = format!("last_git_sha_{}", repo_id);
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
    Ok(())
}

/// Bump the echo generation on an open connection (same transaction as reset).
pub fn bump_repo_echo_generation_tx(tx: &Connection, repo_id: &str) -> Result<i64, DatabaseError> {
    let next = repo_echo_generation_tx(tx, repo_id)? + 1;
    let key = format!("{GENERATION_KV_PREFIX}{repo_id}");
    let now = Utc::now().to_rfc3339();
    tx.execute(
        "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, next.to_string(), now],
    )?;
    split_unified_git_cursor_to_outbound_kv_tx(tx, repo_id)?;
    Ok(next)
}

/// Bump the echo generation after a reset/re-anchor so stale receipts fail closed.
pub fn bump_repo_echo_generation(db: &Database, repo_id: &str) -> Result<i64, DatabaseError> {
    let conn = db.conn();
    bump_repo_echo_generation_tx(&conn, repo_id)
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

pub fn handled_git_no_target_state_key(repo_id: &str, generation: i64, git_sha: &str) -> String {
    if generation == PAIR_GENERATION {
        format!("handled_git_no_target_{}_{}", repo_id, git_sha)
    } else {
        format!(
            "handled_git_no_target_{}_g{}_{}",
            repo_id, generation, git_sha
        )
    }
}

pub fn handled_svn_no_target_state_key(repo_id: &str, generation: i64, svn_rev: i64) -> String {
    if generation == PAIR_GENERATION {
        format!("handled_svn_no_target_{}_{}", repo_id, svn_rev)
    } else {
        format!(
            "handled_svn_no_target_{}_g{}_{}",
            repo_id, generation, svn_rev
        )
    }
}

/// Resolve a stored Git no-target receipt for the active generation, including
/// the legacy generation-1 key shape.
pub fn read_git_no_target_receipt(
    db: &Database,
    repo_id: &str,
    git_sha: &str,
) -> Result<Option<serde_json::Value>, DatabaseError> {
    let generation = repo_echo_generation(db, repo_id)?;
    let keys = [
        handled_git_no_target_state_key(repo_id, generation, git_sha),
        format!("handled_git_no_target_{}_{}", repo_id, git_sha),
    ];
    for key in keys {
        let Some(raw) = db.get_state(&key)? else {
            continue;
        };
        let receipt = serde_json::from_str::<serde_json::Value>(&raw).ok();
        let Some(record) = receipt else {
            continue;
        };
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
    let generation = repo_echo_generation(db, repo_id)?;
    let mut keys = Vec::new();
    keys.push(format!("handled_git_no_target_{}_{}", repo_id, git_sha));
    for gen in 1..=generation {
        keys.push(handled_git_no_target_state_key(repo_id, gen, git_sha));
    }
    let mut records = Vec::new();
    for key in keys {
        let Some(raw) = db.get_state(&key)? else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        if record["repo_id"] == repo_id && record["git_sha"] == git_sha {
            records.push(record);
        }
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
    Ok(
        verify_no_target_receipt(&record, repo_id, git_sha, projection, generation)
            == NoTargetReceiptVerdict::Accepted,
    )
}

/// Resolve a stored SVN no-target receipt for the active generation.
pub fn read_svn_no_target_receipt(
    db: &Database,
    repo_id: &str,
    svn_rev: i64,
) -> Result<Option<serde_json::Value>, DatabaseError> {
    let generation = repo_echo_generation(db, repo_id)?;
    let keys = [
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
        if receipt_generation_accepted(&record, generation) {
            return Ok(Some(record));
        }
    }
    Ok(None)
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

    #[test]
    fn stale_generation_receipt_does_not_suppress() {
        let db = setup_db();
        let sha = "a".repeat(40);
        db.set_state(
            &format!("handled_git_no_target_pair_{sha}"),
            &serde_json::json!({
                "version": 1,
                "repo_id": "pair",
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
        let sha = "d".repeat(40);
        db.set_state(&format!("handled_git_no_target_pair_{sha}"), "not-json")
            .unwrap();
        assert!(
            !stored_git_no_target_receipt_exists(&db, "pair", &sha, "{}").unwrap(),
            "arbitrary legacy kv bytes must not count as a verified no-target receipt"
        );
    }

    #[test]
    fn stale_generation_svn_receipt_is_not_read() {
        let db = setup_db();
        bump_repo_echo_generation(&db, "pair").unwrap();
        db.set_state(
            "handled_svn_no_target_pair_3",
            &serde_json::json!({
                "version": 1,
                "repo_id": "pair",
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
    fn active_generation_receipt_suppresses() {
        let db = setup_db();
        let sha = "b".repeat(40);
        let generation = repo_echo_generation(&db, "pair").unwrap();
        let mut receipt = serde_json::json!({
            "version": 1,
            "repo_id": "pair",
            "git_sha": sha,
            "outcome": "filtered",
            "projection": "{}",
        });
        attach_generation_to_receipt(&mut receipt, generation);
        db.set_state(
            &handled_git_no_target_state_key("pair", generation, &sha),
            &receipt.to_string(),
        )
        .unwrap();
        let loaded = read_git_no_target_receipt(&db, "pair", &sha)
            .unwrap()
            .unwrap();
        assert_eq!(
            verify_no_target_receipt(&loaded, "pair", &sha, "{}", generation),
            NoTargetReceiptVerdict::Accepted
        );
    }
}
