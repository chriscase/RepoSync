//! #54 v15 rebuild and no-target reader regressions. File-backed where the
//! property depends on durable storage (restart, abandoned transaction,
//! process abort).

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::*;
use crate::db::lineage_checkpoints::{
    load_directional_checkpoint, record_mapping_outcome, MappingWrite,
};
use crate::db::schema::{self, CURRENT_SCHEMA_VERSION};
use crate::errors::DatabaseError;

fn user_version(conn: &Connection) -> u32 {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

fn git_sha_not_null(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT \"notnull\" FROM pragma_table_info('commit_map') WHERE name='git_sha'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn staging_tables(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'commit_map_v15'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn commit_map_sequence(conn: &Connection) -> Option<i64> {
    conn.query_row(
        "SELECT seq FROM sqlite_sequence WHERE name='commit_map'",
        [],
        |row| row.get(0),
    )
    .optional()
    .unwrap()
}

/// Every non-commit_map schema object and every row of every other table, so
/// a migration that touches anything else is detected.
fn everything_else(conn: &Connection) -> Vec<String> {
    let mut out = Vec::new();
    let mut stmt = conn
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_schema
             WHERE tbl_name <> 'commit_map' AND name <> 'sqlite_sequence' ORDER BY type, name",
        )
        .unwrap();
    let schema: Vec<(String, String, String, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for (kind, name, table, sql) in &schema {
        out.push(format!("schema {kind} {name} {table} {sql:?}"));
        if kind == "table" {
            let mut rows = conn.prepare(&format!("SELECT * FROM \"{name}\"")).unwrap();
            let width = rows.column_count();
            let mut values: Vec<String> = rows
                .query_map([], |row| {
                    let mut line = Vec::new();
                    for index in 0..width {
                        line.push(format!(
                            "{:?}",
                            row.get::<_, rusqlite::types::Value>(index)?
                        ));
                    }
                    Ok(line.join("|"))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            values.sort();
            out.extend(values.into_iter().map(|v| format!("row {name} {v}")));
        }
    }
    let mut sequences = conn
        .prepare("SELECT name, seq FROM sqlite_sequence WHERE name <> 'commit_map' ORDER BY name")
        .unwrap();
    out.extend(
        sequences
            .query_map([], |row| {
                Ok(format!(
                    "seq {} {}",
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
    );
    out
}

/// A file-backed operational v14 database with realistic, awkward data:
/// sparse and negative ids, a deleted tail (sequence above MAX(id)), both
/// directions, three ownership kinds, a historical empty-string SHA and
/// non-ASCII text.
fn v14_file(dir: &Path) -> PathBuf {
    let path = dir.join("reposync.db");
    let db = Database::new(&path).unwrap();
    schema::run_migrations_up_to(&db.conn(), 14).unwrap();
    let conn = db.conn();
    assert_eq!(user_version(&conn), 14);
    assert_eq!(git_sha_not_null(&conn), 1);
    conn.execute_batch(
        "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,scope_uuid)
         VALUES ('pair','pair','file:///repo','trunk','','','r','main',1,'t','t','scope-pair'),
                ('other','other','file:///other','trunk','','','o','main',0,'t','t','scope-other');
         INSERT INTO commit_map (id, svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id) VALUES
           (-3, 1, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'svn_to_git', '2026-01-01T00:00:00Z', 'ünïcødé', 'A <a@x>', 'pair'),
           (2, 2, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'git_to_svn', '2026-01-02T00:00:00Z', '', '', 'pair'),
           (7, 3, '', 'svn_to_git', '2026-01-03T00:00:00Z', 'legacy', 'legacy', NULL),
           (40, 4, 'cccccccccccccccccccccccccccccccccccccccc', 'svn_to_git', '2026-01-04T00:00:00Z', 'o', 'O', 'other'),
           (41, 5, 'dddddddddddddddddddddddddddddddddddddddd', 'svn_to_git', '2026-01-05T00:00:00Z', 'o', 'O', 'other');
         INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at) VALUES (6, 'eeee', 'svn_to_git', 't');
         DELETE FROM commit_map WHERE svn_rev = 6;
         INSERT INTO audit_log (action, created_at) VALUES ('keep', 't');
         INSERT INTO kv_state (key, value, updated_at) VALUES ('marker', 'preserve', 't');",
    )
    .unwrap();
    assert_eq!(commit_map_sequence(&conn), Some(42));
    path
}

#[test]
fn v15_ddl_is_byte_identical_to_candidate_v13_rebuild() {
    let candidate = include_str!("candidate_migration.rs");
    assert!(
        candidate.contains(&format!(
            "CREATE TABLE commit_map_v13{COMMIT_MAP_V15_TABLE_BODY}"
        )),
        "operational v15 table body drifted from candidate v13 V13_REBUILD"
    );
    assert!(
        candidate.contains(&format!(
            "const V13_INDEXES: &str = r#\"{COMMIT_MAP_V15_INDEXES}\"#;"
        )),
        "operational v15 index DDL drifted from candidate v13 V13_INDEXES"
    );
}

#[test]
fn fresh_database_reaches_v15_with_nullable_git_sha() {
    let conn = Connection::open_in_memory().unwrap();
    schema::run_migrations(&conn).unwrap();
    assert_eq!(CURRENT_SCHEMA_VERSION, NULLABLE_COMMIT_MAP_VERSION);
    assert_eq!(user_version(&conn), 15);
    assert_eq!(git_sha_not_null(&conn), 0);
    assert_eq!(classify_shape(&conn).unwrap(), Shape::NullableV15);
    assert_eq!(staging_tables(&conn), 0);
    assert_eq!(commit_map_sequence(&conn), None);
    // Repeating the registry is a no-op.
    let before = stored_registry_schema(&conn).unwrap();
    schema::run_migrations(&conn).unwrap();
    assert_eq!(stored_registry_schema(&conn).unwrap(), before);
}

#[test]
fn v14_upgrade_preserves_rows_ids_sequence_and_every_other_table_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = v14_file(dir.path());
    let (digest, others) = {
        let db = Database::new(&path).unwrap();
        let conn = db.conn();
        (
            commit_map_row_digest(&conn).unwrap(),
            everything_else(&conn),
        )
    };
    assert_eq!(digest.rows, 5);
    {
        let db = Database::new(&path).unwrap();
        db.initialize().unwrap();
        let conn = db.conn();
        assert_eq!(user_version(&conn), 15);
        assert_eq!(git_sha_not_null(&conn), 0);
        assert_eq!(commit_map_row_digest(&conn).unwrap(), digest);
        assert_eq!(commit_map_sequence(&conn), Some(42), "never MAX(id)");
        assert_eq!(everything_else(&conn), others);
        assert_eq!(staging_tables(&conn), 0);
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }
    // Restart: a new handle sees the same v15 state and initialize is a no-op.
    let db = Database::new(&path).unwrap();
    let schema_before = stored_registry_schema(&db.conn()).unwrap();
    db.initialize().unwrap();
    let conn = db.conn();
    assert_eq!(user_version(&conn), 15);
    assert_eq!(commit_map_row_digest(&conn).unwrap(), digest);
    assert_eq!(stored_registry_schema(&conn).unwrap(), schema_before);
    assert_eq!(everything_else(&conn), others);
    // The next AUTOINCREMENT id continues after the historical high-water mark.
    drop(conn);
    let id = db
        .insert_commit_map(9, &"f".repeat(40), "svn_to_git", "a", "A")
        .unwrap();
    assert_eq!(id, 43);
    // The empty-string legacy row survives and is labelled, not reinterpreted.
    let rows = db.list_commit_map_rows(None, 100).unwrap();
    let legacy = rows.iter().find(|row| row.id == 7).unwrap();
    assert_eq!(legacy.git_sha.as_deref(), Some(""));
    assert_eq!(legacy.target, CommitMapTarget::LegacyEmptyValue);
}

#[test]
fn absent_sequence_stays_absent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reposync.db");
    {
        let db = Database::new(&path).unwrap();
        schema::run_migrations_up_to(&db.conn(), 14).unwrap();
        assert_eq!(commit_map_sequence(&db.conn()), None);
    }
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    assert_eq!(user_version(&db.conn()), 15);
    assert_eq!(commit_map_sequence(&db.conn()), None);
}

#[test]
fn additional_indexes_and_triggers_on_commit_map_are_recreated_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let path = v14_file(dir.path());
    let extra_before = {
        let db = Database::new(&path).unwrap();
        db.conn()
            .execute_batch(
                "CREATE INDEX idx_operator_synced_at ON commit_map(synced_at);
                 CREATE TRIGGER operator_touch AFTER INSERT ON commit_map
                 BEGIN UPDATE kv_state SET updated_at = NEW.synced_at WHERE key = 'marker'; END;",
            )
            .unwrap();
        let extras = extra_objects(&db.conn()).unwrap();
        extras
    };
    assert_eq!(extra_before.len(), 2);
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    assert_eq!(user_version(&db.conn()), 15);
    assert_eq!(extra_objects(&db.conn()).unwrap(), extra_before);
    db.insert_commit_map(10, &"1".repeat(40), "svn_to_git", "a", "A")
        .unwrap();
    let touched: String = db
        .conn()
        .query_row(
            "SELECT updated_at FROM kv_state WHERE key = 'marker'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(touched, "t", "recreated trigger still fires");
}

/// Assert the file is still exactly the seeded v14 database.
fn assert_untouched_v14(path: &Path, digest: &RowDigest, others: &[String]) {
    let conn = Connection::open(path).unwrap();
    assert_eq!(user_version(&conn), 14);
    assert_eq!(git_sha_not_null(&conn), 1);
    assert_eq!(&commit_map_row_digest(&conn).unwrap(), digest);
    assert_eq!(commit_map_sequence(&conn), Some(42));
    assert_eq!(everything_else(&conn), others);
    assert_eq!(staging_tables(&conn), 0);
}

fn expect_migration_failed(result: Result<(), DatabaseError>, needle: &str) {
    match result {
        Err(DatabaseError::MigrationFailed { version, detail }) => {
            assert_eq!(version, 15);
            assert!(detail.contains(needle), "{detail}");
        }
        other => panic!("expected MigrationFailed(15) containing {needle:?}, got {other:?}"),
    }
}

#[test]
fn unsupported_v14_shapes_are_refused_before_any_write() {
    let cases: [(&str, &str); 3] = [
        (
            "ALTER TABLE commit_map ADD COLUMN operator_note TEXT",
            "columns differ",
        ),
        ("DROP INDEX idx_commit_map_repo_git", "idx_commit_map_repo_git"),
        (
            "CREATE TABLE operator_child (id INTEGER PRIMARY KEY, map_id INTEGER REFERENCES commit_map(id))",
            "foreign key",
        ),
    ];
    for (mutation, needle) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = v14_file(dir.path());
        let (digest, others) = {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(mutation).unwrap();
            (
                commit_map_row_digest(&conn).unwrap(),
                everything_else(&conn),
            )
        };
        let db = Database::new(&path).unwrap();
        expect_migration_failed(db.initialize(), needle);
        drop(db);
        assert_untouched_v14(&path, &digest, &others);
    }
}

#[test]
fn views_and_foreign_triggers_that_query_commit_map_survive_and_still_work() {
    // Modern ALTER TABLE RENAME would refuse these; the rebuild must keep them
    // byte-identical and pointing at the rebuilt table.
    let dir = tempfile::tempdir().unwrap();
    let path = v14_file(dir.path());
    let before = {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE VIEW operator_view AS SELECT svn_rev, git_sha FROM commit_map;
             CREATE TRIGGER operator_guard BEFORE DELETE ON repositories
             WHEN EXISTS (SELECT 1 FROM commit_map WHERE repo_id = OLD.id)
             BEGIN SELECT RAISE(ABORT, 'repository still has mappings'); END;",
        )
        .unwrap();
        external_dependents(&conn).unwrap()
    };
    assert_eq!(before.len(), 2);
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    let conn = db.conn();
    assert_eq!(user_version(&conn), 15);
    assert_eq!(external_dependents(&conn).unwrap(), before);
    let legacy_alter: i64 = conn
        .pragma_query_value(None, "legacy_alter_table", |row| row.get(0))
        .unwrap();
    assert_eq!(legacy_alter, 0, "legacy_alter_table is restored");
    let through_view: i64 = conn
        .query_row("SELECT COUNT(*) FROM operator_view", [], |row| row.get(0))
        .unwrap();
    assert_eq!(through_view, 5);
    let blocked = conn.execute("DELETE FROM repositories WHERE id = 'pair'", []);
    assert!(
        blocked
            .unwrap_err()
            .to_string()
            .contains("repository still has mappings"),
        "the foreign trigger still reads the rebuilt table"
    );
}

#[test]
fn mixed_case_cascading_foreign_key_child_is_refused_before_any_write() {
    // RS54-R1: identifiers are case-insensitive, so these children reference
    // commit_map. DROP TABLE would cascade-delete their rows.
    for target in ["COMMIT_MAP", "\"Commit_Map\""] {
        let dir = tempfile::tempdir().unwrap();
        let path = v14_file(dir.path());
        let (digest, others) = {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE operator_child (
                   id INTEGER PRIMARY KEY,
                   map_id INTEGER REFERENCES {target}(id) ON DELETE CASCADE
                 );
                 INSERT INTO operator_child VALUES (1, 2), (2, 40);"
            ))
            .unwrap();
            let reported: String = conn
                .query_row(
                    "SELECT \"table\" FROM pragma_foreign_key_list('operator_child')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_ne!(
                reported, "commit_map",
                "the hazard needs a non-lowercase target"
            );
            (
                commit_map_row_digest(&conn).unwrap(),
                everything_else(&conn),
            )
        };
        {
            let db = Database::new(&path).unwrap();
            expect_migration_failed(db.initialize(), "foreign key");
        }
        // Genuine reopen of the file: parent, child and version are unchanged.
        assert_untouched_v14(&path, &digest, &others);
        let conn = Connection::open(&path).unwrap();
        let children: Vec<(i64, i64)> = conn
            .prepare("SELECT id, map_id FROM operator_child ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(children, vec![(1, 2), (2, 40)], "{target}");
    }
}

const FIXTURE_V12: &str = include_str!("../../../daemon/tests/fixtures/schema-v12.sql");
const FIXTURE_COMMIT_MAP_TAIL: &str = "git_author  TEXT    NOT NULL DEFAULT ''\n        );";

/// Build a file at the frozen genuine v12 fixture, with its `commit_map` DDL
/// optionally edited, plus rows. Returns the path.
fn fixture_v12_file(dir: &Path, edit: Option<(&str, &str)>) -> PathBuf {
    let mut sql = FIXTURE_V12.to_string();
    if let Some((old, new)) = edit {
        assert_eq!(sql.matches(old).count(), 1, "fixture edit anchor {old:?}");
        sql = sql.replacen(old, new, 1);
    }
    let path = dir.join("reposync.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(&sql).unwrap();
    assert_eq!(user_version(&conn), 12);
    conn.execute_batch(
        "INSERT INTO users (id, username, display_name, email, password_hash, created_at, updated_at)
           VALUES ('u1', 'u1', 'U', 'u@x', 'h', 't', 't');
         INSERT INTO commit_map (id, svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id) VALUES
           (5, 1, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'svn_to_git', 't', 'u1', 'u1', NULL),
           (9, 2, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'git_to_svn', 't', 'u1', 'u1', NULL);",
    )
    .unwrap();
    path
}

#[test]
fn genuine_v12_fixture_and_formatting_only_variants_upgrade_to_v15() {
    for edit in [
        None,
        Some((
            FIXTURE_COMMIT_MAP_TAIL,
            "git_author TEXT NOT NULL DEFAULT ''\n\n\t);",
        )),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_v12_file(dir.path(), edit);
        let digest = commit_map_row_digest(&Connection::open(&path).unwrap()).unwrap();
        let db = Database::new(&path).unwrap();
        db.initialize().unwrap();
        assert_eq!(user_version(&db.conn()), 15, "{edit:?}");
        assert_eq!(git_sha_not_null(&db.conn()), 0);
        assert_eq!(commit_map_row_digest(&db.conn()).unwrap(), digest);
    }
}

#[test]
fn unsupported_commit_map_table_constraints_are_refused_before_any_rebuild_write() {
    // RS54-R2: semantics pragma_table_info cannot see. The registry's v13 and
    // v14 commit normally; v15 must refuse with commit_map untouched.
    let cases: [(&str, &str); 4] = [
        (
            "git_author  TEXT    NOT NULL DEFAULT '',\n            UNIQUE (svn_rev, direction)\n        );",
            "constraint",
        ),
        (
            "git_author  TEXT    NOT NULL DEFAULT '' COLLATE NOCASE\n        );",
            "differs from operational v14",
        ),
        (
            "git_author  TEXT    NOT NULL DEFAULT '',\n            CHECK (length(git_sha) > 0)\n        );",
            "differs from operational v14",
        ),
        (
            "git_author  TEXT    NOT NULL DEFAULT '' REFERENCES users(id)\n        );",
            "differs from operational v14",
        ),
    ];
    for (tail, needle) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture_v12_file(dir.path(), Some((FIXTURE_COMMIT_MAP_TAIL, tail)));
        let (digest, table_sql, indexes) = {
            let conn = Connection::open(&path).unwrap();
            let sql: String = conn
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE name='commit_map'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let indexes: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_index_list('commit_map')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            (commit_map_row_digest(&conn).unwrap(), sql, indexes)
        };
        {
            let db = Database::new(&path).unwrap();
            expect_migration_failed(db.initialize(), needle);
        }
        let conn = Connection::open(&path).unwrap();
        assert_eq!(user_version(&conn), 14, "{tail}");
        assert_eq!(git_sha_not_null(&conn), 1);
        assert_eq!(commit_map_row_digest(&conn).unwrap(), digest);
        assert_eq!(staging_tables(&conn), 0);
        let after: String = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='commit_map'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, table_sql, "the original definition is kept");
        let after_indexes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_index_list('commit_map')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after_indexes, indexes, "constraint autoindexes are kept");
    }
}

