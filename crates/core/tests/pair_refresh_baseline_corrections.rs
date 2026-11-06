//! Grok baseline omission and duplicate-receipt reproductions.
#[path = "fixtures/pair_refresh_job.rs"]
mod job;

use reposync_core::db::pair_refresh_operations::{
    capture_refresh_registration, PairRefreshPhase, RefreshJobPins,
};
use reposync_core::db::Database;
use reposync_core::pair_refresh::svn_observation::SvnRefreshPublicationReceipt;
use uuid::Uuid;

fn complete(db: &Database, pins: &RefreshJobPins, revision: i64) {
    let id = pins.git.operation_id;
    let mut copy = pins.svn.pins.lineage.child_copy.clone();
    copy.inspected_revision = revision;
    db.record_pair_refresh_svn_intent(id).unwrap();
    db.record_pair_refresh_svn_receipt(
        id,
        SvnRefreshPublicationReceipt {
            operation_id: id,
            revision,
            child_url: pins.svn.pins.lineage.pins.child_url.clone(),
            child_copy: copy,
            git_tree: pins.git.tree_sha.clone(),
            mergeinfo: pins.svn.mergeinfo.clone(),
            properties: pins.svn.result_properties.clone(),
        },
    )
    .unwrap();
    db.record_pair_refresh_git_intent(id).unwrap();
    db.record_pair_refresh_git_receipt(id, &pins.git.prepared_sha)
        .unwrap();
    assert_eq!(
        db.finalize_pair_refresh(id).unwrap().phase,
        PairRefreshPhase::Completed
    );
}

fn advance_parent(db: &Database) {
    db.conn()
        .execute(
            "UPDATE repositories SET last_git_sha=?1,last_svn_rev=9 WHERE id='parent'",
            ["9".repeat(40)],
        )
        .unwrap();
    db.set_state("last_svn_rev_parent", "9").unwrap();
    reposync_core::echo_receipt_scope::write_scoped_last_git_sha_kv(
        &db.conn(),
        "parent",
        &"9".repeat(40),
        "t",
    )
    .unwrap();
}

fn next(db: &Database, prior: &RefreshJobPins) -> RefreshJobPins {
    let mut pins = prior.clone();
    pins.child = capture_refresh_registration(&db.conn(), "child").unwrap();
    pins.parent = capture_refresh_registration(&db.conn(), "parent").unwrap();
    pins.git.operation_id = Uuid::new_v4();
    pins.svn.operation_id = pins.git.operation_id;
    pins.git.prepared_sha = "8".repeat(40);
    pins.git.tree_sha = "6".repeat(40);
    pins.git.pins.child_tip = pins.child.last_git_sha.clone();
    pins.git.pins.parent_tip = pins.parent.last_git_sha.clone();
    pins.git.pins.parent_baseline_tip = prior.parent.last_git_sha.clone();
    pins.svn.pins.baseline_git_tree = prior.git.tree_sha.clone();
    pins.svn.pins.parent_git_tree = pins.git.tree_sha.clone();
    pins.svn.pins.previous_mergeinfo = Some(prior.svn.mergeinfo.clone());
    pins.svn.result_git_tree = pins.git.tree_sha.clone();
    pins.svn.pins.lineage.child_copy.inspected_revision = pins.child.last_svn_rev;
    let lineage = &mut pins.svn.pins.lineage.pins;
    lineage.child.child_revision = pins.child.last_svn_rev;
    lineage.child.child_mapping_revision = pins.child.last_svn_rev;
    lineage.child.parent_baseline_revision = prior.parent.last_svn_rev;
    lineage.parent_revision = pins.parent.last_svn_rev;
    pins.svn.mergeinfo = "/trunk:3-9".into();
    pins.svn
        .result_properties
        .get_mut("")
        .unwrap()
        .insert("svn:mergeinfo".into(), pins.svn.mergeinfo.clone());
    pins
}

