//! Production journal-admission boundaries. Full writer-entrance and remote
//! publication integration remains a separate requirement before API enablement.

#[path = "fixtures/pair_refresh_job.rs"]
mod job;

use reposync_core::db::git_push_operations::GitPushIntent;
use reposync_core::db::svn_commit_operations::SvnCommitIntent;
use reposync_core::db::team_cycle_mapping_operations::{
    TeamCycleMappingDirection, TeamCycleMappingIntent, TeamCycleMappingOutcome,
};
use reposync_core::db::Database;
use reposync_core::errors::DatabaseError;

const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NEW: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TREE: &str = "cccccccccccccccccccccccccccccccccccccccc";

fn svn_intent(id: &str) -> SvnCommitIntent<'_> {
    SvnCommitIntent {
        repo_id: id,
        initiator_id: "writer",
        request_id: "other",
        target_fingerprint: "fp",
        source_git_sha: NEW,
        source_git_parent: Some(OLD),
        source_git_tree: TREE,
        target_svn_uuid: "svn-repository",
        target_svn_path: "https://svn.example/repo/trunk",
        target_svn_root_url: "https://svn.example/repo",
        target_svn_branch_path: "/trunk",
        pre_write_svn_rev: 4,
        pre_write_svn_tree: TREE,
        projection: "{}",
        intended_changed_paths: vec![],
        intended_svn_tree: TREE,
        author: "writer",
        source_message: "message",
    }
}

fn git_intent(id: &str) -> GitPushIntent<'_> {
    GitPushIntent {
        repo_id: id,
        initiator_id: "writer",
        request_id: "other",
        target_fingerprint: "fp",
        source_svn_rev: 4,
        source_svn_author: "writer",
        source_svn_message: "message",
        pre_push_git_remote: "origin",
        pre_push_git_branch: "feature",
        pre_push_git_sha: OLD,
        pre_push_git_tree: Some(TREE),
        intended_local_git_sha: NEW,
        intended_local_git_parent: Some(OLD),
        intended_local_git_tree: TREE,
    }
}

fn team_intent(id: &str) -> TeamCycleMappingIntent<'_> {
    TeamCycleMappingIntent {
        repo_id: id,
        initiator_id: "writer",
        request_id: "other",
        target_fingerprint: "fp",
        direction: TeamCycleMappingDirection::GitToSvn,
        outcome: TeamCycleMappingOutcome::GitNoSvnDelta,
        source_svn_rev: None,
        source_git_sha: Some(NEW),
        pre_write_svn_rev: 4,
        pre_write_git_sha: OLD,
        projection: "{}",
        intended_target_proof: None,
    }
}

fn refused<T>(result: Result<T, DatabaseError>) {
    match result {
        Ok(_) => panic!("another writer was admitted through a durable refresh hold"),
        Err(error) => assert!(
            error.to_string().contains("durable pair refresh owns"),
            "{error}"
        ),
    }
}

