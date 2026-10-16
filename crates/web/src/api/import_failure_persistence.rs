use super::{
    apply_import_failure_evidence, persist_admitted_outcome, propagate_child_admission_refusal,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::api::status::AppError;
use crate::AppState;
use axum::extract::{Path, Query, State};
use reposync_core::config::AppConfig;
use reposync_core::db::import_operations::ImportOperationState;
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::{ImportPhase, ImportProgress};
use reposync_core::late_pair_publish::LatePairPublishRefusal;
use reposync_core::models::Repository;
use reposync_core::svn::SvnClient;
use reposync_core::sync_engine::SyncEngine;

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
fn resume_checkpoint_note_yields_to_an_unpersisted_failure_without_relabeling_lifecycle() {
    let mut object = serde_json::Map::new();
    object.insert("lifecycle".into(), serde_json::json!("running"));
    apply_import_failure_evidence(
        &mut object,
        Some("Resuming import from durable checkpoint"),
        &["import resume stopped with error: svn info refused".into()],
    );
    assert_eq!(object["lifecycle"], "running");
    assert_eq!(
        object["outcome_detail"],
        "import resume stopped with error: svn info refused"
    );
    assert_eq!(object["error"], object["outcome_detail"]);
}

#[test]
fn resume_checkpoint_note_stays_when_no_failure_was_recorded() {
    let mut object = serde_json::Map::new();
    object.insert("lifecycle".into(), serde_json::json!("running"));
    apply_import_failure_evidence(
        &mut object,
        Some("  Resuming import from durable checkpoint  "),
        &["   ".into()],
    );
    assert_eq!(object["lifecycle"], "running");
    assert_eq!(
        object["outcome_detail"],
        "Resuming import from durable checkpoint"
    );
    assert!(object.get("error").is_none());
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
    let AppError::BadRequest(message) = propagate_child_admission_refusal(
        &db,
        "parent",
        &refusal,
        Some(super::ChildAdmissionIdentity {
            fingerprint: "fingerprint",
            git_branch: "feature",
            svn_branch: "branches/feature",
        }),
    ) else {
        panic!("child admission refusal must stay a bad request");
    };
    assert!(message.starts_with("replay_failed: missing git ref"));
    assert!(message.contains("database_error: database error: database is locked"));
}

#[test]
fn completed_branch_does_not_taint_a_different_operation_failure() {
    let (_dir, db) = admitted_db();
    let mut operation = db
        .create_late_pair_publish_operation(
            "parent",
            "user",
            "request-a",
            "fingerprint-a",
            "feature-a",
            "branches/feature-a",
            "a".repeat(40).as_str(),
            "b".repeat(40).as_str(),
            1,
            1,
            "v1",
        )
        .unwrap();
    operation.state =
        reposync_core::db::late_pair_publish_operations::LatePairPublishState::Completed;
    operation.outcome_detail = Some("branch A retained replay error".into());
    db.update_late_pair_publish_operation(operation).unwrap();
    let refusal = LatePairPublishRefusal {
        reason: "database_error".into(),
        detail: "database error: database is locked".into(),
        plan: None,
    };
    let AppError::BadRequest(message) = propagate_child_admission_refusal(
        &db,
        "parent",
        &refusal,
        Some(super::ChildAdmissionIdentity {
            fingerprint: "fingerprint-b",
            git_branch: "feature-b",
            svn_branch: "branches/feature-b",
        }),
    ) else {
        panic!("child admission refusal must stay a bad request");
    };
    assert_eq!(
        message,
        "database_error: database error: database is locked"
    );
    assert!(!message.contains("branch A retained replay error"));
}

#[test]
fn missing_admission_identity_returns_only_the_current_refusal() {
    let (_dir, db) = admitted_db();
    let mut operation = db
        .create_late_pair_publish_operation(
            "parent",
            "user",
            "request-a",
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
    operation.outcome_detail = Some("same operation detail".into());
    db.update_late_pair_publish_operation(operation).unwrap();
    let refusal = LatePairPublishRefusal {
        reason: "database_error".into(),
        detail: "database error: database is locked".into(),
        plan: None,
    };
    let AppError::BadRequest(message) =
        propagate_child_admission_refusal(&db, "parent", &refusal, None)
    else {
        panic!("child admission refusal must stay a bad request");
    };
    assert_eq!(
        message,
        "database_error: database error: database is locked"
    );
}

const STORED_LATE_PAIR_FAILURE: &str = "replay_failed: missing git ref";
const CURRENT_LOCK_REFUSAL: &str = "database_error: database error: database is locked";

fn late_pair_operation_with_stored_failure(db: &Database) {
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
    operation.outcome_detail = Some(STORED_LATE_PAIR_FAILURE.into());
    db.update_late_pair_publish_operation(operation).unwrap();
}

fn assert_late_pair_fallback_does_not_match(db: &Database) {
    let refusal = LatePairPublishRefusal {
        reason: "database_error".into(),
        detail: "database error: database is locked".into(),
        plan: None,
    };
    let AppError::BadRequest(message) = propagate_child_admission_refusal(
        db,
        "parent",
        &refusal,
        Some(super::ChildAdmissionIdentity {
            fingerprint: "fingerprint",
            git_branch: "feature",
            svn_branch: "branches/feature",
        }),
    ) else {
        panic!("child admission refusal must stay a bad request");
    };
    assert_eq!(message, CURRENT_LOCK_REFUSAL);
    assert!(!message.contains(STORED_LATE_PAIR_FAILURE));
}

#[test]
fn late_pair_error_fallback_misses_when_only_the_fingerprint_differs() {
    let (_dir, db) = admitted_db();
    late_pair_operation_with_stored_failure(&db);
    let mut operation = db
        .latest_late_pair_publish_operation("parent")
        .unwrap()
        .unwrap();
    operation.target_fingerprint = "fingerprint-other".into();
    db.update_late_pair_publish_operation(operation).unwrap();
    let stored = db
        .latest_late_pair_publish_operation("parent")
        .unwrap()
        .unwrap();
    assert_eq!(stored.target_fingerprint, "fingerprint-other");
    assert_eq!(stored.git_branch, "feature");
    assert_eq!(stored.svn_branch, "branches/feature");
    assert!(!stored.state.is_terminal());
    assert_late_pair_fallback_does_not_match(&db);
}

#[test]
fn late_pair_error_fallback_misses_when_only_the_git_branch_differs() {
    let (_dir, db) = admitted_db();
    late_pair_operation_with_stored_failure(&db);
    let mut operation = db
        .latest_late_pair_publish_operation("parent")
        .unwrap()
        .unwrap();
    operation.git_branch = "feature-other".into();
    db.update_late_pair_publish_operation(operation).unwrap();
    let stored = db
        .latest_late_pair_publish_operation("parent")
        .unwrap()
        .unwrap();
    assert_eq!(stored.target_fingerprint, "fingerprint");
    assert_eq!(stored.git_branch, "feature-other");
    assert_eq!(stored.svn_branch, "branches/feature");
    assert!(!stored.state.is_terminal());
    assert_late_pair_fallback_does_not_match(&db);
}

#[test]
fn late_pair_error_fallback_misses_when_only_the_svn_branch_differs() {
    let (_dir, db) = admitted_db();
    late_pair_operation_with_stored_failure(&db);
    let mut operation = db
        .latest_late_pair_publish_operation("parent")
        .unwrap()
        .unwrap();
    operation.svn_branch = "branches/feature-other".into();
    db.update_late_pair_publish_operation(operation).unwrap();
    let stored = db
        .latest_late_pair_publish_operation("parent")
        .unwrap()
        .unwrap();
    assert_eq!(stored.target_fingerprint, "fingerprint");
    assert_eq!(stored.git_branch, "feature");
    assert_eq!(stored.svn_branch, "branches/feature-other");
    assert!(!stored.state.is_terminal());
    assert_late_pair_fallback_does_not_match(&db);
}

#[test]
fn late_pair_error_fallback_misses_when_only_the_lifecycle_is_terminal() {
    let (_dir, db) = admitted_db();
    late_pair_operation_with_stored_failure(&db);
    let mut operation = db
        .latest_late_pair_publish_operation("parent")
        .unwrap()
        .unwrap();
    operation.state =
        reposync_core::db::late_pair_publish_operations::LatePairPublishState::Completed;
    db.update_late_pair_publish_operation(operation).unwrap();
    let stored = db
        .latest_late_pair_publish_operation("parent")
        .unwrap()
        .unwrap();
    assert_eq!(stored.target_fingerprint, "fingerprint");
    assert_eq!(stored.git_branch, "feature");
    assert_eq!(stored.svn_branch, "branches/feature");
    assert!(stored.state.is_terminal());
    assert_late_pair_fallback_does_not_match(&db);
}

#[test]
fn child_admission_lock_without_stored_detail_is_not_relabeled() {
    let (_dir, db) = admitted_db();
    let refusal = LatePairPublishRefusal {
        reason: "database_error".into(),
        detail: "database error: database is locked".into(),
        plan: None,
    };
    let AppError::BadRequest(message) =
        propagate_child_admission_refusal(&db, "parent", &refusal, None)
    else {
        panic!("child admission refusal must stay a bad request");
    };
    assert_eq!(
        message,
        "database_error: database error: database is locked"
    );
}

struct OpenSubscriber {
    opens: Arc<AtomicUsize>,
}

impl tracing::Subscriber for OpenSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "reposync_core::db"
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = MessageVisitor { message: None };
        event.record(&mut visitor);
        if visitor
            .message
            .as_deref()
            .is_some_and(|message| message.contains("opening database"))
        {
            self.opens.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

struct MessageVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn repo_import_handler_persists_failure_on_the_admitted_database() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    db.insert_repository(&Repository {
        id: "repo-import".into(),
        name: "import".into(),
        svn_url: "file:///nonexistent".into(),
        svn_branch: "trunk".into(),
        svn_username: "fixture".into(),
        git_provider: "gitea".into(),
        git_api_url: format!("file://{}", dir.path().display()),
        git_repo: "empty".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 30,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: false,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".into(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    })
    .unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"file:///nonexistent\"\nusername = \"fixture\"\n[github]\nrepo = \"local/import\"\n",
            dir.path().display()
        ),
    )
    .unwrap();
    let mut config = AppConfig::load_from_file(&config_path).unwrap();
    config.web.admin_password = Some("secret".into());
    let bare = dir.path().join("empty.git");
    assert!(std::process::Command::new("git")
        .args(["init", "--bare", bare.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let git_repo = dir
        .path()
        .join("repos")
        .join("repo-import")
        .join("git-repo");
    std::fs::create_dir_all(&git_repo).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "--initial-branch", "main"])
        .current_dir(&git_repo)
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args(["remote", "add", "origin", bare.to_str().unwrap()])
        .current_dir(&git_repo)
        .status()
        .unwrap()
        .success());
    let dummy = dir.path().join("dummy-git");
    std::fs::create_dir_all(&dummy).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init"])
        .current_dir(&dummy)
        .status()
        .unwrap()
        .success());
    let engine = SyncEngine::new(
        config.clone(),
        Database::in_memory().unwrap(),
        SvnClient::new("file:///nonexistent", "fixture", ""),
        GitClient::new(&dummy).unwrap(),
        Arc::new(IdentityMapper::new(&reposync_core::config::IdentityConfig::default()).unwrap()),
    );
    let (trigger, _rx) = tokio::sync::mpsc::channel(1);
    let (ws, _) = tokio::sync::broadcast::channel(1);
    let state = Arc::new(AppState {
        db,
        sync_engine: Arc::new(engine),
        config,
        sync_trigger: trigger,
        ws_broadcast: ws,
        sessions: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        config_path,
        prev_net_snapshot: std::sync::Mutex::new(None),
        repo_import_progress: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        login_attempts: std::sync::Mutex::new(std::collections::HashMap::new()),
        import_handles: tokio::sync::Mutex::new(Vec::new()),
    });
    state.sessions.write().await.insert(
        "token".into(),
        chrono::Utc::now() + chrono::Duration::hours(1),
    );
    let opens = Arc::new(AtomicUsize::new(0));
    let _tracing = tracing::subscriber::set_default(OpenSubscriber {
        opens: opens.clone(),
    });
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        axum::http::HeaderValue::from_static("Bearer token"),
    );
    let started = match super::start_repo_import(
        State(state.clone()),
        headers,
        Path("repo-import".into()),
        Query(super::ImportQuery::default()),
        axum::body::Bytes::new(),
    )
    .await
    {
        Ok(started) => started,
        Err(error) => panic!(
            "import handler did not admit work on the existing database: {}",
            match error {
                AppError::BadRequest(message)
                | AppError::NotFound(message)
                | AppError::Unauthorized(message)
                | AppError::Forbidden(message)
                | AppError::Conflict(message)
                | AppError::Internal(message) => message,
            }
        ),
    };
    let operation_id = started.0["operation_id"].as_str().unwrap().to_string();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let stored = loop {
        let current = state
            .db
            .get_import_operation("repo-import", &operation_id)
            .unwrap()
            .unwrap();
        if current.state == ImportOperationState::Failed {
            break current;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "repo import did not persist the first failure on the admitted database: {:?} {}",
            current.state,
            current.outcome_detail.unwrap_or_default()
        );
        tokio::task::yield_now().await;
    };
    let detail = stored.outcome_detail.unwrap_or_default();
    assert!(
        detail.starts_with("import stopped with error; inspect local work:"),
        "{detail}"
    );
    assert_eq!(
        opens.load(Ordering::SeqCst),
        0,
        "failure persistence must reuse the admitted database instead of opening another"
    );
}

