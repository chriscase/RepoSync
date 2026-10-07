//! Receipt-backed echo suppression tests for team mode (RS-05 slice).

use chrono::Utc;
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
