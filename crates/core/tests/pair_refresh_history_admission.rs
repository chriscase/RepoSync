//! Retained history reconciliation cannot be bypassed by fresh input capture.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;

fn fixture() -> job::Fixture {
    let f = job::Fixture::new();
    f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
    f.db.conn().execute("UPDATE repositories SET git_repo='owner/repo',git_api_url='https://git.example/api/v1',svn_branch=CASE WHEN id='child' THEN 'branches/f' ELSE 'trunk' END", []).unwrap();
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

fn history_keys(f: &job::Fixture) -> Vec<String> {
    let pins = f.pins();
    let mut keys = reposync_core::db::personal_scope::personal_history_block_keys().to_vec();
    keys.push("team_history_block_global".into());
    for pin in [pins.child, pins.parent] {
        keys.push(format!("team_history_block_{}", pin.repo_id));
        keys.push(format!("team_history_block_{}", pin.scope_uuid));
    }
    keys
}

#[test]
fn scoped_legacy_global_and_personal_blocks_refuse_without_any_refresh_write() {
    let f = fixture();
    for key in history_keys(&f) {
        for value in ["{\"state\":\"reconciliation_required\",\"reason\":\"observed_remote_rewrite\",\"durable\":true}", "corrupt evidence", "{\"state\":\"blocked\",\"durable\":false}"] {
            f.db.set_state(&key,value).unwrap();
            let before=rows(&f);
            let error=f.db.capture_pair_refresh_execution_configuration("child").err().unwrap().to_string();
            assert!(error.contains("history inspection must be quiet"));
            assert_eq!(rows(&f),before);
            assert!(f.no_hold(None));
            f.db.delete_kv_state(&key).unwrap();
        }
    }
}

#[test]
fn history_block_arriving_after_preview_refuses_admission_and_retains_later_prepared_job() {
    let f = fixture();
    let mut pins = f.pins();
    pins.credential_context_digest =
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap()
            .pin()
            .credential_context_digest
            .clone();
    let busy = f.busy();
    let digest = pins.approval_digest().unwrap();
    f.db.set_state("team_history_block_child", "retained rewrite")
        .unwrap();
    let before = rows(&f);
    assert!(f
        .db
        .admit_pair_refresh_with_registered_sources(
            "admin",
            "request",
            &digest,
            pins.clone(),
            &busy
        )
        .is_err());
    assert_eq!(rows(&f), before);
    assert!(f.no_hold(None));
    f.db.delete_kv_state("team_history_block_child").unwrap();
    let op =
        f.db.admit_pair_refresh_with_registered_sources("admin", "request", &digest, pins, &busy)
            .unwrap();
    f.db.conn().execute("INSERT INTO kv_state(key,value,updated_at) VALUES('team_history_block_parent','retained rewrite','fixture')",[]).unwrap();
    let before = rows(&f);
    assert!(f
        .db
        .require_registered_pair_refresh_execution(op.id, &busy)
        .is_err());
    assert!(f
        .db
        .record_pair_refresh_svn_intent_with_registered_sources(op.id, &busy)
        .is_err());
    assert_eq!(rows(&f), before);
    assert!(!f.no_hold(Some("child")));
    assert!(!f.no_hold(Some("parent")));
    assert_eq!(
        f.db.pair_refresh_operation(op.id).unwrap().phase,
        reposync_core::db::pair_refresh_operations::PairRefreshPhase::Prepared
    );
}

#[test]
fn unrelated_history_blocks_and_explicitly_cleared_values_do_not_replace_pair_proof() {
    let f = fixture();
    let scope: String =
        f.db.conn()
            .query_row(
                "SELECT scope_uuid FROM repositories WHERE id='unrelated'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    f.db.set_state(
        "team_history_block_unrelated",
        "unrelated retained evidence",
    )
    .unwrap();
    f.db.set_state(
        &format!("team_history_block_{scope}"),
        "unrelated retained evidence",
    )
    .unwrap();
    for key in history_keys(&f) {
        f.db.set_state(&key, "").unwrap();
    }
    let before = rows(&f);
    let configuration =
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap();
    assert_eq!(rows(&f), before);
    let mut pins = f.pins();
    pins.credential_context_digest = configuration.pin().credential_context_digest.clone();
    let busy = f.busy();
    let digest = pins.approval_digest().unwrap();
    let op =
        f.db.admit_pair_refresh_with_registered_sources("admin", "request", &digest, pins, &busy)
            .unwrap();
    assert!(f
        .db
        .require_registered_pair_refresh_execution(op.id, &busy)
        .is_ok());
}
