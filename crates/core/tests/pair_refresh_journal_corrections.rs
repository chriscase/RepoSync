//! Regression controls for Grok's journal correspondence and evidence findings.
#[path = "fixtures/pair_refresh_job.rs"]
mod job;

use reposync_core::db::pair_refresh_operations::{
    capture_refresh_registration, PairRefreshPhase, RefreshJobPins,
};
use reposync_core::pair_refresh::svn_observation::SvnRefreshPublicationReceipt;
use rusqlite::params;

fn confirm(f: &job::Fixture, pins: &RefreshJobPins, id: uuid::Uuid) {
    let mut copy = pins.svn.pins.lineage.child_copy.clone();
    copy.inspected_revision = 7;
    let receipt = SvnRefreshPublicationReceipt {
        operation_id: id,
        revision: 7,
        child_url: pins.svn.pins.lineage.pins.child_url.clone(),
        child_copy: copy,
        git_tree: pins.git.tree_sha.clone(),
        mergeinfo: pins.svn.mergeinfo.clone(),
        properties: pins.svn.result_properties.clone(),
    };
    f.db.record_pair_refresh_svn_intent(id).unwrap();
    f.db.record_pair_refresh_svn_receipt(id, receipt).unwrap();
    f.db.record_pair_refresh_git_intent(id).unwrap();
    f.db.record_pair_refresh_git_receipt(id, &pins.git.prepared_sha)
        .unwrap();
}

fn map(f: &job::Fixture, owner: &str, rev: i64, sha: &str, direction: &str) {
    f.db.conn().execute("INSERT INTO commit_map(repo_id,svn_rev,git_sha,direction,synced_at) VALUES(?1,?2,?3,?4,'t')", params![owner,rev,sha,direction]).unwrap();
}

#[test]
fn conflicting_correspondence_rolls_back_completion_and_retains_both_holds() {
    for (rev, direction, matching_sha) in [
        (7, "svn_to_git", false),
        (6, "git_to_svn", true),
        (7, "svn_to_git", true),
    ] {
        let f = job::Fixture::new();
        let pins = f.pins();
        let guard = f.busy();
        let id = f.admit(pins.clone(), &guard);
        confirm(&f, &pins, id);
        let sha = if matching_sha {
            pins.git.prepared_sha.clone()
        } else {
            "e".repeat(40)
        };
        map(&f, "child", rev, &sha, direction);
        let before = capture_refresh_registration(&f.db.conn(), "child").unwrap();
        assert!(f.db.finalize_pair_refresh(id).is_err());
        assert_eq!(
            capture_refresh_registration(&f.db.conn(), "child").unwrap(),
            before
        );
        assert_eq!(
            f.db.pair_refresh_operation(id).unwrap().phase,
            PairRefreshPhase::GitObserved
        );
        assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
        let count: i64 =
            f.db.conn()
                .query_row("SELECT COUNT(*) FROM sync_records", [], |r| r.get(0))
                .unwrap();
        assert_eq!(count, 0);
    }
}

#[test]
fn completion_inserts_exact_child_map_once_without_adopting_parent_map() {
    for preexisting in [false, true] {
        let f = job::Fixture::new();
        let pins = f.pins();
        let guard = f.busy();
        let id = f.admit(pins.clone(), &guard);
        confirm(&f, &pins, id);
        let parent = capture_refresh_registration(&f.db.conn(), "parent").unwrap();
        map(&f, "parent", 7, &pins.git.prepared_sha, "svn_to_git");
        if preexisting {
            map(&f, "child", 7, &pins.git.prepared_sha, "git_to_svn");
        }
        assert_eq!(
            f.db.finalize_pair_refresh(id).unwrap().phase,
            PairRefreshPhase::Completed
        );
        f.db.finalize_pair_refresh(id).unwrap();
        let count:i64=f.db.conn().query_row("SELECT COUNT(*) FROM commit_map WHERE repo_id='child' AND svn_rev=7 AND git_sha=?1 AND direction='git_to_svn'",[&pins.git.prepared_sha],|r|r.get(0)).unwrap();
        assert_eq!(count, 1);
        let parent_count:i64=f.db.conn().query_row("SELECT COUNT(*) FROM commit_map WHERE repo_id='parent' AND svn_rev=7 AND git_sha=?1 AND direction='svn_to_git'",[&pins.git.prepared_sha],|r|r.get(0)).unwrap();
        assert_eq!(parent_count, 1);
        assert_eq!(
            capture_refresh_registration(&f.db.conn(), "parent").unwrap(),
            parent
        );
        assert!(f.no_hold(Some("child")) && f.no_hold(Some("parent")));
    }
}

