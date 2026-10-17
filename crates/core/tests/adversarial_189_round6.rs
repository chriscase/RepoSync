//! Grok round-6 (#189): atomic removal across deferred and ordinary storage, scoped KV mirror gating.

use reposync_core::db::repo_scope_identity::last_git_sha_kv_key;
use reposync_core::db::repo_scope_identity::legacy_last_git_sha_kv_key;
use reposync_core::db::{schema, Database};
use reposync_core::echo_receipt_scope::{
    read_scoped_last_git_sha_kv, read_team_inbound_git_checkpoint_kv, write_scoped_last_git_sha_kv,
};
use rusqlite::{params, Connection};
use tempfile::TempDir;

fn seed_removal_history(conn: &Connection, repo_id: &str) {
    let sha = "a".repeat(40);
    conn.execute(
        "INSERT INTO commit_map (svn_rev,git_sha,direction,synced_at,svn_author,git_author,repo_id)
         VALUES (2,?1,'svn_to_git','t','','',?2)",
        params![sha, repo_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sync_records (id,repo_id,svn_rev,git_sha,direction,timestamp,synced_at,status)
         VALUES (?1,?2,2,?3,'svn_to_git','t','t','applied')",
        params![format!("{repo_id}-sync"), repo_id, sha],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO conflicts (id,repo_id,file_path,conflict_type,created_at)
         VALUES (?1,?2,'fixture.txt','content','t')",
        params![format!("{repo_id}-conflict"), repo_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO audit_log (repo_id,action,details,created_at)
         VALUES (?1,'fixture','preserve this audit','t')",
        [repo_id],
    )
    .unwrap();
    for (key, value) in [
        (legacy_last_git_sha_kv_key(repo_id), sha),
        (
            format!("secret_git_token_{repo_id}"),
            "fixture-secret".into(),
        ),
    ] {
        conn.execute(
            "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, 't')",
            params![key, value],
        )
        .unwrap();
    }
}

fn deferred_lone_v12_file_db(tmp: &TempDir) -> Database {
    let path = tmp.path().join("reposync.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        schema::run_migrations_up_to(&conn, 12).unwrap();
        conn.execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,last_svn_rev,last_git_sha)
             VALUES ('pair','pair','file:///fixture','trunk','','','','main',1,'t','t',2,'abc')",
            [],
        )
        .unwrap();
        seed_removal_history(&conn, "pair");
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            12
        );
    }
    Database::new(&path).unwrap()
}

fn ordinary_file_db(tmp: &TempDir, multiple_repositories: bool) -> Database {
    let db = Database::new(tmp.path().join("reposync.db")).unwrap();
    db.initialize().unwrap();
    for repo_id in if multiple_repositories {
        vec!["pair", "pair_g2"]
    } else {
        vec!["pair"]
    } {
        let scope = reposync_core::db::repo_scope_identity::new_scope_uuid();
        db.conn().execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,last_svn_rev,last_git_sha,scope_uuid)
             VALUES (?1,?1,'file:///fixture','trunk','','','','main',1,'t','t',2,'abc',?2)",
            params![repo_id, scope],
        ).unwrap();
        seed_removal_history(&db.conn(), repo_id);
        db.set_state(&last_git_sha_kv_key(&scope), &"b".repeat(40))
            .unwrap();
    }
    db
}

fn in_transaction(conn: &rusqlite::Connection) -> bool {
    // Use SQLite's infallible connection state: an unsupported/erroring PRAGMA
    // must never be interpreted as proof that a transaction was rolled back.
    !conn.is_autocommit()
}

fn removal_snapshot(conn: &Connection) -> (u32, Vec<Vec<Vec<rusqlite::types::Value>>>) {
    let version = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let rows = [
        "SELECT * FROM repositories ORDER BY id",
        "SELECT * FROM commit_map ORDER BY id",
        "SELECT * FROM sync_records ORDER BY id",
        "SELECT * FROM conflicts ORDER BY id",
        "SELECT * FROM audit_log ORDER BY id",
        "SELECT * FROM kv_state ORDER BY key",
        "SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name",
    ]
    .iter()
    .map(|sql| {
        let mut statement = conn.prepare(sql).unwrap();
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    })
    .collect();
    (version, rows)
}

fn assert_hard_delete_rolls_back_partial_history(
    tmp: &TempDir,
    db: &Database,
    expected_version: u32,
) {
    for table in ["commit_map", "sync_records", "conflicts", "audit_log"] {
        let count: i64 = db
            .conn()
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE repo_id = 'pair'"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "targeted fixture row missing from {table}");
    }
    assert_eq!(
        db.get_state("secret_git_token_pair").unwrap().as_deref(),
        Some("fixture-secret")
    );
    db.conn()
        .execute_batch(
            "CREATE TRIGGER fail_pair_removal BEFORE DELETE ON repositories
         WHEN OLD.id = 'pair'
         BEGIN
             SELECT CASE WHEN
                 EXISTS (SELECT 1 FROM commit_map WHERE repo_id = 'pair') OR
                 EXISTS (SELECT 1 FROM sync_records WHERE repo_id = 'pair') OR
                 EXISTS (SELECT 1 FROM conflicts WHERE repo_id = 'pair') OR
                 EXISTS (SELECT 1 FROM audit_log WHERE repo_id = 'pair') OR
                 EXISTS (SELECT 1 FROM kv_state WHERE key = 'secret_git_token_pair')
             THEN RAISE(ABORT, 'failure injected before targeted data deletion')
             ELSE RAISE(ABORT, 'injected failure after targeted data deletion') END;
         END;",
        )
        .unwrap();
    let observer = Connection::open(tmp.path().join("reposync.db")).unwrap();
    let before = removal_snapshot(&db.conn());
    assert_eq!(before.0, expected_version);
    assert_eq!(removal_snapshot(&observer), before);

    let err = db.hard_delete_repository("pair").unwrap_err();
    assert!(
        err.to_string()
            .contains("injected failure after targeted data deletion"),
        "the failure must occur after real history/credential mutations: {err}"
    );

    assert!(!in_transaction(&db.conn()));
    assert!(!in_transaction(&observer));
    assert_eq!(
        removal_snapshot(&db.conn()),
        before,
        "writer must see restored registration, history, credentials and schema"
    );
    assert_eq!(
        removal_snapshot(&observer),
        before,
        "second connection must see the same preserved durable state"
    );
}

