//! Baseline selection reads current registration and exact completed evidence.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;

use reposync_core::db::pair_refresh_operations::{PairRefreshPhase, RefreshJobPins};
use reposync_core::pair_refresh::svn_observation::SvnRefreshPublicationReceipt;

fn fixture() -> job::Fixture {
    let f = job::Fixture::new();
    f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
    f.db.conn().execute("UPDATE repositories SET git_repo='owner/repo',git_api_url='https://git.example/api/v1'", []).unwrap();
    f.db.conn()
        .execute(
            "UPDATE repositories SET svn_branch='branches/f' WHERE id='child'",
            [],
        )
        .unwrap();
    f
}

fn rows(f: &job::Fixture) -> Vec<(String, String, String)> {
    f.db.conn()
        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn complete(f: &job::Fixture) -> RefreshJobPins {
    let configuration =
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap();
    let mut pins = f.pins();
    pins.credential_context_digest = configuration.pin().credential_context_digest.clone();
    let busy = f.busy();
    let id =
        f.db.admit_pair_refresh_with_registered_sources(
            "admin",
            "first",
            &pins.approval_digest().unwrap(),
            pins.clone(),
            &busy,
        )
        .unwrap()
        .id;
    let mut copy = pins.svn.pins.lineage.child_copy.clone();
    copy.inspected_revision = 7;
    f.db.record_pair_refresh_svn_intent(id).unwrap();
    f.db.record_pair_refresh_svn_receipt(
        id,
        SvnRefreshPublicationReceipt {
            operation_id: id,
            revision: 7,
            child_url: pins.svn.pins.lineage.pins.child_url.clone(),
            child_copy: copy,
            git_tree: pins.git.tree_sha.clone(),
            mergeinfo: pins.svn.mergeinfo.clone(),
            properties: pins.svn.result_properties.clone(),
        },
    )
    .unwrap();
    f.db.record_pair_refresh_git_intent(id).unwrap();
    f.db.record_pair_refresh_git_receipt(id, &pins.git.prepared_sha)
        .unwrap();
    assert_eq!(
        f.db.finalize_pair_refresh(id).unwrap().phase,
        PairRefreshPhase::Completed
    );
    pins
}

fn child_checkpoints(f: &job::Fixture, sha: &str, revision: i64) {
    f.db.conn()
        .execute(
            "UPDATE repositories SET last_git_sha=?1,last_svn_rev=?2 WHERE id='child'",
            rusqlite::params![sha, revision],
        )
        .unwrap();
    f.db.set_state("last_svn_rev_child", &revision.to_string())
        .unwrap();
    reposync_core::echo_receipt_scope::write_scoped_last_git_sha_kv(
        &f.db.conn(),
        "child",
        sha,
        "t",
    )
    .unwrap();
}

#[test]
fn absent_completed_receipt_only_requests_independent_first_copy_proof() {
    let f = fixture();
    let configuration =
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap();
    let before = rows(&f);
    assert!(f
        .db
        .capture_completed_pair_refresh_baseline(&configuration)
        .unwrap()
        .is_none());
    assert_eq!(rows(&f), before);
    assert!(f.no_hold(None));
}

#[test]
fn exact_completed_baseline_survives_reopen_parent_advance_and_credential_rotation() {
    let f = fixture();
    let pins = complete(&f);
    f.db.conn()
        .execute(
            "UPDATE repositories SET last_git_sha=?1,last_svn_rev=9 WHERE id='parent'",
            [&"9".repeat(40)],
        )
        .unwrap();
    f.db.set_state("last_svn_rev_parent", "9").unwrap();
    reposync_core::echo_receipt_scope::write_scoped_last_git_sha_kv(
        &f.db.conn(),
        "parent",
        &"9".repeat(40),
        "t",
    )
    .unwrap();
    f.db.set_state(
        "secret_git_token_parent",
        "disposable-rotated-baseline-token",
    )
    .unwrap();
    let db = reposync_core::db::Database::new(f.dir.path().join("reposync.db")).unwrap();
    let configuration = db
        .capture_pair_refresh_execution_configuration("child")
        .unwrap();
    let before = rows(&f);
    let selected = db
        .capture_completed_pair_refresh_baseline(&configuration)
        .unwrap()
        .unwrap();
    assert_eq!(selected.id, pins.git.operation_id);
    assert_eq!(selected.pins.parent.last_git_sha, "b".repeat(40));
    assert_eq!(selected.pins.parent.last_svn_rev, 4);
    assert_eq!(selected.svn_receipt.as_ref().unwrap().revision, 7);
    assert_eq!(
        selected.svn_receipt.as_ref().unwrap().mergeinfo,
        "/trunk:3-4"
    );
    assert_ne!(
        selected.pins.credential_context_digest,
        configuration.pin().credential_context_digest
    );
    assert_eq!(rows(&f), before);
    assert!(f.no_hold(None));
}

#[test]
fn partial_or_changed_child_checkpoints_never_fall_back_to_the_initial_copy() {
    for change in ["git", "svn", "both"] {
        let f = fixture();
        let pins = complete(&f);
        let changed_sha = "f".repeat(40);
        child_checkpoints(
            &f,
            if change == "svn" {
                &pins.git.prepared_sha
            } else {
                &changed_sha
            },
            if change == "git" { 7 } else { 9 },
        );
        let configuration =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let before = rows(&f);
        assert!(
            f.db.capture_completed_pair_refresh_baseline(&configuration)
                .is_err(),
            "{change}"
        );
        assert_eq!(rows(&f), before);
        assert_eq!(
            f.db.pair_refresh_operation(pins.git.operation_id)
                .unwrap()
                .phase,
            PairRefreshPhase::Completed
        );
        assert!(f.no_hold(None));
    }
}

#[test]
fn registration_or_echo_drift_cannot_reuse_a_completed_baseline() {
    for id in ["child", "parent"] {
        for change in ["configuration", "generation"] {
            let f = fixture();
            let pins = complete(&f);
            if change == "configuration" {
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET git_branch=git_branch||'-changed' WHERE id=?1",
                        [id],
                    )
                    .unwrap();
            } else {
                reposync_core::echo_receipt_scope::bump_repo_echo_generation(&f.db, id).unwrap();
            }
            let configuration =
                f.db.capture_pair_refresh_execution_configuration("child")
                    .unwrap();
            let before = rows(&f);
            assert!(
                f.db.capture_completed_pair_refresh_baseline(&configuration)
                    .is_err(),
                "{id}/{change}"
            );
            assert_eq!(rows(&f), before);
            assert_eq!(
                f.db.pair_refresh_operation(pins.git.operation_id)
                    .unwrap()
                    .phase,
                PairRefreshPhase::Completed
            );
            assert!(f.no_hold(None));
        }
    }
}