#[test]
fn commit_map_insert_abort_rolls_back_cursors_document_and_holds() {
    let f = job::Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    let id = f.admit(pins.clone(), &guard);
    confirm(&f, &pins, id);
    let before = capture_refresh_registration(&f.db.conn(), "child").unwrap();
    f.db.conn().execute_batch("CREATE TRIGGER fail_map BEFORE INSERT ON commit_map BEGIN SELECT RAISE(ABORT,'map fault'); END;").unwrap();
    assert!(f.db.finalize_pair_refresh(id).is_err());
    assert_eq!(
        capture_refresh_registration(&f.db.conn(), "child").unwrap(),
        before
    );
    assert_eq!(
        f.db.pair_refresh_operation(id).unwrap().phase,
        PairRefreshPhase::GitObserved
    );
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
    f.db.conn().execute_batch("DROP TRIGGER fail_map").unwrap();
    f.db.finalize_pair_refresh(id).unwrap();
}

#[test]
fn split_or_missing_checkpoint_copies_refuse_capture_and_admission() {
    for repo in ["child", "parent"] {
        for (git, missing) in [(true, false), (true, true), (false, false), (false, true)] {
            let f = job::Fixture::new();
            let pins = f.pins();
            let guard = f.busy();
            let pin = if repo == "child" {
                &pins.child
            } else {
                &pins.parent
            };
            let key = if git {
                reposync_core::db::repo_scope_identity::last_git_sha_kv_key(&pin.scope_uuid)
            } else {
                format!("last_svn_rev_{repo}")
            };
            if missing {
                f.db.conn()
                    .execute("DELETE FROM kv_state WHERE key=?1", [&key])
                    .unwrap();
            } else {
                f.db.conn()
                    .execute(
                        "UPDATE kv_state SET value=?1 WHERE key=?2",
                        params![if git { "e".repeat(40) } else { "9".into() }, key],
                    )
                    .unwrap();
            }
            assert!(capture_refresh_registration(&f.db.conn(), repo).is_err());
            assert!(f
                .db
                .admit_pair_refresh(
                    "admin",
                    "request",
                    &pins.approval_digest().unwrap(),
                    pins,
                    &guard
                )
                .is_err());
            assert!(f.no_hold(None));
        }
    }
}

#[test]
fn contradictory_copy_origin_or_root_mergeinfo_refuses_without_journal_mutation() {
    for fault in 0..8 {
        let f = job::Fixture::new();
        let mut pins = f.pins();
        let guard = f.busy();
        match fault {
            0 => pins.svn.pins.lineage.child_copy.repository_uuid = "foreign".into(),
            1 => pins.svn.pins.lineage.child_copy.child_path = "/other".into(),
            2 => pins.svn.pins.lineage.child_copy.parent_path = "/other".into(),
            3 => pins.svn.pins.lineage.child_copy.copy_from_revision = 1,
            4 => pins.svn.pins.lineage.child_copy.inspected_revision = 9,
            5 => pins
                .svn
                .result_properties
                .get_mut("")
                .unwrap()
                .insert("svn:mergeinfo".into(), "/other:1-9".into())
                .map(|_| ())
                .unwrap(),
            6 => {
                pins.svn.result_properties.remove("");
            }
            _ => pins.child.scoped_svn_checkpoint = Some("9".into()),
        }
        assert!(f
            .db
            .admit_pair_refresh(
                "admin",
                "request",
                &pins.approval_digest().unwrap(),
                pins,
                &guard
            )
            .is_err());
        assert!(f.no_hold(None));
        let rows: i64 =
            f.db.conn()
                .query_row(
                    "SELECT COUNT(*) FROM kv_state WHERE key LIKE 'pair_refresh_execution_%'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(rows, 0);
    }
}
