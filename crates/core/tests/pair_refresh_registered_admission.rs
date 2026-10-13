//! Synthetic storage controls only. Endpoint binding is not live remote proof.
#[path = "fixtures/pair_refresh_job.rs"]
#[allow(dead_code)]
mod job;

use reposync_core::db::pair_refresh_operations::{PairRefreshPhase, RefreshJobPins};
use reposync_core::pair_refresh::svn_observation::SvnRefreshPublicationReceipt;

fn fixture() -> job::Fixture {
    let f = job::Fixture::new();
    f.db.set_state("encryption_key", &"41".repeat(32)).unwrap();
    f.db.conn().execute("UPDATE repositories SET git_repo='owner/repo',git_api_url='https://git.example/api/v1',svn_branch=CASE WHEN id='child' THEN 'branches/f' ELSE 'trunk' END", []).unwrap();
    f
}

fn pins(f: &job::Fixture) -> RefreshJobPins {
    let mut pins = f.pins();
    pins.credential_context_digest =
        f.db.capture_pair_refresh_execution_configuration("child")
            .unwrap()
            .pin()
            .credential_context_digest
            .clone();
    pins
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

fn receipt(pins: &RefreshJobPins) -> SvnRefreshPublicationReceipt {
    let lineage = &pins.svn.pins.lineage.pins;
    let mut child_copy = pins.svn.pins.lineage.child_copy.clone();
    child_copy.inspected_revision = 5;
    SvnRefreshPublicationReceipt {
        operation_id: pins.git.operation_id,
        revision: 5,
        child_url: lineage.child_url.clone(),
        mergeinfo: pins.svn.mergeinfo.clone(),
        git_tree: pins.git.tree_sha.clone(),
        properties: pins.svn.result_properties.clone(),
        child_copy,
    }
}

#[test]
fn registered_admission_is_idempotent_and_intent_is_never_retry_permission() {
    let f = fixture();
    let pins = pins(&f);
    let busy = f.busy();
    let digest = pins.approval_digest().unwrap();
    let op =
        f.db.admit_pair_refresh_with_registered_sources(
            "admin",
            "request",
            &digest,
            pins.clone(),
            &busy,
        )
        .unwrap();
    assert_eq!(op.phase, PairRefreshPhase::Prepared);
    assert!(!f.no_hold(Some("child")));
    assert!(!f.no_hold(Some("parent")));
    let journal_rows = || {
        rows(&f)
            .into_iter()
            .filter(|(key, _, _)| key != "writer_fence_v1:lease")
            .collect::<Vec<_>>()
    };
    // A successful repeated call renews the writer lease; every intent,
    // checkpoint, request and scope-hold row remains byte-identical.
    let before = journal_rows();
    assert_eq!(
        f.db.admit_pair_refresh_with_registered_sources(
            "admin",
            "request",
            &digest,
            pins.clone(),
            &busy
        )
        .unwrap(),
        op
    );
    assert_eq!(journal_rows(), before);
    let (intent, configuration) =
        f.db.record_pair_refresh_svn_intent_with_registered_sources(op.id, &busy)
            .unwrap();
    assert_eq!(intent.phase, PairRefreshPhase::SvnIntent);
    configuration.require_job_configuration(&pins).unwrap();
    let before = rows(&f);
    assert!(f
        .db
        .record_pair_refresh_svn_intent_with_registered_sources(op.id, &busy)
        .is_err());
    assert_eq!(rows(&f), before);
    f.db.record_pair_refresh_svn_receipt(op.id, receipt(&pins))
        .unwrap();
    let (intent, _) =
        f.db.record_pair_refresh_git_intent_with_registered_sources(op.id, &busy)
            .unwrap();
    assert_eq!(intent.phase, PairRefreshPhase::GitIntent);
    let before = rows(&f);
    assert!(f
        .db
        .record_pair_refresh_git_intent_with_registered_sources(op.id, &busy)
        .is_err());
    assert_eq!(rows(&f), before);
}

#[test]
fn approved_digest_cannot_authorize_substituted_registered_endpoints() {
    for drift in ["git", "branch", "svn", "incomplete-auth"] {
        let f = fixture();
        if drift == "incomplete-auth" {
            f.db.conn()
                .execute(
                    "UPDATE repositories SET svn_username='fixture' WHERE id='child'",
                    [],
                )
                .unwrap();
        }
        let mut pins = f.pins();
        pins.credential_context_digest =
            f.db.resolve_pair_refresh_credentials("child", "parent")
                .unwrap()
                .context_digest()
                .into();
        match drift {
            "git" => pins.git_remote_url = "https://foreign.example/owner/repo.git".into(),
            "branch" => pins.git_branch = "substituted".into(),
            "svn" => {
                pins.svn.pins.lineage.pins.repository_root_url =
                    "https://foreign.example/repo".into();
                pins.svn.pins.lineage.pins.child_url =
                    "https://foreign.example/repo/branches/f".into();
                pins.svn.pins.lineage.pins.parent_url = "https://foreign.example/repo/trunk".into();
            }
            "incomplete-auth" => {}
            _ => unreachable!(),
        }
        let busy = f.busy();
        let before = rows(&f);
        let digest = pins.approval_digest().unwrap();
        let error = f
            .db
            .admit_pair_refresh_with_registered_sources("admin", "request", &digest, pins, &busy)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("execution configuration"),
            "{drift}: {error}"
        );
        assert_eq!(rows(&f), before);
        assert!(f.no_hold(None));
    }
}

