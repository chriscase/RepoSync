//! Storage-boundary adversarial controls. Synthetic prepared/observed evidence
//! exercises journal invariants only; real remote proof belongs to the separate
//! Git/SVN suites and the still-required production execution/recovery suite.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use reposync_core::busy::{self, PairBusyGuard};
use reposync_core::data_dir::DataDirOwner;
use reposync_core::db::pair_refresh_operations::{
    capture_refresh_registration, require_no_pair_refresh_hold, PairRefreshPhase, RefreshJobPins,
};
use reposync_core::db::Database;
use reposync_core::pair_refresh::lineage::{
    SvnCopyProof, SvnLineagePins, SvnRefreshLineagePins, SvnRefreshLineageProof,
};
use reposync_core::pair_refresh::preparation::{CleanChildGitPins, PreparedCleanChildGit};
use reposync_core::pair_refresh::publication::PinnedPushPolicy;
use reposync_core::pair_refresh::svn_observation::SvnRefreshPublicationReceipt;
use reposync_core::pair_refresh::svn_preparation::{CleanChildSvnPins, PreparedCleanChildSvn};
use rusqlite::params;
use uuid::Uuid;

static SERIAL: Mutex<()> = Mutex::new(());

struct Fixture {
    db: Database,
    _owner: DataDirOwner,
    dir: tempfile::TempDir,
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let serial = SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (db, owner) = Database::open_with_exclusive_owner(dir.path()).unwrap();
        seed(&db);
        Self {
            db,
            _owner: owner,
            dir,
            _serial: serial,
        }
    }
    fn pins(&self) -> RefreshJobPins {
        pins(&self.db, self.dir.path())
    }
    fn busy(&self) -> PairBusyGuard {
        busy::try_acquire_pair("parent", "child").unwrap()
    }
    fn admit(&self, pins: RefreshJobPins, guard: &PairBusyGuard) -> Uuid {
        let digest = pins.approval_digest().unwrap();
        self.db
            .admit_pair_refresh("admin", "request", &digest, pins, guard)
            .unwrap()
            .id
    }
    fn no_hold(&self, repo: Option<&str>) -> bool {
        require_no_pair_refresh_hold(&self.db.conn(), repo).is_ok()
    }
}

fn seed(db: &Database) {
    for (id, parent, sha, rev, branch) in [
        ("parent", None, "b".repeat(40), 4, "main"),
        ("child", Some("parent"), "a".repeat(40), 3, "feature"),
        ("unrelated", None, "e".repeat(40), 2, "other"),
    ] {
        db.conn().execute("INSERT INTO repositories(id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors,parent_id,scope_uuid) VALUES(?1,?1,'https://svn.example/repo','trunk','','gitea','https://git.example','repo',?2,'team',5,0,0,1,'t','t',?3,?4,'idle',0,0,?5,?6)", params![id, branch, rev, sha, parent, Uuid::new_v4().to_string()]).unwrap();
        db.set_state(&format!("last_svn_rev_{id}"), &rev.to_string())
            .unwrap();
        reposync_core::echo_receipt_scope::write_scoped_last_git_sha_kv(&db.conn(), id, &sha, "t")
            .unwrap();
    }
}

