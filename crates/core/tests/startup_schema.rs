//! Ordinary startup used by daemon callers: current operational schema (v15),
//! no candidate registry.
//!
//! The case id `STARTUP_V14` and the test name `ordinary_startup_stays_v14`
//! are pinned in `docs/reliability/required-cases.json` and are kept as
//! identities. Since #54 the current operational schema is v15, which keeps
//! every v14 canonical table and makes `commit_map.git_sha` nullable.
//! `Database::new`/`initialize` stays valid without the data-dir lock; exclusive
//! owner is `Database::open_with_exclusive_owner` (daemon ordinary startup).
use reposync_core::db::Database;
use reposync_core::errors::DatabaseError;

#[test]
fn ordinary_startup_stays_v14() {
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
            15
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
                    "SELECT count(*) FROM sqlite_schema WHERE name IN ('canonical_lineage','canonical_checkpoints','canonical_mapping_outcomes')",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            3
        );
        assert_eq!(
            db.conn()
                .query_row(
                    "SELECT [notnull] FROM pragma_table_info('commit_map') WHERE name='git_sha'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    let db = Database::new(&path).unwrap();
    db.initialize().unwrap();
    assert_eq!(
        db.conn()
            .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        15
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"STARTUP_V14","user_version":15,"candidate_tables":0,"git_sha_notnull":0,"repeat_initialize_and_restart":true})
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
            assert_eq!(supported, 15);
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
        serde_json::json!({"case":"STARTUP_FUTURE_REFUSED","found":99,"supported":15,"user_version_unchanged":true})
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
        15
    );
    let err = match Database::open_with_exclusive_owner(t.path()) {
        Ok(_) => panic!("expected second exclusive owner to be refused"),
        Err(err) => err,
    };
    assert!(
        matches!(err, DatabaseError::DataDirInUse(_)),
        "expected DataDirInUse, got {err:?}"
    );
    let fence_epoch = owner.fence.epoch();
    assert!(fence_epoch >= 1);
    drop(owner);
    let (reopen, _owner2) = Database::open_with_exclusive_owner(t.path()).unwrap();
    assert_eq!(
        reopen
            .conn()
            .pragma_query_value::<i64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        15
    );
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        serde_json::json!({"case":"STARTUP_EXCLUSIVE_OWNER","second_startup_refused":true,"reacquire_after_drop":true,"writer_fence_epoch":fence_epoch})
    );
}
