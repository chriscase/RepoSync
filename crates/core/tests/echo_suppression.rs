//! Receipt-backed echo suppression tests for team mode (RS-05 slice).

use chrono::Utc;
use reposync_core::db::git_push_operations::{git_push_target_fingerprint, GitPushIntent};
use reposync_core::echo_suppression::{
    classify_incoming_git_commit, classify_incoming_svn_revision, EchoDisposition, TeamEchoContext,
    SYNC_MARKER,
};
use reposync_core::models::{SyncDirection, SyncRecord, SyncRecordStatus};
use reposync_core::Database;

fn setup_db() -> Database {
    let db = Database::in_memory().unwrap();
    db.initialize().unwrap();
    db
}

fn insert_applied_record(
    db: &Database,
    repo_id: &str,
    direction: SyncDirection,
    svn_rev: i64,
    git_sha: &str,
) {
    let now = Utc::now();
    db.insert_sync_record(&SyncRecord {
        id: uuid::Uuid::new_v4().to_string(),
        repo_id: Some(repo_id.to_string()),
        svn_revision: Some(svn_rev),
        git_hash: Some(git_sha.to_string()),
        direction,
        author: "fixture".into(),
        message: "fixture".into(),
        timestamp: now,
        synced_at: now,
        status: SyncRecordStatus::Applied,
    })
    .unwrap();
}

fn ctx<'a>(db: &'a Database, repo_id: &'a str) -> TeamEchoContext<'a> {
    TeamEchoContext {
        db,
        repo_id,
        no_target_projection: "{}",
    }
}

#[test]
fn repo_scoped_svn_echo_does_not_leak_across_managed_repositories() {
    let db = setup_db();
    let rev = 7_i64;
    let git_sha = "1".repeat(40);
    insert_applied_record(&db, "repo-a", SyncDirection::GitToSvn, rev, &git_sha);

    let marker = format!("team commit\n\n{SYNC_MARKER} synced from Git abcdef01");
    assert_eq!(
        classify_incoming_svn_revision(&ctx(&db, "repo-a"), rev, &marker).unwrap(),
        EchoDisposition::SkipEcho
    );
    assert_eq!(
        classify_incoming_svn_revision(&ctx(&db, "repo-b"), rev, &marker).unwrap(),
        EchoDisposition::ApplyGenuineWithMarkerHint
    );
    assert!(db.has_repo_emitted_svn_revision("repo-a", rev).unwrap());
    assert!(!db.has_repo_emitted_svn_revision("repo-b", rev).unwrap());
}

#[test]
fn forged_git_marker_is_retained_for_apply_and_true_echo_is_skipped() {
    let db = setup_db();
    let forged_sha = "2".repeat(40);
    let echoed_sha = "3".repeat(40);
    insert_applied_record(&db, "pair", SyncDirection::SvnToGit, 11, &echoed_sha);

    let forged_message = format!("user change\n\n{SYNC_MARKER} forged marker");
    assert_eq!(
        classify_incoming_git_commit(&ctx(&db, "pair"), &forged_sha, &forged_message)
            .unwrap()
            .unwrap(),
        EchoDisposition::ApplyGenuineWithMarkerHint
    );
    assert_eq!(
        classify_incoming_git_commit(&ctx(&db, "pair"), &echoed_sha, "edited away marker")
            .unwrap()
            .unwrap(),
        EchoDisposition::SkipEcho
    );

    let pending: Vec<String> = [forged_sha.clone(), echoed_sha.clone()]
        .into_iter()
        .filter(|sha| {
            classify_incoming_git_commit(&ctx(&db, "pair"), sha, "")
                .unwrap()
                .unwrap()
                != EchoDisposition::SkipEcho
        })
        .collect();
    assert_eq!(pending, vec![forged_sha]);
}

