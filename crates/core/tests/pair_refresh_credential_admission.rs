//! Credential context is a strict local prerequisite, not live remote proof.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;

use reposync_core::db::pair_refresh_operations::PairRefreshPhase;
use reposync_core::db::Database;
use rusqlite::params;

fn establish(db: &Database) {
    db.set_state("encryption_key", &"41".repeat(32)).unwrap();
}

fn context(db: &Database) -> String {
    db.resolve_pair_refresh_credentials("child", "parent")
        .unwrap()
        .context_digest()
        .to_owned()
}

fn rows(db: &Database) -> Vec<(String, String, String)> {
    db.conn()
        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn snapshot_resolves_inheritance_atomically_without_mutating_or_exposing_credentials() {
    let f = job::Fixture::new();
    establish(&f.db);
    f.db.set_state("secret_git_token_parent", "parent-token")
        .unwrap();
    f.db.set_state("secret_git_token_child", "child-token")
        .unwrap();
    f.db.set_state("secret_svn_password", "global-password")
        .unwrap();
    let before = rows(&f.db);
    let captured =
        f.db.resolve_pair_refresh_credentials("child", "parent")
            .unwrap();
    assert_eq!(captured.child_git_token(), Some("child-token"));
    assert_eq!(captured.parent_git_token(), Some("parent-token"));
    assert_eq!(captured.child_svn_password(), Some("global-password"));
    assert_eq!(captured.parent_svn_password(), Some("global-password"));
    assert_eq!(captured.context_digest().len(), 64);
    assert_eq!(captured.context_digest(), context(&f.db));
    assert_eq!(rows(&f.db), before);
    assert!(f.no_hold(None));
}

#[test]
fn missing_managed_values_use_anonymous_clients_without_configuration_fallback() {
    let f = job::Fixture::new();
    establish(&f.db);
    f.db.set_state("git_token", "legacy-fallback").unwrap();
    f.db.set_state("svn_password", "legacy-fallback").unwrap();
    let captured =
        f.db.resolve_pair_refresh_credentials("child", "parent")
            .unwrap();
    assert!(captured.child_git_token().is_none());
    assert!(captured.parent_git_token().is_none());
    assert!(captured.child_svn_password().is_none());
    assert!(captured.parent_svn_password().is_none());
}

#[test]
fn explicit_child_parent_and_global_revocation_refuse_instead_of_borrowing_secrets() {
    for key in [
        "secret_git_token_child",
        "secret_git_token_parent",
        "secret_git_token",
        "secret_svn_password_child",
        "secret_svn_password_parent",
        "secret_svn_password",
    ] {
        let f = job::Fixture::new();
        establish(&f.db);
        f.db.set_state("secret_git_token", "global-token").unwrap();
        f.db.set_state("secret_svn_password", "global-password")
            .unwrap();
        f.db.set_state(key, "").unwrap();
        let before = rows(&f.db);
        let error =
            f.db.resolve_pair_refresh_credentials("child", "parent")
                .err()
                .unwrap()
                .to_string();
        assert!(
            error.contains("explicit credential revocation"),
            "{key}: {error}"
        );
        assert_eq!(rows(&f.db), before);
        assert!(f.no_hold(None));
    }
}

#[test]
fn narrower_values_take_precedence_but_the_parent_must_resolve_its_own_access() {
    let f = job::Fixture::new();
    establish(&f.db);
    f.db.set_state("secret_git_token", "").unwrap();
    f.db.set_state("secret_svn_password", "").unwrap();
    f.db.set_state("secret_git_token_parent", "parent-token")
        .unwrap();
    f.db.set_state("secret_svn_password_parent", "parent-password")
        .unwrap();
    let captured =
        f.db.resolve_pair_refresh_credentials("child", "parent")
            .unwrap();
    assert_eq!(captured.child_git_token(), Some("parent-token"));
    assert_eq!(captured.child_svn_password(), Some("parent-password"));
    f.db.set_state("secret_git_token_child", "child-token")
        .unwrap();
    f.db.set_state("secret_git_token_parent", "").unwrap();
    assert!(f
        .db
        .resolve_pair_refresh_credentials("child", "parent")
        .is_err());
}

#[test]
fn missing_cyclic_duplicate_and_malformed_ancestry_refuse_even_with_global_values() {
    for fault in 0..5 {
        let f = job::Fixture::new();
        establish(&f.db);
        f.db.set_state("secret_git_token", "global-token").unwrap();
        match fault {
            0 => {
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET parent_id='absent' WHERE id='parent'",
                        [],
                    )
                    .unwrap()
            }
            1 => {
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET parent_id='child' WHERE id='parent'",
                        [],
                    )
                    .unwrap()
            }
            2 => {
                // Disable the unique index only to inject contradictory evidence.
                f.db.conn()
                    .execute_batch("DROP INDEX idx_repositories_scope_uuid;")
                    .unwrap();
                f.db.conn().execute("UPDATE repositories SET scope_uuid=(SELECT scope_uuid FROM repositories WHERE id='parent') WHERE id='child'", []).unwrap()
            }
            3 => {
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET scope_uuid='malformed' WHERE id='parent'",
                        [],
                    )
                    .unwrap()
            }
            _ => {
                f.db.conn()
                    .execute_batch("DROP INDEX idx_repositories_scope_uuid;")
                    .unwrap();
                f.db.conn().execute("UPDATE repositories SET scope_uuid=(SELECT scope_uuid FROM repositories WHERE id='parent') WHERE id='unrelated'", []).unwrap()
            }
        };
        let before = rows(&f.db);
        assert!(
            f.db.resolve_pair_refresh_credentials("child", "parent")
                .is_err(),
            "fault {fault}"
        );
        assert_eq!(rows(&f.db), before);
    }
}

