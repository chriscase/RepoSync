//! Observing an old installation cannot activate the schema upgrade.
use reposync_core::db::{schema, Database};

fn seed_legacy(db: &Database) {
    db.conn()
        .execute_batch(include_str!("../../daemon/tests/fixtures/schema-v12.sql"))
        .unwrap();
    db.conn()
        .execute_batch(
            "INSERT INTO repositories (id,name,svn_url,created_at,updated_at,parent_id,allowed_paths)
             VALUES ('parent','Parent','file:///fictional-parent','t','t',NULL,'[\"src/\"]'),
                    ('child','Child','file:///fictional-child','t','t','parent',NULL);",
        )
        .unwrap();
}

fn schema_stamp(db: &Database) -> (u32, String) {
    let conn = db.conn();
    (
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap(),
        conn.query_row(
            "SELECT sql FROM sqlite_schema WHERE name='repositories'",
            [],
            |row| row.get(0),
        )
        .unwrap(),
    )
}

#[test]
fn genuine_v12_v14_and_v15_can_be_observed_without_writes() {
    for version in [12, 14, 15] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reposync.db");
        let db = Database::new(&path).unwrap();
        seed_legacy(&db);
        schema::run_migrations_up_to(&db.conn(), version).unwrap();
        db.conn().pragma_update(None, "query_only", true).unwrap();
        let before = schema_stamp(&db);
        assert_eq!(before.0, version);
        assert!(!before.1.contains("path_rewrite_rules"));
        let parent = db.get_repository("parent").unwrap().unwrap();
        assert_eq!(parent.allowed_paths.as_deref(), Some(r#"["src/"]"#));
        assert_eq!(parent.path_rewrite_rules, None);
        assert!(db.get_repository("absent").unwrap().is_none());
        let rows = db.list_repositories().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|repo| repo.path_rewrite_rules.is_none()));
        let children = db.list_child_repositories("parent").unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, "child");
        assert!(db.team_path_rewrite("parent").unwrap().is_none());
        assert_eq!(schema_stamp(&db), before);
    }
}

#[test]
fn missing_column_on_v16_and_early_column_on_v15_both_refuse_all_reads() {
    for forged_old_stamp in [false, true] {
        let db = Database::in_memory().unwrap();
        seed_legacy(&db);
        db.initialize().unwrap();
        if forged_old_stamp {
            // Adversarial only: an old metadata stamp must not hide real rules.
            db.conn().pragma_update(None, "user_version", 15).unwrap();
        } else {
            db.conn()
                .execute_batch("ALTER TABLE repositories DROP COLUMN path_rewrite_rules")
                .unwrap();
        }
        db.conn().pragma_update(None, "query_only", true).unwrap();
        let before = schema_stamp(&db);
        assert!(db.get_repository("parent").is_err());
        assert!(db.list_repositories().is_err());
        assert!(db.list_child_repositories("parent").is_err());
        assert!(db.team_path_rewrite("parent").is_err());
        assert_eq!(schema_stamp(&db), before);
    }
}
