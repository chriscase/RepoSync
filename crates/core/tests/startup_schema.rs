//! Ordinary startup used by daemon callers: schema v12, no candidate registry.
//! `Database::new`/`initialize` stays valid without the data-dir lock; exclusive
//! owner is `Database::open_with_exclusive_owner` (daemon ordinary startup).
use reposync_core::db::Database;
use reposync_core::errors::DatabaseError;
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

#[test]
fn future_user_version_is_refused_on_ordinary_open() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("reposync.db");
    {
        let db = Database::new(&path).unwrap();
        db.initialize().unwrap();
        db.conn().pragma_update(None, "user_version", 99).unwrap();
    }
    let err = match Database::new(&path) {
        Ok(_) => panic!("expected future schema to be refused"),
        Err(err) => err,
    };
    match err {
        DatabaseError::UnsupportedSchema { found, supported } => {
            assert_eq!(found, 99);
            assert_eq!(supported, 12);
        }
        other => panic!("expected UnsupportedSchema, got {other:?}"),
    }
    let version: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 99);
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"STARTUP_FUTURE_REFUSED","found":99,"supported":12,"user_version_unchanged":true})
    );
}

#[test]
fn exclusive_data_dir_owner_blocks_second_startup() {
    let t = tempfile::tempdir().unwrap();
    let (db, owner) = Database::open_with_exclusive_owner(t.path()).unwrap();
    assert_eq!(
        db.conn()
            .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        12
    );
    let err = match Database::open_with_exclusive_owner(t.path()) {
        Ok(_) => panic!("expected second exclusive owner to be refused"),
        Err(err) => err,
    };
    assert!(
        matches!(err, DatabaseError::DataDirInUse(_)),
        "expected DataDirInUse, got {err:?}"
    );
    drop(owner);
    let (reopen, _owner2) = Database::open_with_exclusive_owner(t.path()).unwrap();
    assert_eq!(
        reopen
            .conn()
            .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        12
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"STARTUP_EXCLUSIVE_OWNER","second_startup_refused":true,"reacquire_after_drop":true})
    );
}
