//! Regression controls for Grok's pre-transaction and persisted-owner findings.
#[path = "fixtures/pair_refresh_job.rs"]
mod job;

use reposync_core::db::managed_remove::ManagedRemoveRemoteOutcome;

#[test]
fn removal_and_restore_refuse_before_key_creation_or_deferred_migration() {
    for key_exists in [false, true] {
        let f = job::Fixture::new();
        if key_exists {
            f.db.set_state("encryption_key", &"11".repeat(32)).unwrap();
        }
        let pins = f.pins();
        let guard = f.busy();
        f.admit(pins, &guard);
        f.db.conn().pragma_update(None, "user_version", 12).unwrap();
        let before =
            f.db.conn()
                .prepare("SELECT key,value FROM kv_state ORDER BY key")
                .unwrap()
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
        for repo in ["child", "parent"] {
            let error = f
                .db
                .complete_managed_remove(repo, "previous", &ManagedRemoveRemoteOutcome::default())
                .unwrap_err();
            assert!(
                error.to_string().contains("durable pair refresh"),
                "{error}"
            );
            let error = f.db.restore_managed_registration(repo).unwrap_err();
            assert!(
                error.to_string().contains("durable pair refresh"),
                "{error}"
            );
            let error = f.db.delete_repository(repo).unwrap_err();
            assert!(
                error.to_string().contains("durable pair refresh"),
                "{error}"
            );
            let error = f.db.hard_delete_repository(repo).unwrap_err();
            assert!(
                error.to_string().contains("durable pair refresh"),
                "{error}"
            );
        }
        let after =
            f.db.conn()
                .prepare("SELECT key,value FROM kv_state ORDER BY key")
                .unwrap()
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
        assert_eq!(after, before);
        let version: i64 =
            f.db.conn()
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap();
        assert_eq!(version, 12);
        assert!(!f.no_hold(None));
    }
}

#[test]
fn incoming_late_pair_identity_cannot_hide_the_persisted_held_parent_or_child() {
    for stored_parent in ["parent", "unrelated"] {
        let f = job::Fixture::new();
        let mut op =
            f.db.create_late_pair_publish_operation(
                stored_parent,
                "writer",
                "previous",
                "fp",
                "new",
                "branches/new",
                &"a".repeat(40),
                &"a".repeat(40),
                3,
                2,
                "v1",
            )
            .unwrap();
        op.child_repo_id = Some("child".into());
        f.db.update_late_pair_publish_operation(op.clone()).unwrap();
        let pins = f.pins();
        let guard = f.busy();
        // Model a stale/orphan late-pair receipt discovered after restart. The
        // refresh admission refuses active evidence, so fault injection removes
        // only those pointers before enrolling the refresh. Stored owner stays.
        for kind in ["active", "latest"] {
            f.db.conn()
                .execute(
                    "DELETE FROM kv_state WHERE key=?1",
                    [format!("late_pair_publish_v1:{kind}:{stored_parent}")],
                )
                .unwrap();
        }
        f.admit(pins, &guard);
        let key = format!("late_pair_publish_v1:document:{}", op.id);
        let before = f.db.get_state(&key).unwrap();
        op.parent_repo_id = "unrelated".into();
        op.child_repo_id = None;
        let error = f.db.update_late_pair_publish_operation(op).unwrap_err();
        assert!(
            error.to_string().contains("durable pair refresh"),
            "{error}"
        );
        assert_eq!(f.db.get_state(&key).unwrap(), before);
        assert!(!f.no_hold(None));
    }
}

#[test]
fn an_orphan_active_pointer_cannot_be_overwritten_under_a_held_owner() {
    let f = job::Fixture::new();
    let mut op =
        f.db.create_late_pair_publish_operation(
            "unrelated",
            "writer",
            "previous",
            "fp",
            "new",
            "branches/new",
            &"a".repeat(40),
            &"a".repeat(40),
            3,
            2,
            "v1",
        )
        .unwrap();
    let pins = f.pins();
    let guard = f.busy();
    f.admit(pins, &guard);
    f.db.set_state("late_pair_publish_v1:active:parent", &op.id)
        .unwrap();
    let key = format!("late_pair_publish_v1:document:{}", op.id);
    let before = f.db.get_state(&key).unwrap();
    op.outcome_detail = Some("attempted overwrite".into());
    let error = f.db.update_late_pair_publish_operation(op).unwrap_err();
    assert!(
        error.to_string().contains("durable pair refresh"),
        "{error}"
    );
    assert_eq!(f.db.get_state(&key).unwrap(), before);
    assert!(!f.no_hold(None));
}
