use super::*;
use crate::db::lineage_checkpoints::{
    load_directional_checkpoint, record_independently_established_svn_uuid, CheckpointDirection,
    MappingOutcome,
};
use std::cell::Cell;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);

thread_local! {
    // SQLite accepts a function pointer for the busy handler. Each test worker owns its handoff.
    static HANDOFF: Cell<Option<(Sender<()>, Receiver<()>)>> = const { Cell::new(None) };
}

fn release_writer(_: i32) -> bool {
    HANDOFF.take().is_some_and(|(waiting, committed)| {
        waiting.send(()).is_ok() && committed.recv_timeout(WAIT).is_ok()
    })
}

fn journal(path: &Path) -> Database {
    let db = Database::new(path).unwrap();
    db.initialize().unwrap();
    pr245_3_register(&db, "pair", "file:///fixture");
    record_independently_established_svn_uuid(&db, "pair", &Uuid::new_v4().to_string()).unwrap();
    db.update_repo_watermark("pair", 2, sample_intent().pre_push_git_sha)
        .unwrap();
    db
}

fn while_writer_is_busy<T: Send>(
    path: &Path,
    db: Database,
    transition: impl FnOnce(&Database) -> Result<T, DatabaseError> + Send,
) -> (Database, T) {
    let mut peer = Connection::open(path).unwrap();
    let writer = peer
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    writer
        .execute(
            "INSERT INTO kv_state(key,value,updated_at) VALUES ('peer','committed','now')",
            [],
        )
        .unwrap();
    let (waiting_tx, waiting_rx) = mpsc::channel();
    let (committed_tx, committed_rx) = mpsc::channel();
    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            HANDOFF.set(Some((waiting_tx, committed_rx)));
            db.conn().busy_handler(Some(release_writer)).unwrap();
            transition(&db)
        });
        // A deferred read-to-write upgrade returns BUSY without calling the handler.
        // A writer reservation invokes it before reading, then continues after COMMIT.
        let waited = waiting_rx.recv_timeout(WAIT);
        writer.commit().unwrap();
        let _ = committed_tx.send(());
        let result = worker
            .join()
            .unwrap()
            .expect("repository transition must wait for the writer");
        waited.expect("SQLite must observe the held writer");
        result
    });
    let reopened = Database::new(path).unwrap();
    assert_eq!(
        reopened.get_state("peer").unwrap().as_deref(),
        Some("committed")
    );
    (reopened, result)
}

fn assert_unadvanced(db: &Database) {
    assert_eq!(
        pr245_3_repo_row(db, "pair"),
        (2, sample_intent().pre_push_git_sha.into(), 0)
    );
    assert_eq!(db.count_sync_records().unwrap(), 0);
    let (scope, generation) = pr245_3_scope(db, "pair");
    assert!(load_directional_checkpoint(
        &db.conn(),
        &scope,
        generation,
        CheckpointDirection::SvnToGit
    )
    .unwrap()
    .is_none());
}

#[test]
fn begin_reserves_writer_before_reading_intent() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reposync.db");
    let (db, op) = while_writer_is_busy(&path, journal(&path), |db| {
        db.begin_svn_to_git_push(sample_intent())
    });
    assert_eq!(op.state, GitPushOperationState::Running);
    assert_eq!(op.pre_push_git_remote, "origin");
    assert_eq!(op.pre_push_git_branch, "main");
    assert_eq!(op.pre_push_git_sha, sample_intent().pre_push_git_sha);
    assert_eq!(
        op.intended_local_git_sha,
        sample_intent().intended_local_git_sha
    );
    assert_eq!(db.active_git_push_operation("pair").unwrap(), Some(op));
    assert_unadvanced(&db);
}

