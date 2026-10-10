//! Durable per-repository scope identity (#63).
//!
//! Managed echo receipts, generations, and Git checkpoint KV keys are scoped by
//! `repositories.scope_uuid`, not the human-chosen `id`, so delete + re-register
//! cannot reuse stale scoped state.

use rusqlite::{params, Connection, OptionalExtension};

use crate::errors::DatabaseError;

pub const SCOPE_UUID_COLUMN: &str = "scope_uuid";

const LEGACY_REPO_ID_KV_READS_KEY: &str = "reposync_v13_legacy_repo_id_kv_reads";
const V13_SCOPE_UUID_MIGRATION_DONE_KEY: &str = "reposync_v13_scope_uuid_migration_done";

const HANDLED_GIT_NO_TARGET_PREFIX: &str = "handled_git_no_target_";
const HANDLED_SVN_NO_TARGET_PREFIX: &str = "handled_svn_no_target_";

/// Inert KV namespace for legacy receipt keys with no live adopter (never read or migrated).
const QUARANTINED_KV_PREFIX: &str = "reposync_v13_quarantined:";

fn is_full_git_sha(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_svn_rev_token(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_digit())
}

fn anchored_git_receipt_suffix(suffix: &str) -> Option<(i64, String)> {
    if is_full_git_sha(suffix) {
        return Some((crate::pair_refresh::PAIR_GENERATION, suffix.to_string()));
    }
    let rest = suffix.strip_prefix('g')?;
    let (gen_digits, sha) = rest.split_once('_')?;
    if gen_digits.is_empty()
        || !gen_digits.chars().all(|c| c.is_ascii_digit())
        || !is_full_git_sha(sha)
    {
        return None;
    }
    let generation = gen_digits.parse().ok()?;
    Some((generation, sha.to_string()))
}

fn anchored_svn_receipt_suffix(suffix: &str) -> Option<(i64, String)> {
    if is_svn_rev_token(suffix) {
        return Some((crate::pair_refresh::PAIR_GENERATION, suffix.to_string()));
    }
    let rest = suffix.strip_prefix('g')?;
    let (gen_digits, rev) = rest.split_once('_')?;
    if gen_digits.is_empty()
        || !gen_digits.chars().all(|c| c.is_ascii_digit())
        || !is_svn_rev_token(rev)
    {
        return None;
    }
    let generation = gen_digits.parse().ok()?;
    Some((generation, rev.to_string()))
}

fn legacy_no_target_key_unambiguous_for_human_id(
    conn: &Connection,
    key: &str,
    repo_id: &str,
    git: bool,
) -> Result<bool, DatabaseError> {
    if !legacy_no_target_key_owned_by_human_id(key, repo_id, git) {
        return Ok(false);
    }
    let mut stmt = conn.prepare("SELECT id FROM repositories WHERE id != ?1")?;
    let longer_ids = stmt
        .query_map([repo_id], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .filter(|other| other.starts_with(&format!("{repo_id}_")))
        .collect::<Vec<_>>();
    for other in longer_ids {
        if legacy_no_target_key_owned_by_human_id(key, &other, git) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn legacy_no_target_key_owned_by_human_id(key: &str, repo_id: &str, git: bool) -> bool {
    let prefix = if git {
        format!("{HANDLED_GIT_NO_TARGET_PREFIX}{repo_id}_")
    } else {
        format!("{HANDLED_SVN_NO_TARGET_PREFIX}{repo_id}_")
    };
    if !key.starts_with(&prefix) {
        return false;
    }
    let suffix = key.strip_prefix(&prefix).unwrap_or("");
    if git {
        anchored_git_receipt_suffix(suffix).is_some()
    } else {
        anchored_svn_receipt_suffix(suffix).is_some()
    }
}

fn no_target_key_owned_by_scope_uuid(key: &str, scope_uuid: &str, git: bool) -> bool {
    let prefix = if git {
        format!("{HANDLED_GIT_NO_TARGET_PREFIX}{scope_uuid}_")
    } else {
        format!("{HANDLED_SVN_NO_TARGET_PREFIX}{scope_uuid}_")
    };
    if !key.starts_with(&prefix) {
        return false;
    }
    let suffix = key.strip_prefix(&prefix).unwrap_or("");
    if git {
        anchored_git_receipt_suffix(suffix).is_some()
    } else {
        anchored_svn_receipt_suffix(suffix).is_some()
    }
}

/// Human `repositories.id` must not collide with another row's durable `scope_uuid`.
pub(crate) fn human_legacy_kv_token_conflicts_with_foreign_scope_uuid(
    conn: &Connection,
    human_id: &str,
    owning_repo_id: &str,
) -> Result<bool, DatabaseError> {
    let conflict: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM repositories WHERE id != ?1 AND scope_uuid = ?2)",
        params![owning_repo_id, human_id],
        |row| row.get(0),
    )?;
    Ok(conflict)
}

fn legacy_human_checkpoint_kv_safe_to_touch(
    conn: &Connection,
    human_id: &str,
    owning_repo_id: &str,
) -> Result<bool, DatabaseError> {
    Ok(!human_legacy_kv_token_conflicts_with_foreign_scope_uuid(
        conn,
        human_id,
        owning_repo_id,
    )?)
}

fn receipt_payload_proves_human_repo(value: &str, repo_id: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(record) => record.get("repo_id").and_then(|v| v.as_str()) == Some(repo_id),
        Err(_) => false,
    }
}

