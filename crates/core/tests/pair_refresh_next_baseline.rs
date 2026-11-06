//! Advanced baselines require exact completed refresh provenance.
#[path = "fixtures/pair_refresh_job.rs"]
mod job;

use reposync_core::db::pair_refresh_operations::{
    capture_refresh_registration, PairRefreshPhase, RefreshJobPins,
};
use reposync_core::db::Database;
use reposync_core::pair_refresh::svn_observation::SvnRefreshPublicationReceipt;
use uuid::Uuid;

fn complete(db: &Database, pins: &RefreshJobPins, revision: i64) {
    let id = pins.git.operation_id;
    let mut copy = pins.svn.pins.lineage.child_copy.clone();
    copy.inspected_revision = revision;
    db.record_pair_refresh_svn_intent(id).unwrap();
    db.record_pair_refresh_svn_receipt(
        id,
        SvnRefreshPublicationReceipt {
            operation_id: id,
            revision,
            child_url: pins.svn.pins.lineage.pins.child_url.clone(),
            child_copy: copy,
            git_tree: pins.git.tree_sha.clone(),
            mergeinfo: pins.svn.mergeinfo.clone(),
            properties: pins.svn.result_properties.clone(),
        },
    )
    .unwrap();
    db.record_pair_refresh_git_intent(id).unwrap();
    db.record_pair_refresh_git_receipt(id, &pins.git.prepared_sha)
        .unwrap();
    assert_eq!(
        db.finalize_pair_refresh(id).unwrap().phase,
        PairRefreshPhase::Completed
    );
}

fn advance_parent(db: &Database) {
    db.conn()
        .execute(
            "UPDATE repositories SET last_git_sha=?1,last_svn_rev=9 WHERE id='parent'",
            ["9".repeat(40)],
        )
        .unwrap();
    db.set_state("last_svn_rev_parent", "9").unwrap();
    reposync_core::echo_receipt_scope::write_scoped_last_git_sha_kv(
        &db.conn(),
        "parent",
        &"9".repeat(40),
        "t",
    )
    .unwrap();
}

fn next(db: &Database, prior: &RefreshJobPins) -> RefreshJobPins {
    let mut pins = prior.clone();
    pins.child = capture_refresh_registration(&db.conn(), "child").unwrap();
    pins.parent = capture_refresh_registration(&db.conn(), "parent").unwrap();
    pins.git.operation_id = Uuid::new_v4();
    pins.svn.operation_id = pins.git.operation_id;
    pins.git.prepared_sha = "8".repeat(40);
    pins.git.tree_sha = "6".repeat(40);
    pins.git.pins.child_tip = pins.child.last_git_sha.clone();
    pins.git.pins.parent_tip = pins.parent.last_git_sha.clone();
    pins.git.pins.parent_baseline_tip = prior.parent.last_git_sha.clone();
    pins.svn.pins.baseline_git_tree = prior.git.tree_sha.clone();
    pins.svn.pins.parent_git_tree = pins.git.tree_sha.clone();
    pins.svn.pins.previous_mergeinfo = Some(prior.svn.mergeinfo.clone());
    pins.svn.result_git_tree = pins.git.tree_sha.clone();
    pins.svn.pins.lineage.child_copy.inspected_revision = pins.child.last_svn_rev;
    let lineage = &mut pins.svn.pins.lineage.pins;
    lineage.child.child_revision = pins.child.last_svn_rev;
    lineage.child.child_mapping_revision = pins.child.last_svn_rev;
    lineage.child.parent_baseline_revision = prior.parent.last_svn_rev;
    lineage.parent_revision = pins.parent.last_svn_rev;
    pins.svn.mergeinfo = "/trunk:3-9".into();
    pins.svn
        .result_properties
        .get_mut("")
        .unwrap()
        .insert("svn:mergeinfo".into(), pins.svn.mergeinfo.clone());
    pins
}

#[test]
fn completed_receipt_supports_next_advance_after_reopen_and_idempotent_completion() {
    let f = job::Fixture::new();
    let prior = f.pins();
    let guard = f.busy();
    f.admit(prior.clone(), &guard);
    complete(&f.db, &prior, 7);
    advance_parent(&f.db);
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    let pins = next(&reopened, &prior);
    let digest = pins.approval_digest().unwrap();
    let op = reopened
        .admit_pair_refresh("admin", "second", &digest, pins.clone(), &guard)
        .unwrap();
    assert_eq!(op.phase, PairRefreshPhase::Prepared);
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
    let parent = capture_refresh_registration(&reopened.conn(), "parent").unwrap();
    complete(&reopened, &pins, 10);
    let done = reopened
        .admit_pair_refresh("admin", "second", &digest, pins.clone(), &guard)
        .unwrap();
    assert_eq!(done.id, op.id);
    assert_eq!(done.phase, PairRefreshPhase::Completed);
    assert_eq!(reopened.finalize_pair_refresh(op.id).unwrap(), done);
    assert_eq!(
        capture_refresh_registration(&reopened.conn(), "parent").unwrap(),
        parent
    );
    let child = capture_refresh_registration(&reopened.conn(), "child").unwrap();
    assert_eq!(child.last_git_sha, pins.git.prepared_sha);
    assert_eq!(child.last_svn_rev, 10);
    let count: i64 = reopened
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM commit_map WHERE repo_id='child'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    assert!(f.no_hold(None));
}