#[test]
fn storage_errors_missing_registration_and_wrong_parent_do_not_become_anonymous_access() {
    let f = job::Fixture::new();
    establish(&f.db);
    assert!(f
        .db
        .resolve_pair_refresh_credentials("absent", "parent")
        .is_err());
    assert!(f
        .db
        .resolve_pair_refresh_credentials("child", "unrelated")
        .is_err());
    assert!(f
        .db
        .resolve_pair_refresh_credentials("child", "child")
        .is_err());
    f.db.conn().execute_batch("DROP TABLE kv_state;").unwrap();
    assert!(f
        .db
        .resolve_pair_refresh_credentials("child", "parent")
        .is_err());
}

#[test]
fn credential_value_timestamp_and_ancestor_identity_each_invalidate_context() {
    for fault in 0..3 {
        let f = job::Fixture::new();
        establish(&f.db);
        f.db.set_state("secret_git_token_parent", "parent-token")
            .unwrap();
        let before = context(&f.db);
        match fault {
            0 => {
                f.db.conn()
                    .execute(
                        "UPDATE kv_state SET value='changed' WHERE key='secret_git_token_parent'",
                        [],
                    )
                    .unwrap()
            }
            1 => f
                .db
                .conn()
                .execute(
                    "UPDATE kv_state SET updated_at='changed' WHERE key='secret_git_token_parent'",
                    [],
                )
                .unwrap(),
            _ => {
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET scope_uuid=?1 WHERE id='parent'",
                        [uuid::Uuid::new_v4().to_string()],
                    )
                    .unwrap()
            }
        };
        assert_ne!(before, context(&f.db), "fault {fault}");
    }
}

#[test]
fn credential_checked_admission_binds_the_snapshot_in_the_hold_transaction() {
    let f = job::Fixture::new();
    establish(&f.db);
    let mut pins = f.pins();
    pins.credential_context_digest = context(&f.db);
    let guard = f.busy();
    let op =
        f.db.admit_pair_refresh_with_current_credentials(
            "admin",
            "live",
            &pins.approval_digest().unwrap(),
            pins.clone(),
            &guard,
        )
        .unwrap();
    assert_eq!(op.phase, PairRefreshPhase::Prepared);
    let captured =
        f.db.require_current_pair_refresh_execution(op.id, &guard)
            .unwrap();
    assert_eq!(captured.context_digest(), pins.credential_context_digest);
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
    f.db.cancel_unpublished_pair_refresh(op.id).unwrap();
    assert!(f
        .db
        .require_current_pair_refresh_execution(op.id, &guard)
        .is_err());
    assert!(f.no_hold(None));
}

#[test]
fn stale_context_and_revocation_refuse_admission_without_any_refresh_document() {
    for revoke in [false, true] {
        let f = job::Fixture::new();
        establish(&f.db);
        let mut pins = f.pins();
        pins.credential_context_digest = context(&f.db);
        f.db.set_state(
            "secret_git_token_child",
            if revoke { "" } else { "new-token" },
        )
        .unwrap();
        let guard = f.busy();
        assert!(f
            .db
            .admit_pair_refresh_with_current_credentials(
                "admin",
                "live",
                &pins.approval_digest().unwrap(),
                pins,
                &guard
            )
            .is_err());
        assert!(f.no_hold(None));
        assert!(rows(&f.db)
            .iter()
            .all(|(key, _, _)| !key.starts_with("pair_refresh_execution_")));
    }
}

#[test]
fn reopened_execution_rechecks_raw_credential_drift_and_retains_both_holds() {
    for revoke in [false, true] {
        let f = job::Fixture::new();
        establish(&f.db);
        f.db.set_state("secret_git_token_child", "child-token")
            .unwrap();
        let mut pins = f.pins();
        pins.credential_context_digest = context(&f.db);
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
        f.db.record_pair_refresh_svn_intent(op.id).unwrap();
        assert!(f
            .db
            .set_state("secret_git_token_child", "new-token")
            .is_err());
        f.db.conn()
            .execute(
                "UPDATE kv_state SET value=?1 WHERE key='secret_git_token_child'",
                [if revoke { "" } else { "new-token" }],
            )
            .unwrap();
        let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
        assert!(reopened
            .require_current_pair_refresh_execution(op.id, &guard)
            .is_err());
        assert_eq!(
            reopened.pair_refresh_operation(op.id).unwrap().phase,
            PairRefreshPhase::SvnIntent
        );
        assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
    }
}

#[test]
fn wrong_busy_capability_cannot_observe_or_continue_a_held_job() {
    let f = job::Fixture::new();
    establish(&f.db);
    let mut pins = f.pins();
    pins.credential_context_digest = context(&f.db);
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
    drop(guard);
    let other = reposync_core::busy::try_acquire_pair("unrelated", "absent").unwrap();
    assert!(f
        .db
        .require_current_pair_refresh_execution(op.id, &other)
        .is_err());
    assert!(!f.no_hold(Some("child")) && !f.no_hold(Some("parent")));
}

#[test]
fn malformed_credential_storage_does_not_turn_revocation_into_missing_access() {
    let f = job::Fixture::new();
    establish(&f.db);
    f.db.conn()
        .execute(
            "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,'t')",
            params!["secret_git_token_child", vec![0xff_u8]],
        )
        .unwrap();
    assert!(f
        .db
        .resolve_pair_refresh_credentials("child", "parent")
        .is_err());
    assert!(f.no_hold(None));
}
