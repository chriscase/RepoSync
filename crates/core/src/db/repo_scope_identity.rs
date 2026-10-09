//! Durable per-repository scope identity (#63).
//!
//! Managed echo receipts, generations, and Git checkpoint KV keys are scoped by
//! `repositories.scope_uuid`, not the human-chosen `id`, so delete + re-register
//! cannot reuse stale scoped state.

use rusqlite::{params, Connection, OptionalExtension};

use crate::errors::DatabaseError;

pub const SCOPE_UUID_COLUMN: &str = "scope_uuid";

const LEGACY_REPO_ID_KV_READS_KEY: &str = "reposync_v13_legacy_repo_id_kv_reads";

const HANDLED_GIT_NO_TARGET_PREFIX: &str = "handled_git_no_target_";
const HANDLED_SVN_NO_TARGET_PREFIX: &str = "handled_svn_no_target_";

/// Parsed tail of a handled_*_no_target KV key (scope token is exact, not prefix-matched).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedNoTargetReceiptKey {
    scope_token: String,
    generation: i64,
    checkpoint: String,
}

fn is_full_git_sha(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_svn_rev_token(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_digit())
}

fn parse_handled_git_no_target_key(key: &str) -> Option<ParsedNoTargetReceiptKey> {
    let body = key.strip_prefix(HANDLED_GIT_NO_TARGET_PREFIX)?;
    let (mid, sha) = body.rsplit_once('_')?;
    if !is_full_git_sha(sha) {
        return None;
    }
    if let Some((scope_token, gen_part)) = mid.rsplit_once('_') {
        if let Some(gen_digits) = gen_part.strip_prefix('g') {
            if gen_digits.chars().all(|c| c.is_ascii_digit()) && !gen_digits.is_empty() {
                let generation = gen_digits.parse().ok()?;
                return Some(ParsedNoTargetReceiptKey {
                    scope_token: scope_token.to_string(),
                    generation,
                    checkpoint: sha.to_string(),
                });
            }
        }
    }
    Some(ParsedNoTargetReceiptKey {
        scope_token: mid.to_string(),
        generation: crate::pair_refresh::PAIR_GENERATION,
        checkpoint: sha.to_string(),
    })
}

fn parse_handled_svn_no_target_key(key: &str) -> Option<ParsedNoTargetReceiptKey> {
    let body = key.strip_prefix(HANDLED_SVN_NO_TARGET_PREFIX)?;
    let (mid, rev) = body.rsplit_once('_')?;
    if !is_svn_rev_token(rev) {
        return None;
    }
    if let Some((scope_token, gen_part)) = mid.rsplit_once('_') {
        if let Some(gen_digits) = gen_part.strip_prefix('g') {
            if gen_digits.chars().all(|c| c.is_ascii_digit()) && !gen_digits.is_empty() {
                let generation = gen_digits.parse().ok()?;
                return Some(ParsedNoTargetReceiptKey {
                    scope_token: scope_token.to_string(),
                    generation,
                    checkpoint: rev.to_string(),
                });
            }
        }
    }
    Some(ParsedNoTargetReceiptKey {
        scope_token: mid.to_string(),
        generation: crate::pair_refresh::PAIR_GENERATION,
        checkpoint: rev.to_string(),
    })
}

fn legacy_no_target_key_owned_by_human_id(key: &str, repo_id: &str, git: bool) -> bool {
    let parsed = if git {
        parse_handled_git_no_target_key(key)
    } else {
        parse_handled_svn_no_target_key(key)
    };
    parsed.is_some_and(|p| p.scope_token == repo_id)
}

fn no_target_key_owned_by_scope_uuid(key: &str, scope_uuid: &str, git: bool) -> bool {
    let parsed = if git {
        parse_handled_git_no_target_key(key)
    } else {
        parse_handled_svn_no_target_key(key)
    };
    parsed.is_some_and(|p| p.scope_token == scope_uuid)
}