#[test]
fn altered_registry_index_semantics_are_refused_before_any_write() {
    let cases: [(&str, &str); 4] = [
        (
            "DROP INDEX idx_commit_map_svn_rev; CREATE UNIQUE INDEX idx_commit_map_svn_rev ON commit_map(svn_rev);",
            "unique or partial",
        ),
        (
            "DROP INDEX idx_commit_map_svn_rev; CREATE INDEX idx_commit_map_svn_rev ON commit_map(svn_rev DESC);",
            "covers",
        ),
        (
            "DROP INDEX idx_commit_map_git_sha; CREATE INDEX idx_commit_map_git_sha ON commit_map(git_sha COLLATE NOCASE);",
            "covers",
        ),
        (
            "DROP INDEX idx_commit_map_repo_svn; CREATE INDEX idx_commit_map_repo_svn ON commit_map(repo_id, svn_rev) WHERE repo_id IS NOT NULL;",
            "unique or partial",
        ),
    ];
    for (mutation, needle) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = v14_file(dir.path());
        let (digest, others, index_sql) = {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(mutation).unwrap();
            let sql: Vec<String> = conn
                .prepare("SELECT sql FROM sqlite_schema WHERE type='index' AND tbl_name='commit_map' ORDER BY name")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            (
                commit_map_row_digest(&conn).unwrap(),
                everything_else(&conn),
                sql,
            )
        };
        {
            let db = Database::new(&path).unwrap();
            expect_migration_failed(db.initialize(), needle);
        }
        assert_untouched_v14(&path, &digest, &others);
        let conn = Connection::open(&path).unwrap();
        let after: Vec<String> = conn
            .prepare("SELECT sql FROM sqlite_schema WHERE type='index' AND tbl_name='commit_map' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(after, index_sql, "{mutation}");
    }
}