#[test]
fn handled_no_target_receipt_suppresses_only_when_admission_scoped() {
    let db = setup_db();
    let sha = "4".repeat(40);
    let projection = "{}";
    let receipt = serde_json::json!({
        "version": 1,
        "repo_id": "pair",
        "git_sha": sha,
        "outcome": "filtered",
        "projection": projection,
    });
    db.set_state(
        &format!("handled_git_no_target_pair_{sha}"),
        &receipt.to_string(),
    )
    .unwrap();
    assert_eq!(
        classify_incoming_git_commit(&ctx(&db, "pair"), &sha, "no marker")
            .unwrap()
            .unwrap(),
        EchoDisposition::SkipEcho
    );

    let other_repo_sha = "5".repeat(40);
    db.set_state(
        &format!("handled_git_no_target_other_{other_repo_sha}"),
        &serde_json::json!({
            "version": 1,
            "repo_id": "other",
            "git_sha": other_repo_sha,
            "outcome": "filtered",
            "projection": projection,
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        classify_incoming_git_commit(&ctx(&db, "pair"), &other_repo_sha, "no marker")
            .unwrap()
            .unwrap(),
        EchoDisposition::ApplyGenuine
    );

    let malformed_sha = "6".repeat(40);
    db.set_state(
        &format!("handled_git_no_target_pair_{malformed_sha}"),
        "not-json",
    )
    .unwrap();
    assert_eq!(
        classify_incoming_git_commit(&ctx(&db, "pair"), &malformed_sha, "no marker")
            .unwrap()
            .unwrap(),
        EchoDisposition::ApplyGenuine
    );

    let weak_v3_sha = "7".repeat(40);
    db.set_state(
        &format!("handled_git_no_target_pair_{weak_v3_sha}"),
        &serde_json::json!({
            "version": 3,
            "repo_id": "pair",
            "git_sha": weak_v3_sha,
            "outcome": "no_svn_delta",
            "projection": projection,
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        classify_incoming_git_commit(&ctx(&db, "pair"), &weak_v3_sha, "no marker")
            .unwrap()
            .unwrap(),
        EchoDisposition::ApplyGenuine
    );
}

#[test]
fn running_journal_defers_marker_without_receipt_for_git_echo() {
    let db = setup_db();
    db.conn()
        .execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
             VALUES ('pair','p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','idle',0,0)",
            [],
        )
        .unwrap();
    let git_sha = "dddddddddddddddddddddddddddddddddddddddd";
    let fingerprint = git_push_target_fingerprint("pair", "origin", "main");
    db.begin_svn_to_git_push(GitPushIntent {
        repo_id: "pair",
        initiator_id: "worker",
        request_id: "req-1",
        target_fingerprint: &fingerprint,
        source_svn_rev: 3,
        source_svn_author: "dev",
        source_svn_message: "add feature",
        pre_push_git_remote: "origin",
        pre_push_git_branch: "main",
        pre_push_git_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        pre_push_git_tree: Some("cccccccccccccccccccccccccccccccccccccccc"),
        intended_local_git_sha: git_sha,
        intended_local_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        intended_local_git_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
    })
    .unwrap();
    let marker = format!("synced\n\n{SYNC_MARKER} synced from SVN r3");
    assert_eq!(
        classify_incoming_git_commit(&ctx(&db, "pair"), git_sha, &marker)
            .unwrap()
            .unwrap(),
        EchoDisposition::DeferPendingJournal
    );
}

#[test]
fn handled_svn_no_target_receipt_suppresses_only_when_admission_scoped() {
    let db = setup_db();
    let svn_rev = 8_i64;
    let projection = "{}";
    let receipt = serde_json::json!({
        "version": 1,
        "repo_id": "pair",
        "svn_revision": svn_rev,
        "outcome": "no_git_content",
        "projection": projection,
    });
    db.set_state(
        &format!("handled_svn_no_target_pair_{svn_rev}"),
        &receipt.to_string(),
    )
    .unwrap();
    assert_eq!(
        classify_incoming_svn_revision(&ctx(&db, "pair"), svn_rev, "no marker").unwrap(),
        EchoDisposition::SkipEcho
    );

    let other_rev = 9_i64;
    db.set_state(
        &format!("handled_svn_no_target_other_{other_rev}"),
        &serde_json::json!({
            "version": 1,
            "repo_id": "other",
            "svn_revision": other_rev,
            "outcome": "no_git_content",
            "projection": projection,
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        classify_incoming_svn_revision(&ctx(&db, "pair"), other_rev, "no marker").unwrap(),
        EchoDisposition::ApplyGenuine
    );

    let malformed_rev = 10_i64;
    db.set_state(
        &format!("handled_svn_no_target_pair_{malformed_rev}"),
        "not-json",
    )
    .unwrap();
    assert_eq!(
        classify_incoming_svn_revision(&ctx(&db, "pair"), malformed_rev, "no marker").unwrap(),
        EchoDisposition::ApplyGenuine
    );

    let weak_rev = 11_i64;
    db.set_state(
        &format!("handled_svn_no_target_pair_{weak_rev}"),
        &serde_json::json!({
            "version": 3,
            "repo_id": "pair",
            "svn_revision": weak_rev,
            "outcome": "no_git_content",
            "projection": projection,
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        classify_incoming_svn_revision(&ctx(&db, "pair"), weak_rev, "no marker").unwrap(),
        EchoDisposition::ApplyGenuine
    );
}

#[test]
fn running_svn_journal_defers_matching_rev_without_marker() {
    let db = setup_db();
    db.conn()
        .execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
             VALUES ('pair','p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','idle',0,0)",
            [],
        )
        .unwrap();
    use reposync_core::db::svn_commit_operations::{
        svn_commit_target_fingerprint, SvnCommitIntent,
    };
    let git_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let fingerprint = svn_commit_target_fingerprint("pair", "uuid", "/repo", "/repo", "{}");
    db.begin_git_to_svn_commit(SvnCommitIntent {
        repo_id: "pair",
        initiator_id: "worker",
        request_id: "req-1",
        target_fingerprint: &fingerprint,
        source_git_sha: git_sha,
        source_git_parent: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        source_git_tree: "cccccccccccccccccccccccccccccccccccccccc",
        target_svn_uuid: "uuid",
        target_svn_path: "/repo",
        target_svn_root_url: "/repo",
        target_svn_branch_path: "",
        pre_write_svn_rev: 4,
        pre_write_svn_tree: "dddddddddddddddddddddddddddddddddddddddd",
        projection: "{}",
        intended_changed_paths: vec![],
        intended_svn_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        author: "dev",
        source_message: "feature",
    })
    .unwrap();
    assert_eq!(
        classify_incoming_svn_revision(&ctx(&db, "pair"), 5, "edited away marker").unwrap(),
        EchoDisposition::DeferPendingJournal
    );
}

#[test]
fn running_git_journal_defers_matching_sha_without_marker() {
    let db = setup_db();
    db.conn()
        .execute(
            "INSERT INTO repositories (id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors)
             VALUES ('pair','p','file:///svn','','','local','','repo','main','team',5,0,0,1,'t','t',2,'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','idle',0,0)",
            [],
        )
        .unwrap();
    let git_sha = "dddddddddddddddddddddddddddddddddddddddd";
    let fingerprint = git_push_target_fingerprint("pair", "origin", "main");
    db.begin_svn_to_git_push(GitPushIntent {
        repo_id: "pair",
        initiator_id: "worker",
        request_id: "req-1",
        target_fingerprint: &fingerprint,
        source_svn_rev: 3,
        source_svn_author: "dev",
        source_svn_message: "add feature",
        pre_push_git_remote: "origin",
        pre_push_git_branch: "main",
        pre_push_git_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        pre_push_git_tree: Some("cccccccccccccccccccccccccccccccccccccccc"),
        intended_local_git_sha: git_sha,
        intended_local_git_parent: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        intended_local_git_tree: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
    })
    .unwrap();
    assert_eq!(
        classify_incoming_git_commit(&ctx(&db, "pair"), git_sha, "edited away marker")
            .unwrap()
            .unwrap(),
        EchoDisposition::DeferPendingJournal
    );
}
