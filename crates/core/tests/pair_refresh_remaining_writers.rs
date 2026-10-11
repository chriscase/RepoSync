//! Remaining local writer gates and live admission conflict checks.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;

use reposync_core::db::Database;
use reposync_core::models::Conflict;
use reposync_core::skip_commit::{execute_exact_skip, reason, SkipCommitRequest};

fn request() -> SkipCommitRequest {
    SkipCommitRequest {
        pinned_cursor: "a".repeat(40),
        selected_commits: vec!["b".repeat(40)],
        expected_remote_tip: "b".repeat(40),
        expected_bridge_tip: None,
        reason: "fixture".into(),
    }
}

fn conflict(owner: Option<&str>) -> Conflict {
    let mut conflict = Conflict::new("file.txt".into());
    conflict.repo_id = owner.map(str::to_owned);
    conflict.svn_content = Some("before".into());
    conflict
}

#[test]
fn conflict_insert_and_refresh_refuse_held_or_global_scopes_without_touching_rows() {
    for owner in [
        Some("child"),
        Some("parent"),
        None,
        Some(""),
        Some("__reposync_personal__"),
        Some("personal"),
    ] {
        let f = job::Fixture::new();
        let existing = conflict(owner);
        f.db.insert_conflict(&existing).unwrap();
        let guard = f.busy();
        f.admit(f.pins(), &guard);
        let mut changed = existing.clone();
        changed.svn_content = Some("after".into());
        assert!(f.db.record_detected_conflict(&changed).is_err());
        assert!(f.db.insert_conflict(&conflict(owner)).is_err());
        assert!(f
            .db
            .insert_conflict_entry("new.txt", "content", None, None, None, None, None)
            .is_err());
        assert_eq!(
            f.db.get_conflict_entry(&existing.id).unwrap().svn_content,
            Some("before".into())
        );
        let count: i64 =
            f.db.conn()
                .query_row("SELECT COUNT(*) FROM conflicts", [], |row| row.get(0))
                .unwrap();
        assert_eq!(count, 1);
        assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
    }
}

#[test]
fn unrelated_conflict_detection_remains_available_but_corrupt_evidence_refuses() {
    let f = job::Fixture::new();
    let guard = f.busy();
    f.admit(f.pins(), &guard);
    let mut entry = conflict(Some("unrelated"));
    f.db.insert_conflict(&entry).unwrap();
    entry.svn_content = Some("updated".into());
    assert_eq!(f.db.record_detected_conflict(&entry).unwrap(), entry.id);
    assert_eq!(
        f.db.get_conflict_entry(&entry.id).unwrap().svn_content,
        Some("updated".into())
    );
    f.db.conn().execute("INSERT INTO kv_state(key,value,updated_at) VALUES('pair_refresh_execution_future:document:x','{}','t')", []).unwrap();
    assert!(f.db.insert_conflict(&conflict(Some("unrelated"))).is_err());
    assert!(f.db.record_detected_conflict(&entry).is_err());
}

#[test]
fn skip_refuses_before_any_git_or_cursor_work_when_refresh_or_preparation_owns_scope() {
    let f = job::Fixture::new();
    let guard = f.busy();
    f.admit(f.pins(), &guard);
    let path = f.dir.path().join("not-a-git-repository");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep.txt"), b"unchanged").unwrap();
    for id in ["child", "parent", "__reposync_personal__"] {
        let error =
            execute_exact_skip(&f.db, id, &path, &"b".repeat(40), None, &request()).unwrap_err();
        assert_eq!(error.code, reason::WRITER_HELD);
    }
    assert_eq!(std::fs::read(path.join("keep.txt")).unwrap(), b"unchanged");
    assert!(!path.join(".git").exists());
    let window =
        f.db.begin_ordinary_writer_window(Some("unrelated"))
            .unwrap();
    assert_eq!(
        execute_exact_skip(&f.db, "unrelated", &path, &"b".repeat(40), None, &request())
            .unwrap_err()
            .code,
        reason::WRITER_HELD
    );
    drop(window);
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
}

#[test]
fn failed_unrelated_skip_releases_its_own_preparation_window() {
    let f = job::Fixture::new();
    let guard = f.busy();
    f.admit(f.pins(), &guard);
    let error = execute_exact_skip(
        &f.db,
        "unrelated",
        f.dir.path(),
        &"b".repeat(40),
        None,
        &request(),
    )
    .unwrap_err();
    assert_ne!(error.code, reason::WRITER_HELD);
    drop(
        f.db.begin_ordinary_writer_window(Some("unrelated"))
            .unwrap(),
    );
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
}

