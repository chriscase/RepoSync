use reposync_core::db::{repo_scope_identity::*, Database};
use reposync_core::echo_receipt_scope::{
    handled_git_no_target_state_key, read_git_no_target_receipt, read_scoped_last_git_sha_kv,
};
use rusqlite::params;

fn database() -> Database {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    db
}

fn insert(db: &Database, id: &str, scope: &str) {
    db.conn().execute(
        "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,last_svn_rev,last_git_sha,scope_uuid) VALUES (?1,?1,'file:///fixture','trunk','','','','main',1,'t','t',0,'',?2)",
        params![id, scope],
    )
    .unwrap();
}

#[test]
fn quarantine_must_not_steal_pair_g2_receipt_via_generation_parse() {
    let db = database();
    insert(&db, "pair", &new_scope_uuid());
    insert(&db, "pair_g2", &new_scope_uuid());
    let sha = "a".repeat(40);
    db.set_state(
        &format!("handled_git_no_target_pair_g2_{sha}"),
        &serde_json::json!({
            "version": 1,
            "repo_id": "pair_g2",
            "git_sha": sha,
            "outcome": "filtered",
            "projection": "{}",
            "generation": 1
        })
        .to_string(),
    )
    .unwrap();
    legacy_repo_id_kv_authoritative(&db.conn()).unwrap();
    assert!(
        db.get_state(&format!("handled_git_no_target_pair_g2_{sha}"))
            .unwrap()
            .is_some(),
        "multi-repo quarantine attributed pair_g2's receipt to pair via _g2 parse"
    );
}

#[test]
fn hard_delete_human_id_equal_to_foreign_scope_uuid_must_not_touch_victim() {
    let db = database();
    let victim_scope = "abcdef01-2345-4678-9123-456789abcdef";
    insert(&db, "victim", victim_scope);
    insert(&db, victim_scope, &new_scope_uuid());
    let sha = "b".repeat(40);
    let receipt_key = handled_git_no_target_state_key(victim_scope, 1, &sha);
    let receipt = serde_json::json!({
        "version": 1,
        "repo_id": "victim",
        "scope_uuid": victim_scope,
        "git_sha": sha,
        "outcome": "filtered",
        "projection": "{}",
        "generation": 1
    })
    .to_string();
    db.set_state(&receipt_key, &receipt).unwrap();
    let checkpoint = "c".repeat(40);
    db.set_state(&last_git_sha_kv_key(victim_scope), &checkpoint)
        .unwrap();
    db.hard_delete_repository(victim_scope).unwrap();
    assert_eq!(
        db.get_state(&receipt_key).unwrap(),
        Some(receipt),
        "purge matched foreign scope_uuid via human id equality"
    );
    assert!(
        db.get_state(&last_git_sha_kv_key(victim_scope))
            .unwrap()
            .is_some(),
        "purge deleted victim UUID checkpoint via last_git_sha_<human id>"
    );
}

#[test]
fn hard_delete_must_not_strip_sibling_credential_prefix() {
    let db = database();
    insert(&db, "b", &new_scope_uuid());
    insert(&db, "ab", &new_scope_uuid());
    db.set_state("secret_git_token_b", "token-b").unwrap();
    db.set_state("secret_git_token_ab", "token-ab").unwrap();
    db.hard_delete_repository("b").unwrap();
    assert_eq!(
        db.get_state("secret_git_token_ab").unwrap(),
        Some("token-ab".into()),
        "credential LIKE %_b deleted secret_git_token_ab"
    );
}

#[test]
fn scoped_checkpoint_reader_survives_v13_migration_without_legacy_mirror() {
    let db = database();
    insert(&db, "pair", &new_scope_uuid());
    let sha = "d".repeat(40);
    db.set_state("last_git_sha_pair", &sha).unwrap();
    migrate_v13_scope_uuid(&db.conn()).unwrap();
    assert!(db.get_state("last_git_sha_pair").unwrap().is_none());
    assert_eq!(
        read_scoped_last_git_sha_kv(&db, "pair").unwrap(),
        Some(sha),
        "readers that only opened last_git_sha_<human id> lose post-migration checkpoint"
    );
}

#[test]
fn malformed_canonical_receipt_fails_closed_when_legacy_mirror_intact() {
    let db = database();
    insert(&db, "pair", &new_scope_uuid());
    insert(&db, "other", &new_scope_uuid());
    let scope = repository_scope_uuid(&db.conn(), "pair").unwrap();
    let sha = "e".repeat(40);
    let legacy = serde_json::json!({
        "version": 1,
        "repo_id": "pair",
        "git_sha": sha,
        "outcome": "filtered",
        "projection": "{}",
        "generation": 1
    });
    db.set_state(
        &format!("handled_git_no_target_pair_{sha}"),
        &legacy.to_string(),
    )
    .unwrap();
    db.set_state(
        &handled_git_no_target_state_key(&scope, 1, &sha),
        "{not-json",
    )
    .unwrap();
    match read_git_no_target_receipt(&db, "pair", &sha) {
        Err(reposync_core::errors::DatabaseError::Other(message))
            if message.contains("malformed scoped no-target receipt") => {}
        other => panic!("corrupt UUID receipt fell through to legacy mirror: {other:?}"),
    }
}
