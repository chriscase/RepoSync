//! Both installed team branches used 13/14 for different migrations than Reliability.
//! Match their SQLite catalog before using the existing operational migrations.
use std::collections::BTreeMap;
use std::sync::OnceLock;

use rusqlite::Connection;

use super::{apply_migration_body, get_schema_version, MIGRATIONS};
use crate::errors::DatabaseError;

// These statements match the two deployed schema registries (blob 7feb27af).
const INSTALLED_ADDITIONS: &str = "
        CREATE INDEX IF NOT EXISTS idx_sync_records_repo_dir_status_rev
            ON sync_records (repo_id, direction, status, svn_rev);
        ALTER TABLE repositories ADD COLUMN cloned_from_id TEXT;
        CREATE INDEX IF NOT EXISTS idx_repositories_cloned_from
            ON repositories(cloned_from_id);";

type Catalog = BTreeMap<String, String>;

fn catalog(conn: &Connection) -> Result<Catalog, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT name,sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

fn build_reference_catalogs() -> Result<(Catalog, Catalog), DatabaseError> {
    let reference = Connection::open_in_memory()?;
    for &(_, _, sql) in MIGRATIONS.iter().take_while(|entry| entry.0 <= 12) {
        reference.execute_batch(sql)?;
    }
    reference.execute_batch(INSTALLED_ADDITIONS)?;
    let installed = catalog(&reference)?;
    apply_scope_and_lineage(&reference)?;
    Ok((installed, catalog(&reference)?))
}

fn reference_catalogs() -> Result<&'static (Catalog, Catalog), DatabaseError> {
    static CATALOGS: OnceLock<Result<(Catalog, Catalog), String>> = OnceLock::new();
    CATALOGS
        .get_or_init(|| build_reference_catalogs().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| DatabaseError::Other(error.clone()))
}

/// Identify an original team installation, a previously converted one or ordinary Reliability.
/// Incomplete and copy-only layouts with colliding version numbers must never be migrated.
pub(crate) fn pending(conn: &Connection, version: u32) -> Result<bool, DatabaseError> {
    if !(13..=14).contains(&version) {
        return Ok(false);
    }
    let (scope, installed_markers, canonical_tables): (bool, bool, u32) = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('repositories') WHERE name='scope_uuid'),
                EXISTS(SELECT 1 FROM sqlite_schema WHERE name IN
                     ('idx_sync_records_repo_dir_status_rev','idx_repositories_cloned_from'))
                 OR EXISTS(SELECT 1 FROM pragma_table_info('repositories') WHERE name='cloned_from_id'),
                (SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name IN
                  ('canonical_lineage','canonical_checkpoints','canonical_mapping_outcomes'))",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if scope
        && !installed_markers
        && ((version == 13 && canonical_tables == 0) || (version == 14 && canonical_tables == 3))
    {
        return Ok(false);
    }
    if version == 14 && installed_markers {
        let (installed, upgraded) = reference_catalogs()?;
        let mut actual = catalog(conn)?;
        if &actual == installed {
            return Ok(true);
        }
        // The optional authenticated refresh key is created on first use.
        use super::super::pair_refresh_authenticated::{KEY_SCHEMA, KEY_TABLE};
        if actual.get(KEY_TABLE).map(String::as_str) == Some(KEY_SCHEMA) {
            actual.remove(KEY_TABLE);
        }
        if scope && &actual == upgraded {
            return Ok(false);
        }
    }
    Err(DatabaseError::Other(format!(
        "unrecognized team-install or partial physical layout at schema version {version}; upgrade refused"
    )))
}

fn apply_scope_and_lineage(conn: &Connection) -> Result<(), DatabaseError> {
    for &(version, _, sql) in MIGRATIONS.iter().filter(|entry| matches!(entry.0, 13 | 14)) {
        apply_migration_body(conn, version, sql)?;
    }
    Ok(())
}