fn pins(db: &Database, root: &std::path::Path) -> RefreshJobPins {
    let operation_id = Uuid::new_v4();
    let child = capture_refresh_registration(&db.conn(), "child").unwrap();
    let parent = capture_refresh_registration(&db.conn(), "parent").unwrap();
    let svn_pins = SvnRefreshLineagePins {
        child: SvnLineagePins {
            repository_uuid: "svn-repository".into(),
            child_path: "/branches/f".into(),
            parent_path: "/trunk".into(),
            child_revision: 3,
            child_mapping_revision: 3,
            parent_baseline_revision: 2,
        },
        repository_root_url: "https://svn.example/repo".into(),
        child_url: "https://svn.example/repo/branches/f".into(),
        parent_url: "https://svn.example/repo/trunk".into(),
        parent_revision: 4,
    };
    let copy = SvnCopyProof {
        repository_uuid: "svn-repository".into(),
        child_path: "/branches/f".into(),
        created_revision: 3,
        parent_path: "/trunk".into(),
        copy_from_revision: 2,
        inspected_revision: 3,
    };
    let properties = BTreeMap::from([(
        "".into(),
        BTreeMap::from([("svn:mergeinfo".into(), "/trunk:3-4".into())]),
    )]);
    RefreshJobPins {
        git: PreparedCleanChildGit {
            pins: CleanChildGitPins {
                child_tip: child.last_git_sha.clone(),
                parent_tip: parent.last_git_sha.clone(),
                parent_baseline_tip: "a".repeat(40),
            },
            operation_id,
            prepared_at: 1_790_000_000,
            prepared_sha: "d".repeat(40),
            tree_sha: "c".repeat(40),
            parent_advance_commits: 1,
        },
        svn: PreparedCleanChildSvn {
            operation_id,
            pins: CleanChildSvnPins {
                lineage: SvnRefreshLineageProof {
                    pins: svn_pins,
                    child_copy: copy,
                    parent_originating_revision: 1,
                },
                baseline_git_tree: "f".repeat(40),
                parent_git_tree: "c".repeat(40),
                previous_mergeinfo: None,
            },
            working_copy: root.join("prepared"),
            mergeinfo: "/trunk:3-4".into(),
            result_git_tree: "c".repeat(40),
            result_properties: properties,
        },
        child,
        parent,
        git_remote_url: "https://git.example/owner/repo.git".into(),
        git_branch: "feature".into(),
        transport_policy: PinnedPushPolicy::SmartHttpV1,
        credential_context_digest: "e".repeat(64),
    }
}

fn receipt(pins: &RefreshJobPins) -> SvnRefreshPublicationReceipt {
    let mut child_copy = pins.svn.pins.lineage.child_copy.clone();
    child_copy.inspected_revision = 7;
    SvnRefreshPublicationReceipt {
        operation_id: pins.git.operation_id,
        revision: 7,
        child_url: pins.svn.pins.lineage.pins.child_url.clone(),
        child_copy,
        git_tree: pins.git.tree_sha.clone(),
        mergeinfo: pins.svn.mergeinfo.clone(),
        properties: pins.svn.result_properties.clone(),
    }
}

fn confirm_both(f: &Fixture, pins: &RefreshJobPins, id: Uuid) {
    f.db.record_pair_refresh_svn_intent(id).unwrap();
    f.db.record_pair_refresh_svn_receipt(id, receipt(pins))
        .unwrap();
    f.db.record_pair_refresh_git_intent(id).unwrap();
    f.db.record_pair_refresh_git_receipt(id, &pins.git.prepared_sha)
        .unwrap();
}

#[test]
fn requires_enrolled_writer_and_the_matching_two_busy_slots() {
    let f = Fixture::new();
    let pins = f.pins();
    let wrong = busy::try_acquire_pair("x", "y").unwrap();
    assert!(f
        .db
        .admit_pair_refresh(
            "admin",
            "request",
            &pins.approval_digest().unwrap(),
            pins.clone(),
            &wrong
        )
        .is_err());
    drop(wrong);
    let no_lease = Database::in_memory().unwrap();
    no_lease.initialize().unwrap();
    seed(&no_lease);
    let pins = pins_for(&no_lease, f.dir.path());
    let guard = f.busy();
    assert!(no_lease
        .admit_pair_refresh(
            "admin",
            "request",
            &pins.approval_digest().unwrap(),
            pins,
            &guard
        )
        .is_err());
    assert!(f.no_hold(None));
    assert!(require_no_pair_refresh_hold(&no_lease.conn(), None).is_ok());
}

fn pins_for(db: &Database, root: &std::path::Path) -> RefreshJobPins {
    pins(db, root)
}

