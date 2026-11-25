//! Both installed team branches used 13/14 for different migrations than Reliability.
//! Match their SQLite catalog before using the existing operational migrations.
use std::collections::BTreeSet;
use std::sync::OnceLock;

use rusqlite::Connection;

use super::{apply_migration_body, get_schema_version, MIGRATIONS};
use crate::errors::DatabaseError;

// These statements match the two deployed schema registries (blob 7feb27af).
pub(in crate::db) const INSTALLED_V13: &str = "
        CREATE INDEX IF NOT EXISTS idx_sync_records_repo_dir_status_rev
            ON sync_records (repo_id, direction, status, svn_rev);";
pub(in crate::db) const INSTALLED_V14: &str = "
        ALTER TABLE repositories ADD COLUMN cloned_from_id TEXT;
        CREATE INDEX IF NOT EXISTS idx_repositories_cloned_from
            ON repositories(cloned_from_id);";

// SQLite internal statistics and autoindexes are SQLite's responsibility.
type Catalog = BTreeSet<(String, String, String, String)>;

fn catalog(conn: &Connection) -> Result<Catalog, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?;
    rows.collect()
}

struct Catalogs {
    installed_13: Catalog,
    installed_14: Catalog,
    operational_13: Catalog,
    operational_14: Catalog,
    converted_14: Catalog,
}

fn shared_schema() -> Result<Connection, DatabaseError> {
    let conn = Connection::open_in_memory()?;
    for &(_, _, sql) in MIGRATIONS.iter().take_while(|entry| entry.0 <= 12) {
        conn.execute_batch(sql)?;
    }
    conn.pragma_update(None, "user_version", 12)?;
    Ok(conn)
}

fn build_reference_catalogs() -> Result<Catalogs, DatabaseError> {
    let installed = shared_schema()?;
    installed.execute_batch(INSTALLED_V13)?;
    let installed_13 = catalog(&installed)?;
    installed.execute_batch(INSTALLED_V14)?;
    let installed_14 = catalog(&installed)?;
    apply_scope_and_lineage(&installed)?;
    let operational = shared_schema()?;
    super::run_migrations_up_to(&operational, 13)?;
    let operational_13 = catalog(&operational)?;
    // Apply the two registry entries directly when building references. Startup
    // admission itself uses these references and must never recurse.
    for &(version, _, sql) in MIGRATIONS.iter().filter(|entry| entry.0 == 14) {
        apply_migration_body(&operational, version, sql)?;
    }
    Ok(Catalogs {
        installed_13,
        installed_14,
        operational_13,
        operational_14: catalog(&operational)?,
        converted_14: catalog(&installed)?,
    })
}

fn reference_catalogs() -> Result<&'static Catalogs, DatabaseError> {
    static CATALOGS: OnceLock<Result<Catalogs, String>> = OnceLock::new();
    CATALOGS
        .get_or_init(|| build_reference_catalogs().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| DatabaseError::Other(error.clone()))
}

fn is_copy_artifact(name: &str) -> bool {
    matches!(
        name,
        "commit_map_v13"
            | "repo_migration_state"
            | "pair_lineages"
            | "pair_frontiers"
            | "pair_outcomes"
            | "legacy_evidence_links"
    )
}

/// Distinguish the converted installed Team registry from a native Reliability database.
pub(crate) fn converted(conn: &Connection) -> Result<bool, DatabaseError> {
    Ok(reference_catalogs()?
        .converted_14
        .is_subset(&catalog(conn)?))
}