fn snapshot(db: &Database) -> Vec<(String, String)> {
    db.conn()
        .prepare("SELECT key,value FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn six_production_admissions_refuse_parent_and_child_without_mutating_any_journal() {
    let f = job::Fixture::new();
    let guard = f.busy();
    let operation = f.admit(f.pins(), &guard);
    let before = snapshot(&f.db);
    for id in ["child", "parent"] {
        refused(f.db.create_import_operation(id, "writer", "other", "fp"));
        refused(f.db.begin_git_to_svn_commit(svn_intent(id)));
        refused(f.db.begin_svn_to_git_push(git_intent(id)));
        refused(f.db.begin_team_cycle_mapping(team_intent(id)));
        refused(f.db.create_late_pair_publish_operation(
            id,
            "writer",
            "other",
            "fp",
            "new-feature",
            "branches/new-feature",
            NEW,
            OLD,
            2,
            2,
            "late_pair_publish_v1",
        ));
        refused(f.db.prepare_managed_remove(id, "writer", "other"));
        assert_eq!(snapshot(&f.db), before);
    }
    assert!(f.db.pair_refresh_operation(operation).is_ok());
    assert!(!f.no_hold(Some("parent")) && !f.no_hold(Some("child")));
}

#[test]
fn registered_unrelated_writer_can_still_enroll_its_own_import() {
    let f = job::Fixture::new();
    let guard = f.busy();
    f.admit(f.pins(), &guard);
    assert!(f.no_hold(Some("unrelated")));
    let unrelated =
        f.db.create_import_operation("unrelated", "writer", "other", "fp")
            .unwrap();
    assert_eq!(unrelated.repo_id, "unrelated");
    assert_eq!(
        f.db.active_import_operation("unrelated")
            .unwrap()
            .unwrap()
            .id,
        unrelated.id
    );
    assert!(!f.no_hold(Some("parent")) && !f.no_hold(Some("child")));
}

#[test]
fn personal_journal_scope_is_global_for_refresh_exclusion() {
    let f = job::Fixture::new();
    let guard = f.busy();
    f.admit(f.pins(), &guard);
    let id = reposync_core::db::personal_scope::PERSONAL_SCOPE_KEY;
    refused(f.db.create_import_operation(id, "writer", "other", "fp"));
    refused(f.db.begin_git_to_svn_commit(svn_intent(id)));
    refused(f.db.begin_svn_to_git_push(git_intent(id)));
    refused(f.db.begin_team_cycle_mapping(team_intent(id)));
    assert!(f.db.active_import_operation(id).unwrap().is_none());
    assert!(f.db.active_git_push_operation(id).unwrap().is_none());
    assert!(f.db.active_svn_commit_operation(id).unwrap().is_none());
}

#[test]
fn database_reopen_and_dropped_process_slots_do_not_allow_other_writer_admission() {
    let f = job::Fixture::new();
    let guard = f.busy();
    f.admit(f.pins(), &guard);
    drop(guard);
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    refused(reopened.create_import_operation("child", "writer", "other", "fp"));
    refused(reopened.begin_git_to_svn_commit(svn_intent("parent")));
    refused(reopened.begin_svn_to_git_push(git_intent("child")));
    refused(reopened.begin_team_cycle_mapping(team_intent("parent")));
    refused(reopened.prepare_managed_remove("child", "writer", "other"));
}

#[test]
fn corrupt_refresh_evidence_blocks_all_six_admissions_even_on_unrelated_scope() {
    let f = job::Fixture::new();
    f.db.set_state("pair_refresh_execution_v99:hold:unknown", "{}")
        .unwrap();
    let results = [
        f.db.create_import_operation("unrelated", "writer", "other", "fp")
            .map(|_| ()),
        f.db.begin_git_to_svn_commit(svn_intent("unrelated"))
            .map(|_| ()),
        f.db.begin_svn_to_git_push(git_intent("unrelated"))
            .map(|_| ()),
        f.db.begin_team_cycle_mapping(team_intent("unrelated"))
            .map(|_| ()),
        f.db.create_late_pair_publish_operation(
            "unrelated",
            "writer",
            "other",
            "fp",
            "new-feature",
            "branches/new-feature",
            NEW,
            OLD,
            2,
            2,
            "late_pair_publish_v1",
        )
        .map(|_| ()),
        f.db.prepare_managed_remove("unrelated", "writer", "other")
            .map(|_| ()),
    ];
    for result in results {
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unknown journal version"));
    }
    assert_eq!(
        snapshot(&f.db)
            .iter()
            .filter(|(key, _)| key.contains(":active:"))
            .count(),
        0
    );
}

#[test]
fn concurrent_import_and_refresh_admission_on_two_connections_have_one_owner() {
    let f = job::Fixture::new();
    let guard = f.busy();
    let pins = f.pins();
    let second = Database::new(f.dir.path().join("reposync.db")).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let other_barrier = barrier.clone();
    let other = std::thread::spawn(move || {
        other_barrier.wait();
        second.create_import_operation("child", "writer", "other", "fp")
    });
    barrier.wait();
    let own = f.db.admit_pair_refresh(
        "admin",
        "request",
        &pins.approval_digest().unwrap(),
        pins,
        &guard,
    );
    let other = other.join().unwrap();
    assert_eq!(usize::from(own.is_ok()) + usize::from(other.is_ok()), 1);
    let has_import = f.db.active_import_operation("child").unwrap().is_some();
    let refresh_holds = !f.no_hold(Some("child"));
    assert_ne!(has_import, refresh_holds);
    let holds = snapshot(&f.db)
        .into_iter()
        .filter(|(key, _)| key.starts_with("pair_refresh_execution_v2:hold:"))
        .count();
    assert_eq!(holds, if refresh_holds { 2 } else { 0 });
}

#[test]
fn resumed_journal_mutations_and_legacy_disable_refuse_held_scopes() {
    use reposync_core::db::import_operations::ImportOperationState;
    let f = job::Fixture::new();
    let guard = f.busy();
    f.admit(f.pins(), &guard);
    let before = snapshot(&f.db);
    for id in ["parent", "child"] {
        refused(f.db.start_import_operation(id, "previous"));
        refused(f.db.finish_import_operation(
            id,
            "previous",
            ImportOperationState::ReconciliationRequired,
            "uncertain",
        ));
        refused(f.db.complete_import_operation(id, "previous", 7, NEW));
        refused(f.db.hold_svn_to_git_reconciliation(id, "previous", "uncertain"));
        refused(f.db.authorize_svn_to_git_resume(id, "previous", "fp"));
        refused(f.db.confirm_svn_to_git_push(id, "previous", NEW, TREE));
        refused(f.db.hold_git_to_svn_reconciliation(id, "previous", "uncertain"));
        refused(f.db.authorize_git_to_svn_resume(id, "previous", "fp"));
        refused(f.db.confirm_git_to_svn_commit(id, "previous", 7, TREE));
        refused(f.db.hold_team_cycle_mapping_reconciliation(id, "previous", "uncertain"));
        refused(f.db.finalize_late_pair_publish_operation(id, "previous"));
        refused(f.db.legacy_disable_repository(id));
        assert!(f.db.get_repository(id).unwrap().unwrap().enabled);
        assert_eq!(snapshot(&f.db), before);
    }
}