#[test]
fn capture_never_invents_missing_registration_identity() {
    let f = Fixture::new();
    f.db.conn()
        .execute(
            "UPDATE repositories SET scope_uuid=NULL WHERE id='child'",
            [],
        )
        .unwrap();
    assert!(capture_refresh_registration(&f.db.conn(), "child").is_err());
    let scope: Option<String> =
        f.db.conn()
            .query_row(
                "SELECT scope_uuid FROM repositories WHERE id='child'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(scope, None);
    assert!(capture_refresh_registration(&f.db.conn(), "missing").is_err());
}

#[test]
fn busy_contention_releases_partial_slot_without_releasing_the_contender() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let contender = busy::try_acquire("z-parent").unwrap();
    assert!(busy::try_acquire_pair("z-parent", "a-child").is_none());
    assert!(!busy::is_busy("a-child"));
    assert!(busy::is_busy("z-parent"));
    drop(contender);
    let guard = busy::try_acquire_pair("z-parent", "a-child").unwrap();
    assert!(busy::is_busy("z-parent") && busy::is_busy("a-child"));
    drop(guard);
    assert!(!busy::is_busy("z-parent") && !busy::is_busy("a-child"));
    assert!(busy::try_acquire_pair("a", "a").is_none());
}

#[test]
fn durable_holds_survive_busy_drop_and_database_reopen() {
    let f = Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    let id = f.admit(pins.clone(), &guard);
    drop(guard);
    assert!(!busy::is_busy("child") && !busy::is_busy("parent"));
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")) && !f.no_hold(None));
    assert!(f.no_hold(Some("unrelated")));
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    reopened.initialize().unwrap();
    assert_eq!(reopened.pair_refresh_operation(id).unwrap().pins, pins);
    assert!(require_no_pair_refresh_hold(&reopened.conn(), Some("child")).is_err());
    assert!(require_no_pair_refresh_hold(&reopened.conn(), Some("parent")).is_err());
}

#[test]
fn one_request_and_both_scopes_are_owned_atomically() {
    let f = Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    let id = f.admit(pins.clone(), &guard);
    assert_eq!(f.admit(pins.clone(), &guard), id);
    let mut different = pins.clone();
    different.credential_context_digest = "f".repeat(64);
    assert!(f
        .db
        .admit_pair_refresh(
            "admin",
            "request",
            &different.approval_digest().unwrap(),
            different,
            &guard
        )
        .is_err());
    assert!(f
        .db
        .admit_pair_refresh(
            "admin",
            "new-request",
            &pins.approval_digest().unwrap(),
            pins.clone(),
            &guard
        )
        .is_err());
    let count: i64 =
        f.db.conn()
            .query_row(
                "SELECT count(*) FROM kv_state WHERE key GLOB 'pair_refresh_execution_v2:hold:*'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 2);
    assert_eq!(
        f.db.pair_refresh_request("admin", "request", &pins.child.scope_uuid)
            .unwrap()
            .unwrap()
            .id,
        id
    );
    assert!(f
        .db
        .pair_refresh_request("someone-else", "request", &pins.child.scope_uuid)
        .unwrap()
        .is_none());
}

#[test]
fn current_configuration_identity_counter_and_checkpoint_are_required() {
    let f = Fixture::new();
    let guard = f.busy();
    let original = f.pins();
    let changes = [
        ("UPDATE repositories SET git_branch='drift' WHERE id='child'", "UPDATE repositories SET git_branch='feature' WHERE id='child'"),
        ("UPDATE repositories SET scope_uuid='b703cfe4-eaf7-4d15-98ea-c1b9f5bfd345' WHERE id='child'", ""),
        ("UPDATE repositories SET last_svn_rev=8 WHERE id='parent'", "UPDATE repositories SET last_svn_rev=4 WHERE id='parent'"),
        ("UPDATE repositories SET parent_id='unrelated' WHERE id='child'", "UPDATE repositories SET parent_id='parent' WHERE id='child'"),
    ];
    for (change, restore) in changes {
        f.db.conn().execute(change, []).unwrap();
        assert!(f
            .db
            .admit_pair_refresh(
                "admin",
                "request",
                &original.approval_digest().unwrap(),
                original.clone(),
                &guard
            )
            .is_err());
        assert!(f.no_hold(None));
        if restore.is_empty() {
            f.db.conn()
                .execute(
                    "UPDATE repositories SET scope_uuid=?1 WHERE id='child'",
                    [&original.child.scope_uuid],
                )
                .unwrap();
        } else {
            f.db.conn().execute(restore, []).unwrap();
        }
    }
    let echo_key = reposync_core::db::repo_scope_identity::repo_echo_generation_kv_key(
        &original.child.scope_uuid,
    );
    f.db.set_state(&echo_key, "2").unwrap();
    assert!(f
        .db
        .admit_pair_refresh(
            "admin",
            "request",
            &original.approval_digest().unwrap(),
            original,
            &guard
        )
        .is_err());
    assert!(f.no_hold(None));
}

#[test]
fn every_existing_operation_pointer_refuses_admission_on_either_repository() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    for prefix in [
        "import_operation_v1",
        "git_to_svn_commit_v1",
        "svn_to_git_push_v1",
        "team_cycle_mapping_v1",
        "late_pair_publish_v1",
        "managed_remove_v1",
    ] {
        for repo in ["child", "parent"] {
            let key = format!("{prefix}:active:{repo}");
            f.db.set_state(&key, "missing-or-corrupt-operation")
                .unwrap();
            assert!(
                f.db.admit_pair_refresh(
                    "admin",
                    "request",
                    &pins.approval_digest().unwrap(),
                    pins.clone(),
                    &guard
                )
                .is_err(),
                "{key}"
            );
            assert!(f.no_hold(None));
            f.db.conn()
                .execute("DELETE FROM kv_state WHERE key=?1", [&key])
                .unwrap();
        }
    }
}