fn receipt_payload_proves_scope_repo(value: &str, repo_id: &str, scope_uuid: &str) -> bool {
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(record) => {
            record.get("repo_id").and_then(|v| v.as_str()) == Some(repo_id)
                && record.get("scope_uuid").and_then(|v| v.as_str()) == Some(scope_uuid)
        }
        Err(_) => false,
    }
}

/// Reject human ids that cannot be distinguished from UUID scope tokens or generation suffixes.
pub fn validate_human_repository_id(repo_id: &str) -> Result<(), DatabaseError> {
    crate::managed_remove::validate_repo_id(repo_id).map_err(|error| {
        DatabaseError::Other(format!("invalid repository id: {}", error.message))
    })?;
    if uuid::Uuid::parse_str(repo_id).is_ok() {
        return Err(DatabaseError::Other(
            "repository id must not be UUID-shaped; use scope_uuid for durable identity".into(),
        ));
    }
    if let Some((_, gen_digits)) = repo_id.rsplit_once("_g") {
        if !gen_digits.is_empty() && gen_digits.chars().all(|c| c.is_ascii_digit()) {
            return Err(DatabaseError::Other(
                "repository id must not end with _g<digits>; that collides with receipt key encoding"
                    .into(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn list_kv_keys_glob(
    conn: &Connection,
    glob: &str,
) -> Result<Vec<String>, DatabaseError> {
    let mut stmt = conn.prepare("SELECT key FROM kv_state WHERE key GLOB ?1")?;
    let keys = stmt
        .query_map([glob], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(keys)
}

fn kv_payload_for_key(conn: &Connection, key: &str) -> Result<Option<String>, DatabaseError> {
    Ok(conn
        .query_row("SELECT value FROM kv_state WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?)
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
        let Some(value) = kv_payload_for_key(conn, &key)? else {
            continue;
        };
        if no_target_key_owned_by_scope_uuid(&key, &scope, true)
            && receipt_payload_proves_scope_repo(&value, repo_id, &scope)
        {
            return Ok(true);
        }
        if legacy_authoritative
            && legacy_no_target_key_unambiguous_for_human_id(conn, &key, repo_id, true)?
            && receipt_payload_proves_human_repo(&value, repo_id)
        {
            return Ok(true);
        }
    }
    for key in list_kv_keys_glob(conn, "handled_svn_no_target_*")? {
        let Some(value) = kv_payload_for_key(conn, &key)? else {
            continue;
        };
        if no_target_key_owned_by_scope_uuid(&key, &scope, false)
            && receipt_payload_proves_scope_repo(&value, repo_id, &scope)
        {
            return Ok(true);
        }
        if legacy_authoritative
            && legacy_no_target_key_unambiguous_for_human_id(conn, &key, repo_id, false)?
            && receipt_payload_proves_human_repo(&value, repo_id)
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

/// Human repository id for new registrations (never UUID-shaped; scope uses [`new_scope_uuid`]).
pub fn new_human_repository_id() -> String {
    format!("repo-{}", uuid::Uuid::new_v4())
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

/// Legacy repo-id KV may be read only when v13 migration explicitly stored `"1"`.
/// Unset and `"0"` are never authoritative. Once revoked (`"0"`), the latch never re-opens.
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
        sweep_unowned_legacy_no_target_receipts(conn)?;
        return Ok(false);
    }
    Ok(flag.as_deref() == Some("1"))
}

fn receipt_payload_claims_live_repository(value: &str, repos: &[(String, Option<String>)]) -> bool {
    let Ok(record) = serde_json::from_str::<serde_json::Value>(value) else {
        return false;
    };
    if let Some(repo_id) = record.get("repo_id").and_then(|v| v.as_str()) {
        if repos.iter().any(|(id, _)| id == repo_id) {
            return true;
        }
    }
    if let Some(scope) = record.get("scope_uuid").and_then(|v| v.as_str()) {
        if repos
            .iter()
            .any(|(_, su)| su.as_deref().is_some_and(|s| s == scope))
        {
            return true;
        }
    }
    false
}

fn legacy_receipt_has_live_adopter(
    conn: &Connection,
    key: &str,
    git: bool,
) -> Result<bool, DatabaseError> {
    let Some(value) = kv_payload_for_key(conn, key)? else {
        return Ok(false);
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT id, {SCOPE_UUID_COLUMN} FROM repositories ORDER BY id"
    ))?;
    let repos = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    if receipt_payload_claims_live_repository(&value, &repos) {
        return Ok(true);
    }
    for (repo_id, scope_uuid) in &repos {
        if let Some(scope) = scope_uuid.as_ref().filter(|s| !s.is_empty()) {
            if no_target_key_owned_by_scope_uuid(key, scope, git) {
                return Ok(true);
            }
        }
        if legacy_no_target_key_unambiguous_for_human_id(conn, key, repo_id, git)?
            && receipt_payload_proves_human_repo(&value, repo_id)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn repository_has_legacy_human_kv(
    conn: &Connection,
    repo_id: &str,
) -> Result<bool, DatabaseError> {
    if legacy_human_checkpoint_kv_safe_to_touch(conn, repo_id, repo_id)? {
        for key in [
            legacy_last_git_sha_kv_key(repo_id),
            legacy_repo_echo_generation_kv_key(repo_id),
            legacy_team_history_block_kv_key(repo_id),
            format!("last_svn_rev_{repo_id}"),
        ] {
            if kv_payload_for_key(conn, &key)?.is_some() {
                return Ok(true);
            }
        }
    }
    for key in list_kv_keys_glob(conn, &format!("handled_git_no_target_{repo_id}_*"))? {
        if !legacy_no_target_key_unambiguous_for_human_id(conn, &key, repo_id, true)? {
            continue;
        }
        let Some(value) = kv_payload_for_key(conn, &key)? else {
            continue;
        };
        if receipt_payload_proves_human_repo(&value, repo_id) {
            return Ok(true);
        }
    }
    for key in list_kv_keys_glob(conn, &format!("handled_svn_no_target_{repo_id}_*"))? {
        if !legacy_no_target_key_unambiguous_for_human_id(conn, &key, repo_id, false)? {
            continue;
        }
        let Some(value) = kv_payload_for_key(conn, &key)? else {
            continue;
        };
        if receipt_payload_proves_human_repo(&value, repo_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn purge_repository_kv_before_row_delete(
    conn: &Connection,
    repo_id: &str,
) -> Result<(), DatabaseError> {
    let scope_uuid: Option<String> = conn
        .query_row(
            &format!("SELECT {SCOPE_UUID_COLUMN} FROM repositories WHERE id = ?1"),
            [repo_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .filter(|value| !value.is_empty());
    purge_repository_scope_kv(conn, repo_id, scope_uuid.as_deref())?;
    Ok(())
}

pub(crate) fn after_repository_row_deleted(conn: &Connection) -> Result<(), DatabaseError> {
    let repo_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))?;
    if repo_count == 0 {
        revoke_legacy_repo_id_kv_reads(conn)?;
    }
    sweep_unowned_legacy_no_target_receipts(conn)?;
    Ok(())
}

/// Scope-identity schema must be current before any registration removal touches scoped KV.
pub(crate) fn ensure_repository_removal_ready(conn: &Connection) -> Result<(), DatabaseError> {
    if super::schema::v13_migration_deferred_for_engine(conn)? {
        super::schema::ensure_v13_migration(conn)?;
    }
    Ok(())
}

/// Shared registration removal: purge scoped/legacy KV, delete the row, latch/sweep.
pub(crate) fn delete_repository_registration_row(
    conn: &Connection,
    repo_id: &str,
) -> Result<(), DatabaseError> {
    purge_repository_kv_before_row_delete(conn, repo_id)?;
    let changed = conn.execute("DELETE FROM repositories WHERE id = ?1", params![repo_id])?;
    if changed == 0 {
        return Err(DatabaseError::NotFound {
            entity: "repository".into(),
            id: repo_id.into(),
        });
    }
    after_repository_row_deleted(conn)?;
    Ok(())
}

fn v13_scope_uuid_migration_already_done(conn: &Connection) -> Result<bool, DatabaseError> {
    Ok(conn
        .query_row(
            "SELECT value FROM kv_state WHERE key = ?1",
            [V13_SCOPE_UUID_MIGRATION_DONE_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .is_some_and(|value| value == "1"))
}

fn mark_v13_scope_uuid_migration_done(conn: &Connection) -> Result<(), DatabaseError> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, '1', ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![V13_SCOPE_UUID_MIGRATION_DONE_KEY, now],
    )?;
    Ok(())
}

fn quarantine_kv_key(conn: &Connection, key: &str) -> Result<(), DatabaseError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT value, updated_at FROM kv_state WHERE key = ?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((value, updated_at)) = row else {
        return Ok(());
    };
    let quarantined = format!("{QUARANTINED_KV_PREFIX}{key}");
    conn.execute(
        "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![quarantined, value, updated_at],
    )?;
    conn.execute("DELETE FROM kv_state WHERE key = ?1", [key])?;
    Ok(())
}

/// Move legacy no-target receipt keys with no live unambiguous adopter into inert storage.
pub(crate) fn sweep_unowned_legacy_no_target_receipts(
    conn: &Connection,
) -> Result<(), DatabaseError> {
    for key in list_kv_keys_glob(conn, "handled_git_no_target_*")? {
        if !legacy_receipt_has_live_adopter(conn, &key, true)? {
            quarantine_kv_key(conn, &key)?;
        }
    }
    for key in list_kv_keys_glob(conn, "handled_svn_no_target_*")? {
        if !legacy_receipt_has_live_adopter(conn, &key, false)? {
            quarantine_kv_key(conn, &key)?;
        }
    }
    Ok(())
}

pub fn set_legacy_repo_id_kv_reads_enabled(
    conn: &Connection,
    enabled: bool,
) -> Result<(), DatabaseError> {
    if enabled {
        let latched_off: bool = conn
            .query_row(
                "SELECT value FROM kv_state WHERE key = ?1",
                [LEGACY_REPO_ID_KV_READS_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .is_some_and(|value| value == "0");
        if latched_off {
            return Ok(());
        }
    }
    write_legacy_repo_id_kv_reads_flag(conn, enabled)
}

fn write_legacy_repo_id_kv_reads_flag(
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
        let Some(value) = kv_payload_for_key(conn, &key)? else {
            continue;
        };
        let delete = scope_uuid.is_some_and(|scope| {
            no_target_key_owned_by_scope_uuid(&key, scope, true)
                && receipt_payload_proves_scope_repo(&value, repo_id, scope)
        }) || (legacy_no_target_key_unambiguous_for_human_id(
            conn, &key, repo_id, true,
        )? && receipt_payload_proves_human_repo(&value, repo_id));
        if delete {
            conn.execute("DELETE FROM kv_state WHERE key = ?1", [&key])?;
        }
    }
    for key in list_kv_keys_glob(conn, "handled_svn_no_target_*")? {
        let Some(value) = kv_payload_for_key(conn, &key)? else {
            continue;
        };
        let delete = scope_uuid.is_some_and(|scope| {
            no_target_key_owned_by_scope_uuid(&key, scope, false)
                && receipt_payload_proves_scope_repo(&value, repo_id, scope)
        }) || (legacy_no_target_key_unambiguous_for_human_id(
            conn, &key, repo_id, false,
        )? && receipt_payload_proves_human_repo(&value, repo_id));
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
    if legacy_human_checkpoint_kv_safe_to_touch(conn, repo_id, repo_id)? {
        for key in [
            legacy_last_git_sha_kv_key(repo_id),
            legacy_repo_echo_generation_kv_key(repo_id),
            legacy_team_history_block_kv_key(repo_id),
            format!("last_svn_rev_{repo_id}"),
        ] {
            conn.execute("DELETE FROM kv_state WHERE key = ?1", [&key])?;
        }
    }
    Ok(())
}

fn quarantine_unscoped_legacy_receipts(
    conn: &Connection,
    repo_id: &str,
) -> Result<(), DatabaseError> {
    for key in list_kv_keys_glob(conn, "handled_git_no_target_*")? {
        if !legacy_no_target_key_unambiguous_for_human_id(conn, &key, repo_id, true)? {
            continue;
        }
        let value: Option<String> = conn
            .query_row("SELECT value FROM kv_state WHERE key = ?1", [&key], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(raw) = value else {
            continue;
        };
        if !receipt_payload_proves_human_repo(&raw, repo_id) {
            continue;
        }
        let has_scope_uuid = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|record| record.get("scope_uuid").cloned())
            .is_some();
        if has_scope_uuid {
            continue;
        }
        // Unscoped legacy receipts with proven human ownership are left for v13 migration.
    }
    for key in list_kv_keys_glob(conn, "handled_svn_no_target_*")? {
        if !legacy_no_target_key_unambiguous_for_human_id(conn, &key, repo_id, false)? {
            continue;
        }
        let value: Option<String> = conn
            .query_row("SELECT value FROM kv_state WHERE key = ?1", [&key], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(raw) = value else {
            continue;
        };
        if !receipt_payload_proves_human_repo(&raw, repo_id) {
            continue;
        }
        let has_scope_uuid = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|record| record.get("scope_uuid").cloned())
            .is_some();
        if has_scope_uuid {
            continue;
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

fn copy_kv_if_absent(
    conn: &Connection,
    from_key: &str,
    to_key: &str,
    remove_source: bool,
) -> Result<(), DatabaseError> {
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
    if migrated > 0 && remove_source {
        conn.execute("DELETE FROM kv_state WHERE key = ?1", [from_key])?;
    }
    Ok(())
}

pub(crate) fn migrate_legacy_kv_for_repo(
    conn: &Connection,
    repo_id: &str,
    scope_uuid: &str,
    preserve_legacy_checkpoint_mirrors: bool,
) -> Result<(), DatabaseError> {
    let remove_legacy_checkpoint = !preserve_legacy_checkpoint_mirrors;
    if legacy_human_checkpoint_kv_safe_to_touch(conn, repo_id, repo_id)? {
        copy_kv_if_absent(
            conn,
            &legacy_last_git_sha_kv_key(repo_id),
            &last_git_sha_kv_key(scope_uuid),
            remove_legacy_checkpoint,
        )?;
        copy_kv_if_absent(
            conn,
            &legacy_repo_echo_generation_kv_key(repo_id),
            &repo_echo_generation_kv_key(scope_uuid),
            remove_legacy_checkpoint,
        )?;
        copy_kv_if_absent(
            conn,
            &legacy_team_history_block_kv_key(repo_id),
            &team_history_block_kv_key(scope_uuid),
            remove_legacy_checkpoint,
        )?;
    }

    for key in list_kv_keys_glob(conn, "handled_git_no_target_*")? {
        if !legacy_no_target_key_unambiguous_for_human_id(conn, &key, repo_id, true)? {
            continue;
        }
        let (value, updated_at): (String, String) = conn.query_row(
            "SELECT value, updated_at FROM kv_state WHERE key = ?1",
            [&key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if !receipt_payload_proves_human_repo(&value, repo_id) {
            continue;
        }
        let prefix = format!("{HANDLED_GIT_NO_TARGET_PREFIX}{repo_id}_");
        let suffix = key.strip_prefix(&prefix).unwrap_or("");
        let Some((generation, checkpoint)) = anchored_git_receipt_suffix(suffix) else {
            continue;
        };
        let scoped_key = crate::echo_receipt_scope::handled_git_no_target_state_key(
            scope_uuid,
            generation,
            &checkpoint,
        );
        copy_kv_if_absent(conn, &key, &scoped_key, true)?;
        if let Ok(mut record) = serde_json::from_str::<serde_json::Value>(&value) {
            attach_scope_uuid_to_receipt(&mut record, scope_uuid);
            conn.execute(
                "UPDATE kv_state SET value = ?1, updated_at = ?2 WHERE key = ?3",
                params![record.to_string(), updated_at, scoped_key],
            )?;
        }
    }
    for key in list_kv_keys_glob(conn, "handled_svn_no_target_*")? {
        if !legacy_no_target_key_unambiguous_for_human_id(conn, &key, repo_id, false)? {
            continue;
        }
        let (value, updated_at): (String, String) = conn.query_row(
            "SELECT value, updated_at FROM kv_state WHERE key = ?1",
            [&key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if !receipt_payload_proves_human_repo(&value, repo_id) {
            continue;
        }
        let prefix = format!("{HANDLED_SVN_NO_TARGET_PREFIX}{repo_id}_");
        let suffix = key.strip_prefix(&prefix).unwrap_or("");
        let Some((generation, checkpoint)) = anchored_svn_receipt_suffix(suffix) else {
            continue;
        };
        let svn_rev = checkpoint.parse::<i64>().unwrap_or(0);
        let scoped_key = crate::echo_receipt_scope::handled_svn_no_target_state_key(
            scope_uuid, generation, svn_rev,
        );
        copy_kv_if_absent(conn, &key, &scoped_key, true)?;
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
    let migration_done = v13_scope_uuid_migration_already_done(conn)?;
    let mut stmt = conn.prepare("SELECT id FROM repositories ORDER BY id")?;
    let repo_ids: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    let enable_legacy_reads = if migration_done {
        false
    } else {
        repo_ids.len() == 1 && repository_has_legacy_human_kv(conn, &repo_ids[0])?
    };

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
        migrate_legacy_kv_for_repo(conn, repo_id, &scope_uuid, enable_legacy_reads)?;
    }

    if !migration_done {
        write_legacy_repo_id_kv_reads_flag(conn, enable_legacy_reads)?;
        if repo_ids.len() > 1 {
            for repo_id in &repo_ids {
                quarantine_unscoped_legacy_receipts(conn, repo_id)?;
            }
        }
        mark_v13_scope_uuid_migration_done(conn)?;
    }

    sweep_unowned_legacy_no_target_receipts(conn)?;
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