#[test]
fn advanced_baseline_and_caller_mergeinfo_cannot_replace_a_completed_receipt() {
    for with_mergeinfo in [false, true] {
        let f = job::Fixture::new();
        let prior = f.pins();
        let guard = f.busy();
        f.db.conn()
            .execute(
                "UPDATE repositories SET last_git_sha=?1,last_svn_rev=7 WHERE id='child'",
                [&prior.git.prepared_sha],
            )
            .unwrap();
        f.db.set_state("last_svn_rev_child", "7").unwrap();
        reposync_core::echo_receipt_scope::write_scoped_last_git_sha_kv(
            &f.db.conn(),
            "child",
            &prior.git.prepared_sha,
            "t",
        )
        .unwrap();
        advance_parent(&f.db);
        let mut pins = next(&f.db, &prior);
        if !with_mergeinfo {
            pins.svn.pins.previous_mergeinfo = None;
            pins.svn.mergeinfo = "/trunk:5-9".into();
            pins.svn
                .result_properties
                .get_mut("")
                .unwrap()
                .insert("svn:mergeinfo".into(), pins.svn.mergeinfo.clone());
        }
        assert!(f
            .db
            .admit_pair_refresh(
                "admin",
                "second",
                &pins.approval_digest().unwrap(),
                pins,
                &guard
            )
            .is_err());
        assert!(f.no_hold(None));
        let count: i64 =
            f.db.conn()
                .query_row(
                    "SELECT COUNT(*) FROM kv_state WHERE key LIKE 'pair_refresh_execution_%'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(count, 0);
    }
}

#[test]
fn next_baseline_refuses_any_drift_from_the_completed_receipt_without_new_holds() {
    for fault in 0..12 {
        let f = job::Fixture::new();
        let prior = f.pins();
        let guard = f.busy();
        f.admit(prior.clone(), &guard);
        complete(&f.db, &prior, 7);
        advance_parent(&f.db);
        let mut pins = next(&f.db, &prior);
        match fault {
            0 => pins.git.pins.parent_baseline_tip = "f".repeat(40),
            1 => pins.svn.pins.baseline_git_tree = "f".repeat(40),
            2 => {
                pins.svn.pins.previous_mergeinfo = Some("/trunk:4".into());
                pins.svn.mergeinfo = "/trunk:4-9".into();
                pins.svn
                    .result_properties
                    .get_mut("")
                    .unwrap()
                    .insert("svn:mergeinfo".into(), pins.svn.mergeinfo.clone());
            }
            3 => pins.svn.pins.lineage.child_copy.copy_from_revision = 1,
            4 => pins.svn.pins.lineage.child_copy.created_revision = 4,
            5 => pins.svn.pins.lineage.parent_originating_revision = 2,
            6 => {
                pins.svn.pins.lineage.pins.repository_root_url = "https://svn.example/other".into();
                pins.svn.pins.lineage.pins.child_url =
                    "https://svn.example/other/branches/f".into();
                pins.svn.pins.lineage.pins.parent_url = "https://svn.example/other/trunk".into();
            }
            7 => pins.git_remote_url = "https://git.example/other/repo.git".into(),
            8 => pins.git_branch = "other".into(),
            9 => pins.child.echo_generation += 1,
            10 => pins.parent.configuration_digest = "f".repeat(64),
            _ => pins.svn.pins.lineage.pins.child.parent_baseline_revision = 5,
        }
        let before = capture_refresh_registration(&f.db.conn(), "child").unwrap();
        assert!(
            f.db.admit_pair_refresh(
                "admin",
                "second",
                &pins.approval_digest().unwrap(),
                pins,
                &guard
            )
            .is_err(),
            "fault {fault}"
        );
        assert_eq!(
            capture_refresh_registration(&f.db.conn(), "child").unwrap(),
            before
        );
        assert!(f.no_hold(None));
        assert_eq!(
            f.db.pair_refresh_operation(prior.git.operation_id)
                .unwrap()
                .phase,
            PairRefreshPhase::Completed
        );
    }
}