#[test]
fn sql_normalization_ignores_only_formatting() {
    assert_eq!(
        normalize_sql("CREATE TABLE t (\n  a  TEXT NOT NULL DEFAULT '' ,\n b INTEGER\n)"),
        normalize_sql("CREATE TABLE t(a TEXT NOT NULL DEFAULT '',b INTEGER)")
    );
    // Literal contents, case and added clauses stay significant.
    assert_ne!(
        normalize_sql("CHECK (d IN ('a', 'b'))"),
        normalize_sql("CHECK (d IN ('A', 'b'))")
    );
    assert_ne!(normalize_sql("DEFAULT ' '"), normalize_sql("DEFAULT '  '"));
    assert_ne!(
        normalize_sql("a TEXT"),
        normalize_sql("a TEXT COLLATE NOCASE")
    );
    assert_ne!(normalize_sql("a text"), normalize_sql("a TEXT"));
}

#[test]
fn injected_failure_at_every_stage_rolls_back_then_retry_succeeds() {
    for stage in [
        "before_create",
        "after_create",
        "after_copy",
        "after_rename",
        "before_validation",
        "after_validation",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = v14_file(dir.path());
        let (digest, others) = {
            let conn = Connection::open(&path).unwrap();
            (
                commit_map_row_digest(&conn).unwrap(),
                everything_else(&conn),
            )
        };
        {
            let db = Database::new(&path).unwrap();
            set_test_fault(Some(stage));
            let result = db.initialize();
            set_test_fault(None);
            expect_migration_failed(result, stage);
        }
        assert_untouched_v14(&path, &digest, &others);
        let db = Database::new(&path).unwrap();
        db.initialize().unwrap();
        assert_eq!(user_version(&db.conn()), 15);
        assert_eq!(commit_map_row_digest(&db.conn()).unwrap(), digest);
    }
}