#[test]
fn deferred_v12_hard_delete_rolls_back_partial_history_on_targeted_failure() {
    let tmp = TempDir::new().unwrap();
    let db = deferred_lone_v12_file_db(&tmp);
    assert_hard_delete_rolls_back_partial_history(&tmp, &db, 12);
}

#[test]
fn ordinary_v13_hard_delete_rolls_back_partial_history_on_targeted_failure() {
    let tmp = TempDir::new().unwrap();
    let db = ordinary_file_db(&tmp, false);
    assert_hard_delete_rolls_back_partial_history(&tmp, &db, 13);
}

#[test]
fn multi_repository_hard_delete_rolls_back_partial_history_on_targeted_failure() {
    let tmp = TempDir::new().unwrap();
    let db = ordinary_file_db(&tmp, true);
    assert!(db.get_repository("pair_g2").unwrap().is_some());
    assert_hard_delete_rolls_back_partial_history(&tmp, &db, 13);
}

#[test]
fn transaction_probe_detects_active_transaction() {
    let conn = Connection::open_in_memory().unwrap();
    assert!(!in_transaction(&conn));
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(in_transaction(&conn));
    conn.execute_batch("ROLLBACK").unwrap();
    assert!(!in_transaction(&conn));
}

#[test]
fn deferred_v12_delete_repository_upgrades_then_removes_or_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let db = deferred_lone_v12_file_db(&tmp);
    db.delete_repository("pair").unwrap();
    assert!(db.get_repository("pair").unwrap().is_none());
    assert!(!in_transaction(&db.conn()));
    let version: u32 = db
        .conn()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert!(version >= 13);
}

#[test]
fn scoped_last_git_sha_kv_skips_human_mirror_unless_legacy_reads_latched() {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    let scope = reposync_core::db::repo_scope_identity::new_scope_uuid();
    db.conn().execute(
        "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,last_svn_rev,last_git_sha,scope_uuid)
         VALUES ('pair','pair','file:///fixture','trunk','','','','main',1,'t','t',0,'',?1)",
        params![scope],
    )
    .unwrap();
    let sha = "b".repeat(40);
    let now = "t";
    write_scoped_last_git_sha_kv(&db.conn(), "pair", &sha, now).unwrap();
    assert_eq!(
        read_scoped_last_git_sha_kv(&db, "pair").unwrap(),
        Some(sha.clone())
    );
    assert_eq!(
        db.get_state(&legacy_last_git_sha_kv_key("pair")).unwrap(),
        None
    );

    db.set_state("reposync_v13_legacy_repo_id_kv_reads", "1")
        .unwrap();
    write_scoped_last_git_sha_kv(&db.conn(), "pair", &sha, now).unwrap();
    assert_eq!(
        db.get_state(&legacy_last_git_sha_kv_key("pair")).unwrap(),
        Some(sha)
    );
}

#[test]
fn scoped_last_git_sha_kv_does_not_mirror_existing_legacy_inbound_without_latch() {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    let scope = reposync_core::db::repo_scope_identity::new_scope_uuid();
    db.conn().execute(
        "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,last_svn_rev,last_git_sha,scope_uuid)
         VALUES ('pair','pair','file:///fixture','trunk','','','','main',1,'t','t',0,'',?1)",
        rusqlite::params![scope],
    )
    .unwrap();
    let handled = "c".repeat(40);
    let emitted = "d".repeat(40);
    db.set_state(&legacy_last_git_sha_kv_key("pair"), &handled)
        .unwrap();
    write_scoped_last_git_sha_kv(&db.conn(), "pair", &emitted, "t").unwrap();
    assert_eq!(
        read_scoped_last_git_sha_kv(&db, "pair").unwrap(),
        Some(emitted.clone())
    );
    assert_eq!(
        db.get_state(&legacy_last_git_sha_kv_key("pair")).unwrap(),
        Some(handled)
    );
}

#[test]
fn team_inbound_git_checkpoint_refuses_legacy_human_key_without_latch() {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    let scope_a = reposync_core::db::repo_scope_identity::new_scope_uuid();
    let scope_b = reposync_core::db::repo_scope_identity::new_scope_uuid();
    db.conn().execute(
        "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,last_svn_rev,last_git_sha,scope_uuid)
         VALUES ('a','a','file:///fixture','trunk','','','','main',1,'t','t',0,'',?1),
                ('b','b','file:///fixture','trunk','','','','main',1,'t','t',0,'',?2)",
        params![scope_a, scope_b],
    )
    .unwrap();
    let stale = "e".repeat(40);
    db.set_state(&legacy_last_git_sha_kv_key("a"), &stale)
        .unwrap();
    assert_eq!(
        db.get_state("reposync_v13_legacy_repo_id_kv_reads")
            .unwrap(),
        Some("0".to_string())
    );
    assert_eq!(read_team_inbound_git_checkpoint_kv(&db, "a").unwrap(), None);
    assert_eq!(db.get_state(&last_git_sha_kv_key(&scope_a)).unwrap(), None);
}
