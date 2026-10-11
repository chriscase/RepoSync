use reposync_core::db::{repo_scope_identity::*, Database};
use reposync_core::echo_receipt_scope::*;
use reposync_core::echo_suppression::{
    classify_incoming_git_commit, EchoDisposition, TeamEchoContext,
};
use rusqlite::params;

fn insert(db: &Database, id: &str) {
    db.conn().execute(
        "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_api_url,git_repo,git_branch,enabled,created_at,updated_at,last_svn_rev,last_git_sha,scope_uuid) VALUES (?1,?1,'file:///fixture','trunk','','','','main',1,'t','t',0,'',?2)",
        params![id,new_scope_uuid()],
    ).unwrap();
}

fn db() -> Database {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    insert(&db, "pair");
    db
}

fn legacy_receipt(db: &Database, sha: &str) {
    db.set_state(&format!("handled_git_no_target_pair_{sha}"), &serde_json::json!({
        "version":1,"repo_id":"pair","git_sha":sha,"outcome":"filtered","projection":"{}","generation":1
    }).to_string()).unwrap();
}

#[test]
fn legacy_receipt_must_not_suppress_reregistered_repo() {
    let db = db();
    let sha = "a".repeat(40);
    legacy_receipt(&db, &sha);
    migrate_v13_scope_uuid(&db.conn()).unwrap();
    let old = repository_scope_uuid(&db.conn(), "pair").unwrap();
    // Exercise the production deletion API, not only a raw DELETE.
    db.hard_delete_repository("pair").unwrap();
    insert(&db, "pair");
    assert_ne!(old, repository_scope_uuid(&db.conn(), "pair").unwrap());
    let ctx = TeamEchoContext {
        db: &db,
        repo_id: "pair",
        no_target_projection: "{}",
    };
    assert_eq!(
        classify_incoming_git_commit(&ctx, &sha, "genuine new registration")
            .unwrap()
            .unwrap(),
        EchoDisposition::ApplyGenuine
    );
}

#[test]
fn confirmed_svn_commit_must_advance_the_uuid_checkpoint() {
    use reposync_core::db::svn_commit_operations::{IntendedPath, SvnCommitIntent};
    let db = db();
    insert(&db, "second");
    let before = "b".repeat(40);
    let after = "a".repeat(40);
    write_scoped_last_git_sha_kv(&db.conn(), "pair", &before, "t").unwrap();
    let op = db
        .begin_git_to_svn_commit(SvnCommitIntent {
            repo_id: "pair",
            initiator_id: "worker",
            request_id: "req",
            target_fingerprint: "fp",
            source_git_sha: &after,
            source_git_parent: Some(&before),
            source_git_tree: "tree",
            target_svn_uuid: "uuid",
            target_svn_path: "file:///fixture/trunk",
            target_svn_root_url: "file:///fixture",
            target_svn_branch_path: "trunk",
            pre_write_svn_rev: 2,
            pre_write_svn_tree: "pre",
            projection: "{}",
            intended_changed_paths: vec![IntendedPath {
                action: "A".into(),
                path: "a.txt".into(),
                content_sha256: Some("d".repeat(64)),
            }],
            intended_svn_tree: "post",
            author: "fixture",
            source_message: "fixture change",
        })
        .unwrap();
    db.confirm_git_to_svn_commit("pair", &op.id, 3, "post")
        .unwrap();
    assert_eq!(db.get_repo_watermark("pair").unwrap().1, after);
    assert_eq!(
        read_scoped_last_git_sha_kv(&db, "pair").unwrap(),
        Some(after),
        "confirmed write advanced repository column but left authoritative UUID checkpoint behind"
    );
}

#[test]
fn v13_migration_must_find_receipt_suffixes() {
    let db = db();
    insert(&db, "second");
    let sha = "b".repeat(40);
    legacy_receipt(&db, &sha);
    migrate_v13_scope_uuid(&db.conn()).unwrap();
    let scope = repository_scope_uuid(&db.conn(), "pair").unwrap();
    let migrated = db
        .get_state(&handled_git_no_target_state_key(&scope, 1, &sha))
        .unwrap();
    assert!(
        migrated.is_some(),
        "v13 omitted the existing receipt, leaving only the repo-id key"
    );
}

#[test]
fn unproven_legacy_receipt_must_not_reactivate_when_second_repo_removed() {
    let db = db();
    insert(&db, "second");
    let sha = "c".repeat(40);
    legacy_receipt(&db, &sha);
    assert!(read_git_no_target_receipt(&db, "pair", &sha)
        .unwrap()
        .is_none());
    db.hard_delete_repository("second").unwrap();
    assert!(
        read_git_no_target_receipt(&db, "pair", &sha)
            .unwrap()
            .is_none(),
        "removing unrelated repository made unproven legacy receipt authoritative"
    );
}