#[test]
fn abandoned_uncommitted_rebuild_leaves_the_file_at_v14() {
    let dir = tempfile::tempdir().unwrap();
    let path = v14_file(dir.path());
    let (digest, others) = {
        let conn = Connection::open(&path).unwrap();
        (
            commit_map_row_digest(&conn).unwrap(),
            everything_else(&conn),
        )
    };
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        migrate_v15(&conn).unwrap();
        assert_eq!(git_sha_not_null(&conn), 0);
        // Dropped without COMMIT.
    }
    assert_untouched_v14(&path, &digest, &others);
}

const ABORT_ENV: &str = "RS54_V15_ABORT_CHILD_DB";

/// Child half of `process_abort_mid_rebuild_leaves_v14`. Inert unless the
/// parent sets the environment variable.
#[test]
#[ignore = "child process helper; run by process_abort_mid_rebuild_leaves_v14"]
fn abort_child_helper() {
    let Ok(path) = std::env::var(ABORT_ENV) else {
        return;
    };
    let db = Database::new(&path).unwrap();
    set_test_fault(Some("abort_after_rename"));
    let _ = db.initialize();
    unreachable!("the injected fault aborts the process mid-transaction");
}

#[test]
fn process_abort_mid_rebuild_leaves_v14_then_upgrade_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let path = v14_file(dir.path());
    let (digest, others) = {
        let conn = Connection::open(&path).unwrap();
        (
            commit_map_row_digest(&conn).unwrap(),
            everything_else(&conn),
        )
    };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "db::nullable_commit_map::tests::abort_child_helper",
            "--ignored",
            "--test-threads=1",
        ])
        .env(ABORT_ENV, &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "child must die mid-transaction: {status:?}"
    );
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(6), "SIGABRT: {status:?}");
    }
    assert_untouched_v14(&path, &digest, &others);
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    assert_eq!(user_version(&db.conn()), 15);
    assert_eq!(commit_map_row_digest(&db.conn()).unwrap(), digest);
}

