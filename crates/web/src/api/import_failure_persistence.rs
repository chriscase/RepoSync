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
        scheduler_service: std::sync::Mutex::new(crate::SchedulerServiceState::running()),
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
        scheduler_service: std::sync::Mutex::new(crate::SchedulerServiceState::running()),
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
        scheduler_service: std::sync::Mutex::new(crate::SchedulerServiceState::running()),
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

async fn credential_http_state(
    repo_id: &str,
) -> (tempfile::TempDir, Arc<AppState>, axum::http::HeaderMap) {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(dir.path().join("reposync.db")).unwrap();
    db.initialize().unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    db.insert_repository(&Repository {
        id: repo_id.into(),
        name: repo_id.into(),
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
        scheduler_service: std::sync::Mutex::new(crate::SchedulerServiceState::running()),
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
    (dir, state, headers)
}

#[tokio::test(flavor = "current_thread")]
async fn start_import_revocation_does_not_claim_a_worker() {
    let repo_id = "import-revoked";
    let (_dir, state, headers) = credential_http_state(repo_id).await;
    state
        .db
        .set_state(&format!("secret_svn_password_{repo_id}"), "")
        .unwrap();
    let err = super::start_repo_import(
        State(state.clone()),
        headers.clone(),
        Path(repo_id.into()),
        Query(super::ImportQuery::default()),
        axum::body::Bytes::new(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, AppError::Forbidden(_)),
        "{}",
        app_error_message(err)
    );
    assert!(state.db.latest_import_operation(repo_id).unwrap().is_none());
    let progress = state.get_repo_import_progress(repo_id).await;
    assert_ne!(progress.read().await.phase, ImportPhase::Importing);

    state
        .db
        .set_state(&format!("secret_svn_password_{repo_id}"), "restored")
        .unwrap();
    state
        .db
        .set_state(&format!("secret_git_token_{repo_id}"), "restored")
        .unwrap();
    let imported_sha = "a".repeat(40);
    state
        .db
        .conn()
        .execute(
            "UPDATE repositories SET last_svn_rev = 5, last_git_sha = ?1 WHERE id = ?2",
            rusqlite::params![imported_sha, repo_id],
        )
        .unwrap();
    let recovered = super::start_repo_import(
        State(state.clone()),
        headers,
        Path(repo_id.into()),
        Query(super::ImportQuery::default()),
        axum::body::Bytes::new(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(recovered, AppError::BadRequest(_)),
        "corrected credentials must pass; a non-pending baseline is a later truthful refusal: {}",
        app_error_message(recovered)
    );
    assert!(state.db.latest_import_operation(repo_id).unwrap().is_none());
    let progress = state.get_repo_import_progress(repo_id).await;
    assert_ne!(progress.read().await.phase, ImportPhase::Importing);
}

#[tokio::test(flavor = "current_thread")]
async fn start_import_read_failure_does_not_claim_a_worker() {
    let repo_id = "import-read-fail";
    let (_dir, state, headers) = credential_http_state(repo_id).await;
    state
        .db
        .conn()
        .execute("ALTER TABLE kv_state RENAME TO kv_state_hidden", [])
        .unwrap();
    let err = super::start_repo_import(
        State(state.clone()),
        headers.clone(),
        Path(repo_id.into()),
        Query(super::ImportQuery::default()),
        axum::body::Bytes::new(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, AppError::Internal(_)),
        "{}",
        app_error_message(err)
    );
    state
        .db
        .conn()
        .execute("ALTER TABLE kv_state_hidden RENAME TO kv_state", [])
        .unwrap();
    assert!(state.db.latest_import_operation(repo_id).unwrap().is_none());
    state
        .db
        .set_state(&format!("secret_svn_password_{repo_id}"), "restored")
        .unwrap();
    state
        .db
        .set_state(&format!("secret_git_token_{repo_id}"), "restored")
        .unwrap();
    let imported_sha = "a".repeat(40);
    state
        .db
        .conn()
        .execute(
            "UPDATE repositories SET last_svn_rev = 5, last_git_sha = ?1 WHERE id = ?2",
            rusqlite::params![imported_sha, repo_id],
        )
        .unwrap();
    let recovered = super::start_repo_import(
        State(state.clone()),
        headers,
        Path(repo_id.into()),
        Query(super::ImportQuery::default()),
        axum::body::Bytes::new(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(recovered, AppError::BadRequest(_)),
        "restored chain must pass; a non-pending baseline is a later truthful refusal: {}",
        app_error_message(recovered)
    );
    assert!(state.db.latest_import_operation(repo_id).unwrap().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn resume_import_revocation_keeps_resume_authorization() {
    let repo_id = "import-resume-revoked";
    let (_dir, state, headers) = credential_http_state(repo_id).await;
    let operation_id = authorize_partial_resume(&state.db, repo_id);
    state
        .db
        .set_state(&format!("secret_git_token_{repo_id}"), "")
        .unwrap();
    let err = super::resume_repo_import(
        State(state.clone()),
        headers.clone(),
        Path((repo_id.into(), operation_id.clone())),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, AppError::Forbidden(_)),
        "{}",
        app_error_message(err)
    );
    let held = state
        .db
        .get_import_operation(repo_id, &operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(held.state, ImportOperationState::ReconciliationRequired);
    assert!(held.resume_authorized);
    let progress = state.get_repo_import_progress(repo_id).await;
    assert_ne!(progress.read().await.phase, ImportPhase::Importing);

    state
        .db
        .set_state(&format!("secret_git_token_{repo_id}"), "restored")
        .unwrap();
    state
        .db
        .set_state(&format!("secret_svn_password_{repo_id}"), "restored")
        .unwrap();
    let recovered = super::resume_repo_import(
        State(state.clone()),
        headers,
        Path((repo_id.into(), operation_id.clone())),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(recovered, AppError::BadRequest(_)),
        "corrected credentials must pass; missing workdir is a later truthful refusal: {}",
        app_error_message(recovered)
    );
    let after = state
        .db
        .get_import_operation(repo_id, &operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.state, ImportOperationState::ReconciliationRequired);
    assert!(after.resume_authorized);
}

#[tokio::test(flavor = "current_thread")]
async fn resume_import_read_failure_keeps_resume_authorization() {
    let repo_id = "import-resume-read-fail";
    let (_dir, state, headers) = credential_http_state(repo_id).await;
    let operation_id = authorize_partial_resume(&state.db, repo_id);
    state
        .db
        .conn()
        .execute("ALTER TABLE kv_state RENAME TO kv_state_hidden", [])
        .unwrap();
    let err = super::resume_repo_import(
        State(state.clone()),
        headers.clone(),
        Path((repo_id.into(), operation_id.clone())),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, AppError::Internal(_)),
        "{}",
        app_error_message(err)
    );
    state
        .db
        .conn()
        .execute("ALTER TABLE kv_state_hidden RENAME TO kv_state", [])
        .unwrap();
    let held = state
        .db
        .get_import_operation(repo_id, &operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(held.state, ImportOperationState::ReconciliationRequired);
    assert!(held.resume_authorized);

    state
        .db
        .set_state(&format!("secret_git_token_{repo_id}"), "restored")
        .unwrap();
    state
        .db
        .set_state(&format!("secret_svn_password_{repo_id}"), "restored")
        .unwrap();
    let recovered = super::resume_repo_import(
        State(state.clone()),
        headers,
        Path((repo_id.into(), operation_id.clone())),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(recovered, AppError::BadRequest(_)),
        "restored chain must pass; missing workdir is a later truthful refusal: {}",
        app_error_message(recovered)
    );
    let after = state
        .db
        .get_import_operation(repo_id, &operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.state, ImportOperationState::ReconciliationRequired);
    assert!(after.resume_authorized);
}

#[tokio::test(flavor = "current_thread")]
async fn resume_preparation_failure_keeps_authorization_then_continues() {
    let repo_id = "import-resume-prepare";
    let dir = tempfile::tempdir().unwrap();
    let svn_repo = dir.path().join("svn-repo");
    let bare = dir.path().join("origin.git");
    let git_repo = dir.path().join("repos").join(repo_id).join("git-repo");
    assert!(std::process::Command::new("svnadmin")
        .args(["create", svn_repo.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let svn_root = format!("file://{}", svn_repo.display());
    let seed = dir.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    std::fs::write(seed.join("history.txt"), b"one\n").unwrap();
    assert!(std::process::Command::new("svn")
        .args([
            "import",
            seed.to_str().unwrap(),
            &format!("{svn_root}/trunk"),
            "-m",
            "one",
            "--username",
            "fixture",
            "--non-interactive",
        ])
        .status()
        .unwrap()
        .success());
    let checkout = dir.path().join("wc");
    assert!(std::process::Command::new("svn")
        .args([
            "checkout",
            &format!("{svn_root}/trunk"),
            checkout.to_str().unwrap(),
            "--username",
            "fixture",
            "--non-interactive",
        ])
        .status()
        .unwrap()
        .success());
    std::fs::write(checkout.join("history.txt"), b"two\n").unwrap();
    assert!(std::process::Command::new("svn")
        .args(["commit", "-m", "two", "--username", "fixture"])
        .current_dir(&checkout)
        .status()
        .unwrap()
        .success());
    let uuid = std::process::Command::new("svnlook")
        .args(["uuid", svn_repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(uuid.status.success());
    let uuid = String::from_utf8(uuid.stdout).unwrap();
    let uuid = uuid.trim();
    assert!(!uuid.is_empty());
    assert!(std::process::Command::new("git")
        .args([
            "init",
            "--bare",
            "--initial-branch",
            "main",
            bare.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
    std::fs::create_dir_all(&git_repo).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&git_repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    git(&["init", "--initial-branch", "main"]);
    git(&["config", "user.name", "Fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(git_repo.join("history.txt"), b"one\n").unwrap();
    git(&["add", "history.txt"]);
    git(&["commit", "-m", "checkpoint"]);
    let origin = bare.to_str().unwrap().to_string();
    git(&["remote", "add", "origin", &origin]);
    git(&["push", "-u", "origin", "main"]);
    let tip = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout).unwrap();
    let tip = tip.trim().to_string();
    let prefix_tree = String::from_utf8(git(&["rev-parse", "HEAD^{tree}"]).stdout).unwrap();
    let prefix_tree = prefix_tree.trim().to_string();
    let bare_before = String::from_utf8(
        std::process::Command::new("git")
            .args([
                "--git-dir",
                bare.to_str().unwrap(),
                "rev-parse",
                "refs/heads/main",
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(bare_before.trim(), tip);

    let db_path = dir.path().join("reposync.db");
    let db = Database::new(&db_path).unwrap();
    db.initialize().unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    db.insert_repository(&Repository {
        id: repo_id.into(),
        name: repo_id.into(),
        svn_url: svn_root.clone(),
        svn_branch: "trunk".into(),
        svn_username: "fixture".into(),
        git_provider: "gitea".into(),
        git_api_url: format!("file://{}", dir.path().display()),
        git_repo: "local/history".into(),
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
    reposync_core::db::lineage_checkpoints::record_independently_established_svn_uuid(
        &db, repo_id, uuid,
    )
    .unwrap();
    db.set_state(&format!("secret_svn_password_{repo_id}"), "fixture")
        .unwrap();
    db.set_state(&format!("secret_git_token_{repo_id}"), "dummy-token")
        .unwrap();
    let operation = db
        .create_import_operation(repo_id, "admin", "resume-request", "fingerprint-fixed")
        .unwrap();
    let operation_id = operation.id.clone();
    db.start_import_operation(repo_id, &operation_id).unwrap();
    db.note_import_total(repo_id, &operation_id, 2).unwrap();
    db.note_import_local(repo_id, &operation_id, 1, &tip, 1, 1)
        .unwrap();
    db.begin_import_publication(repo_id, &operation_id, "refs/heads/main", &tip)
        .unwrap();
    db.confirm_import_publication(repo_id, &operation_id, &tip)
        .unwrap();
    db.finish_import_operation(
        repo_id,
        &operation_id,
        ImportOperationState::ReconciliationRequired,
        "worker stopped before resume",
    )
    .unwrap();
    let mut held = db
        .get_import_operation(repo_id, &operation_id)
        .unwrap()
        .unwrap();
    held.resume_authorized = true;
    db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key=?2",
            rusqlite::params![
                serde_json::to_string(&held).unwrap(),
                format!("import_operation_v1:document:{operation_id}")
            ],
        )
        .unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"{svn_root}\"\nusername = \"fixture\"\n[github]\nrepo = \"local/history\"\n",
            dir.path().display()
        ),
    )
    .unwrap();
    let mut config = AppConfig::load_from_file(&config_path).unwrap();
    config.web.admin_password = Some("secret".into());
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
        SvnClient::new(&svn_root, "fixture", ""),
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
        scheduler_service: std::sync::Mutex::new(crate::SchedulerServiceState::running()),
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
    git(&["remote", "remove", "origin"]);
    let refused = super::resume_repo_import(
        State(state.clone()),
        headers.clone(),
        Path((repo_id.into(), operation_id.clone())),
    )
    .await
    .unwrap_err();
    let refused_message = app_error_message(refused);
    assert!(
        refused_message.contains("failed to set git credentials"),
        "missing origin must fail credential remote lookup after the workdir exists: {refused_message}"
    );
    assert!(matches!(
        state
            .db
            .get_import_operation(repo_id, &operation_id)
            .unwrap()
            .unwrap()
            .state,
        ImportOperationState::ReconciliationRequired
    ));
    let app = axum::Router::new()
        .merge(super::routes())
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let resume_url = format!("http://{addr}/api/repos/{repo_id}/import/{operation_id}/resume");
    let http_refused = client
        .post(&resume_url)
        .bearer_auth("token")
        .send()
        .await
        .unwrap();
    assert_eq!(
        http_refused.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    let held = state
        .db
        .get_import_operation(repo_id, &operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(held.state, ImportOperationState::ReconciliationRequired);
    assert!(held.resume_authorized);
    assert_eq!(held.last_confirmed_git_sha.as_deref(), Some(tip.as_str()));
    assert_eq!(held.target_fingerprint, "fingerprint-fixed");
    let progress = state.get_repo_import_progress(repo_id).await;
    assert_ne!(progress.read().await.phase, ImportPhase::Importing);
    assert_ne!(progress.read().await.phase, ImportPhase::Completed);
    assert!(state.import_handles.lock().await.is_empty());
    let bare_held = String::from_utf8(
        std::process::Command::new("git")
            .args([
                "--git-dir",
                bare.to_str().unwrap(),
                "rev-parse",
                "refs/heads/main",
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(bare_held.trim(), tip);
    let local_held = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout).unwrap();
    assert_eq!(local_held.trim(), tip);

    git(&["remote", "add", "origin", &origin]);
    let started = client
        .post(&resume_url)
        .bearer_auth("token")
        .send()
        .await
        .unwrap();
    let started_status = started.status();
    let started_body = started.text().await.unwrap();
    assert!(
        started_status.is_success(),
        "{started_status} {started_body}"
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut finished = None;
    while tokio::time::Instant::now() < deadline {
        let current = state
            .db
            .get_import_operation(repo_id, &operation_id)
            .unwrap()
            .unwrap();
        if matches!(
            current.state,
            ImportOperationState::Completed
                | ImportOperationState::Failed
                | ImportOperationState::ReconciliationRequired
        ) && current.state != ImportOperationState::ReconciliationRequired
        {
            finished = Some(current);
            break;
        }
        if current.state == ImportOperationState::ReconciliationRequired
            && !current.resume_authorized
        {
            finished = Some(current);
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let finished = finished.expect("resume did not reach a terminal operation");
    assert_eq!(
        finished.state,
        ImportOperationState::Completed,
        "{:?}",
        finished.outcome_detail
    );
    let remote_tip = String::from_utf8(
        std::process::Command::new("git")
            .args([
                "--git-dir",
                bare.to_str().unwrap(),
                "rev-parse",
                "refs/heads/main",
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let remote_tip = remote_tip.trim().to_string();
    assert_ne!(remote_tip, tip);
    let remote_file = std::process::Command::new("git")
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "show",
            "refs/heads/main:history.txt",
        ])
        .output()
        .unwrap();
    assert_eq!(remote_file.stdout, b"two\n");
    let parent = String::from_utf8(
        std::process::Command::new("git")
            .args([
                "--git-dir",
                bare.to_str().unwrap(),
                "rev-parse",
                "refs/heads/main^",
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(parent.trim(), tip);
    assert_eq!(
        String::from_utf8(
            std::process::Command::new("git")
                .args([
                    "--git-dir",
                    bare.to_str().unwrap(),
                    "rev-parse",
                    &format!("{tip}^{{tree}}")
                ])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim(),
        prefix_tree
    );
    drop(state);
    server.abort();
    let reopened = Database::new(&db_path).unwrap();
    let again = reopened
        .get_import_operation(repo_id, &operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(again.state, ImportOperationState::Completed);
    assert_eq!(
        again.last_confirmed_git_sha.as_deref(),
        Some(remote_tip.as_str())
    );
    let reopened_remote = String::from_utf8(
        std::process::Command::new("git")
            .args([
                "--git-dir",
                bare.to_str().unwrap(),
                "rev-parse",
                "refs/heads/main",
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(reopened_remote.trim(), remote_tip);
}

#[cfg(test)]
mod pr245_original_http_acceptance {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use reposync_core::config::{AppConfig, IdentityConfig};
    use reposync_core::db::import_operations::{import_target_fingerprint, ImportOperationState};
    use reposync_core::db::lineage_checkpoints::record_independently_established_svn_uuid;
    use reposync_core::db::Database;
    use reposync_core::git::remote_url::derive_git_remote_url;
    use reposync_core::git::GitClient;
    use reposync_core::identity::IdentityMapper;
    use reposync_core::import::ImportPhase;
    use reposync_core::models::Repository;
    use reposync_core::svn::SvnClient;
    use reposync_core::sync_engine::SyncEngine;

    use crate::AppState;

    const SVN_CANARY: &str = "pr245-canary-svn";
    const GIT_CANARY: &str = "pr245-canary-git";
    const SESSION: &str = "pr245-session";

    #[derive(Clone, Copy)]
    enum Case {
        StartRevoked,
        StartRead,
        ResumeRevoked,
        ResumeRead,
    }

    impl Case {
        fn is_start(self) -> bool {
            matches!(self, Self::StartRevoked | Self::StartRead)
        }

        fn is_read(self) -> bool {
            matches!(self, Self::StartRead | Self::ResumeRead)
        }
    }

    struct Exchange {
        status: u16,
        body: String,
    }

    struct Live {
        _root: tempfile::TempDir,
        state: Arc<AppState>,
        db_path: PathBuf,
        repo_id: String,
        home: PathBuf,
        svn_repo: PathBuf,
        bare: PathBuf,
        workdir: PathBuf,
        prefix: Option<String>,
        operation_id: Option<String>,
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_start_revocation_refuses_then_recovers() {
        exercise(Case::StartRevoked).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_start_read_failure_refuses_then_recovers() {
        exercise(Case::StartRead).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_resume_revocation_preserves_authority_then_recovers() {
        exercise(Case::ResumeRevoked).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_resume_read_failure_preserves_authority_then_recovers() {
        exercise(Case::ResumeRead).await;
    }

    async fn exercise(case: Case) {
        let live = prepare(case).await;
        let (addr, server) = listen(live.state.clone()).await;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();
        let start_url = format!("http://{addr}/api/repos/{}/import", live.repo_id);
        let resume_url = live.operation_id.as_ref().map(|operation_id| {
            format!(
                "http://{addr}/api/repos/{}/import/{operation_id}/resume",
                live.repo_id
            )
        });
        let before = snapshot(&live);
        if case.is_read() {
            install_read_fault(&live);
        } else {
            install_revocation(&live);
        }
        prove_credential_fault(&live, case);
        let refused = if case.is_start() {
            post(&client, &start_url, "pr245-refuse-start").await
        } else {
            post(
                &client,
                resume_url.as_deref().unwrap(),
                "pr245-refuse-resume",
            )
            .await
        };
        if case.is_read() {
            assert_eq!(refused.status, 500, "{}", refused.body);
            assert!(
                refused.body.contains("internal server error"),
                "{}",
                refused.body
            );
        } else {
            assert_eq!(refused.status, 403, "{}", refused.body);
            assert!(refused.body.contains("revoked"), "{}", refused.body);
        }
        assert_quiet(&live).await;
        assert_eq!(snapshot(&live), before);
        if case.is_start() {
            assert!(live
                .state
                .db
                .latest_import_operation(&live.repo_id)
                .unwrap()
                .is_none());
        } else {
            assert_held(&live);
        }
        assert!(
            reposync_core::busy::try_acquire(&live.repo_id).is_some(),
            "busy slot was held after credential refusal"
        );
        if case.is_read() {
            clear_read_fault(&live);
        } else {
            clear_revocation(&live);
        }
        prove_credentials_restored(&live);
        let recovered = if case.is_start() {
            post(&client, &start_url, "pr245-recover-start").await
        } else {
            post(
                &client,
                resume_url.as_deref().unwrap(),
                "pr245-recover-resume",
            )
            .await
        };
        assert!(
            (200..300).contains(&recovered.status),
            "{} {}",
            recovered.status,
            recovered.body
        );
        let operation_id = wait_completed(&live, case).await;
        let after = snapshot(&live);
        assert_ne!(after.git_count, before.git_count);
        assert_eq!(after.svn_youngest, before.svn_youngest);
        assert_recovered_content(&live, &after);
        let repeated = if case.is_start() {
            post(&client, &start_url, "pr245-repeat-start").await
        } else {
            post(
                &client,
                &format!(
                    "http://{addr}/api/repos/{}/import/{operation_id}/resume",
                    live.repo_id
                ),
                "pr245-repeat-resume",
            )
            .await
        };
        assert_eq!(repeated.status, 400, "{}", repeated.body);
        assert_eq!(snapshot(&live), after);
        let handles = std::mem::take(&mut *live.state.import_handles.lock().await);
        for handle in handles {
            tokio::time::timeout(std::time::Duration::from_secs(20), handle)
                .await
                .expect("import worker did not finish")
                .expect("import worker panicked");
        }
        assert!(
            reposync_core::busy::try_acquire(&live.repo_id).is_some(),
            "busy slot still held after the worker finished"
        );
        server.abort();
        match server.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {}
            Err(error) => panic!("loopback server task panicked: {error}"),
        }
        let db_path = live.db_path.clone();
        let repo_id = live.repo_id.clone();
        let Live { _root, state, .. } = live;
        drop(state);
        let reopened = Database::new(&db_path).unwrap();
        let stored = reopened
            .latest_import_operation(&repo_id)
            .unwrap()
            .expect("completed import");
        assert_eq!(stored.id, operation_id);
        assert_eq!(stored.state, ImportOperationState::Completed);
        drop(reopened);
        drop(_root);
    }

    async fn prepare(case: Case) -> Live {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().canonicalize().unwrap();
        let home = data_dir.join("home");
        let svn_config = data_dir.join("svn-config");
        let svn_repo = data_dir.join("svn-repo");
        let bare = data_dir.join("origin.git");
        std::fs::create_dir_all(&home).unwrap();
        write_svn_config(&svn_config);
        run(
            "svnadmin",
            &["create", svn_repo.to_str().unwrap()],
            None,
            &home,
        );
        let svn_uuid = run(
            "svnlook",
            &["uuid", svn_repo.to_str().unwrap()],
            None,
            &home,
        );
        assert!(uuid::Uuid::parse_str(&svn_uuid).is_ok());
        assert_eq!(
            run(
                "svnlook",
                &["youngest", svn_repo.to_str().unwrap()],
                None,
                &home
            ),
            "0"
        );
        let repo_id = format!("pr245-{}", uuid::Uuid::new_v4());
        assert!(uuid::Uuid::parse_str(&repo_id).is_err());
        let svn_root = format!("file://{}", svn_repo.display());
        let db_path = data_dir.join("reposync.db");
        let db = Database::new(&db_path).unwrap();
        db.initialize().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let repository = Repository {
            id: repo_id.clone(),
            name: repo_id.clone(),
            svn_url: svn_root.clone(),
            svn_branch: "trunk".into(),
            svn_username: "fixture".into(),
            git_provider: "gitea".into(),
            git_api_url: format!("file://{}", data_dir.display()),
            git_repo: "origin".into(),
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
        };
        db.insert_repository(&repository).unwrap();
        record_independently_established_svn_uuid(&db, &repo_id, &svn_uuid)
            .expect("svn uuid recorded before history");
        import_history(&svn_repo, &svn_root, &svn_config, &home);
        assert_eq!(
            run(
                "svnlook",
                &["youngest", svn_repo.to_str().unwrap()],
                None,
                &home
            ),
            "2"
        );
        let config_path = data_dir.join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"{svn_root}\"\nusername = \"fixture\"\n[github]\nrepo = \"local/history\"\n",
                data_dir.display()
            ),
        )
        .unwrap();
        let mut config = AppConfig::load_from_file(&config_path).unwrap();
        config.web.admin_password = Some("pr245-admin".into());
        assert_eq!(config.daemon.data_dir, data_dir);
        let workdir = config
            .daemon
            .data_dir
            .join("repos")
            .join(&repo_id)
            .join("git-repo");
        let derived = derive_git_remote_url(&repository.git_api_url, None, &repository.git_repo);
        assert_eq!(derived, format!("file://{}", bare.display()));
        let prefix = if case.is_start() {
            run(
                "git",
                &[
                    "init",
                    "--bare",
                    "--initial-branch",
                    "main",
                    bare.to_str().unwrap(),
                ],
                None,
                &home,
            );
            assert!(!git_ok(
                &[
                    "--git-dir",
                    bare.to_str().unwrap(),
                    "rev-parse",
                    "--verify",
                    "refs/heads/main"
                ],
                None,
                &home
            ));
            None
        } else {
            publish_prefix(&bare, &workdir, &home);
            Some(run("git", &["rev-parse", "HEAD"], Some(&workdir), &home))
        };
        db.set_state(&format!("secret_svn_password_{repo_id}"), SVN_CANARY)
            .unwrap();
        db.set_state(&format!("secret_git_token_{repo_id}"), GIT_CANARY)
            .unwrap();
        db.set_state("pr245_non_secret_probe", "present").unwrap();
        let operation_id = if case.is_start() {
            None
        } else {
            Some(hold_resume(
                &db,
                &repo_id,
                &workdir,
                prefix.as_deref().unwrap(),
            ))
        };
        let dummy = data_dir.join("dummy-git");
        std::fs::create_dir_all(&dummy).unwrap();
        run(
            "git",
            &["init", "--initial-branch", "main"],
            Some(&dummy),
            &home,
        );
        let engine = SyncEngine::new(
            config.clone(),
            Database::in_memory().unwrap(),
            SvnClient::new(&svn_root, "fixture", ""),
            GitClient::new(&dummy).unwrap(),
            Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
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
            import_progress: Arc::new(tokio::sync::RwLock::new(
                reposync_core::import::ImportProgress::default(),
            )),
            config_path,
            prev_net_snapshot: std::sync::Mutex::new(None),
            repo_import_progress: tokio::sync::RwLock::new(std::collections::HashMap::new()),
            login_attempts: std::sync::Mutex::new(std::collections::HashMap::new()),
            import_handles: tokio::sync::Mutex::new(Vec::new()),
            scheduler_service: std::sync::Mutex::new(crate::SchedulerServiceState::running()),
        });
        state.sessions.write().await.insert(
            SESSION.into(),
            chrono::Utc::now() + chrono::Duration::hours(1),
        );
        Live {
            _root: root,
            state,
            db_path,
            repo_id,
            home,
            svn_repo,
            bare,
            workdir,
            prefix,
            operation_id,
        }
    }

    fn hold_resume(db: &Database, repo_id: &str, workdir: &Path, tip: &str) -> String {
        let stored = db.get_repository(repo_id).unwrap().unwrap();
        let fingerprint = import_target_fingerprint(&stored, workdir);
        let operation = db
            .create_import_operation(repo_id, "legacy", "pr245-hold", &fingerprint)
            .unwrap();
        let operation_id = operation.id.clone();
        db.start_import_operation(repo_id, &operation_id).unwrap();
        db.note_import_total(repo_id, &operation_id, 2).unwrap();
        db.note_import_local(repo_id, &operation_id, 1, tip, 1, 1)
            .unwrap();
        db.stage_import_commit_map(repo_id, &operation_id, 1, tip, "fixture", "Fixture")
            .unwrap();
        db.begin_import_publication(repo_id, &operation_id, "refs/heads/main", tip)
            .unwrap();
        db.finish_import_operation(
            repo_id,
            &operation_id,
            ImportOperationState::ReconciliationRequired,
            "worker stopped before resume",
        )
        .unwrap();
        let guard = reposync_core::busy::try_acquire(repo_id).expect("busy slot for reconcile");
        let reconciled = db
            .reconcile_verified_import(repo_id, &operation_id, workdir, "refs/heads/main", tip)
            .expect("legitimate resume authority");
        drop(guard);
        assert!(reconciled.resume_authorized);
        assert!(!reconciled.completed);
        assert_eq!(
            reconciled.operation.state,
            ImportOperationState::ReconciliationRequired
        );
        assert_eq!(
            reconciled.operation.last_confirmed_git_sha.as_deref(),
            Some(tip)
        );
        operation_id
    }

    fn install_revocation(live: &Live) {
        live.state
            .db
            .set_state(&svn_key(&live.repo_id), "")
            .unwrap();
    }

    fn clear_revocation(live: &Live) {
        live.state
            .db
            .set_state(&svn_key(&live.repo_id), SVN_CANARY)
            .unwrap();
    }

    fn install_read_fault(live: &Live) {
        let changed = live
            .state
            .db
            .conn()
            .execute(
                "UPDATE kv_state SET value = X'FF70723234352D372D72656164' WHERE key = ?1",
                [&svn_key(&live.repo_id)],
            )
            .unwrap();
        assert_eq!(changed, 1, "credential row was not the stored secret");
    }

    fn clear_read_fault(live: &Live) {
        live.state
            .db
            .set_state(&svn_key(&live.repo_id), SVN_CANARY)
            .unwrap();
    }

    fn prove_credential_fault(live: &Live, case: Case) {
        assert_eq!(
            live.state
                .db
                .get_state("pr245_non_secret_probe")
                .unwrap()
                .as_deref(),
            Some("present")
        );
        assert_eq!(
            live.state
                .db
                .managed_remove_blocks_new_work(&live.repo_id)
                .unwrap(),
            false
        );
        let svn = live
            .state
            .db
            .try_resolve_credential_chain_state(&live.repo_id, "secret_svn_password");
        let git = live
            .state
            .db
            .try_resolve_credential_chain_state(&live.repo_id, "secret_git_token")
            .expect("git token read stays available");
        assert_eq!(git.value.as_deref(), Some(GIT_CANARY));
        assert!(!git.explicitly_revoked);
        if case.is_read() {
            let error = svn.expect_err("svn credential read must fail");
            let text = error.to_string().to_ascii_lowercase();
            let qualified =
                text.contains("utf-8") || text.contains("blob") || text.contains("invalid");
            assert!(
                qualified,
                "credential read took an unrelated failure: {text}"
            );
        } else {
            let state = svn.expect("revocation is a successful revoked read");
            assert!(state.explicitly_revoked);
            assert!(state.value.is_none());
        }
    }

    fn prove_credentials_restored(live: &Live) {
        let svn = live
            .state
            .db
            .try_resolve_credential_chain_state(&live.repo_id, "secret_svn_password")
            .unwrap();
        assert!(!svn.explicitly_revoked);
        assert_eq!(svn.value.as_deref(), Some(SVN_CANARY));
        let git = live
            .state
            .db
            .try_resolve_credential_chain_state(&live.repo_id, "secret_git_token")
            .unwrap();
        assert_eq!(git.value.as_deref(), Some(GIT_CANARY));
    }

    fn svn_key(repo_id: &str) -> String {
        format!("secret_svn_password_{repo_id}")
    }

    async fn listen(state: Arc<AppState>) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let app = axum::Router::new()
            .merge(super::super::routes())
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, server)
    }

    async fn post(client: &reqwest::Client, url: &str, request_id: &str) -> Exchange {
        let response = client
            .post(url)
            .bearer_auth(SESSION)
            .header("x-request-id", request_id)
            .send()
            .await
            .expect("loopback http");
        let status = response.status().as_u16();
        let raw = response.text().await.expect("http body");
        assert!(!raw.contains(SVN_CANARY), "{}", redact(&raw));
        assert!(!raw.contains(GIT_CANARY), "{}", redact(&raw));
        Exchange {
            status,
            body: redact(&raw),
        }
    }

    fn redact(body: &str) -> String {
        body.replace(SVN_CANARY, "[redacted]")
            .replace(GIT_CANARY, "[redacted]")
    }

    async fn assert_quiet(live: &Live) {
        let progress = live.state.get_repo_import_progress(&live.repo_id).await;
        let phase = progress.read().await.phase.clone();
        assert_ne!(phase, ImportPhase::Importing);
        assert_ne!(phase, ImportPhase::Completed);
        assert!(live.state.import_handles.lock().await.is_empty());
        if let Some(operation_id) = &live.operation_id {
            let operation = live
                .state
                .db
                .get_import_operation(&live.repo_id, operation_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                operation.state,
                ImportOperationState::ReconciliationRequired
            );
            assert!(operation.resume_authorized);
            assert_ne!(operation.state, ImportOperationState::Running);
        }
    }

    fn assert_held(live: &Live) {
        let operation_id = live.operation_id.as_deref().unwrap();
        let operation = live
            .state
            .db
            .get_import_operation(&live.repo_id, operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            operation.state,
            ImportOperationState::ReconciliationRequired
        );
        assert!(operation.resume_authorized);
        assert_eq!(
            operation.last_confirmed_git_sha.as_deref(),
            live.prefix.as_deref()
        );
        assert_eq!(
            live.state
                .db
                .get_repository(&live.repo_id)
                .unwrap()
                .unwrap()
                .last_svn_rev,
            0
        );
    }

    async fn wait_completed(live: &Live, case: Case) -> String {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
        let mut last = String::new();
        while tokio::time::Instant::now() < deadline {
            let current = if case.is_start() {
                live.state
                    .db
                    .latest_import_operation(&live.repo_id)
                    .unwrap()
            } else {
                live.state
                    .db
                    .get_import_operation(&live.repo_id, live.operation_id.as_deref().unwrap())
                    .unwrap()
            };
            if let Some(operation) = current {
                let detail = operation.outcome_detail.clone().unwrap_or_default();
                last = format!("{:?} {detail}", operation.state);
                if operation.state == ImportOperationState::Completed {
                    return operation.id;
                }
                let lost_authority = operation.state
                    == ImportOperationState::ReconciliationRequired
                    && !operation.resume_authorized
                    && detail != "worker stopped before resume";
                if operation.state == ImportOperationState::Failed || lost_authority {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("import did not complete: {last}");
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct NativeSnap {
        svn_youngest: String,
        git_count: Option<String>,
        tip: Option<String>,
        map_count: i64,
        last_svn_rev: i64,
        last_git_sha: String,
    }

    fn snapshot(live: &Live) -> NativeSnap {
        let tip = if git_ok(
            &[
                "--git-dir",
                live.bare.to_str().unwrap(),
                "rev-parse",
                "refs/heads/main",
            ],
            None,
            &live.home,
        ) {
            Some(run(
                "git",
                &[
                    "--git-dir",
                    live.bare.to_str().unwrap(),
                    "rev-parse",
                    "refs/heads/main",
                ],
                None,
                &live.home,
            ))
        } else {
            None
        };
        let git_count = tip.as_ref().map(|_| {
            run(
                "git",
                &[
                    "--git-dir",
                    live.bare.to_str().unwrap(),
                    "rev-list",
                    "--count",
                    "refs/heads/main",
                ],
                None,
                &live.home,
            )
        });
        let repo = live
            .state
            .db
            .get_repository(&live.repo_id)
            .unwrap()
            .unwrap();
        NativeSnap {
            svn_youngest: run(
                "svnlook",
                &["youngest", live.svn_repo.to_str().unwrap()],
                None,
                &live.home,
            ),
            git_count,
            tip,
            map_count: live
                .state
                .db
                .conn()
                .query_row(
                    "SELECT COUNT(*) FROM commit_map WHERE repo_id=?1",
                    [&live.repo_id],
                    |row| row.get(0),
                )
                .unwrap(),
            last_svn_rev: repo.last_svn_rev,
            last_git_sha: repo.last_git_sha,
        }
    }

    fn assert_recovered_content(live: &Live, after: &NativeSnap) {
        assert_eq!(after.svn_youngest, "2");
        assert_eq!(after.git_count.as_deref(), Some("2"));
        let tip = after.tip.as_deref().unwrap();
        assert_eq!(after.last_git_sha, tip);
        assert_eq!(after.last_svn_rev, 2);
        assert_eq!(
            stdout(
                "git",
                &[
                    "--git-dir",
                    live.bare.to_str().unwrap(),
                    "show",
                    "refs/heads/main:history.txt"
                ],
                None,
                &live.home
            ),
            b"two\n"
        );
        if let Some(prefix) = &live.prefix {
            assert_eq!(
                run(
                    "git",
                    &[
                        "--git-dir",
                        live.bare.to_str().unwrap(),
                        "rev-parse",
                        "refs/heads/main^"
                    ],
                    None,
                    &live.home
                ),
                *prefix
            );
            assert_eq!(after.git_count.as_deref(), Some("2"));
        } else {
            assert_eq!(
                stdout(
                    "git",
                    &[
                        "--git-dir",
                        live.bare.to_str().unwrap(),
                        "show",
                        "refs/heads/main^:history.txt"
                    ],
                    None,
                    &live.home
                ),
                b"one\n"
            );
        }
        assert!(after.map_count >= 1);
    }

    fn import_history(svn_repo: &Path, svn_root: &str, svn_config: &Path, home: &Path) {
        let seed = svn_repo.parent().unwrap().join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        std::fs::write(seed.join("history.txt"), b"one\n").unwrap();
        svn(
            svn_config,
            home,
            None,
            &[
                "import",
                seed.to_str().unwrap(),
                &format!("{svn_root}/trunk"),
                "-m",
                "one",
            ],
        );
        let checkout = svn_repo.parent().unwrap().join("wc");
        svn(
            svn_config,
            home,
            None,
            &[
                "checkout",
                &format!("{svn_root}/trunk"),
                checkout.to_str().unwrap(),
            ],
        );
        std::fs::write(checkout.join("history.txt"), b"two\n").unwrap();
        svn(svn_config, home, Some(&checkout), &["commit", "-m", "two"]);
    }

    fn publish_prefix(bare: &Path, workdir: &Path, home: &Path) {
        run(
            "git",
            &[
                "init",
                "--bare",
                "--initial-branch",
                "main",
                bare.to_str().unwrap(),
            ],
            None,
            home,
        );
        std::fs::create_dir_all(workdir).unwrap();
        run(
            "git",
            &["init", "--initial-branch", "main"],
            Some(workdir),
            home,
        );
        run(
            "git",
            &["config", "user.name", "Fixture"],
            Some(workdir),
            home,
        );
        run(
            "git",
            &["config", "user.email", "fixture@example.invalid"],
            Some(workdir),
            home,
        );
        std::fs::write(workdir.join("history.txt"), b"one\n").unwrap();
        run("git", &["add", "history.txt"], Some(workdir), home);
        run("git", &["commit", "-m", "checkpoint"], Some(workdir), home);
        run(
            "git",
            &["remote", "add", "origin", bare.to_str().unwrap()],
            Some(workdir),
            home,
        );
        run(
            "git",
            &["push", "-u", "origin", "main"],
            Some(workdir),
            home,
        );
    }

    fn write_svn_config(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("config"),
            "[auth]\npassword-stores =\nstore-passwords = no\nstore-auth-creds = no\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("servers"),
            "[global]\nstore-passwords = no\nstore-plaintext-passwords = no\n",
        )
        .unwrap();
    }

    fn svn(config: &Path, home: &Path, dir: Option<&Path>, args: &[&str]) {
        let config = config.to_str().unwrap();
        let mut full = vec![
            "--config-dir",
            config,
            "--non-interactive",
            "--no-auth-cache",
            "--username",
            "fixture",
            "--password",
            "fixture",
        ];
        full.extend_from_slice(args);
        run("svn", &full, dir, home);
    }

    fn run(program: &str, args: &[&str], dir: Option<&Path>, home: &Path) -> String {
        String::from_utf8(stdout(program, args, dir, home))
            .unwrap()
            .trim()
            .to_string()
    }

    fn git_ok(args: &[&str], dir: Option<&Path>, home: &Path) -> bool {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args)
            .env("HOME", home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        if let Some(dir) = dir {
            cmd.current_dir(dir);
        }
        cmd.status().map(|status| status.success()).unwrap_or(false)
    }

    fn stdout(program: &str, args: &[&str], dir: Option<&Path>, home: &Path) -> Vec<u8> {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args)
            .env("HOME", home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        if let Some(dir) = dir {
            cmd.current_dir(dir);
        }
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
}
