use super::{
    apply_import_failure_evidence, persist_admitted_outcome, propagate_child_admission_refusal,
};
use crate::api::status::AppError;
use reposync_core::db::import_operations::ImportOperationState;
use reposync_core::db::Database;
use reposync_core::late_pair_publish::LatePairPublishRefusal;

fn admitted_db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("reposync.db")).unwrap();
    db.initialize().unwrap();
    (dir, db)
}

#[test]
fn admitted_handle_persists_the_first_import_failure() {
    let (_dir, db) = admitted_db();
    let operation = db
        .create_import_operation("repo-a", "user", "request", "fingerprint")
        .unwrap();
    assert_eq!(operation.state, ImportOperationState::Queued);
    let detail = "import stopped with error; inspect local work: svn export refused";
    assert!(persist_admitted_outcome(
        &db,
        "repo-a",
        &operation.id,
        ImportOperationState::Failed,
        detail,
    )
    .is_none());
    let stored = db
        .get_import_operation("repo-a", &operation.id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, ImportOperationState::Failed);
    assert_eq!(stored.outcome_detail.as_deref(), Some(detail));
}

#[test]
fn empty_durable_detail_reports_the_first_failure_without_relabeling_lifecycle() {
    let mut object = serde_json::Map::new();
    object.insert("lifecycle".into(), serde_json::json!("running"));
    apply_import_failure_evidence(
        &mut object,
        None,
        &["import stopped with error; inspect local work: first".into()],
    );
    assert_eq!(object["lifecycle"], "running");
    assert_eq!(
        object["outcome_detail"],
        "import stopped with error; inspect local work: first"
    );
    assert_eq!(object["error"], object["outcome_detail"]);
}

#[test]
fn durable_detail_is_not_replaced_by_a_later_lock_message() {
    let mut object = serde_json::Map::new();
    apply_import_failure_evidence(
        &mut object,
        Some("import stopped with error; inspect local work: first"),
        &["database error: database is locked".into()],
    );
    assert_eq!(
        object["outcome_detail"],
        "import stopped with error; inspect local work: first"
    );
    assert!(object.get("error").is_none());
}

#[test]
fn child_admission_lock_keeps_a_stored_first_failure() {
    let (_dir, db) = admitted_db();
    let mut operation = db
        .create_late_pair_publish_operation(
            "parent",
            "user",
            "request",
            "fingerprint",
            "feature",
            "branches/feature",
            "a".repeat(40).as_str(),
            "b".repeat(40).as_str(),
            1,
            1,
            "v1",
        )
        .unwrap();
    operation.outcome_detail = Some("replay_failed: missing git ref".into());
    db.update_late_pair_publish_operation(operation).unwrap();
    let refusal = LatePairPublishRefusal {
        reason: "database_error".into(),
        detail: "database error: database is locked".into(),
        plan: None,
    };
    let AppError::BadRequest(message) = propagate_child_admission_refusal(&db, "parent", &refusal)
    else {
        panic!("child admission refusal must stay a bad request");
    };
    assert!(message.starts_with("replay_failed: missing git ref"));
    assert!(message.contains("database_error: database error: database is locked"));
}

#[test]
fn child_admission_lock_without_stored_detail_is_not_relabeled() {
    let (_dir, db) = admitted_db();
    let refusal = LatePairPublishRefusal {
        reason: "database_error".into(),
        detail: "database error: database is locked".into(),
        plan: None,
    };
    let AppError::BadRequest(message) = propagate_child_admission_refusal(&db, "parent", &refusal)
    else {
        panic!("child admission refusal must stay a bad request");
    };
    assert_eq!(
        message,
        "database_error: database error: database is locked"
    );
}
