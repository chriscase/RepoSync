//! Grok round-4 (#189): stale legacy receipt resurrection via sole-repo re-insert.

use reposync_core::db::{repo_scope_identity::*, Database};
use reposync_core::echo_receipt_scope::{
    bump_repo_echo_generation, handled_git_no_target_state_key, read_git_no_target_receipt,
};
use reposync_core::echo_suppression::{
    classify_incoming_git_commit, EchoDisposition, TeamEchoContext,
};
use reposync_core::models::Repository;
use rusqlite::params;

const LEGACY_READS_FLAG: &str = "reposync_v13_legacy_repo_id_kv_reads";
const QUARANTINE_PREFIX: &str = "reposync_v13_quarantined:";

fn database() -> Database {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    db
}

fn insert_row(db: &Database, id: &str) {
    db.conn().execute(
        "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,last_svn_rev,last_git_sha,scope_uuid) VALUES (?1,?1,'file:///fixture','trunk','','','','main',1,'t','t',0,'',?2)",
        params![id, new_scope_uuid()],
    )
    .unwrap();
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

fn write_gen2_pair_ambiguous_receipt(db: &Database, sha: &str) {
    let key = handled_git_no_target_state_key("pair", 2, sha);
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
}

fn classify_pair(db: &Database, sha: &str) -> EchoDisposition {
    let ctx = TeamEchoContext {
        db,
        repo_id: "pair",
        no_target_projection: "{}",
    };
    classify_incoming_git_commit(&ctx, sha, "genuine new registration")
        .unwrap()
        .unwrap()
}

fn assert_no_resurrection(db: &Database, sha: &str, bump_echo: bool) {
    if bump_echo {
        bump_repo_echo_generation(db, "pair").unwrap();
    }
    assert!(
        read_git_no_target_receipt(db, "pair", sha)
            .unwrap()
            .is_none(),
        "stale receipt resurrected onto re-inserted pair"
    );
    assert_eq!(
        classify_pair(db, sha),
        EchoDisposition::ApplyGenuine,
        "echo disposition must not suppress a genuinely new registration"
    );
}

fn setup_migrated_pair_and_sibling(db: &Database, sha: &str) {
    insert_row(db, "pair");
    insert_row(db, "pair_g2");
    write_gen2_pair_ambiguous_receipt(db, sha);
    migrate_v13_scope_uuid(&db.conn()).unwrap();
}

fn delete_pair_then_pair_g2(db: &Database) {
    db.hard_delete_repository("pair").unwrap();
    db.hard_delete_repository("pair_g2").unwrap();
}

fn delete_pair_g2_then_pair(db: &Database) {
    db.hard_delete_repository("pair_g2").unwrap();
    db.hard_delete_repository("pair").unwrap();
}

fn reinsert_pair(db: &Database) {
    db.insert_repository(&minimal_repository("pair")).unwrap();
}

fn assert_orphan_inert(db: &Database, sha: &str) {
    let active = handled_git_no_target_state_key("pair", 2, sha);
    assert!(
        db.get_state(&active).unwrap().is_none(),
        "ambiguous legacy receipt must not remain in active KV"
    );
    if db
        .get_state(&format!("{QUARANTINE_PREFIX}{active}"))
        .unwrap()
        .is_none()
    {
        assert!(
            read_git_no_target_receipt(db, "pair", sha)
                .unwrap()
                .is_none(),
            "orphan must be purged or quarantined, never readable"
        );
    }
}

#[test]
fn pair_then_pair_g2_delete_before_echo_bump_must_not_resurrect() {
    let db = database();
    let sha = "1".repeat(40);
    setup_migrated_pair_and_sibling(&db, &sha);
    delete_pair_then_pair_g2(&db);
    assert_orphan_inert(&db, &sha);
    reinsert_pair(&db);
    assert_no_resurrection(&db, &sha, false);
}

#[test]
fn pair_then_pair_g2_delete_after_echo_bump_must_not_resurrect() {
    let db = database();
    let sha = "2".repeat(40);
    setup_migrated_pair_and_sibling(&db, &sha);
    delete_pair_then_pair_g2(&db);
    reinsert_pair(&db);
    assert_no_resurrection(&db, &sha, true);
}

#[test]
fn pair_g2_then_pair_delete_before_echo_bump_must_not_resurrect() {
    let db = database();
    let sha = "3".repeat(40);
    setup_migrated_pair_and_sibling(&db, &sha);
    delete_pair_g2_then_pair(&db);
    assert_orphan_inert(&db, &sha);
    reinsert_pair(&db);
    assert_no_resurrection(&db, &sha, false);
}

#[test]
fn pair_g2_then_pair_delete_after_echo_bump_must_not_resurrect() {
    let db = database();
    let sha = "4".repeat(40);
    setup_migrated_pair_and_sibling(&db, &sha);
    delete_pair_g2_then_pair(&db);
    reinsert_pair(&db);
    assert_no_resurrection(&db, &sha, true);
}

#[test]
fn daemon_orphaned_credential_reinsert_must_not_resurrect_legacy_receipt() {
    let db = database();
    let sha = "5".repeat(40);
    insert_row(&db, "pair");
    write_gen2_pair_ambiguous_receipt(&db, &sha);
    migrate_v13_scope_uuid(&db.conn()).unwrap();
    db.hard_delete_repository("pair").unwrap();
    assert_orphan_inert(&db, &sha);
    db.set_state("secret_svn_password_pair", "orphaned-credential")
        .unwrap();
    let reuse_id = db
        .conn()
        .query_row(
            "SELECT key FROM kv_state WHERE key LIKE 'secret_svn_password_%' AND key != 'secret_svn_password' LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    let id = reuse_id
        .strip_prefix("secret_svn_password_")
        .expect("credential key shape");
    assert_eq!(id, "pair");
    db.insert_repository(&minimal_repository(id)).unwrap();
    assert!(
        !legacy_repo_id_kv_authoritative(&db.conn()).unwrap(),
        "daemon-style sole re-insert must not re-enable legacy KV authority"
    );
    assert_no_resurrection(&db, &sha, false);
}

#[test]
fn new_sole_repository_must_not_reenable_legacy_authority_after_v13_latch() {
    let db = database();
    insert_row(&db, "legacy");
    migrate_v13_scope_uuid(&db.conn()).unwrap();
    assert_eq!(
        db.get_state(LEGACY_READS_FLAG).unwrap(),
        Some("1".into()),
        "sole repo at v13 migration may read legacy KV during transition"
    );
    db.hard_delete_repository("legacy").unwrap();
    assert_eq!(
        db.get_state(LEGACY_READS_FLAG).unwrap(),
        Some("0".into()),
        "deleting the last repository must latch legacy reads off"
    );
    db.insert_repository(&minimal_repository("legacy")).unwrap();
    assert_eq!(
        db.get_state(LEGACY_READS_FLAG).unwrap(),
        Some("0".into()),
        "insert_repository must not set legacy reads back to enabled"
    );
    assert!(
        !legacy_repo_id_kv_authoritative(&db.conn()).unwrap(),
        "re-inserted sole repository must not treat legacy KV as authoritative"
    );
    assert!(
        set_legacy_repo_id_kv_reads_enabled(&db.conn(), true).is_ok(),
        "set enabled true should no-op without error when latched off"
    );
    assert_eq!(
        db.get_state(LEGACY_READS_FLAG).unwrap(),
        Some("0".into()),
        "legacy authority latch must be one-way"
    );
}
