//! Configuration, credentials and typed checkpoint writer exclusion controls.
#[path = "fixtures/pair_refresh_job.rs"]
mod job;

use reposync_core::db::Database;
use reposync_core::errors::DatabaseError;

fn held<T>(result: Result<T, DatabaseError>) {
    match result {
        Err(e) => assert!(e.to_string().contains("durable pair refresh"), "{e}"),
        Ok(_) => panic!("held mutation succeeded"),
    }
}

#[test]
fn registration_and_checkpoint_mutations_refuse_without_partial_changes() {
    let f = job::Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    let id = f.admit(pins.clone(), &guard);
    for repo in ["child", "parent"] {
        let before = f.db.get_repository(repo).unwrap().unwrap();
        let mut changed = before.clone();
        changed.git_branch = "different".into();
        held(f.db.update_repository(&changed));
        held(f.db.update_repo_watermark(repo, 99, &"e".repeat(40)));
        held(f.db.update_repo_watermark_columns_only(repo, 99, &"e".repeat(40)));
        held(f.db.advance_svn_watermark(repo, 99));
        held(f.db.advance_all_watermarks(repo, &"e".repeat(40)));
        held(f.db.advance_exact_skip_watermarks(
            repo,
            &"e".repeat(40),
            &["e".repeat(40)],
            "excluded",
        ));
        held(f.db.reset_repo_sync_mappings_for_reimport(repo));
        held(f.db.delete_sync_records_for_repo(repo));
        held(f.db.delete_commit_map_for_repo(repo));
        held(f.db.delete_repository(repo));
        held(f.db.hard_delete_repository(repo));
        held(reposync_core::echo_receipt_scope::bump_repo_echo_generation(&f.db, repo));
        let after = f.db.get_repository(repo).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(after).unwrap(),
            serde_json::to_value(before).unwrap()
        );
    }
    held(f.db.clear_sync_data());
    held(f.db.set_watermark("svn_rev", "99"));
    assert_eq!(f.db.pair_refresh_operation(id).unwrap().pins, pins);
    assert!(!f.no_hold(Some("parent")) && !f.no_hold(Some("child")));
}

#[test]
fn ancestor_credentials_and_chain_configuration_are_held_but_unrelated_changes_run() {
    let f = job::Fixture::new();
    let mut ancestor = f.db.get_repository("unrelated").unwrap().unwrap();
    ancestor.id = "grandparent".into();
    ancestor.name = "grandparent".into();
    f.db.insert_repository(&ancestor).unwrap();
    let mut parent = f.db.get_repository("parent").unwrap().unwrap();
    parent.parent_id = Some("grandparent".into());
    f.db.update_repository(&parent).unwrap();
    let keys = [
        "secret_git_token_grandparent",
        "secret_svn_password_grandparent",
        "secret_git_token_parent",
        "secret_svn_password_child",
        "secret_git_token",
        "secret_svn_password",
    ];
    for key in keys {
        f.db.set_state(key, "fixture-value").unwrap();
    }
    let pins = f.pins();
    let guard = f.busy();
    f.admit(pins, &guard);
    ancestor.svn_username = "different".into();
    held(f.db.update_repository(&ancestor));
    held(f.db.delete_repository("grandparent"));
    held(f.db.hard_delete_repository("grandparent"));
    for key in keys {
        held(f.db.set_state(key, ""));
        held(f.db.delete_kv_state(key));
        assert_eq!(
            f.db.get_state(key).unwrap().as_deref(),
            Some("fixture-value")
        );
    }
    let mut unrelated = f.db.get_repository("unrelated").unwrap().unwrap();
    unrelated.git_branch = "another".into();
    f.db.update_repository(&unrelated).unwrap();
    f.db.set_state("secret_git_token_unrelated", "other-fixture-value")
        .unwrap();
    f.db.delete_kv_state("secret_git_token_unrelated").unwrap();
    assert_eq!(
        f.db.get_repository("unrelated")
            .unwrap()
            .unwrap()
            .git_branch,
        "another"
    );
    // Session/status persistence does not mutate the held configuration.
    f.db.set_state("last_sync_at", "status-only").unwrap();
    f.db.set_state("secret_admin_password_hash", "fixture-hash")
        .unwrap();
    assert!(!f.no_hold(Some("child")));
}

