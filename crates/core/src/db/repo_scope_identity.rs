//! Durable per-repository scope identity (#63).
//!
//! Managed echo receipts, generations, and Git checkpoint KV keys are scoped by
//! `repositories.scope_uuid`, not the human-chosen `id`, so delete + re-register
//! cannot reuse stale scoped state.

use rusqlite::{params, Connection, OptionalExtension};

use crate::errors::DatabaseError;

pub const SCOPE_UUID_COLUMN: &str = "scope_uuid";

/// Load the durable scope UUID for a managed repository row, assigning one lazily
/// when legacy rows predate v13.
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

/// Legacy repo-id KV may be read only when at most one managed repository exists.
pub fn legacy_repo_id_kv_authoritative(conn: &Connection) -> Result<bool, DatabaseError> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))?;
    Ok(count <= 1)
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

fn migrate_legacy_kv_for_repo(
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

    let git_prefix = format!("handled_git_no_target_{repo_id}_");
    let svn_prefix = format!("handled_svn_no_target_{repo_id}_");
    let mut stmt = conn.prepare("SELECT key, value, updated_at FROM kv_state WHERE key LIKE ?1")?;
    let git_rows: Vec<(String, String, String)> = stmt
        .query_map([&git_prefix], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    for (key, value, updated_at) in git_rows {
        let suffix = key.strip_prefix(&git_prefix).unwrap_or("");
        let scoped_key = format!("handled_git_no_target_{scope_uuid}_{suffix}");
        copy_kv_if_absent(conn, &key, &scoped_key)?;
        if let Ok(mut record) = serde_json::from_str::<serde_json::Value>(&value) {
            attach_scope_uuid_to_receipt(&mut record, scope_uuid);
            conn.execute(
                "UPDATE kv_state SET value = ?1, updated_at = ?2 WHERE key = ?3",
                params![record.to_string(), updated_at, scoped_key],
            )?;
        }
    }
    let mut stmt = conn.prepare("SELECT key, value, updated_at FROM kv_state WHERE key LIKE ?1")?;
    let svn_rows: Vec<(String, String, String)> = stmt
        .query_map([&svn_prefix], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .filter_map(|r| r.ok())
        .collect();
    for (key, value, updated_at) in svn_rows {
        let suffix = key.strip_prefix(&svn_prefix).unwrap_or("");
        let scoped_key = format!("handled_svn_no_target_{scope_uuid}_{suffix}");
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
    for repo_id in repo_ids {
        let existing: Option<String> = conn
            .query_row(
                &format!("SELECT {SCOPE_UUID_COLUMN} FROM repositories WHERE id = ?1"),
                [&repo_id],
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
                params![scope_uuid, repo_id],
            )?;
            scope_uuid
        };
        migrate_legacy_kv_for_repo(conn, &repo_id, &scope_uuid)?;
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