fn app_error_message(error: AppError) -> String {
    match error {
        AppError::BadRequest(message)
        | AppError::NotFound(message)
        | AppError::Unauthorized(message)
        | AppError::Forbidden(message)
        | AppError::Conflict(message)
        | AppError::Internal(message) => message,
    }
}

fn authorize_partial_resume(db: &Database, repo_id: &str) -> String {
    let operation = db
        .create_import_operation(repo_id, "admin", "resume-request", "fingerprint")
        .unwrap();
    db.start_import_operation(repo_id, &operation.id).unwrap();
    db.note_import_total(repo_id, &operation.id, 3).unwrap();
    let tip = "a".repeat(40);
    db.note_import_local(repo_id, &operation.id, 1, &tip, 1, 1)
        .unwrap();
    db.begin_import_publication(repo_id, &operation.id, "refs/heads/main", &tip)
        .unwrap();
    db.confirm_import_publication(repo_id, &operation.id, &tip)
        .unwrap();
    db.finish_import_operation(
        repo_id,
        &operation.id,
        ImportOperationState::ReconciliationRequired,
        "worker stopped before resume",
    )
    .unwrap();
    let mut held = db
        .get_import_operation(repo_id, &operation.id)
        .unwrap()
        .unwrap();
    assert!(
        reposync_core::db::import_operations::import_resume_checkpoint(&held).is_some(),
        "fixture must be a confirmed partial checkpoint"
    );
    held.resume_authorized = true;
    db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key=?2",
            rusqlite::params![
                serde_json::to_string(&held).unwrap(),
                format!("import_operation_v1:document:{}", operation.id)
            ],
        )
        .unwrap();
    operation.id
}