#[test]
fn drift_or_reconciliation_before_intent_preserves_prepared_phase_and_both_holds() {
    for fault in [
        "credential",
        "parent-branch",
        "conflict",
        "reconciliation",
        "busy",
    ] {
        let f = fixture();
        let pins = pins(&f);
        let busy = f.busy();
        let digest = pins.approval_digest().unwrap();
        let op = f
            .db
            .admit_pair_refresh_with_registered_sources("admin", "request", &digest, pins, &busy)
            .unwrap();
        match fault {
            "credential" => {
                f.db.conn().execute("INSERT INTO kv_state(key,value,updated_at) VALUES('secret_git_token_child','','drift')", []).unwrap();
            }
            "parent-branch" => {
                f.db.conn()
                    .execute(
                        "UPDATE repositories SET git_branch='drift' WHERE id='parent'",
                        [],
                    )
                    .unwrap();
            }
            "conflict" => {
                f.db.conn().execute("INSERT INTO conflicts(id,repo_id,git_sha,svn_rev,file_path,conflict_type,status,created_at) VALUES('drift','child','sha',1,'a','content','detected','t')", []).unwrap();
            }
            "reconciliation" => {
                f.db.hold_pair_refresh_reconciliation(op.id, "operator inspection required")
                    .unwrap();
            }
            "busy" => {}
            _ => unreachable!(),
        }
        let unrelated;
        let guard = if fault == "busy" {
            unrelated =
                reposync_core::busy::try_acquire_pair("unrelated-a", "unrelated-b").unwrap();
            &unrelated
        } else {
            &busy
        };
        let before = rows(&f);
        assert!(
            f.db.record_pair_refresh_svn_intent_with_registered_sources(op.id, guard)
                .is_err(),
            "{fault}"
        );
        assert_eq!(rows(&f), before);
        assert!(!f.no_hold(Some("child")));
        assert!(!f.no_hold(Some("parent")));
        // Raw registration drift deliberately makes the journal unreadable;
        // its durable operation still remains Prepared, with no intent write.
        let encoded: String =
            f.db.conn()
                .query_row(
                    "SELECT value FROM kv_state WHERE key=?1",
                    [format!("pair_refresh_execution_v2:document:{}", op.id)],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&encoded).unwrap()["phase"],
            "prepared"
        );
    }
}

#[test]
fn failed_intent_storage_does_not_return_clients_or_advance_phase() {
    let f = fixture();
    let pins = pins(&f);
    let busy = f.busy();
    let digest = pins.approval_digest().unwrap();
    let op =
        f.db.admit_pair_refresh_with_registered_sources("admin", "request", &digest, pins, &busy)
            .unwrap();
    f.db.conn().execute_batch("CREATE TRIGGER fail_intent BEFORE UPDATE ON kv_state WHEN NEW.key LIKE 'pair_refresh_execution_v2:document:%' BEGIN SELECT RAISE(ABORT,'injected intent storage failure'); END;").unwrap();
    let before = rows(&f);
    assert!(f
        .db
        .record_pair_refresh_svn_intent_with_registered_sources(op.id, &busy)
        .is_err());
    assert_eq!(rows(&f), before);
    assert_eq!(
        f.db.pair_refresh_operation(op.id).unwrap().phase,
        PairRefreshPhase::Prepared
    );
    assert!(!f.no_hold(Some("child")));
    assert!(!f.no_hold(Some("parent")));
}