#[test]
fn stale_configuration_and_active_job_refuse_without_new_writes() {
    for change in ["credential", "held"] {
        let f = fixture();
        let configuration =
            f.db.capture_pair_refresh_execution_configuration("child")
                .unwrap();
        let busy = f.busy();
        if change == "credential" {
            f.db.set_state("secret_git_token_parent", "disposable-new-baseline-token")
                .unwrap();
        } else {
            let mut pins = f.pins();
            pins.credential_context_digest = configuration.pin().credential_context_digest.clone();
            f.db.admit_pair_refresh_with_registered_sources(
                "admin",
                "first",
                &pins.approval_digest().unwrap(),
                pins,
                &busy,
            )
            .unwrap();
        }
        let before = rows(&f);
        assert!(
            f.db.capture_completed_pair_refresh_baseline(&configuration)
                .is_err(),
            "{change}"
        );
        assert_eq!(rows(&f), before);
        assert_eq!(f.no_hold(None), change == "credential");
    }
}

#[test]
fn unknown_journal_evidence_is_not_treated_as_an_absent_baseline() {
    let f = fixture();
    let configuration =
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap();
    f.db.set_state("pair_refresh_execution_v99:unknown", "retained")
        .unwrap();
    let before = rows(&f);
    assert!(f
        .db
        .capture_completed_pair_refresh_baseline(&configuration)
        .is_err());
    assert_eq!(rows(&f), before);
}
