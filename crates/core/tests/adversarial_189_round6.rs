//! Grok round-6 (#189): atomic removal on deferred v12, scoped KV mirror gating.

use reposync_core::db::repo_scope_identity::legacy_last_git_sha_kv_key;
use reposync_core::db::{schema, Database};
use reposync_core::echo_receipt_scope::{
    read_scoped_last_git_sha_kv, write_scoped_last_git_sha_kv,
};
use reposync_core::errors::DatabaseError;
use rusqlite::{params, Connection};
use tempfile::TempDir;

fn deferred_lone_v12_file_db(tmp: &TempDir) -> Database {
    let path = tmp.path().join("reposync.db");
    let sha = "a".repeat(40);
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
        conn.execute(
            "INSERT INTO commit_map (svn_rev,git_sha,direction,synced_at,svn_author,git_author,repo_id)
             VALUES (2,?1,'svn_to_git','t','','','pair')",
            params![sha],
        )
        .unwrap();
        let now = "t";
        conn.execute(
            "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)",
            params![legacy_last_git_sha_kv_key("pair"), sha, now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO kv_state (key, value, updated_at) VALUES (?1, ?2, ?3)",
            params!["secret_git_token_pair", "fixture-secret", now],
        )
        .unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
                .unwrap(),
            12
        );
    }
    Database::new(&path).unwrap()
}

fn in_transaction(conn: &rusqlite::Connection) -> bool {
    conn.pragma_query_value(None, "transaction_state", |row| row.get::<_, String>(0))
        .map(|state| state != "none")
        .unwrap_or(false)
}

#[test]
fn deferred_v12_hard_delete_rolls_back_partial_history_on_missing_repo() {
    let tmp = TempDir::new().unwrap();
    let db = deferred_lone_v12_file_db(&tmp);
    let map_before: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM commit_map WHERE repo_id = 'pair'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(map_before, 1);
    assert!(db.get_state("secret_git_token_pair").unwrap().is_some());

    let err = db.hard_delete_repository("missing").unwrap_err();
    assert!(matches!(err, DatabaseError::NotFound { .. }));

    assert!(!in_transaction(&db.conn()));
    let map_after: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM commit_map WHERE repo_id = 'pair'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(map_after, map_before);
    assert!(db.get_state("secret_git_token_pair").unwrap().is_some());
    assert!(db.get_repository("pair").unwrap().is_some());
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