#[test]
fn unknown_or_corrupt_hold_evidence_blocks_even_unrelated_and_global_writers() {
    let f = Fixture::new();
    for key in [
        "pair_refresh_execution_v99:hold:anything",
        "pair_refresh_execution_v2:hold:bad",
        "pair_refresh_execution_v2:document:bad",
    ] {
        f.db.set_state(key, "{}").unwrap();
        assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("unrelated")) && !f.no_hold(None));
        f.db.conn()
            .execute("DELETE FROM kv_state WHERE key=?1", [key])
            .unwrap();
    }
    let guard = f.busy();
    let pins = f.pins();
    let id = f.admit(pins.clone(), &guard);
    let hold = format!("pair_refresh_execution_v2:hold:{}", pins.child.scope_uuid);
    f.db.conn()
        .execute("DELETE FROM kv_state WHERE key=?1", [&hold])
        .unwrap();
    assert!(!f.no_hold(Some("unrelated")));
    assert!(f.db.pair_refresh_operation(id).is_err());
    assert!(f.db.cancel_unpublished_pair_refresh(id).is_err());
}

#[test]
fn noncanonical_and_unknown_document_fields_never_drop_evidence() {
    let f = Fixture::new();
    let guard = f.busy();
    let id = f.admit(f.pins(), &guard);
    let key = format!("pair_refresh_execution_v2:document:{id}");
    let raw = f.db.get_state(&key).unwrap().unwrap();
    for corrupt in [
        format!(" {raw}"),
        raw.replacen("\"pins\":{", "\"pins\":{\"unknown_proof\":true,", 1),
        raw.replacen("\"version\":2", "\"version\":2,\"version\":2", 1),
    ] {
        f.db.set_state(&key, &corrupt).unwrap();
        assert!(f.db.pair_refresh_operation(id).is_err());
        assert!(!f.no_hold(None));
    }
    f.db.set_state(&key, &raw).unwrap();
    assert_eq!(
        f.db.pair_refresh_operation(id).unwrap().phase,
        PairRefreshPhase::Prepared
    );
}

#[test]
fn intent_phases_refuse_blind_retry_and_cancel_after_possible_publication() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let id = f.admit(pins.clone(), &guard);
    assert!(f.db.record_pair_refresh_git_intent(id).is_err());
    assert!(f.db.finalize_pair_refresh(id).is_err());
    f.db.record_pair_refresh_svn_intent(id).unwrap();
    assert!(f.db.record_pair_refresh_svn_intent(id).is_err());
    assert!(f.db.cancel_unpublished_pair_refresh(id).is_err());
    assert!(f
        .db
        .record_pair_refresh_git_receipt(id, &pins.git.prepared_sha)
        .is_err());
    assert_eq!(
        f.db.pair_refresh_operation(id).unwrap().phase,
        PairRefreshPhase::SvnIntent
    );
    assert!(!f.no_hold(None));
}