#[test]
fn body_is_a_no_op_on_v15_and_refused_outside_a_transaction() {
    let conn = Connection::open_in_memory().unwrap();
    schema::run_migrations(&conn).unwrap();
    let before = stored_registry_schema(&conn).unwrap();
    conn.execute_batch("BEGIN").unwrap();
    migrate_v15(&conn).unwrap();
    conn.execute_batch("COMMIT").unwrap();
    assert_eq!(stored_registry_schema(&conn).unwrap(), before);
    expect_migration_failed(migrate_v15(&conn), "transaction");
}

#[test]
fn future_schema_is_refused_without_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reposync.db");
    {
        let db = Database::new(&path).unwrap();
        db.initialize().unwrap();
        db.conn()
            .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)
            .unwrap();
    }
    match Database::new(&path) {
        Err(DatabaseError::UnsupportedSchema { found, supported }) => {
            assert_eq!(found, 16);
            assert_eq!(supported, 15);
        }
        Err(other) => panic!("expected UnsupportedSchema, got {other:?}"),
        Ok(_) => panic!("expected UnsupportedSchema"),
    }
    let conn = Connection::open(&path).unwrap();
    assert_eq!(user_version(&conn), 16);
}

#[test]
fn multi_repository_v12_upgrades_straight_through_v15_preserving_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reposync.db");
    let digest = {
        let db = Database::new(&path).unwrap();
        schema::run_migrations_up_to(&db.conn(), 12).unwrap();
        let conn = db.conn();
        conn.execute_batch(
            "INSERT INTO repositories (id,name,svn_url,created_at,updated_at) VALUES
               ('a','a','file:///a','t','t'), ('b','b','file:///b','t','t');
             INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, repo_id) VALUES
               (1, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'svn_to_git', 't', 'a'),
               (1, 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'svn_to_git', 't', 'b');",
        )
        .unwrap();
        commit_map_row_digest(&conn).unwrap()
    };
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    assert_eq!(user_version(&db.conn()), 15);
    assert_eq!(commit_map_row_digest(&db.conn()).unwrap(), digest);
}

// ---------------------------------------------------------------------------
// No-target writer / reader contract
// ---------------------------------------------------------------------------

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn managed(path: &Path) -> Database {
    let db = Database::new(path).unwrap();
    db.initialize().unwrap();
    db.conn()
        .execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,scope_uuid)
             VALUES ('pair','pair','file:///repo','trunk','','','r','main',1,'t','t','scope-pair')",
            [],
        )
        .unwrap();
    db
}