/// Abort every update that would store the resume failure detail.
///
/// The trigger stays installed. It is not removed after it fires, and a later
/// write of that detail would abort too. The handler attempts the terminal
/// write once. This does not show that a later write would succeed.
fn abort_resume_failure_terminal_writes(db: &Database, operation_id: &str) {
    let key = format!("import_operation_v1:document:{operation_id}");
    db.conn()
        .execute_batch(&format!(
            "CREATE TRIGGER abort_resume_failure_terminal_writes
             BEFORE UPDATE ON kv_state
             WHEN NEW.key = '{key}'
              AND instr(NEW.value, 'import resume stopped with error') > 0
             BEGIN
               SELECT RAISE(ABORT, 'resume failure terminal write aborted');
             END;"
        ))
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn resume_handler_shows_unpersisted_failure_when_terminal_write_fails_once() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let repo_id = "repo-resume";
    let now = chrono::Utc::now().to_rfc3339();
    db.insert_repository(&Repository {
        id: repo_id.into(),
        name: "resume".into(),
        svn_url: "file:///nonexistent".into(),
        svn_branch: "trunk".into(),
        svn_username: "fixture".into(),
        git_provider: "gitea".into(),
        git_api_url: format!("file://{}", dir.path().display()),
        git_repo: "empty".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 30,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: false,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".into(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    })
    .unwrap();
    let operation_id = authorize_partial_resume(&db, repo_id);
    abort_resume_failure_terminal_writes(&db, &operation_id);
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"file:///nonexistent\"\nusername = \"fixture\"\n[github]\nrepo = \"local/resume\"\n",
            dir.path().display()
        ),
    )
    .unwrap();
    let mut config = AppConfig::load_from_file(&config_path).unwrap();
    config.web.admin_password = Some("secret".into());
    let bare = dir.path().join("empty.git");
    assert!(std::process::Command::new("git")
        .args(["init", "--bare", bare.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let git_repo = dir.path().join("repos").join(repo_id).join("git-repo");
    std::fs::create_dir_all(&git_repo).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "--initial-branch", "main"])
        .current_dir(&git_repo)
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args(["remote", "add", "origin", bare.to_str().unwrap()])
        .current_dir(&git_repo)
        .status()
        .unwrap()
        .success());
    let dummy = dir.path().join("dummy-git");
    std::fs::create_dir_all(&dummy).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init"])
        .current_dir(&dummy)
        .status()
        .unwrap()
        .success());
    let engine = SyncEngine::new(
        config.clone(),
        Database::in_memory().unwrap(),
        SvnClient::new("file:///nonexistent", "fixture", ""),
        GitClient::new(&dummy).unwrap(),
        Arc::new(IdentityMapper::new(&reposync_core::config::IdentityConfig::default()).unwrap()),
    );
    let (trigger, _rx) = tokio::sync::mpsc::channel(1);
    let (ws, _) = tokio::sync::broadcast::channel(1);
    let state = Arc::new(AppState {
        db,
        sync_engine: Arc::new(engine),
        config,
        sync_trigger: trigger,
        ws_broadcast: ws,
        sessions: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        config_path,
        prev_net_snapshot: std::sync::Mutex::new(None),
        repo_import_progress: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        login_attempts: std::sync::Mutex::new(std::collections::HashMap::new()),
        import_handles: tokio::sync::Mutex::new(Vec::new()),
    });
    state.sessions.write().await.insert(
        "token".into(),
        chrono::Utc::now() + chrono::Duration::hours(1),
    );
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        axum::http::HeaderValue::from_static("Bearer token"),
    );
    let resumed = super::resume_repo_import(
        State(state.clone()),
        headers.clone(),
        Path((repo_id.into(), operation_id.clone())),
    )
    .await
    .unwrap_or_else(|error| panic!("resume handler refused: {}", app_error_message(error)));
    assert_eq!(resumed.0["ok"], true);
    assert_eq!(resumed.0["lifecycle"], "running");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let progress = state.get_repo_import_progress(repo_id).await;
        let current = progress.read().await;
        if current.phase == ImportPhase::Failed {
            assert!(
                current
                    .errors
                    .iter()
                    .any(|detail| { detail.starts_with("import resume stopped with error:") }),
                "in-memory errors: {:?}",
                current.errors
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "resumed import did not fail: {:?}",
            current.phase
        );
        drop(current);
        tokio::task::yield_now().await;
    }
    let status = super::repo_import_status(State(state.clone()), headers, Path(repo_id.into()))
        .await
        .unwrap_or_else(|error| panic!("status handler failed: {}", app_error_message(error)));
    let detail = status.0["outcome_detail"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        detail.starts_with("import resume stopped with error:"),
        "{detail}"
    );
    assert!(
        !detail.contains("Resuming import from durable checkpoint"),
        "{detail}"
    );
    assert_eq!(status.0["error"], status.0["outcome_detail"]);
    assert_eq!(status.0["phase"], "failed");
    assert_eq!(status.0["lifecycle"], "running");
    let stored = state
        .db
        .get_import_operation(repo_id, &operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, ImportOperationState::Running);
    assert_eq!(
        stored.outcome_detail.as_deref(),
        Some("Resuming import from durable checkpoint")
    );
}

