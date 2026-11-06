//! Grok round-5 (#189): explicit legacy authority, unified removal cleanup, sweep ownership.

use reposync_core::db::managed_remove::{ManagedRemoveRemoteOutcome, RemovalAdvance};
use reposync_core::db::{repo_scope_identity::*, Database};
use reposync_core::echo_receipt_scope::{
    handled_git_no_target_state_key, read_git_no_target_receipt, read_scoped_last_git_sha_kv,
};
use reposync_core::managed_remove::remove_owned_repo_tree;
use reposync_core::models::Repository;
use rusqlite::params;

const LEGACY_READS_FLAG: &str = "reposync_v13_legacy_repo_id_kv_reads";
const V13_MIGRATION_DONE: &str = "reposync_v13_scope_uuid_migration_done";

fn database() -> Database {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    db
}

fn insert_row(db: &Database, id: &str) -> String {
    let scope = new_scope_uuid();
    db.conn().execute(
        "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,last_svn_rev,last_git_sha,scope_uuid) VALUES (?1,?1,'file:///fixture','trunk','','','','main',1,'t','t',0,'',?2)",
        params![id, scope],
    )
    .unwrap();
    scope
}

fn minimal_repository(id: &str) -> Repository {
    Repository {
        id: id.to_string(),
        name: id.to_string(),
        svn_url: "file:///fixture".into(),
        svn_branch: "trunk".into(),
        svn_username: String::new(),
        git_provider: "local".into(),
        git_api_url: String::new(),
        git_repo: String::new(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 5,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: true,
        created_by: None,
        parent_id: None,
        created_at: "t".into(),
        updated_at: "t".into(),
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".into(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    }
}

fn seed_legacy_checkpoint_and_receipt(db: &Database, id: &str, scope: &str) {
    let sha = "f".repeat(40);
    db.set_state(&legacy_last_git_sha_kv_key(id), &sha).unwrap();
    db.set_state(
        &handled_git_no_target_state_key(id, 2, &sha),
        &serde_json::json!({
            "version": 1,
            "repo_id": id,
            "scope_uuid": scope,
            "git_sha": sha,
            "outcome": "filtered",
            "projection": "{}",
            "generation": 2
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn delete_repository_then_reinsert_must_not_read_surviving_human_id_kv() {
    let db = database();
    let scope = insert_row(&db, "pair");
    seed_legacy_checkpoint_and_receipt(&db, "pair", &scope);
    assert!(
        !legacy_repo_id_kv_authoritative(&db.conn()).unwrap(),
        "fresh v13 DB must not treat unset flag as authoritative"
    );
    db.delete_repository("pair").unwrap();
    db.insert_repository(&minimal_repository("pair")).unwrap();
    let sha = "f".repeat(40);
    assert_eq!(
        db.get_state(&legacy_last_git_sha_kv_key("pair")).unwrap(),
        None,
        "soft delete must purge human-id checkpoint mirrors"
    );
    assert!(
        read_scoped_last_git_sha_kv(&db, "pair").unwrap().is_none(),
        "re-inserted repo must not read a deleted row's checkpoint"
    );
    assert!(
        read_git_no_target_receipt(&db, "pair", &sha)
            .unwrap()
            .is_none(),
        "re-inserted repo must not resurrect filtered receipts"
    );
}

#[test]
fn double_migrate_v13_must_not_reopen_latched_legacy_reads() {
    let db = database();
    assert_eq!(
        db.get_state(LEGACY_READS_FLAG).unwrap(),
        Some("0".into()),
        "empty v13 migration must latch legacy reads off"
    );

    // Simulate v12 → v13 upgrade with legacy human-id KV present at migration time.
    db.conn()
        .execute_batch(
            "DROP INDEX IF EXISTS idx_repositories_scope_uuid;
         ALTER TABLE repositories DROP COLUMN scope_uuid;
         PRAGMA user_version = 12;",
        )
        .unwrap();
    db.conn()
        .execute(
            "DELETE FROM kv_state WHERE key IN (?1, ?2)",
            params![LEGACY_READS_FLAG, V13_MIGRATION_DONE],
        )
        .unwrap();
    db.conn().execute(
        "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,last_svn_rev,last_git_sha)
         VALUES ('legacy','legacy','file:///fixture','trunk','','','','main',1,'t','t',0,'')",
        [],
    )
    .unwrap();
    let legacy_sha = "a".repeat(40);
    db.set_state(&legacy_last_git_sha_kv_key("legacy"), &legacy_sha)
        .unwrap();
    db.initialize().unwrap();
    assert_eq!(db.get_state(LEGACY_READS_FLAG).unwrap(), Some("1".into()));
    assert_eq!(db.get_state(V13_MIGRATION_DONE).unwrap(), Some("1".into()));

    revoke_legacy_repo_id_kv_reads(&db.conn()).unwrap();
    migrate_v13_scope_uuid(&db.conn()).unwrap();
    assert_eq!(
        db.get_state(LEGACY_READS_FLAG).unwrap(),
        Some("0".into()),
        "second migrate_v13 must not move the latch back to enabled"
    );
}

#[test]
fn sweep_must_keep_pair_gen2_mirror_while_pair_g2_lives() {
    let db = database();
    insert_row(&db, "pair");
    insert_row(&db, "pair_g2");
    let sha = "9".repeat(40);
    let key = handled_git_no_target_state_key("pair", 2, &sha);
    db.set_state(
        &key,
        &serde_json::json!({
            "version": 1,
            "repo_id": "pair",
            "git_sha": sha,
            "outcome": "filtered",
            "projection": "{}",
            "generation": 2
        })
        .to_string(),
    )
    .unwrap();
    legacy_repo_id_kv_authoritative(&db.conn()).unwrap();
    assert!(
        db.get_state(&key).unwrap().is_some(),
        "payload-owned gen-2 mirror must not be quarantined while pair still exists"
    );
}

#[test]
fn hard_delete_clears_human_id_mirrors() {
    let db = database();
    let scope = insert_row(&db, "pair");
    seed_legacy_checkpoint_and_receipt(&db, "pair", &scope);
    db.hard_delete_repository("pair").unwrap();
    assert_kv_cleared(&db, "pair");
}

#[test]
fn delete_repository_clears_human_id_mirrors() {
    let db = database();
    let scope = insert_row(&db, "pair");
    seed_legacy_checkpoint_and_receipt(&db, "pair", &scope);
    db.delete_repository("pair").unwrap();
    assert_kv_cleared(&db, "pair");
}

#[test]
fn managed_remove_completion_clears_human_id_mirrors() {
    let db = database();
    let scope = insert_row(&db, "quiet");
    seed_legacy_checkpoint_and_receipt(&db, "quiet", &scope);
    let RemovalAdvance::Cleanup { operation } =
        db.prepare_managed_remove("quiet", "admin", "req").unwrap()
    else {
        panic!("expected cleanup advance");
    };
    remove_owned_repo_tree(tempfile::tempdir().unwrap().path(), "quiet").unwrap();
    db.complete_managed_remove(
        "quiet",
        &operation.id,
        &ManagedRemoveRemoteOutcome::untouched(),
    )
    .unwrap();
    assert_kv_cleared(&db, "quiet");
}

fn assert_kv_cleared(db: &Database, id: &str) {
    let sha = "f".repeat(40);
    assert_eq!(db.get_state(&legacy_last_git_sha_kv_key(id)).unwrap(), None);
    assert_eq!(
        db.get_state(&handled_git_no_target_state_key(id, 2, &sha))
            .unwrap(),
        None
    );
}