#[test]
fn credential_checked_admission_refuses_every_unresolved_owner_and_allows_resolved_rows() {
    for owner in [
        Some("child"),
        Some("parent"),
        None,
        Some(""),
        Some("__reposync_personal__"),
        Some("personal"),
    ] {
        let f = job::Fixture::new();
        f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
        let entry = conflict(owner);
        f.db.insert_conflict(&entry).unwrap();
        let mut pins = f.pins();
        pins.credential_context_digest =
            f.db.resolve_pair_refresh_credentials("child", "parent")
                .unwrap()
                .context_digest()
                .to_owned();
        let guard = f.busy();
        assert!(f
            .db
            .admit_pair_refresh_with_current_credentials(
                "admin",
                "live",
                &pins.approval_digest().unwrap(),
                pins.clone(),
                &guard
            )
            .is_err());
        assert!(f.no_hold(None));
        f.db.resolve_conflict(&entry.id, "resolved", "accept_svn", "admin")
            .unwrap();
        let op =
            f.db.admit_pair_refresh_with_current_credentials(
                "admin",
                "live",
                &pins.approval_digest().unwrap(),
                pins,
                &guard,
            )
            .unwrap();
        f.db.cancel_unpublished_pair_refresh(op.id).unwrap();
    }
}

#[test]
fn reopened_execution_rechecks_injected_conflicts_without_advancing_or_releasing() {
    let f = job::Fixture::new();
    f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
    let mut pins = f.pins();
    pins.credential_context_digest =
        f.db.resolve_pair_refresh_credentials("child", "parent")
            .unwrap()
            .context_digest()
            .to_owned();
    let guard = f.busy();
    let op =
        f.db.admit_pair_refresh_with_current_credentials(
            "admin",
            "live",
            &pins.approval_digest().unwrap(),
            pins,
            &guard,
        )
        .unwrap();
    f.db.conn().execute("INSERT INTO conflicts(id,file_path,conflict_type,status,created_at,repo_id) VALUES('injected','f','content','deferred','t','parent')", []).unwrap();
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    assert!(reopened
        .require_current_pair_refresh_execution(op.id, &guard)
        .is_err());
    assert_eq!(reopened.pair_refresh_operation(op.id).unwrap(), op);
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
}

#[test]
fn personal_checkpoint_and_mapping_writes_refuse_under_refresh_without_partial_state() {
    let f = job::Fixture::new();
    let guard = f.busy();
    f.admit(f.pins(), &guard);
    let before: i64 =
        f.db.conn()
            .query_row("SELECT COUNT(*) FROM commit_map", [], |row| row.get(0))
            .unwrap();
    assert!(f
        .db
        .record_personal_import_revision(99, &"9".repeat(40), "svn", "git")
        .is_err());
    assert!(f
        .db
        .checkpoint_personal_snapshot_import(99, &"9".repeat(40), "svn", "git", "svn_rev")
        .is_err());
    assert!(f
        .db
        .finalize_personal_full_import_watermarks(99, &"9".repeat(40))
        .is_err());
    assert!(f
        .db
        .set_legacy_import_git_sha_watermark(&"9".repeat(40))
        .is_err());
    assert_eq!(
        f.db.conn()
            .query_row("SELECT COUNT(*) FROM commit_map", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    assert!(f.db.get_watermark("svn_rev").unwrap().is_none());
    assert!(f.db.get_watermark("git_sha").unwrap().is_none());
}

#[tokio::test]
async fn scheduler_reconciliation_refuses_before_resolving_credentials_and_releases_unrelated_window(
) {
    let f = job::Fixture::new();
    let guard = f.busy();
    f.admit(f.pins(), &guard);
    for id in ["child", "parent"] {
        let repo = f.db.get_repository(id).unwrap().unwrap();
        let error = reposync_core::auto_reconcile::reconcile_held_external_writes_with_resolved_credentials(&f.db, &repo, Some(f.dir.path())).await.unwrap_err();
        assert!(error.to_string().contains("pair refresh"));
    }
    let repo = f.db.get_repository("unrelated").unwrap().unwrap();
    f.db.set_state("secret_svn_password_unrelated", "").unwrap();
    let error =
        reposync_core::auto_reconcile::reconcile_held_external_writes_with_resolved_credentials(
            &f.db,
            &repo,
            Some(f.dir.path()),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("explicitly revoked"));
    drop(
        f.db.begin_ordinary_writer_window(Some("unrelated"))
            .unwrap(),
    );
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
}
