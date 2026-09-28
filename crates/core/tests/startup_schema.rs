//! Runs with default features as well as the fixture build. This is the startup
//! implementation used by daemon/installer callers, with no candidate registry.
use reposync_core::db::Database;
#[test]
fn ordinary_startup_stays_v12() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("reposync.db");
    {
        let db = Database::new(&path).unwrap();
        db.initialize().unwrap();
        db.initialize().unwrap();
        assert_eq!(
            db.conn()
                .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            12
        );
        assert_eq!(
            db.conn()
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'pair_%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            db.conn()
                .query_row(
                    "SELECT [notnull] FROM pragma_table_info('commit_map') WHERE name='git_sha'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
    }
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    assert_eq!(
        db.conn()
            .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        12
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"STARTUP_V12","user_version":12,"candidate_tables":0,"git_sha_notnull":1,"repeat_initialize_and_restart":true})
    );
}
