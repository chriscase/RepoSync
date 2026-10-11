//! Conflict writes honor refresh holds in the same storage transaction.
#[path = "fixtures/pair_refresh_job.rs"]
mod job;
use reposync_core::conflict::resolver::ConflictResolver;
use reposync_core::db::Database;
use rusqlite::params;

fn stored_content(db: &Database, id: &str) -> Option<String> {
    db.conn()
        .query_row(
            "SELECT resolved_content FROM conflicts WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

fn conflict(db: &Database, owner: Option<&str>) -> String {
    let id = db
        .insert_conflict_entry(
            "f",
            "content",
            Some("svn"),
            Some("git"),
            Some("base"),
            Some(2),
            Some(&"a".repeat(40)),
        )
        .unwrap();
    db.conn()
        .execute(
            "UPDATE conflicts SET repo_id=?1 WHERE id=?2",
            params![owner, id],
        )
        .unwrap();
    id
}

#[test]
fn held_and_legacy_conflict_resolution_content_and_defer_preserve_every_field() {
    for owner in [
        Some("child"),
        Some("parent"),
        None,
        Some(""),
        Some("__reposync_personal__"),
    ] {
        let f = job::Fixture::new();
        let id = conflict(&f.db, owner);
        let before = f.db.get_conflict_entry(&id).unwrap();
        let before_content = stored_content(&f.db, &id);
        let guard = f.busy();
        f.admit(f.pins(), &guard);
        assert!(f
            .db
            .resolve_conflict(&id, "resolved", "custom", "admin")
            .is_err());
        assert!(f
            .db
            .resolve_conflict_with_content(&id, "resolved", "custom", "admin", Some("merged"))
            .is_err());
        assert!(f.db.store_resolved_content(&id, "merged").is_err());
        assert!(f.db.defer_conflict(&id).is_err());
        assert!(ConflictResolver::accept_merged(&id, "merged", "admin", &f.db).is_err());
        let after = f.db.get_conflict_entry(&id).unwrap();
        assert_eq!(after.status, before.status);
        assert_eq!(after.resolution, before.resolution);
        assert_eq!(stored_content(&f.db, &id), before_content);
        assert_eq!(after.resolved_at, before.resolved_at);
        assert_eq!(after.resolved_by, before.resolved_by);
        assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
    }
}

#[test]
fn unrelated_conflict_and_status_updates_remain_available_under_refresh() {
    let f = job::Fixture::new();
    let id = conflict(&f.db, Some("unrelated"));
    let guard = f.busy();
    f.admit(f.pins(), &guard);
    ConflictResolver::accept_merged(&id, "merged", "admin", &f.db).unwrap();
    let row = f.db.get_conflict_entry(&id).unwrap();
    assert_eq!(row.status, "resolved");
    assert_eq!(stored_content(&f.db, &id).as_deref(), Some("merged"));
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
}

#[test]
fn content_insert_failure_rolls_back_resolution_status_and_identity() {
    let f = job::Fixture::new();
    let id = conflict(&f.db, Some("child"));
    f.db.conn().execute_batch("CREATE TRIGGER fail_content BEFORE UPDATE OF resolved_content ON conflicts BEGIN SELECT RAISE(ABORT,'content fault'); END;").unwrap();
    assert!(ConflictResolver::accept_merged(&id, "merged", "admin", &f.db).is_err());
    let row = f.db.get_conflict_entry(&id).unwrap();
    assert_eq!(row.status, "detected");
    assert!(stored_content(&f.db, &id).is_none());
    assert!(row.resolved_at.is_none());
    assert!(row.resolved_by.is_none());
    f.db.conn()
        .execute_batch("DROP TRIGGER fail_content")
        .unwrap();
    ConflictResolver::accept_merged(&id, "merged", "admin", &f.db).unwrap();
    assert_eq!(stored_content(&f.db, &id).as_deref(), Some("merged"));
}

#[test]
fn reopened_and_corrupt_refresh_evidence_block_conflict_writes() {
    let f = job::Fixture::new();
    let id = conflict(&f.db, Some("unrelated"));
    let guard = f.busy();
    f.admit(f.pins(), &guard);
    f.db.conn().execute("INSERT INTO kv_state(key,value,updated_at) VALUES('pair_refresh_execution_future:document:x','{}','t')",[]).unwrap();
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    assert!(reopened
        .resolve_conflict(&id, "resolved", "accept_git", "admin")
        .is_err());
    assert_eq!(reopened.get_conflict_entry(&id).unwrap().status, "detected");
}