pub(crate) fn upgrade_on_claim(tx: &Connection) -> Result<(), DatabaseError> {
    if pending(tx, get_schema_version(tx)?)? {
        if tx.is_autocommit() {
            return Err(DatabaseError::Other(
                "installed team upgrade requires a reserved transaction".into(),
            ));
        }
        let has_scope_markers: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM kv_state WHERE key GLOB 'reposync_v13_*')",
            [],
            |row| row.get(0),
        )?;
        if has_scope_markers {
            return Err(DatabaseError::Other(
                "installed schema already contains operational scope migration evidence".into(),
            ));
        }
        apply_scope_and_lineage(tx)?;
        let missing_scope: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM repositories WHERE scope_uuid IS NULL OR length(scope_uuid) != 36)",
            [], |row| row.get(0))?;
        if missing_scope {
            return Err(DatabaseError::Other(
                "installed repository could not be assigned a durable scope".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn installed(path: &std::path::Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(include_str!(
            "../../../../daemon/tests/fixtures/schema-v12.sql"
        ))
        .unwrap();
        conn.execute_batch(INSTALLED_ADDITIONS).unwrap();
        conn.execute_batch("PRAGMA user_version=14;
            INSERT INTO repositories(id,name,svn_url,created_at,updated_at,last_svn_rev,last_git_sha,cloned_from_id)
              VALUES('team','Deployment','file:///fixture','t','t',42,'abc','historical-source');
            INSERT INTO kv_state(key,value,updated_at) VALUES
              ('last_git_sha_team','abc','t'),('user-configuration','keep this','t');").unwrap();
        conn
    }

    #[test]
    fn recognized_install_upgrades_with_existing_scope_migration_and_survives_restart() {
        let _serial = crate::writer_fence::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reposync.db");
        let old = installed(&path);
        assert!(pending(&old, 14).unwrap());
        drop(old);
        assert!(Database::new(&path).is_err());
        let (db, owner) = Database::open_with_exclusive_owner(dir.path()).unwrap();
        let scope: String = db
            .conn()
            .query_row(
                "SELECT scope_uuid FROM repositories WHERE id='team'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!scope.is_empty());
        drop(db);
        drop(owner);
        let db = Database::open_cli_writer(dir.path()).unwrap();
        let unchanged: (String, i64, String, String) = db.conn().query_row(
            "SELECT scope_uuid,last_svn_rev,last_git_sha,cloned_from_id FROM repositories WHERE id='team'",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(
            unchanged,
            (scope.clone(), 42, "abc".into(), "historical-source".into())
        );
        assert_eq!(
            db.get_state(&format!("last_git_sha_{scope}"))
                .unwrap()
                .as_deref(),
            Some("abc")
        );
        assert_eq!(
            db.get_state("user-configuration").unwrap().as_deref(),
            Some("keep this")
        );
        assert!(!pending(&db.conn(), 14).unwrap());
    }

    #[test]
    fn ordinary_writer_migrates_and_refuses_incomplete_or_unrecognized_layouts_before_claim() {
        let _serial = crate::writer_fence::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reposync.db");
        let conn = installed(&path);
        conn.execute_batch("DROP INDEX idx_repositories_cloned_from;")
            .unwrap();
        let before = catalog(&conn).unwrap();
        assert!(Database::open_cli_writer(dir.path()).is_err());
        assert!(Database::open_with_exclusive_owner(dir.path()).is_err());
        assert_eq!(catalog(&conn).unwrap(), before);
        let keys: i64 = conn
            .query_row("SELECT count(*) FROM kv_state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(keys, 2);
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_repositories_cloned_from
            ON repositories(cloned_from_id);",
        )
        .unwrap();
        drop(conn);
        let db = Database::open_cli_writer(dir.path()).unwrap();
        assert!(!pending(&db.conn(), 14).unwrap());
    }

    #[test]
    fn migration_error_rolls_back_both_catalog_and_writer_claim() {
        let _serial = crate::writer_fence::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let conn = installed(&dir.path().join("reposync.db"));
        conn.execute("UPDATE repositories SET id=CAST(x'80' AS TEXT)", [])
            .unwrap();
        let before = catalog(&conn).unwrap();
        assert!(Database::open_with_exclusive_owner(dir.path()).is_err());
        assert_eq!(catalog(&conn).unwrap(), before);
        assert_eq!(get_schema_version(&conn).unwrap(), 14);
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM kv_state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2);
        let id: String = conn
            .query_row("SELECT hex(id) FROM repositories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(id, "80");
    }

    #[test]
    fn interrupted_conversion_rolls_back_legacy_layout_checkpoints_and_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let conn = installed(&dir.path().join("reposync.db"));
        let before = catalog(&conn).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        upgrade_on_claim(&conn).unwrap();
        assert!(!pending(&conn, 14).unwrap());
        conn.execute_batch("ROLLBACK").unwrap();
        assert_eq!(catalog(&conn).unwrap(), before);
        assert!(pending(&conn, 14).unwrap());
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM kv_state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2);
    }
}