fn record_filtered(db: &Database, svn_rev: i64) {
    let conn = db.conn();
    let generation = current_generation_read_only(&conn, "pair", "scope-pair").unwrap();
    let key = svn_source_key(svn_rev);
    record_mapping_outcome(
        &conn,
        "scope-pair",
        generation,
        MappingWrite {
            direction: CheckpointDirection::SvnToGit,
            source_key: &key,
            outcome: MappingOutcome::IntentionallyFilteredNoTarget,
            target_git_sha: None,
            target_svn_rev: None,
            reason: Some("all changed paths outside allowed_paths"),
            policy_snapshot: Some("{\"allowed_paths\":[\"team/\"]}"),
        },
    )
    .unwrap();
}

fn side_effect_snapshot(db: &Database) -> (Vec<String>, i64, i64) {
    let conn = db.conn();
    let kv: Vec<String> = conn
        .prepare("SELECT key || '=' || value FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let checkpoints: i64 = conn
        .query_row("SELECT COUNT(*) FROM canonical_checkpoints", [], |row| {
            row.get(0)
        })
        .unwrap();
    let records: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_records", [], |row| row.get(0))
        .unwrap();
    (kv, checkpoints, records)
}

#[test]
fn proven_no_target_row_is_synced_has_no_sha_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reposync.db");
    {
        let db = managed(&path);
        record_filtered(&db, 7);
        let before = side_effect_snapshot(&db);
        let repo_before = db.get_repository("pair").unwrap().unwrap();
        let id = db
            .insert_commit_map_with_target(7, None, "svn_to_git", "dev", "", Some("pair"))
            .unwrap();
        assert!(id > 0);
        // Only the commit_map row was written.
        assert_eq!(side_effect_snapshot(&db), before);
        let repo_after = db.get_repository("pair").unwrap().unwrap();
        assert_eq!(repo_after.last_svn_rev, repo_before.last_svn_rev);
        assert_eq!(repo_after.last_git_sha, repo_before.last_git_sha);
        let conn = db.conn();
        let generation = current_generation_read_only(&conn, "pair", "scope-pair").unwrap();
        assert!(load_directional_checkpoint(
            &conn,
            "scope-pair",
            generation,
            CheckpointDirection::SvnToGit
        )
        .unwrap()
        .is_none());
    }
    // Restart on the same file.
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    assert!(db.is_svn_rev_synced(7).unwrap());
    assert_eq!(db.get_git_sha_for_svn_rev(7).unwrap(), None);
    assert!(db.list_commit_map(100).unwrap().is_empty());
    let rows = db.list_commit_map_rows(Some("pair"), 100).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].git_sha, None);
    assert_eq!(rows[0].target, CommitMapTarget::FilteredNoTarget);
    assert_eq!(rows[0].svn_rev, 7);
}

#[test]
fn no_target_writes_without_proof_or_outside_contract_are_refused_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db = managed(&dir.path().join("reposync.db"));
    record_filtered(&db, 7);
    let count = |db: &Database| -> i64 {
        db.conn()
            .query_row("SELECT COUNT(*) FROM commit_map", [], |row| row.get(0))
            .unwrap()
    };
    let attempts: [(i64, Option<&str>, &str, Option<&str>); 6] = [
        (8, None, "svn_to_git", Some("pair")),  // no canonical outcome
        (7, None, "git_to_svn", Some("pair")),  // wrong direction
        (7, None, "svn_to_git", None),          // unscoped
        (7, None, "svn_to_git", Some("")),      // empty scope
        (7, None, "svn_to_git", Some("ghost")), // unregistered repository
        (7, Some(""), "svn_to_git", Some("pair")), // empty-string sentinel
    ];
    for (rev, sha, direction, repo) in attempts {
        let before = count(&db);
        assert!(
            db.insert_commit_map_with_target(rev, sha, direction, "a", "A", repo)
                .is_err(),
            "{rev} {sha:?} {direction} {repo:?} must be refused"
        );
        assert_eq!(count(&db), before);
    }
    // A repository without scope_uuid has no canonical scope to prove against.
    db.conn()
        .execute(
            "INSERT INTO repositories (id,name,svn_url,created_at,updated_at) VALUES ('bare','bare','file:///b','t','t')",
            [],
        )
        .unwrap();
    assert!(db
        .insert_commit_map_with_target(7, None, "svn_to_git", "a", "A", Some("bare"))
        .is_err());
    assert_eq!(
        db.conn()
            .query_row(
                "SELECT scope_uuid FROM repositories WHERE id='bare'",
                [],
                |row| row.get::<_, Option<String>>(0)
            )
            .unwrap(),
        None,
        "classification must not mint a scope_uuid"
    );
    // Applied outcomes are not no-target proof.
    let sha = "9".repeat(40);
    db.insert_commit_map_with_repo(11, &sha, "svn_to_git", "a", "A", Some("pair"))
        .unwrap();
    assert!(db
        .insert_commit_map_with_target(11, None, "svn_to_git", "a", "A", Some("pair"))
        .is_err());
}

