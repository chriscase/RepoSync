//! First-run storage and existing evidence have different startup boundaries.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;

use reposync_core::db::Database;

fn schema(db: &Database) -> Vec<(String, String)> {
    db.conn()
        .prepare("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn fresh_personal_storage_is_initialized_before_admission_and_remains_exclusive() {
    let db = Database::in_memory().unwrap();
    assert!(schema(&db).is_empty());
    let window = db.begin_personal_startup_writer_window().unwrap();
    assert!(!schema(&db).is_empty());
    assert!(db.begin_personal_startup_writer_window().is_err());
    assert!(db.list_commit_map(10).unwrap().is_empty());
    assert!(db.get_watermark("svn_rev").unwrap().is_none());
    assert!(db.get_watermark("git_sha").unwrap().is_none());
    window.finish_preparation().unwrap();
    drop(db.begin_personal_startup_writer_window().unwrap());
}

#[test]
fn existing_refresh_hold_refuses_personal_startup_before_schema_repair() {
    let f = job::Fixture::new();
    let guard = f.busy();
    let id = f.admit(f.pins(), &guard);
    let op = f.db.pair_refresh_operation(id).unwrap();
    // A migration would update this version. Startup must reject the held scope
    // before attempting it, even though the table/identity evidence is usable.
    f.db.conn().pragma_update(None, "user_version", 0).unwrap();
    let before = schema(&f.db);
    let error = f.db.begin_personal_startup_writer_window().err().unwrap();
    assert!(
        error.to_string().contains("durable pair refresh"),
        "{error}"
    );
    let version: i64 =
        f.db.conn()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
    assert_eq!(version, 0);
    assert_eq!(schema(&f.db), before);
    assert_eq!(f.db.pair_refresh_operation(id).unwrap(), op);
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
}

#[test]
fn partially_initialized_unknown_storage_is_not_assumed_to_be_fresh() {
    let db = Database::in_memory().unwrap();
    db.conn().execute_batch("CREATE TABLE fixture_unknown(value TEXT NOT NULL); INSERT INTO fixture_unknown VALUES('preserve')").unwrap();
    let before = schema(&db);
    assert!(db.begin_personal_startup_writer_window().is_err());
    assert_eq!(schema(&db), before);
    let value: String = db
        .conn()
        .query_row("SELECT value FROM fixture_unknown", [], |r| r.get(0))
        .unwrap();
    assert_eq!(value, "preserve");
}