fn rows(db: &Database) -> Vec<(String, String)> {
    db.conn()
        .prepare("SELECT key,value FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn claim_original_copy(mut pins: RefreshJobPins, prior: &RefreshJobPins) -> RefreshJobPins {
    pins.svn.pins.previous_mergeinfo = None;
    pins.svn.pins.lineage.pins.child.parent_baseline_revision =
        prior.svn.pins.lineage.child_copy.copy_from_revision;
    pins.git.pins.parent_baseline_tip = prior.git.pins.parent_baseline_tip.clone();
    pins.svn.pins.baseline_git_tree = prior.svn.pins.baseline_git_tree.clone();
    pins
}

#[test]
fn omitting_mergeinfo_cannot_discard_exact_or_partial_completed_checkpoints() {
    for drift in ["exact", "git", "svn"] {
        let f = job::Fixture::new();
        let prior = f.pins();
        let guard = f.busy();
        f.admit(prior.clone(), &guard);
        complete(&f.db, &prior, 7);
        advance_parent(&f.db);
        if drift == "git" {
            let sha = "5".repeat(40);
            f.db.conn()
                .execute(
                    "UPDATE repositories SET last_git_sha=?1 WHERE id='child'",
                    [&sha],
                )
                .unwrap();
            reposync_core::echo_receipt_scope::write_scoped_last_git_sha_kv(
                &f.db.conn(),
                "child",
                &sha,
                "t",
            )
            .unwrap();
        } else if drift == "svn" {
            f.db.conn()
                .execute(
                    "UPDATE repositories SET last_svn_rev=8 WHERE id='child'",
                    [],
                )
                .unwrap();
            f.db.set_state("last_svn_rev_child", "8").unwrap();
        }
        let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
        let pins = claim_original_copy(next(&reopened, &prior), &prior);
        let digest = pins.approval_digest().unwrap();
        let before = rows(&reopened);
        let error = reopened
            .admit_pair_refresh("admin", "second", &digest, pins, &guard)
            .unwrap_err();
        assert!(
            error.to_string().contains(if drift == "exact" {
                "advanced baseline differs"
            } else {
                "partially match"
            }),
            "{drift}: {error}"
        );
        assert_eq!(rows(&reopened), before);
        assert!(f.no_hold(None));
    }
}

#[test]
fn stored_omitted_provenance_is_refused_globally_without_releasing_its_holds() {
    let f = job::Fixture::new();
    let prior = f.pins();
    let guard = f.busy();
    f.admit(prior.clone(), &guard);
    complete(&f.db, &prior, 7);
    advance_parent(&f.db);
    let pins = next(&f.db, &prior);
    let mut op =
        f.db.admit_pair_refresh(
            "admin",
            "second",
            &pins.approval_digest().unwrap(),
            pins,
            &guard,
        )
        .unwrap();
    op.pins = claim_original_copy(op.pins, &prior);
    op.approval_digest = op.pins.approval_digest().unwrap();
    f.db.set_state(
        &format!("pair_refresh_execution_v2:document:{}", op.id),
        &serde_json::to_string(&op).unwrap(),
    )
    .unwrap();
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    let before = rows(&reopened);
    assert!(reopened
        .pair_refresh_operation(op.id)
        .unwrap_err()
        .to_string()
        .contains("advanced baseline differs"));
    assert!(reopened
        .begin_ordinary_writer_window(Some("unrelated"))
        .is_err());
    assert_eq!(rows(&reopened), before);
    assert_eq!(
        before
            .iter()
            .filter(|(key, _)| key.starts_with("pair_refresh_execution_v2:hold:"))
            .count(),
        2
    );
}

#[test]
fn duplicate_completed_receipts_fail_on_read_even_without_a_later_operation() {
    use sha2::{Digest, Sha256};
    let f = job::Fixture::new();
    let pins = f.pins();
    let guard = f.busy();
    f.admit(pins.clone(), &guard);
    complete(&f.db, &pins, 7);
    let original = f.db.pair_refresh_operation(pins.git.operation_id).unwrap();
    let mut duplicate = original.clone();
    duplicate.id = Uuid::new_v4();
    duplicate.pins.git.operation_id = duplicate.id;
    duplicate.pins.svn.operation_id = duplicate.id;
    duplicate.svn_receipt.as_mut().unwrap().operation_id = duplicate.id;
    duplicate.request_id = "duplicate".into();
    duplicate.approval_digest = duplicate.pins.approval_digest().unwrap();
    f.db.set_state(
        &format!("pair_refresh_execution_v2:document:{}", duplicate.id),
        &serde_json::to_string(&duplicate).unwrap(),
    )
    .unwrap();
    let request_digest = hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            &duplicate.initiator_id,
            &duplicate.request_id,
            &duplicate.pins.child.scope_uuid,
        ))
        .unwrap(),
    ));
    f.db.set_state(
        &format!("pair_refresh_execution_v2:request:{request_digest}"),
        &duplicate.id.to_string(),
    )
    .unwrap();
    let reopened = Database::new(f.dir.path().join("reposync.db")).unwrap();
    let before = rows(&reopened);
    assert!(reopened
        .pair_refresh_operation(original.id)
        .unwrap_err()
        .to_string()
        .contains("duplicate completed child receipt"));
    assert!(reopened.pair_refresh_operation(duplicate.id).is_err());
    assert!(reopened
        .begin_ordinary_writer_window(Some("unrelated"))
        .is_err());
    assert_eq!(rows(&reopened), before);
}