#[test]
fn receipts_must_match_pinned_operation_revision_tree_properties_and_git_sha() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let id = f.admit(pins.clone(), &guard);
    f.db.record_pair_refresh_svn_intent(id).unwrap();
    let original = receipt(&pins);
    for n in 0..6 {
        let mut wrong = original.clone();
        match n {
            0 => wrong.operation_id = Uuid::new_v4(),
            1 => {
                wrong.revision = 3;
                wrong.child_copy.inspected_revision = 3;
            }
            2 => wrong.git_tree = "f".repeat(40),
            3 => wrong.child_copy.copy_from_revision = 1,
            4 => wrong.mergeinfo = "/other:3-4".into(),
            _ => {
                wrong.properties.clear();
            }
        }
        assert!(f.db.record_pair_refresh_svn_receipt(id, wrong).is_err());
        assert_eq!(
            f.db.pair_refresh_operation(id).unwrap().phase,
            PairRefreshPhase::SvnIntent
        );
    }
    f.db.record_pair_refresh_svn_receipt(id, original).unwrap();
    f.db.record_pair_refresh_git_intent(id).unwrap();
    assert!(f
        .db
        .record_pair_refresh_git_receipt(id, &pins.child.last_git_sha)
        .is_err());
    assert!(f
        .db
        .record_pair_refresh_git_receipt(id, &pins.parent.last_git_sha)
        .is_err());
    f.db.record_pair_refresh_git_receipt(id, &pins.git.prepared_sha)
        .unwrap();
    assert!(!f.no_hold(None));
}

#[test]
fn reconciliation_retains_phase_and_both_holds_after_identity_drift() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let id = f.admit(pins.clone(), &guard);
    f.db.record_pair_refresh_svn_intent(id).unwrap();
    f.db.conn()
        .execute(
            "UPDATE repositories SET scope_uuid=?1 WHERE id='child'",
            [Uuid::new_v4().to_string()],
        )
        .unwrap();
    assert!(f
        .db
        .record_pair_refresh_svn_receipt(id, receipt(&pins))
        .is_err());
    let held =
        f.db.hold_pair_refresh_reconciliation(id, "remote response was lost; registration drifted")
            .unwrap();
    assert_eq!(held.phase, PairRefreshPhase::SvnIntent);
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
    assert!(f.db.cancel_unpublished_pair_refresh(id).is_err());
    assert!(f.db.record_pair_refresh_svn_intent(id).is_err());
    assert!(f.db.finalize_pair_refresh(id).is_err());
}

#[test]
fn unpublished_cancellation_releases_only_its_two_holds_and_retains_request() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let id = f.admit(pins.clone(), &guard);
    assert_eq!(
        f.db.cancel_unpublished_pair_refresh(id).unwrap().phase,
        PairRefreshPhase::CancelledBeforePublication
    );
    assert!(f.no_hold(None));
    assert_eq!(f.admit(pins, &guard), id);
    assert!(f.db.record_pair_refresh_svn_intent(id).is_err());
}

#[test]
fn checkpoint_failure_rolls_back_mapping_cursors_document_and_hold_release() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let id = f.admit(pins.clone(), &guard);
    confirm_both(&f, &pins, id);
    f.db.conn().execute_batch("CREATE TRIGGER deny_parent_hold_release BEFORE DELETE ON kv_state WHEN OLD.key GLOB 'pair_refresh_execution_v2:hold:*' BEGIN SELECT RAISE(ABORT,'hold release failure'); END;").unwrap();
    assert!(f.db.finalize_pair_refresh(id).is_err());
    assert_eq!(
        capture_refresh_registration(&f.db.conn(), "child").unwrap(),
        pins.child
    );
    assert_eq!(
        f.db.pair_refresh_operation(id).unwrap().phase,
        PairRefreshPhase::GitObserved
    );
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
    let records: i64 =
        f.db.conn()
            .query_row("SELECT count(*) FROM sync_records", [], |row| row.get(0))
            .unwrap();
    assert_eq!(records, 0);
    assert_eq!(
        f.db.get_state("last_svn_rev_child").unwrap(),
        Some("3".into())
    );
    assert_eq!(
        f.db.get_state(
            &reposync_core::db::repo_scope_identity::last_git_sha_kv_key(&pins.child.scope_uuid)
        )
        .unwrap(),
        Some(pins.child.last_git_sha.clone())
    );
    f.db.conn()
        .execute_batch("DROP TRIGGER deny_parent_hold_release")
        .unwrap();
    assert_eq!(
        f.db.finalize_pair_refresh(id).unwrap().phase,
        PairRefreshPhase::Completed
    );
}

