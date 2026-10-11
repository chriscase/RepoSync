use reposync_core::db::{repo_scope_identity::*, Database};
use reposync_core::echo_receipt_scope::handled_git_no_target_state_key;
use rusqlite::params;

fn database() -> Database {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    db
}

fn insert(db: &Database, id: &str, scope: &str) {
    db.conn().execute(
        "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,last_svn_rev,last_git_sha,scope_uuid) VALUES (?1,?1,'file:///fixture','trunk','','','','main',1,'t','t',0,'',?2)",
        params![id,scope],
    ).unwrap();
}

fn legacy_receipt(db: &Database, id: &str, sha: &str) {
    db.set_state(&format!("handled_git_no_target_{id}_{sha}"), &serde_json::json!({
        "version":1,"repo_id":id,"git_sha":sha,"outcome":"filtered","projection":"{}","generation":1
    }).to_string()).unwrap();
}

#[test]
fn v12_upgrade_preserves_receipts_for_ids_with_common_prefix() {
    let db = database();
    insert(&db, "pair", &new_scope_uuid());
    insert(&db, "pair2", &new_scope_uuid());
    let sha = "b".repeat(40);
    legacy_receipt(&db, "pair2", &sha);
    // Restore the actual pre-v13 schema shape, then exercise ordinary startup migration.
    db.conn().execute_batch("DROP INDEX idx_repositories_scope_uuid; ALTER TABLE repositories DROP COLUMN scope_uuid; PRAGMA user_version = 12;").unwrap();
    db.initialize().unwrap();
    let scope = repository_scope_uuid(&db.conn(), "pair2").unwrap();
    assert!(
        db.get_state(&handled_git_no_target_state_key(&scope, 1, &sha))
            .unwrap()
            .is_some(),
        "migration for pair stole pair2's receipt before pair2 could migrate it"
    );
}

#[test]
fn v12_upgrade_preserves_receipts_for_ids_with_underscore_prefix() {
    let db = database();
    insert(&db, "pair", &new_scope_uuid());
    insert(&db, "pair_child", &new_scope_uuid());
    let sha = "c".repeat(40);
    legacy_receipt(&db, "pair_child", &sha);
    db.conn().execute_batch("DROP INDEX idx_repositories_scope_uuid; ALTER TABLE repositories DROP COLUMN scope_uuid; PRAGMA user_version = 12;").unwrap();
    db.initialize().unwrap();
    let scope = repository_scope_uuid(&db.conn(), "pair_child").unwrap();
    assert!(
        db.get_state(&handled_git_no_target_state_key(&scope, 1, &sha))
            .unwrap()
            .is_some(),
        "prefix matching crossed into the pair_child namespace"
    );
}

#[test]
fn deleting_human_id_must_not_delete_another_repository_uuid_receipt() {
    let db = database();
    insert(&db, "abc", &new_scope_uuid());
    let other_scope = "abcdef01-2345-4678-9123-456789abcdef";
    insert(&db, "independent", other_scope);
    let sha = "d".repeat(40);
    let key = handled_git_no_target_state_key(other_scope, 1, &sha);
    let receipt = serde_json::json!({"version":1,"repo_id":"independent","scope_uuid":other_scope,
        "git_sha":sha,"outcome":"filtered","projection":"{}","generation":1})
    .to_string();
    db.set_state(&key, &receipt).unwrap();
    db.hard_delete_repository("abc").unwrap();
    assert_eq!(
        db.get_state(&key).unwrap(),
        Some(receipt),
        "deleting human id abc destroyed another repository's UUID-owned receipt"
    );
}