fn list_kv_keys_glob(conn: &Connection, glob: &str) -> Result<Vec<String>, DatabaseError> {
    let mut stmt = conn.prepare("SELECT key FROM kv_state WHERE key GLOB ?1")?;
    let keys = stmt
        .query_map([glob], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(keys)
}

fn legacy_receipt_payload_matches_repo(value: &str, repo_id: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(record) => record.get("repo_id").and_then(|v| v.as_str()) == Some(repo_id),
        Err(_) => false,
    }
}

/// True when any no-target receipt KV key is owned by this repository (exact scope token).
pub fn repository_has_stored_no_target_receipt_kv(
    conn: &Connection,
    repo_id: &str,
) -> Result<bool, DatabaseError> {
    let scope = match repository_scope_uuid(conn, repo_id) {
        Ok(scope) => scope,
        Err(DatabaseError::NotFound { entity, .. }) if entity == "repository" => {
            repo_id.to_string()
        }
        Err(other) => return Err(other),
    };
    let legacy_authoritative = legacy_repo_id_kv_authoritative(conn)?;
    for key in list_kv_keys_glob(conn, "handled_git_no_target_*")? {
        if no_target_key_owned_by_scope_uuid(&key, &scope, true)
            || (legacy_authoritative && legacy_no_target_key_owned_by_human_id(&key, repo_id, true))
        {
            return Ok(true);
        }
    }
    for key in list_kv_keys_glob(conn, "handled_svn_no_target_*")? {
        if no_target_key_owned_by_scope_uuid(&key, &scope, false)
            || (legacy_authoritative
                && legacy_no_target_key_owned_by_human_id(&key, repo_id, false))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Scope token for KV/receipt keys: durable `scope_uuid` when the repository row
/// exists, otherwise the caller-supplied id (late pair bootstrap before registration).
pub fn repository_scope_token(conn: &Connection, repo_id: &str) -> Result<String, DatabaseError> {
    match repository_scope_uuid(conn, repo_id) {
        Ok(scope) => Ok(scope),
        Err(DatabaseError::NotFound { entity, .. }) if entity == "repository" => {
            Ok(repo_id.to_string())
        }
        Err(other) => Err(other),
    }
}

pub fn repository_scope_uuid(conn: &Connection, repo_id: &str) -> Result<String, DatabaseError> {
    let row_exists: bool = conn
        .query_row(
            "SELECT 1 FROM repositories WHERE id = ?1",
            [repo_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !row_exists {
        return Err(DatabaseError::NotFound {
            entity: "repository".into(),
            id: repo_id.into(),
        });
    }
    let existing: Option<String> = conn
        .query_row(
            &format!("SELECT {SCOPE_UUID_COLUMN} FROM repositories WHERE id = ?1"),
            [repo_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .filter(|value| !value.is_empty());
    if let Some(value) = existing {
        return Ok(value);
    }
    let scope_uuid = new_scope_uuid();
    let updated = conn.execute(
        &format!(
            "UPDATE repositories SET {SCOPE_UUID_COLUMN} = ?1 WHERE id = ?2 AND ({SCOPE_UUID_COLUMN} IS NULL OR {SCOPE_UUID_COLUMN} = '')"
        ),
        params![scope_uuid, repo_id],
    )?;
    if updated > 0 {
        return Ok(scope_uuid);
    }
    conn.query_row(
        &format!("SELECT {SCOPE_UUID_COLUMN} FROM repositories WHERE id = ?1"),
        [repo_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()?
    .flatten()
    .filter(|value| !value.is_empty())
    .ok_or_else(|| DatabaseError::NotFound {
        entity: "repository scope_uuid".into(),
        id: repo_id.into(),
    })
}

pub fn new_scope_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn last_git_sha_kv_key(scope_uuid: &str) -> String {
    format!("last_git_sha_{scope_uuid}")
}

pub fn legacy_last_git_sha_kv_key(repo_id: &str) -> String {
    format!("last_git_sha_{repo_id}")
}

pub fn repo_echo_generation_kv_key(scope_uuid: &str) -> String {
    format!("repo_echo_generation_{scope_uuid}")
}

pub fn legacy_repo_echo_generation_kv_key(repo_id: &str) -> String {
    format!("repo_echo_generation_{repo_id}")
}

pub fn team_history_block_kv_key(scope_uuid: &str) -> String {
    format!("team_history_block_{scope_uuid}")
}

pub fn legacy_team_history_block_kv_key(repo_id: &str) -> String {
    format!("team_history_block_{repo_id}")
}

/// Legacy repo-id KV may be read only when v13 migration bound it for a sole repository,
/// or before any repository row exists (pre-registration callers). Live row count must
/// never flip an explicit multi-repo revoke (`"0"`) back to authoritative.
pub fn legacy_repo_id_kv_authoritative(conn: &Connection) -> Result<bool, DatabaseError> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))?;
    let flag: Option<String> = conn
        .query_row(
            "SELECT value FROM kv_state WHERE key = ?1",
            [LEGACY_REPO_ID_KV_READS_KEY],
            |row| row.get(0),
        )
        .optional()?;
    if count > 1 {
        if flag.as_deref() != Some("0") {
            revoke_legacy_repo_id_kv_reads(conn)?;
            let mut stmt = conn.prepare("SELECT id FROM repositories ORDER BY id")?;
            let repo_ids: Vec<String> = stmt
                .query_map([], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();
            for repo_id in &repo_ids {
                quarantine_unscoped_legacy_receipts(conn, repo_id)?;
            }
        }
        return Ok(false);
    }
    match flag.as_deref() {
        Some("1") => Ok(true),
        Some("0") => Ok(false),
        Some(_) => Ok(false),
        None => match count {
            0 => Ok(true),
            1 => {
                ensure_sole_repository_legacy_kv_bound(conn)?;
                Ok(true)
            }
            _ => Ok(false),
        },
    }
}

/// Bind legacy repo-id KV to the sole repository's `scope_uuid` once (durable flag absent).
fn ensure_sole_repository_legacy_kv_bound(conn: &Connection) -> Result<(), DatabaseError> {
    let repo_id: String = conn.query_row(
        "SELECT id FROM repositories ORDER BY id LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let scope_uuid = repository_scope_uuid(conn, &repo_id)?;
    migrate_legacy_kv_for_repo(conn, &repo_id, &scope_uuid)?;
    set_legacy_repo_id_kv_reads_enabled(conn, true)?;
    Ok(())
}

pub fn set_legacy_repo_id_kv_reads_enabled(
    conn: &Connection,
    enabled: bool,
) -> Result<(), DatabaseError> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![
            LEGACY_REPO_ID_KV_READS_KEY,
            if enabled { "1" } else { "0" },
            now
        ],
    )?;
    Ok(())
}

pub fn revoke_legacy_repo_id_kv_reads(conn: &Connection) -> Result<(), DatabaseError> {
    set_legacy_repo_id_kv_reads_enabled(conn, false)
}

/// Remove durable echo/checkpoint KV for one repository (legacy + UUID scoped).
pub fn purge_repository_scope_kv(
    conn: &Connection,
    repo_id: &str,
    scope_uuid: Option<&str>,
) -> Result<(), DatabaseError> {
    for key in list_kv_keys_glob(conn, "handled_git_no_target_*")? {
        let delete = legacy_no_target_key_owned_by_human_id(&key, repo_id, true)
            || scope_uuid.is_some_and(|scope| no_target_key_owned_by_scope_uuid(&key, scope, true));
        if delete {
            conn.execute("DELETE FROM kv_state WHERE key = ?1", [&key])?;
        }
    }
    for key in list_kv_keys_glob(conn, "handled_svn_no_target_*")? {
        let delete = legacy_no_target_key_owned_by_human_id(&key, repo_id, false)
            || scope_uuid
                .is_some_and(|scope| no_target_key_owned_by_scope_uuid(&key, scope, false));
        if delete {
            conn.execute("DELETE FROM kv_state WHERE key = ?1", [&key])?;
        }
    }
    if let Some(scope) = scope_uuid {
        for key in [
            last_git_sha_kv_key(scope),
            repo_echo_generation_kv_key(scope),
            team_history_block_kv_key(scope),
        ] {
            conn.execute("DELETE FROM kv_state WHERE key = ?1", [&key])?;
        }
    }
    for key in [
        legacy_last_git_sha_kv_key(repo_id),
        legacy_repo_echo_generation_kv_key(repo_id),
        legacy_team_history_block_kv_key(repo_id),
        format!("last_svn_rev_{repo_id}"),
    ] {
        conn.execute("DELETE FROM kv_state WHERE key = ?1", [&key])?;
    }
    Ok(())
}

fn quarantine_unscoped_legacy_receipts(
    conn: &Connection,
    repo_id: &str,
) -> Result<(), DatabaseError> {
    for key in list_kv_keys_glob(conn, "handled_git_no_target_*")? {
        if !legacy_no_target_key_owned_by_human_id(&key, repo_id, true) {
            continue;
        }
        let value: Option<String> = conn
            .query_row("SELECT value FROM kv_state WHERE key = ?1", [&key], |row| {
                row.get(0)
            })
            .optional()?;
        let quarantine = match value {
            None => true,
            Some(raw) => serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .and_then(|record| record.get("scope_uuid").cloned())
                .is_none(),
        };
        if quarantine {
            conn.execute("DELETE FROM kv_state WHERE key = ?1", [&key])?;
        }
    }
    for key in list_kv_keys_glob(conn, "handled_svn_no_target_*")? {
        if !legacy_no_target_key_owned_by_human_id(&key, repo_id, false) {
            continue;
        }
        let value: Option<String> = conn
            .query_row("SELECT value FROM kv_state WHERE key = ?1", [&key], |row| {
                row.get(0)
            })
            .optional()?;
        let quarantine = match value {
            None => true,
            Some(raw) => serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .and_then(|record| record.get("scope_uuid").cloned())
                .is_none(),
        };
        if quarantine {
            conn.execute("DELETE FROM kv_state WHERE key = ?1", [&key])?;
        }
    }
    Ok(())
}

pub fn receipt_scope_uuid_matches(record: &serde_json::Value, scope_uuid: &str) -> bool {
    match record.get("scope_uuid") {
        Some(value) => value.as_str() == Some(scope_uuid),
        None => false,
    }
}

pub fn attach_scope_uuid_to_receipt(receipt: &mut serde_json::Value, scope_uuid: &str) {
    if let Some(obj) = receipt.as_object_mut() {
        obj.insert("scope_uuid".into(), scope_uuid.into());
    }
}

fn copy_kv_if_absent(conn: &Connection, from_key: &str, to_key: &str) -> Result<(), DatabaseError> {
    if from_key == to_key {
        return Ok(());
    }
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM kv_state WHERE key = ?1",
        [to_key],
        |row| row.get(0),
    )?;
    if exists > 0 {
        return Ok(());
    }
    let migrated = conn.execute(
        "INSERT INTO kv_state (key, value, updated_at)
         SELECT ?1, value, updated_at FROM kv_state WHERE key = ?2",
        params![to_key, from_key],
    )?;
    if migrated > 0 {
        conn.execute("DELETE FROM kv_state WHERE key = ?1", [from_key])?;
    }
    Ok(())
}

pub(crate) fn migrate_legacy_kv_for_repo(
    conn: &Connection,
    repo_id: &str,
    scope_uuid: &str,
) -> Result<(), DatabaseError> {
    copy_kv_if_absent(
        conn,
        &legacy_last_git_sha_kv_key(repo_id),
        &last_git_sha_kv_key(scope_uuid),
    )?;
    copy_kv_if_absent(
        conn,
        &legacy_repo_echo_generation_kv_key(repo_id),
        &repo_echo_generation_kv_key(scope_uuid),
    )?;
    copy_kv_if_absent(
        conn,
        &legacy_team_history_block_kv_key(repo_id),
        &team_history_block_kv_key(scope_uuid),
    )?;

    for key in list_kv_keys_glob(conn, "handled_git_no_target_*")? {
        if !legacy_no_target_key_owned_by_human_id(&key, repo_id, true) {
            continue;
        }
        let (value, updated_at): (String, String) = conn.query_row(
            "SELECT value, updated_at FROM kv_state WHERE key = ?1",
            [&key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if !legacy_receipt_payload_matches_repo(&value, repo_id) {
            continue;
        }
        let Some(parsed) = parse_handled_git_no_target_key(&key) else {
            continue;
        };
        let scoped_key = crate::echo_receipt_scope::handled_git_no_target_state_key(
            scope_uuid,
            parsed.generation,
            &parsed.checkpoint,
        );
        copy_kv_if_absent(conn, &key, &scoped_key)?;
        if let Ok(mut record) = serde_json::from_str::<serde_json::Value>(&value) {
            attach_scope_uuid_to_receipt(&mut record, scope_uuid);
            conn.execute(
                "UPDATE kv_state SET value = ?1, updated_at = ?2 WHERE key = ?3",
                params![record.to_string(), updated_at, scoped_key],
            )?;
        }
    }
    for key in list_kv_keys_glob(conn, "handled_svn_no_target_*")? {
        if !legacy_no_target_key_owned_by_human_id(&key, repo_id, false) {
            continue;
        }
        let (value, updated_at): (String, String) = conn.query_row(
            "SELECT value, updated_at FROM kv_state WHERE key = ?1",
            [&key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if !legacy_receipt_payload_matches_repo(&value, repo_id) {
            continue;
        }
        let Some(parsed) = parse_handled_svn_no_target_key(&key) else {
            continue;
        };
        let svn_rev = parsed.checkpoint.parse::<i64>().unwrap_or(0);
        let scoped_key = crate::echo_receipt_scope::handled_svn_no_target_state_key(
            scope_uuid,
            parsed.generation,
            svn_rev,
        );
        copy_kv_if_absent(conn, &key, &scoped_key)?;
        if let Ok(mut record) = serde_json::from_str::<serde_json::Value>(&value) {
            attach_scope_uuid_to_receipt(&mut record, scope_uuid);
            conn.execute(
                "UPDATE kv_state SET value = ?1, updated_at = ?2 WHERE key = ?3",
                params![record.to_string(), updated_at, scoped_key],
            )?;
        }
    }
    Ok(())
}

/// v13: assign `scope_uuid` to every repository and migrate legacy scoped KV when safe.
pub fn migrate_v13_scope_uuid(conn: &Connection) -> Result<(), DatabaseError> {
    let mut stmt = conn.prepare("SELECT id FROM repositories ORDER BY id")?;
    let repo_ids: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    for repo_id in &repo_ids {
        let existing: Option<String> = conn
            .query_row(
                &format!("SELECT {SCOPE_UUID_COLUMN} FROM repositories WHERE id = ?1"),
                [repo_id.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        let scope_uuid = if let Some(value) = existing.filter(|v| !v.is_empty()) {
            value
        } else {
            let scope_uuid = new_scope_uuid();
            conn.execute(
                &format!(
                    "UPDATE repositories SET {SCOPE_UUID_COLUMN} = ?1 WHERE id = ?2 AND ({SCOPE_UUID_COLUMN} IS NULL OR {SCOPE_UUID_COLUMN} = '')"
                ),
                params![scope_uuid, repo_id.as_str()],
            )?;
            scope_uuid
        };
        migrate_legacy_kv_for_repo(conn, repo_id, &scope_uuid)?;
    }
    if repo_ids.len() == 1 {
        set_legacy_repo_id_kv_reads_enabled(conn, true)?;
    } else if repo_ids.len() > 1 {
        set_legacy_repo_id_kv_reads_enabled(conn, false)?;
        for repo_id in &repo_ids {
            quarantine_unscoped_legacy_receipts(conn, repo_id)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::run_migrations;
    use crate::echo_receipt_scope::{
        attach_generation_to_receipt, handled_git_no_target_state_key, repo_echo_generation,
    };
    use crate::echo_suppression::{classify_incoming_git_commit, EchoDisposition, TeamEchoContext};
    use rusqlite::Connection;

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        conn
    }

    fn insert_repo(conn: &Connection, id: &str) {
        let scope_uuid = new_scope_uuid();
        conn.execute(
            "INSERT INTO repositories (id, name, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, sync_mode, poll_interval_secs, lfs_threshold_mb, auto_merge, enabled, created_at, updated_at, last_svn_rev, last_git_sha, sync_status, total_syncs, total_errors, consecutive_errors, scope_uuid)
             VALUES (?1, ?1, 'file:///x', '', '', 'local', '', '', 'main', 'team', 5, 0, 0, 1, 't', 't', 0, '', 'idle', 0, 0, 0, ?2)",
            params![id, scope_uuid],
        )
        .unwrap();
    }

    #[test]
    fn migration_backfill_is_idempotent() {
        let conn = setup_conn();
        conn.execute(
            "INSERT INTO repositories (id, name, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, sync_mode, poll_interval_secs, lfs_threshold_mb, auto_merge, enabled, created_at, updated_at, last_svn_rev, last_git_sha, sync_status, total_syncs, total_errors, consecutive_errors)
             VALUES ('legacy', 'legacy', 'file:///x', '', '', 'local', '', '', 'main', 'team', 5, 0, 0, 1, 't', 't', 0, '', 'idle', 0, 0, 0)",
            [],
        )
        .unwrap();
        migrate_v13_scope_uuid(&conn).unwrap();
        let before = repository_scope_uuid(&conn, "legacy").unwrap();
        migrate_v13_scope_uuid(&conn).unwrap();
        assert_eq!(repository_scope_uuid(&conn, "legacy").unwrap(), before);
    }

    #[test]
    fn hard_delete_purges_legacy_receipt_suffix_keys() {
        let db = crate::db::Database::in_memory().unwrap();
        db.initialize().unwrap();
        insert_repo(&db.conn(), "pair");
        let sha = "f".repeat(40);
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
        db.hard_delete_repository("pair").unwrap();
        assert!(db
            .get_state(&format!("handled_git_no_target_pair_{sha}"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn delete_reregister_same_id_old_receipt_does_not_suppress() {
        let db = crate::db::Database::in_memory().unwrap();
        db.initialize().unwrap();
        insert_repo(&db.conn(), "pair");
        let old_scope = repository_scope_uuid(&db.conn(), "pair").unwrap();
        let sha = "a".repeat(40);
        let generation = repo_echo_generation(&db, "pair").unwrap();
        let mut receipt = serde_json::json!({
            "version": 1,
            "repo_id": "pair",
            "scope_uuid": old_scope,
            "git_sha": sha,
            "outcome": "filtered",
            "projection": "{}",
        });
        attach_generation_to_receipt(&mut receipt, generation);
        db.set_state(
            &handled_git_no_target_state_key(&old_scope, generation, &sha),
            &receipt.to_string(),
        )
        .unwrap();
        db.conn()
            .execute("DELETE FROM repositories WHERE id = 'pair'", [])
            .unwrap();
        insert_repo(&db.conn(), "pair");
        let new_scope = repository_scope_uuid(&db.conn(), "pair").unwrap();
        assert_ne!(old_scope, new_scope);
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
    fn v13_migration_escapes_wildcard_repo_id_in_receipt_prefix() {
        let conn = setup_conn();
        let wild = "wild%_id";
        let scope_uuid = new_scope_uuid();
        conn.execute(
            "INSERT INTO repositories (id, name, svn_url, svn_branch, svn_username, git_provider, git_api_url, git_repo, git_branch, sync_mode, poll_interval_secs, lfs_threshold_mb, auto_merge, enabled, created_at, updated_at, last_svn_rev, last_git_sha, sync_status, total_syncs, total_errors, consecutive_errors, scope_uuid)
             VALUES (?1, ?1, 'file:///x', '', '', 'local', '', '', 'main', 'team', 5, 0, 0, 1, 't', 't', 0, '', 'idle', 0, 0, 0, ?2)",
            params![wild, scope_uuid],
        )
        .unwrap();
        let sha = "d".repeat(40);
        let decoy_sha = "e".repeat(40);
        conn.execute(
            "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, 't')",
            params![
                format!("handled_git_no_target_{wild}_{sha}"),
                serde_json::json!({
                    "version": 1,
                    "repo_id": wild,
                    "git_sha": sha,
                    "outcome": "filtered",
                    "projection": "{}",
                    "generation": 1,
                })
                .to_string()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, 't')",
            params![
                format!("handled_git_no_target_{wild}x_{decoy_sha}"),
                serde_json::json!({
                    "version": 1,
                    "repo_id": "wild%x",
                    "git_sha": decoy_sha,
                    "outcome": "filtered",
                    "projection": "{}",
                    "generation": 1,
                })
                .to_string()
            ],
        )
        .unwrap();
        migrate_v13_scope_uuid(&conn).unwrap();
        let scope = repository_scope_uuid(&conn, wild).unwrap();
        assert!(
            conn.query_row(
                "SELECT COUNT(*) FROM kv_state WHERE key = ?1",
                params![handled_git_no_target_state_key(&scope, 1, &sha)],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
                > 0,
            "expected receipt migrated under scope_uuid key"
        );
        assert!(
            conn.query_row(
                "SELECT COUNT(*) FROM kv_state WHERE key = ?1",
                params![handled_git_no_target_state_key(&scope, 1, &decoy_sha)],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
                == 0,
            "LIKE migration must not pick up keys from a different repo id prefix"
        );
    }

    #[test]
    fn write_scoped_inbound_checkpoint_does_not_block() {
        let db = crate::db::Database::in_memory().unwrap();
        db.initialize().unwrap();
        insert_repo(&db.conn(), "pair");
        let before = "b".repeat(40);
        let after = "a".repeat(40);
        crate::echo_receipt_scope::write_scoped_last_git_sha_kv(&db.conn(), "pair", &before, "t")
            .unwrap();
        use crate::db::svn_commit_operations::{IntendedPath, SvnCommitIntent};
        let op = db
            .begin_git_to_svn_commit(SvnCommitIntent {
                repo_id: "pair",
                initiator_id: "worker",
                request_id: "req",
                target_fingerprint: "fp",
                source_git_sha: &after,
                source_git_parent: Some(&before),
                source_git_tree: "tree",
                target_svn_uuid: "uuid",
                target_svn_path: "file:///fixture/trunk",
                target_svn_root_url: "file:///fixture",
                target_svn_branch_path: "trunk",
                pre_write_svn_rev: 2,
                pre_write_svn_tree: "pre",
                projection: "{}",
                intended_changed_paths: vec![IntendedPath {
                    action: "A".into(),
                    path: "a.txt".into(),
                    content_sha256: Some("d".repeat(64)),
                }],
                intended_svn_tree: "post",
                author: "fixture",
                source_message: "fixture change",
            })
            .unwrap();
        db.confirm_git_to_svn_commit("pair", &op.id, 3, "post")
            .unwrap();
        assert_eq!(
            crate::echo_receipt_scope::read_scoped_last_git_sha_kv(&db, "pair").unwrap(),
            Some(after),
        );
    }

    #[test]
    fn legacy_receipt_without_scope_uuid_is_not_authoritative() {
        let db = crate::db::Database::in_memory().unwrap();
        db.initialize().unwrap();
        insert_repo(&db.conn(), "one");
        insert_repo(&db.conn(), "two");
        let sha = "b".repeat(40);
        db.set_state(
            &format!("handled_git_no_target_one_{sha}"),
            &serde_json::json!({
                "version": 1,
                "repo_id": "one",
                "git_sha": sha,
                "outcome": "filtered",
                "projection": "{}",
                "generation": 1,
            })
            .to_string(),
        )
        .unwrap();
        let ctx = TeamEchoContext {
            db: &db,
            repo_id: "one",
            no_target_projection: "{}",
        };
        assert_eq!(
            classify_incoming_git_commit(&ctx, &sha, "no marker")
                .unwrap()
                .unwrap(),
            EchoDisposition::ApplyGenuine
        );
    }
}