#[test]
fn completed_checkpoint_is_atomic_idempotent_and_preserves_parent_and_echo_generations() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let id = f.admit(pins.clone(), &guard);
    confirm_both(&f, &pins, id);
    let done = f.db.finalize_pair_refresh(id).unwrap();
    assert_eq!(done.phase, PairRefreshPhase::Completed);
    assert_eq!(f.db.finalize_pair_refresh(id).unwrap(), done);
    assert_eq!(f.admit(pins.clone(), &guard), id);
    assert!(f.no_hold(None));
    assert_eq!(
        capture_refresh_registration(&f.db.conn(), "parent").unwrap(),
        pins.parent
    );
    let child = capture_refresh_registration(&f.db.conn(), "child").unwrap();
    assert_eq!(child.last_svn_rev, 7);
    assert_eq!(child.last_git_sha, pins.git.prepared_sha);
    assert_eq!(child.scope_uuid, pins.child.scope_uuid);
    assert_eq!(child.echo_generation, pins.child.echo_generation);
    assert_eq!(
        f.db.get_state("last_svn_rev_child").unwrap(),
        Some("7".into())
    );
    assert_eq!(
        f.db.get_state(
            &reposync_core::db::repo_scope_identity::last_git_sha_kv_key(&pins.child.scope_uuid)
        )
        .unwrap(),
        Some(pins.git.prepared_sha.clone())
    );
    let records: (i64, String, String, i64, String) =
        f.db.conn()
            .query_row(
                "SELECT count(*),repo_id,direction,svn_rev,status FROM sync_records",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
    assert_eq!(
        records,
        (1, "child".into(), "git_to_svn".into(), 7, "applied".into())
    );
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    assert_eq!(reopened.pair_refresh_operation(id).unwrap(), done);
}

#[test]
fn two_database_connections_cannot_admit_two_jobs_for_one_pair() {
    let f = Fixture::new();
    let guard = std::sync::Arc::new(f.busy());
    let mut pins = f.pins();
    let db_path = f.dir.path().join("reposync.db");
    let second = Database::new(&db_path).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let first_pins = pins.clone();
    let first_guard = guard.clone();
    let first_barrier = barrier.clone();
    let handle = std::thread::spawn(move || {
        first_barrier.wait();
        second.admit_pair_refresh(
            "admin",
            "first",
            &first_pins.approval_digest().unwrap(),
            first_pins,
            &first_guard,
        )
    });
    pins.git.operation_id = Uuid::new_v4();
    pins.svn.operation_id = pins.git.operation_id;
    barrier.wait();
    let own = f.db.admit_pair_refresh(
        "admin",
        "second",
        &pins.approval_digest().unwrap(),
        pins,
        &guard,
    );
    let other = handle.join().unwrap();
    assert_eq!(usize::from(own.is_ok()) + usize::from(other.is_ok()), 1);
    let holds: i64 =
        f.db.conn()
            .query_row(
                "SELECT count(*) FROM kv_state WHERE key GLOB 'pair_refresh_execution_v2:hold:*'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(holds, 2);
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
}

#[test]
fn scoped_checkpoint_drift_refuses_admission_and_completion() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let key = reposync_core::db::repo_scope_identity::last_git_sha_kv_key(&pins.child.scope_uuid);
    f.db.set_state(&key, &"e".repeat(40)).unwrap();
    assert!(f
        .db
        .admit_pair_refresh(
            "admin",
            "request",
            &pins.approval_digest().unwrap(),
            pins.clone(),
            &guard
        )
        .is_err());
    f.db.set_state(&key, &pins.child.last_git_sha).unwrap();
    let id = f.admit(pins.clone(), &guard);
    confirm_both(&f, &pins, id);
    f.db.set_state("last_svn_rev_parent", "99").unwrap();
    assert!(f.db.finalize_pair_refresh(id).is_err());
    assert_eq!(
        f.db.pair_refresh_operation(id).unwrap().phase,
        PairRefreshPhase::GitObserved
    );
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
}

