use super::{
    apply_import_failure_evidence, persist_admitted_outcome, propagate_child_admission_refusal,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::api::status::AppError;
use crate::AppState;
use axum::extract::{Path, Query, State};
use reposync_core::config::AppConfig;
use reposync_core::db::import_operations::ImportOperationState;
use reposync_core::db::Database;
use reposync_core::git::GitClient;
use reposync_core::identity::IdentityMapper;
use reposync_core::import::ImportProgress;
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