#[test]
fn some_sha_behavior_is_unchanged_including_the_canonical_applied_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let db = managed(&dir.path().join("reposync.db"));
    let id = db
        .insert_commit_map_with_target(5, Some(SHA_A), "svn_to_git", "a", "A", Some("pair"))
        .unwrap();
    let conn = db.conn();
    let generation = current_generation_read_only(&conn, "pair", "scope-pair").unwrap();
    let checkpoint = load_directional_checkpoint(
        &conn,
        "scope-pair",
        generation,
        CheckpointDirection::SvnToGit,
    )
    .unwrap()
    .unwrap();
    assert_eq!(checkpoint.outcome, MappingOutcome::AppliedAndVerified);
    assert_eq!(checkpoint.target_git_sha.as_deref(), Some(SHA_A));
    drop(conn);
    let rows = db.list_commit_map_rows(None, 10).unwrap();
    assert_eq!(rows[0].id, id);
    assert_eq!(rows[0].target, CommitMapTarget::GitCommit);
    assert_eq!(db.list_commit_map(10).unwrap()[0].git_sha, SHA_A);
}

/// `(svn_rev, git_sha, direction, repo_id)` for a raw `commit_map` insert.
type RawRow<'a> = (i64, Option<&'a str>, &'a str, Option<&'a str>);

/// Insert raw rows the API would refuse, as restored or hand-edited data can.
fn raw_rows(db: &Database, rows: &[RawRow<'_>]) {
    for (rev, sha, direction, repo) in rows {
        db.conn()
            .execute(
                "INSERT INTO commit_map (svn_rev, git_sha, direction, synced_at, svn_author, git_author, repo_id)
                 VALUES (?1, ?2, ?3, 't', '', '', ?4)",
                params![rev, sha, direction, repo],
            )
            .unwrap();
    }
}

#[test]
fn unverified_null_rows_are_inert_for_every_value_reader_and_predicate() {
    let dir = tempfile::tempdir().unwrap();
    let db = managed(&dir.path().join("reposync.db"));
    raw_rows(
        &db,
        &[
            (3, Some(SHA_A), "svn_to_git", Some("pair")),
            (4, None, "svn_to_git", Some("pair")), // scoped but unproven
            (9, None, "svn_to_git", None),         // unscoped
            (10, None, "git_to_svn", Some("pair")), // impossible direction
        ],
    );
    for rev in [4, 9, 10] {
        assert!(!db.is_svn_rev_synced(rev).unwrap(), "r{rev}");
        assert_eq!(db.get_git_sha_for_svn_rev(rev).unwrap(), None, "r{rev}");
    }
    assert!(db.is_svn_rev_synced(3).unwrap());
    assert_eq!(db.get_last_git_hash().unwrap().as_deref(), Some(SHA_A));
    assert_eq!(db.get_last_svn_revision().unwrap(), Some(3));
    assert_eq!(db.list_commit_map(100).unwrap().len(), 1);
    let rows = db.list_commit_map_rows(None, 100).unwrap();
    assert_eq!(rows.len(), 4);
    for row in rows.iter().filter(|row| row.git_sha.is_none()) {
        assert_eq!(row.target, CommitMapTarget::NoTargetUnverified, "{row:?}");
    }
}

#[test]
fn value_readers_equal_the_same_table_with_null_rows_removed() {
    // Newest NULL, all NULL, interleaved, both directions, two repositories.
    let layouts: Vec<Vec<RawRow<'_>>> = vec![
        vec![
            (1, Some(SHA_A), "svn_to_git", Some("pair")),
            (2, None, "svn_to_git", Some("pair")),
        ],
        vec![
            (1, None, "svn_to_git", Some("pair")),
            (2, None, "svn_to_git", None),
        ],
        vec![
            (5, None, "svn_to_git", Some("pair")),
            (5, Some(SHA_B), "svn_to_git", Some("other")),
            (6, Some(SHA_A), "git_to_svn", Some("pair")),
            (7, None, "svn_to_git", Some("pair")),
        ],
        vec![],
    ];
    for layout in layouts {
        let dir = tempfile::tempdir().unwrap();
        let with_nulls = managed(&dir.path().join("with.db"));
        let without = managed(&dir.path().join("without.db"));
        for db in [&with_nulls, &without] {
            db.insert_sync_record(&crate::models::SyncRecord {
                id: "fallback".into(),
                repo_id: None,
                svn_revision: Some(1),
                git_hash: Some("fallbackfallbackfallbackfallbackfallback".into()),
                direction: crate::models::SyncDirection::SvnToGit,
                author: String::new(),
                message: String::new(),
                timestamp: chrono::Utc::now(),
                synced_at: chrono::Utc::now(),
                status: crate::models::SyncRecordStatus::Applied,
            })
            .unwrap();
        }
        raw_rows(&with_nulls, &layout);
        let kept: Vec<_> = layout
            .iter()
            .copied()
            .filter(|row| row.1.is_some())
            .collect();
        raw_rows(&without, &kept);
        assert_eq!(
            with_nulls.get_last_git_hash().unwrap(),
            without.get_last_git_hash().unwrap(),
            "{layout:?}"
        );
        assert_eq!(
            with_nulls.get_last_svn_revision().unwrap(),
            without.get_last_svn_revision().unwrap(),
            "{layout:?}"
        );
        for rev in 0..=8 {
            assert_eq!(
                with_nulls.get_git_sha_for_svn_rev(rev).unwrap(),
                without.get_git_sha_for_svn_rev(rev).unwrap(),
                "r{rev} {layout:?}"
            );
            assert_eq!(
                with_nulls.is_svn_rev_synced(rev).unwrap(),
                without.is_svn_rev_synced(rev).unwrap(),
                "r{rev} {layout:?}"
            );
        }
        let ids = |db: &Database| -> Vec<(i64, String)> {
            db.list_commit_map(100)
                .unwrap()
                .into_iter()
                .map(|e| (e.svn_rev, e.git_sha))
                .collect()
        };
        assert_eq!(ids(&with_nulls), ids(&without), "{layout:?}");
    }
}