#[test]
fn every_publication_phase_survives_reopened_database_with_both_holds() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let id = f.admit(pins.clone(), &guard);
    for phase in [
        PairRefreshPhase::Prepared,
        PairRefreshPhase::SvnIntent,
        PairRefreshPhase::SvnObserved,
        PairRefreshPhase::GitIntent,
        PairRefreshPhase::GitObserved,
    ] {
        match phase {
            PairRefreshPhase::Prepared => {}
            PairRefreshPhase::SvnIntent => {
                f.db.record_pair_refresh_svn_intent(id).unwrap();
            }
            PairRefreshPhase::SvnObserved => {
                f.db.record_pair_refresh_svn_receipt(id, receipt(&pins))
                    .unwrap();
            }
            PairRefreshPhase::GitIntent => {
                f.db.record_pair_refresh_git_intent(id).unwrap();
            }
            PairRefreshPhase::GitObserved => {
                f.db.record_pair_refresh_git_receipt(id, &pins.git.prepared_sha)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
        assert_eq!(
            reopened.pair_refresh_operation(id).unwrap(),
            f.db.pair_refresh_operation(id).unwrap()
        );
        assert_eq!(reopened.pair_refresh_operation(id).unwrap().phase, phase);
        assert!(require_no_pair_refresh_hold(&reopened.conn(), Some("child")).is_err());
        assert!(require_no_pair_refresh_hold(&reopened.conn(), Some("parent")).is_err());
    }
}

#[test]
fn newer_writer_epoch_refuses_progress_without_releasing_holds() {
    let f = Fixture::new();
    let guard = f.busy();
    let id = f.admit(f.pins(), &guard);
    f.db.conn().execute("UPDATE kv_state SET value=json_set(value,'$.epoch',json_extract(value,'$.epoch')+1) WHERE key='writer_fence_v1:lease'", []).unwrap();
    assert!(f.db.record_pair_refresh_svn_intent(id).is_err());
    assert!(f.db.cancel_unpublished_pair_refresh(id).is_err());
    assert!(f
        .db
        .hold_pair_refresh_reconciliation(id, "stale writer")
        .is_err());
    assert_eq!(
        f.db.pair_refresh_operation(id).unwrap().phase,
        PairRefreshPhase::Prepared
    );
    assert!(!f.no_hold(None));
}

#[test]
fn existing_foreign_correspondence_refuses_completion_without_adoption() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let id = f.admit(pins.clone(), &guard);
    confirm_both(&f, &pins, id);
    f.db.conn().execute("INSERT INTO sync_records(id,repo_id,svn_rev,git_sha,direction,timestamp,synced_at,status) VALUES('foreign','child',7,?1,'svn_to_git','t','t','applied')", [&pins.git.prepared_sha]).unwrap();
    assert!(f.db.finalize_pair_refresh(id).is_err());
    assert_eq!(
        capture_refresh_registration(&f.db.conn(), "child").unwrap(),
        pins.child
    );
    assert_eq!(
        f.db.pair_refresh_operation(id).unwrap().phase,
        PairRefreshPhase::GitObserved
    );
    assert!(!f.no_hold(None));
    let count: i64 =
        f.db.conn()
            .query_row("SELECT count(*) FROM sync_records", [], |row| row.get(0))
            .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn exact_receipts_reconcile_lost_responses_without_authorizing_another_write() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let id = f.admit(pins.clone(), &guard);
    f.db.record_pair_refresh_svn_intent(id).unwrap();
    f.db.hold_pair_refresh_reconciliation(id, "SVN response lost")
        .unwrap();
    let mut wrong = receipt(&pins);
    wrong.operation_id = Uuid::new_v4();
    assert!(f.db.record_pair_refresh_svn_receipt(id, wrong).is_err());
    assert!(f
        .db
        .pair_refresh_operation(id)
        .unwrap()
        .reconciliation_detail
        .is_some());
    assert!(f.db.record_pair_refresh_svn_intent(id).is_err());
    assert!(f.db.cancel_unpublished_pair_refresh(id).is_err());
    let observed =
        f.db.record_pair_refresh_svn_receipt(id, receipt(&pins))
            .unwrap();
    assert_eq!(observed.phase, PairRefreshPhase::SvnObserved);
    assert_eq!(observed.reconciliation_detail, None);
    assert!(!f.no_hold(None));
    f.db.record_pair_refresh_git_intent(id).unwrap();
    f.db.hold_pair_refresh_reconciliation(id, "Git response lost")
        .unwrap();
    assert!(f.db.record_pair_refresh_git_intent(id).is_err());
    assert!(f
        .db
        .record_pair_refresh_git_receipt(id, &pins.parent.last_git_sha)
        .is_err());
    assert!(f
        .db
        .pair_refresh_operation(id)
        .unwrap()
        .reconciliation_detail
        .is_some());
    let observed =
        f.db.record_pair_refresh_git_receipt(id, &pins.git.prepared_sha)
            .unwrap();
    assert_eq!(observed.phase, PairRefreshPhase::GitObserved);
    assert_eq!(observed.reconciliation_detail, None);
    assert!(!f.no_hold(None));
    assert_eq!(
        f.db.finalize_pair_refresh(id).unwrap().phase,
        PairRefreshPhase::Completed
    );
    assert!(f.no_hold(None));
}