/// True only for the complete installed schema. Converted and native operational
/// layouts must contain all registered objects. Runtime additions are permitted.
pub(crate) fn pending(conn: &Connection, version: u32) -> Result<bool, DatabaseError> {
    if version < 13 {
        // The legacy runner committed its DDL before setting user_version. A
        // crash between those writes can leave the deployed v13 index on v12.
        let mut objects =
            conn.prepare("SELECT lower(name) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'")?;
        let names = objects.query_map([], |r| r.get::<_, String>(0))?;
        let mut partial = false;
        for name in names {
            let name = name?;
            if is_copy_artifact(&name)
                || name.starts_with("canonical_")
                || matches!(
                    name.as_str(),
                    "idx_canonical_mapping_outcomes_scope"
                        | "idx_sync_records_repo_dir_status_rev"
                        | "idx_repositories_cloned_from"
                        | "idx_repositories_scope_uuid"
                )
            {
                partial = true;
                break;
            }
        }
        let scope_or_clone_column: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('repositories')
             WHERE name COLLATE NOCASE IN ('cloned_from_id','scope_uuid'))",
            [],
            |row| row.get(0),
        )?;
        if partial || scope_or_clone_column {
            return Err(DatabaseError::Other(
                "partial schema ahead of its registry version; upgrade refused".into(),
            ));
        }
        return Ok(false);
    }
    if version > 14 {
        return Ok(false);
    }
    let known = reference_catalogs()?;
    let actual = catalog(conn)?;
    if (version == 13 && actual == known.installed_13)
        || (version == 14 && actual == known.installed_14)
    {
        return Ok(true);
    }
    let has_mixed_objects = actual.iter().any(|(_, name, _, _)| {
        let identifier = name.to_ascii_lowercase();
        is_copy_artifact(&identifier) || (version == 13 && identifier.starts_with("canonical_"))
    });
    if !has_mixed_objects {
        // Converted storage retains both legacy indexes and the clone column.
        if version == 14 && known.converted_14.is_subset(&actual) {
            return Ok(false);
        }
        let operational = if version == 13 {
            &known.operational_13
        } else {
            &known.operational_14
        };
        let has_installed_index = actual.iter().any(|(_, name, _, _)| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "idx_sync_records_repo_dir_status_rev" | "idx_repositories_cloned_from"
            )
        });
        if !has_installed_index && operational.is_subset(&actual) {
            return Ok(false);
        }
    }
    Err(DatabaseError::Other(format!(
        "unrecognized physical layout at schema version {version}; upgrade refused"
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
        if get_schema_version(tx)? == 13 {
            tx.execute_batch(INSTALLED_V14)?;
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
        conn.execute_batch(INSTALLED_V13).unwrap();
        conn.execute_batch(INSTALLED_V14).unwrap();
        conn.execute_batch("PRAGMA user_version=14;
            INSERT INTO repositories(id,name,svn_url,created_at,updated_at,last_svn_rev,last_git_sha,cloned_from_id)
              VALUES('team','Deployment','file:///fixture','t','t',42,'abc','historical-source');
            INSERT INTO kv_state(key,value,updated_at) VALUES
              ('last_git_sha_team','abc','t'),('user-configuration','keep this','t');").unwrap();
        conn
    }

    #[test]
    fn offline_preflight_reports_both_installed_catalogs_and_ambiguous_history() {
        use crate::db::upgrade_preflight::inspect;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.db");
        let open = || Connection::open(&path).unwrap();
        let source = open();
        source
            .execute_batch(include_str!(
                "../../../../daemon/tests/fixtures/schema-v12.sql"
            ))
            .unwrap();
        source.execute_batch(INSTALLED_V13).unwrap();
        source
            .execute_batch(
                "PRAGMA user_version=13;
            INSERT INTO repositories (id,name,svn_url,created_at,updated_at)
            VALUES ('test','Team','file:///test','t','t');",
            )
            .unwrap();
        drop(source);
        let clean = inspect(&path).unwrap();
        assert_eq!(clean.schema_version, 13);
        assert!(!clean.needs_review());

        let source = open();
        source
            .execute_batch(
                "INSERT INTO kv_state(key,value,updated_at) VALUES ('last_sync_at','t','t');
            INSERT INTO audit_log(action,created_at) VALUES ('sync_cycle','t');",
            )
            .unwrap();
        drop(source);
        let poll = inspect(&path).unwrap();
        assert!(poll.poll_only && poll.needs_review());
        assert!(!poll.pairs[0].retained && !poll.unattributed_history);

        let source = open();
        source.execute_batch(INSTALLED_V14).unwrap();
        source
            .execute_batch(
                "PRAGMA user_version=14;
            INSERT INTO kv_state(key,value,updated_at)
            VALUES ('last_git_sha_test','history','t'),('secret_svn_password_test','ignored','t');",
            )
            .unwrap();
        drop(source);
        let held = inspect(&path).unwrap();
        assert_eq!(held.schema_version, 14);
        assert!(!held.poll_only);
        assert!(held.pairs[0].retained && held.needs_review());
        assert!(!held.unattributed_history && !held.unexpected_identity_records);

        let source = open();
        source
            .execute_batch(
                "DELETE FROM kv_state;
            INSERT INTO kv_state(key,value,updated_at) VALUES
                ('last_git_hash','history','t'),('import_operation_v1:latest:test','missing','t');",
            )
            .unwrap();
        drop(source);
        let unknown = inspect(&path).unwrap();
        assert!(!unknown.pairs[0].retained);
        assert!(unknown.unattributed_history && unknown.unexpected_identity_records);
        assert!(unknown.needs_review());

        let source = open();
        source
            .execute_batch(
                "DELETE FROM kv_state;
            INSERT INTO audit_log(action, repo_id, svn_rev, created_at)
            VALUES ('sync','test',2,'t'),('import_full',NULL,2,'t');",
            )
            .unwrap();
        drop(source);
        let audit_only = inspect(&path).unwrap();
        assert!(audit_only.pairs[0].retained && audit_only.unattributed_history);

        open()
            .execute_batch("DROP INDEX idx_repositories_cloned_from;")
            .unwrap();
        assert!(inspect(&path).is_err());
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
    fn complete_installed_v13_upgrades_and_mixed_operational_layouts_are_refused() {
        let conn = shared_schema().unwrap();
        conn.execute_batch(INSTALLED_V13).unwrap();
        assert!(pending(&conn, 12).is_err());
        conn.execute_batch("PRAGMA user_version=13; BEGIN IMMEDIATE")
            .unwrap();
        assert!(pending(&conn, 13).unwrap());
        upgrade_on_claim(&conn).unwrap();
        conn.execute_batch("COMMIT").unwrap();
        assert!(!pending(&conn, 14).unwrap());
        let native = Connection::open_in_memory().unwrap();
        super::super::run_migrations(&native).unwrap();
        native
            .execute_batch("CREATE TABLE extension_notes(note TEXT)")
            .unwrap();
        assert!(!pending(&native, 14).unwrap());
        native
            .execute_batch("CREATE TABLE Pair_Lineages(id TEXT)")
            .unwrap();
        assert!(pending(&native, 14).is_err());
        native
            .execute_batch(
                "DROP TABLE Pair_Lineages; DROP INDEX idx_canonical_mapping_outcomes_scope",
            )
            .unwrap();
        assert!(pending(&native, 14).is_err());
    }

    #[test]
    fn owned_startup_preserves_v12_with_a_partial_canonical_table() {
        let _serial = crate::writer_fence::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reposync.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!(
            "../../../../daemon/tests/fixtures/schema-v12.sql"
        ))
        .unwrap();
        conn.execute_batch("CREATE TABLE Canonical_Lineage(incomplete TEXT)")
            .unwrap();
        let before = catalog(&conn).unwrap();
        assert!(Database::open_with_exclusive_owner(dir.path()).is_err());
        assert!(Database::open_cli_writer(dir.path()).is_err());
        assert_eq!(catalog(&conn).unwrap(), before);
        assert_eq!(get_schema_version(&conn).unwrap(), 12);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM kv_state", [], |row| row
                .get::<_, usize>(0))
                .unwrap(),
            0
        );
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