/// Accept one SVN connection and hold it until `release` is signaled.
/// Dropping the socket then makes that attempt fail.
struct HeldSvn {
    port: u16,
    release: std::sync::mpsc::Sender<()>,
    accepted: Arc<AtomicBool>,
}

impl HeldSvn {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicBool::new(false));
        let flag = accepted.clone();
        let (release, release_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || loop {
            if release_rx.try_recv().is_ok() {
                break;
            }
            match listener.accept() {
                Ok((mut sock, _)) => {
                    flag.store(true, Ordering::SeqCst);
                    let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(200)));
                    let mut buf = [0_u8; 2048];
                    let _ = std::io::Read::read(&mut sock, &mut buf);
                    let _ = release_rx.recv();
                    let _ = sock.shutdown(std::net::Shutdown::Both);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(_) => break,
            }
        });
        Self {
            port,
            release,
            accepted,
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn resume_status_ignores_previous_attempt_error_then_shows_current_failure() {
    const PREVIOUS: &str = "previous-attempt-error: stale checkpoint export";
    let held = HeldSvn::start();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let repo_id = "repo-resume-current";
    let svn_url = format!("http://127.0.0.1:{}/import", held.port);
    let now = chrono::Utc::now().to_rfc3339();
    db.insert_repository(&Repository {
        id: repo_id.into(),
        name: "resume".into(),
        svn_url: svn_url.clone(),
        svn_branch: "trunk".into(),
        svn_username: "fixture".into(),
        git_provider: "gitea".into(),
        git_api_url: format!("file://{}", dir.path().display()),
        git_repo: "empty".into(),
        git_branch: "main".into(),
        sync_mode: "team".into(),
        poll_interval_secs: 30,
        lfs_threshold_mb: 0,
        auto_merge: false,
        enabled: false,
        created_by: None,
        parent_id: None,
        created_at: now.clone(),
        updated_at: now,
        last_svn_rev: 0,
        last_git_sha: String::new(),
        last_sync_at: None,
        sync_status: "idle".into(),
        total_syncs: 0,
        total_errors: 0,
        allowed_paths: None,
        blocked_patterns: None,
        consecutive_errors: 0,
        teams_webhook_url: None,
    })
    .unwrap();
    let operation_id = authorize_partial_resume(&db, repo_id);
    abort_resume_failure_terminal_writes(&db, &operation_id);
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"{svn_url}\"\nusername = \"fixture\"\n[github]\nrepo = \"local/resume\"\n",
            dir.path().display()
        ),
    )
    .unwrap();
    let mut config = AppConfig::load_from_file(&config_path).unwrap();
    config.web.admin_password = Some("secret".into());
    let bare = dir.path().join("empty.git");
    assert!(std::process::Command::new("git")
        .args(["init", "--bare", bare.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let git_repo = dir.path().join("repos").join(repo_id).join("git-repo");
    std::fs::create_dir_all(&git_repo).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "--initial-branch", "main"])
        .current_dir(&git_repo)
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args(["remote", "add", "origin", bare.to_str().unwrap()])
        .current_dir(&git_repo)
        .status()
        .unwrap()
        .success());
    let dummy = dir.path().join("dummy-git");
    std::fs::create_dir_all(&dummy).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init"])
        .current_dir(&dummy)
        .status()
        .unwrap()
        .success());
    let engine = SyncEngine::new(
        config.clone(),
        Database::in_memory().unwrap(),
        SvnClient::new(&svn_url, "fixture", ""),
        GitClient::new(&dummy).unwrap(),
        Arc::new(IdentityMapper::new(&reposync_core::config::IdentityConfig::default()).unwrap()),
    );
    let (trigger, _rx) = tokio::sync::mpsc::channel(1);
    let (ws, _) = tokio::sync::broadcast::channel(1);
    let state = Arc::new(AppState {
        db,
        sync_engine: Arc::new(engine),
        config,
        sync_trigger: trigger,
        ws_broadcast: ws,
        sessions: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        import_progress: Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
        config_path,
        prev_net_snapshot: std::sync::Mutex::new(None),
        repo_import_progress: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        login_attempts: std::sync::Mutex::new(std::collections::HashMap::new()),
        import_handles: tokio::sync::Mutex::new(Vec::new()),
    });
    {
        let progress = state.get_repo_import_progress(repo_id).await;
        let mut current = progress.write().await;
        current.phase = ImportPhase::Failed;
        current.errors.push(PREVIOUS.into());
    }
    state.sessions.write().await.insert(
        "token".into(),
        chrono::Utc::now() + chrono::Duration::hours(1),
    );
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        axum::http::HeaderValue::from_static("Bearer token"),
    );
    let resumed = super::resume_repo_import(
        State(state.clone()),
        headers.clone(),
        Path((repo_id.into(), operation_id.clone())),
    )
    .await
    .unwrap_or_else(|error| panic!("resume handler refused: {}", app_error_message(error)));
    assert_eq!(resumed.0["ok"], true);
    assert_eq!(resumed.0["lifecycle"], "running");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if held.accepted.load(Ordering::SeqCst) {
            break;
        }
        let progress = state.get_repo_import_progress(repo_id).await;
        let current = progress.read().await;
        assert!(
            current.phase != ImportPhase::Failed,
            "resumed worker failed before the healthy status check: {:?}",
            current.errors
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "svn client did not connect during the healthy resume"
        );
        drop(current);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let healthy =
        super::repo_import_status(State(state.clone()), headers.clone(), Path(repo_id.into()))
            .await
            .unwrap_or_else(|error| panic!("status handler failed: {}", app_error_message(error)));
    assert_eq!(
        healthy.0["outcome_detail"],
        "Resuming import from durable checkpoint"
    );
    assert!(healthy.0.get("error").is_none());
    assert_eq!(healthy.0["errors"].as_array().map(Vec::len), Some(0));
    assert_eq!(healthy.0["lifecycle"], "running");
    assert_eq!(healthy.0["phase"], "importing");
    assert!(
        !healthy.0.to_string().contains(PREVIOUS),
        "healthy resume showed the previous attempt: {}",
        healthy.0
    );
    held.release.send(()).unwrap();
    let failure_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let current_failure = loop {
        let progress = state.get_repo_import_progress(repo_id).await;
        let current = progress.read().await;
        if current.phase == ImportPhase::Failed {
            let detail = current
                .errors
                .iter()
                .find(|detail| detail.starts_with("import resume stopped with error:"))
                .cloned()
                .unwrap_or_else(|| panic!("current failure missing: {:?}", current.errors));
            assert!(
                !current
                    .errors
                    .iter()
                    .any(|detail| detail.contains(PREVIOUS)),
                "previous attempt still in progress errors: {:?}",
                current.errors
            );
            break detail;
        }
        assert!(
            tokio::time::Instant::now() < failure_deadline,
            "current resume did not fail: {:?}",
            current.phase
        );
        drop(current);
        tokio::task::yield_now().await;
    };
    let status = super::repo_import_status(State(state.clone()), headers, Path(repo_id.into()))
        .await
        .unwrap_or_else(|error| panic!("status handler failed: {}", app_error_message(error)));
    assert_eq!(status.0["outcome_detail"], current_failure.trim());
    assert!(current_failure.trim().contains("failed to get SVN info"));
    assert_eq!(status.0["error"], status.0["outcome_detail"]);
    assert!(
        !status.0.to_string().contains(PREVIOUS),
        "current failure status showed the previous attempt: {}",
        status.0
    );
    assert_ne!(
        status.0["outcome_detail"],
        "Resuming import from durable checkpoint"
    );
    assert_eq!(status.0["phase"], "failed");
    assert_eq!(status.0["lifecycle"], "running");
    let stored = state
        .db
        .get_import_operation(repo_id, &operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, ImportOperationState::Running);
    assert_eq!(
        stored.outcome_detail.as_deref(),
        Some("Resuming import from durable checkpoint")
    );
}