#[test]
fn uncertain_latest_operation_without_active_pointer_is_not_completion_proof() {
    use reposync_core::db::late_pair_publish_operations::LatePairPublishState;
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let mut prior =
        f.db.create_late_pair_publish_operation(
            "parent",
            "admin",
            "previous",
            "fingerprint",
            "feature",
            "branches/f",
            &pins.parent.last_git_sha,
            &pins.git.pins.parent_baseline_tip,
            2,
            2,
            "late_pair_publish_v1",
        )
        .unwrap();
    f.db.conn()
        .execute(
            "DELETE FROM kv_state WHERE key='late_pair_publish_v1:active:parent'",
            [],
        )
        .unwrap();
    let key = format!("late_pair_publish_v1:document:{}", prior.id);
    prior.state = LatePairPublishState::ReconciliationRequired;
    f.db.set_state(&key, &serde_json::to_string(&prior).unwrap())
        .unwrap();
    assert!(f
        .db
        .admit_pair_refresh(
            "admin",
            "request",
            &pins.approval_digest().unwrap(),
            pins.clone(),
            &guard
        )
        .is_err());
    assert!(f.no_hold(None));
    prior.state = LatePairPublishState::Completed;
    f.db.set_state(&key, &serde_json::to_string(&prior).unwrap())
        .unwrap();
    let id = f.admit(pins, &guard);
    assert_eq!(
        f.db.pair_refresh_operation(id).unwrap().phase,
        PairRefreshPhase::Prepared
    );
}

#[test]
fn missing_latest_document_never_becomes_a_quiet_operation() {
    let f = Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    for prefix in [
        "import_operation_v1",
        "git_to_svn_commit_v1",
        "svn_to_git_push_v1",
        "team_cycle_mapping_v1",
        "late_pair_publish_v1",
        "managed_remove_v1",
    ] {
        let key = format!("{prefix}:latest:parent");
        f.db.set_state(&key, "missing-document").unwrap();
        assert!(
            f.db.admit_pair_refresh(
                "admin",
                "request",
                &pins.approval_digest().unwrap(),
                pins.clone(),
                &guard
            )
            .is_err(),
            "{key}"
        );
        assert!(f.no_hold(None));
        f.db.conn()
            .execute("DELETE FROM kv_state WHERE key=?1", [&key])
            .unwrap();
    }
}