#[test]
fn confirm_reserves_writer_before_reading_receipts() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reposync.db");
    let db = journal(&path);
    let intent = db.begin_svn_to_git_push(sample_intent()).unwrap();
    let (db, op) = while_writer_is_busy(&path, db, |db| {
        db.confirm_svn_to_git_push(
            "pair",
            &intent.id,
            &intent.intended_local_git_sha,
            &intent.intended_local_git_tree,
        )
    });
    assert_eq!(op.id, intent.id);
    assert_eq!(op.state, GitPushOperationState::Completed);
    assert_eq!(
        db.get_git_push_operation("pair", &intent.id).unwrap(),
        Some(op.clone())
    );
    assert_eq!(
        op.last_confirmed_git_sha.as_ref(),
        Some(&intent.intended_local_git_sha)
    );
    assert_eq!(
        op.last_confirmed_git_tree.as_ref(),
        Some(&intent.intended_local_git_tree)
    );
    assert!(db.active_git_push_operation("pair").unwrap().is_none());
    assert_eq!(
        pr245_3_repo_row(&db, "pair"),
        (3, intent.intended_local_git_sha.clone(), 1)
    );
    assert_eq!(db.count_sync_records().unwrap(), 1);
    let receipt_id: String = db
        .conn()
        .query_row(
            "SELECT id FROM sync_records WHERE repo_id='pair' AND svn_rev=3 AND git_sha=?1
         AND direction='svn_to_git' AND status='applied' AND author=?2 AND message=?3",
            params![
                intent.intended_local_git_sha,
                intent.source_svn_author,
                intent.source_svn_message
            ],
            |row| row.get(0),
        )
        .unwrap();
    assert!(Uuid::parse_str(&receipt_id).is_ok());
    let (scope, generation) = pr245_3_scope(&db, "pair");
    let checkpoint = load_directional_checkpoint(
        &db.conn(),
        &scope,
        generation,
        CheckpointDirection::SvnToGit,
    )
    .unwrap()
    .unwrap();
    assert_eq!(checkpoint.handled_source_key, "svn:3");
    assert_eq!(checkpoint.outcome, MappingOutcome::AppliedAndVerified);
    assert_eq!(
        checkpoint.target_git_sha,
        Some(intent.intended_local_git_sha)
    );
}

#[test]
fn hold_reserves_writer_before_reading_active_push() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reposync.db");
    let db = journal(&path);
    let intent = db.begin_svn_to_git_push(sample_intent()).unwrap();
    let (db, op) = while_writer_is_busy(&path, db, |db| {
        db.hold_svn_to_git_reconciliation("pair", &intent.id, "checkpoint write failed after push")
    });
    assert_eq!(op.id, intent.id);
    assert_eq!(op.state, GitPushOperationState::ReconciliationRequired);
    assert_eq!(
        op.outcome_detail.as_deref(),
        Some("checkpoint write failed after push")
    );
    assert!(!op.resume_authorized);
    assert!(op.last_confirmed_git_sha.is_none());
    assert_eq!(db.active_git_push_operation("pair").unwrap(), Some(op));
    assert_unadvanced(&db);
}

#[test]
fn reconciliation_status_reserves_writer_before_checking_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reposync.db");
    let db = journal(&path);
    pr245_3_register(&db, "sibling", "file:///sibling");
    db.update_repo_sync_status("pair", "applying").unwrap();
    let intent = db.begin_svn_to_git_push(sample_intent()).unwrap();
    let held = db
        .hold_svn_to_git_reconciliation("pair", &intent.id, "push was rejected")
        .unwrap();
    let (db, ()) = while_writer_is_busy(&path, db, |db| {
        db.update_repo_sync_status("pair", "reconciliation_required")
    });
    assert_eq!(
        db.get_repository("pair").unwrap().unwrap().sync_status,
        "reconciliation_required"
    );
    assert_eq!(
        db.get_repository("sibling").unwrap().unwrap().sync_status,
        "idle"
    );
    assert_eq!(held.state, GitPushOperationState::ReconciliationRequired);
    assert_eq!(db.active_git_push_operation("pair").unwrap(), Some(held));
    assert_unadvanced(&db);
}
