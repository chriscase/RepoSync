//! Generation-scoped echo receipt keys and validation (#63).
//!
//! Until pair-generation tables are activated, each repository uses a monotonic
//! `repo_echo_generation_<repo_id>` kv counter (defaulting to 1). Receipts
//! written after a bump carry the active generation; older receipts cannot
//! suppress or satisfy a later generation.

use rusqlite::{Connection, OptionalExtension};

use crate::db::Database;
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

/// Bump the echo generation after a reset/re-anchor so stale receipts fail closed.
pub fn bump_repo_echo_generation(db: &Database, repo_id: &str) -> Result<i64, DatabaseError> {
    let next = repo_echo_generation(db, repo_id)? + 1;
    db.set_state(
        &format!("{GENERATION_KV_PREFIX}{repo_id}"),
        &next.to_string(),
    )?;
    Ok(next)
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
            verify_no_target_receipt(&loaded, "pair", &sha, "{}"),
            NoTargetReceiptVerdict::Accepted
        );
    }
}