#[test]
fn proof_follows_the_current_generation() {
    let dir = tempfile::tempdir().unwrap();
    let db = managed(&dir.path().join("reposync.db"));
    record_filtered(&db, 7);
    db.insert_commit_map_with_target(7, None, "svn_to_git", "a", "A", Some("pair"))
        .unwrap();
    assert!(db.is_svn_rev_synced(7).unwrap());
    // A lineage generation change makes the old proof inapplicable.
    db.set_state(&repo_echo_generation_kv_key("scope-pair"), "2")
        .unwrap();
    assert!(!db.is_svn_rev_synced(7).unwrap());
    assert_eq!(
        db.list_commit_map_rows(None, 10).unwrap()[0].target,
        CommitMapTarget::NoTargetUnverified
    );
}

#[test]
fn read_only_generation_matches_the_general_helper_without_writing() {
    let cases: [(&[(&str, &str)], usize); 4] = [
        (&[], 1),
        (&[("scoped", "3")], 1),
        (&[("legacy", "4"), ("flag", "1")], 1),
        (&[("legacy", "4"), ("flag", "1")], 2),
    ];
    for (kv, repositories) in cases {
        let db = Database::in_memory().unwrap();
        schema::run_migrations(&db.conn()).unwrap();
        for index in 0..repositories {
            db.conn()
                .execute(
                    "INSERT INTO repositories (id,name,svn_url,created_at,updated_at,scope_uuid) VALUES (?1,?1,'file:///r','t','t',?2)",
                    params![format!("r{index}"), format!("scope-r{index}")],
                )
                .unwrap();
        }
        for (kind, value) in kv {
            let key = match *kind {
                "scoped" => repo_echo_generation_kv_key("scope-r0"),
                "legacy" => legacy_repo_echo_generation_kv_key("r0"),
                _ => LEGACY_REPO_ID_KV_READS_KEY.to_string(),
            };
            db.set_state(&key, value).unwrap();
        }
        let before = side_effect_snapshot(&db);
        let read_only = current_generation_read_only(&db.conn(), "r0", "scope-r0").unwrap();
        assert_eq!(
            side_effect_snapshot(&db),
            before,
            "read-only lookup wrote state"
        );
        let general = crate::echo_receipt_scope::repo_echo_generation_tx(&db.conn(), "r0").unwrap();
        assert_eq!(read_only, general, "kv={kv:?} repositories={repositories}");
    }
}

#[test]
fn personal_predicates_do_not_count_unproven_null_rows() {
    let db = Database::in_memory().unwrap();
    schema::run_migrations(&db.conn()).unwrap();
    let personal = crate::db::personal_scope::PERSONAL_SCOPE_KEY;
    raw_rows(
        &db,
        &[
            (1, None, "svn_to_git", Some(personal)),
            (2, None, "svn_to_git", None),
            (3, Some(SHA_A), "svn_to_git", Some(personal)),
        ],
    );
    // No sync-record receipts exist, so only commit_map decides here.
    assert!(!db.has_personal_svn_to_git_receipt_scoped(1).unwrap());
    assert!(!db.is_personal_svn_rev_synced(1).unwrap());
    assert!(!db.is_personal_svn_rev_synced(2).unwrap());
    assert!(
        db.is_personal_svn_rev_synced(3).unwrap(),
        "positive control"
    );
}

#[test]
fn personal_single_scope_branch_does_not_count_unproven_null_rows() {
    // Registering a managed repository with the legacy personal id narrows the
    // personal read set to the scope key alone (the single-id query branch).
    let db = Database::in_memory().unwrap();
    schema::run_migrations(&db.conn()).unwrap();
    db.conn()
        .execute(
            "INSERT INTO repositories (id,name,svn_url,created_at,updated_at) VALUES (?1,?1,'file:///p','t','t')",
            [crate::db::personal_scope::LEGACY_PERSONAL_REPO_ID],
        )
        .unwrap();
    assert_eq!(
        db.personal_repo_ids_for_read().unwrap(),
        vec![crate::db::personal_scope::PERSONAL_SCOPE_KEY]
    );
    let personal = crate::db::personal_scope::PERSONAL_SCOPE_KEY;
    raw_rows(
        &db,
        &[
            (4, None, "svn_to_git", Some(personal)),
            (5, Some(SHA_B), "svn_to_git", Some(personal)),
        ],
    );
    assert!(!db.is_personal_svn_rev_synced(4).unwrap());
    assert!(
        db.is_personal_svn_rev_synced(5).unwrap(),
        "positive control"
    );
}