#[test]
fn encrypted_credential_and_encryption_key_changes_refuse_and_preserve_ciphertext() {
    let f = job::Fixture::new();
    for key in ["git_token", "svn_password"] {
        f.db.store_encrypted_secret(key, "fixture-ciphertext", "fixture-nonce")
            .unwrap();
    }
    f.db.set_state("encryption_key", "fixture-key").unwrap();
    let pins = f.pins();
    let guard = f.busy();
    f.admit(pins, &guard);
    for key in ["git_token", "svn_password"] {
        held(f.db.store_encrypted_secret(key, "replacement", "nonce"));
        held(f.db.delete_secret(key));
        assert_eq!(
            f.db.get_encrypted_secret(key).unwrap(),
            Some(("fixture-ciphertext".into(), "fixture-nonce".into()))
        );
    }
    held(f.db.set_state("encryption_key", "replacement"));
    held(f.db.delete_kv_state("encryption_key"));
    assert_eq!(
        f.db.get_state("encryption_key").unwrap().as_deref(),
        Some("fixture-key")
    );
    f.db.store_encrypted_secret("unrelated-secret", "fixture", "nonce")
        .unwrap();
    f.db.delete_secret("unrelated-secret").unwrap();
    assert!(!f.no_hold(Some("parent")));
}

#[test]
fn registration_guards_survive_database_reopen_and_process_slot_drop() {
    let f = job::Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    f.admit(pins, &guard);
    drop(guard);
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    let mut child = reopened.get_repository("child").unwrap().unwrap();
    child.enabled = false;
    held(reopened.update_repository(&child));
    held(reopened.set_state("secret_git_token_child", ""));
    assert!(!f.no_hold(None));
}

#[test]
fn two_connections_cannot_admit_a_refresh_and_mutate_its_approved_configuration() {
    let f = job::Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    let other = Database::new(f.dir.path().join("reposync.db")).unwrap();
    let mut changed = other.get_repository("child").unwrap().unwrap();
    changed.git_branch = "different".into();
    let barrier = std::sync::Barrier::new(2);
    let (admitted, updated) = std::thread::scope(|scope| {
        let updater = scope.spawn(|| {
            barrier.wait();
            other.update_repository(&changed).is_ok()
        });
        barrier.wait();
        let admitted =
            f.db.admit_pair_refresh(
                "admin",
                "request",
                &pins.approval_digest().unwrap(),
                pins,
                &guard,
            )
            .is_ok();
        (admitted, updater.join().unwrap())
    });
    assert!(
        admitted ^ updated,
        "exactly one side must own the configuration"
    );
    assert_eq!(
        f.db.get_repository("child").unwrap().unwrap().git_branch,
        if updated { "different" } else { "feature" }
    );
    assert_eq!(f.no_hold(None), !admitted);
}

#[test]
fn corrupt_refresh_evidence_refuses_configuration_and_credentials_without_mutation() {
    let f = job::Fixture::new();
    f.db.set_state("pair_refresh_execution_future:hold:unknown", "corrupt")
        .unwrap();
    let before = f.db.get_repository("unrelated").unwrap().unwrap();
    let mut changed = before.clone();
    changed.git_branch = "different".into();
    assert!(f.db.update_repository(&changed).is_err());
    assert!(f.db.set_state("secret_git_token_unrelated", "").is_err());
    assert!(f
        .db
        .store_encrypted_secret("git_token", "fixture", "nonce")
        .is_err());
    assert_eq!(
        serde_json::to_value(f.db.get_repository("unrelated").unwrap().unwrap()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert!(f
        .db
        .get_state("secret_git_token_unrelated")
        .unwrap()
        .is_none());
    assert!(f.db.get_encrypted_secret("git_token").unwrap().is_none());
    assert!(!f.no_hold(None));
}
