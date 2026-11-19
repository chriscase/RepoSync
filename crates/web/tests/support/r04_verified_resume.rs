use super::import_workers::{cleanup_failed_imports, join_last};
use super::{
    authed_client, import_fixture, import_fixture_engine, minimal_config, rs67_pairing_storage,
    spawn_isolated_test_server, svn_youngest, TestServerGuard, TEST_TOKEN,
};
use axum::Router;
use git2::Repository;
use reposync_core::{db::Database, import::ImportProgress};
use reposync_web::{api, AppState, WebServer};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::{json, Value};
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

#[path = "r04_verified_resume/history.rs"]
mod history;
use history::{observe, Phase};

const MAIN: &str = "refs/heads/main";
const REQUEST: &str = "r04-verified-resume";
const FAULT: &str = "r04 rejected first batch receipt";

fn commit_revision(root: &Path, rev: i64) {
    let work = root.join("svn-wc");
    std::fs::write(work.join("history.txt"), format!("revision {rev}\n")).unwrap();
    let output = Command::new("svn")
        .current_dir(work)
        .args([
            "commit",
            "--non-interactive",
            "-m",
            &format!("revision {rev}"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "r{rev}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn reopen(root: &Path) -> (SocketAddr, Arc<AppState>, TestServerGuard) {
    let db = Database::new(root.join("reposync.db")).unwrap();
    db.initialize().unwrap();
    let config = minimal_config(root);
    let engine = import_fixture_engine(root, &config);
    let (trigger, _) = tokio::sync::mpsc::channel(1);
    let state = WebServer::new(
        config,
        db,
        engine,
        trigger,
        root.join("config.toml"),
        Arc::new(tokio::sync::RwLock::new(ImportProgress::default())),
    )
    .app_state();
    state.sessions.write().await.insert(
        TEST_TOKEN.into(),
        chrono::Utc::now() + chrono::Duration::hours(1),
    );
    let routes = Router::new()
        .merge(api::repos::routes())
        .with_state(state.clone());
    let (addr, server) = spawn_isolated_test_server(routes);
    (addr, state, server)
}

#[derive(Clone)]
struct Case {
    root: PathBuf,
    bare: PathBuf,
    id: String,
}

#[derive(Clone)]
struct App {
    case: Case,
    state: Arc<AppState>,
    client: Client,
    base: String,
}

impl App {
    fn new(case: &Case, addr: SocketAddr, state: Arc<AppState>) -> Self {
        Self {
            case: case.clone(),
            state,
            client: authed_client(),
            base: format!("http://{addr}/api/repos/{}/import", case.id),
        }
    }

    fn shutdown(self, mut server: TestServerGuard, tmp: &mut Option<TempDir>, stage: &str) {
        let state = Arc::downgrade(&self.state);
        drop(self); // Close the HTTP client before waiting for graceful shutdown.
        let stopped = server.stop().and_then(|()| {
            if state.upgrade().is_some() {
                Err("old AppState is still owned")
            } else {
                Ok(())
            }
        });
        if let Err(error) = stopped {
            let path = tmp.take().unwrap().keep();
            panic!(
                "R04 {stage} shutdown failed: {error}; retained fixture at {}",
                path.display()
            );
        }
    }

    // The caller owns this server and tempdir throughout observation. Preserve the
    // fixture even if an HTTP failure occurs before the worker handle is registered.
    async fn run<T, F>(&self, tmp: &mut Option<TempDir>, check: impl FnOnce(Self) -> F) -> T
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        match tokio::spawn(check(self.clone())).await {
            Ok(value) => value,
            Err(error) => {
                let path = tmp.take().unwrap().keep();
                eprintln!("R04 retained fixture at {} after {error}", path.display());
                cleanup_failed_imports(&self.state).await;
                if error.is_panic() {
                    std::panic::resume_unwind(error.into_panic());
                }
                panic!("R04 observer ended: {error}");
            }
        }
    }

    async fn status(&self, op_id: &str, lifecycle: &str, can_resume: bool) -> Value {
        let status = response(
            self.client.get(format!("{}/status", self.base)),
            StatusCode::OK,
        )
        .await;
        assert_eq!(status["operation_id"], op_id, "{status}");
        assert_eq!(status["lifecycle"], lifecycle, "{status}");
        assert_eq!(status["can_resume"], can_resume, "{status}");
        status
    }

    async fn action(&self, op_id: &str, name: &str, code: StatusCode) -> Value {
        response(
            self.client.post(format!("{}/{op_id}/{name}", self.base)),
            code,
        )
        .await
    }

    fn active(&self) -> Option<String> {
        self.state
            .db
            .get_state(&format!("import_operation_v1:active:{}", self.case.id))
            .unwrap()
    }

    fn observation(&self, op_id: &str, phase: Phase) -> Value {
        observe(
            &self.case.root,
            &self.case.bare,
            &self.case.id,
            op_id,
            phase,
        )
    }

    // Detect even an equivalent-content publication or an update to a receipt on refusal.
    fn snapshot(&self) -> (Vec<Vec<String>>, String, usize, i64) {
        let git = Repository::open_bare(&self.case.bare).unwrap();
        let tip = git.refname_to_id(MAIN).unwrap().to_string();
        let updates = git.reflog(MAIN).unwrap().len();
        (
            rs67_pairing_storage(&self.state.db),
            tip,
            updates,
            svn_youngest(&self.case.root.join("svn-repo")),
        )
    }

    async fn first_publication(self) -> (String, Value) {
        let body = response(
            self.client
                .post(&self.base)
                .header("x-request-id", REQUEST)
                .json(&json!({"import_mode":"full"})),
            StatusCode::OK,
        )
        .await;
        assert_eq!(body["lifecycle"], "queued", "{body}");
        let id = body["operation_id"].as_str().unwrap().to_owned();
        join_last(&self.state, 1).await.unwrap();
        let status = self.status(&id, "reconciliation_required", false).await;
        let op = self
            .state
            .db
            .get_import_operation(&self.case.id, &id)
            .unwrap()
            .unwrap();
        assert_eq!(self.active().as_deref(), Some(id.as_str()));
        let cause = op.outcome_detail.as_deref().unwrap();
        assert!(
            cause.contains("published ref verified but receipt failed") && cause.contains(FAULT),
            "{cause}"
        );
        assert_eq!(status["total_revs"], 51);
        assert_eq!(
            status["intended_git_sha"],
            op.intended_git_sha.as_deref().unwrap()
        );
        let mut held = self.observation(&id, Phase::Held);
        held["publication_failure"] = json!(cause);
        (id, held)
    }

    async fn resume(self, id: String, held: Value) -> Value {
        self.status(&id, "reconciliation_required", false).await;
        assert_eq!(self.active().as_deref(), Some(id.as_str()));
        // Only the fixture failure injection is removed. The service must inspect
        // the original publication and authorize resume through the normal routes.
        self.state
            .db
            .conn()
            .execute_batch("DROP TRIGGER r04_batch_receipt")
            .unwrap();
        let before = self.snapshot();
        let error = self.action(&id, "resume", StatusCode::BAD_REQUEST).await;
        assert_eq!(
            error["error"],
            "import resume is not authorized for this operation"
        );
        let stale = self
            .action("missing-operation", "resume", StatusCode::NOT_FOUND)
            .await;
        assert_eq!(stale["error"], "import operation not found for repository");
        assert_eq!(self.snapshot(), before);
        assert!(self.state.import_handles.lock().await.is_empty());

        let proof = self.action(&id, "reconcile", StatusCode::OK).await;
        assert_eq!(proof["operation_id"], id);
        assert_eq!(proof["observed_remote_git_sha"], held["prefix_sha"]);
        assert_eq!(proof["publication_proved"], true);
        assert_eq!(proof["publication_receipt_recorded"], true);
        assert_eq!(proof["checkpoint_completed"], false);
        assert_eq!(proof["resume_authorized"], true);
        self.status(&id, "reconciliation_required", true).await;
        assert_eq!(self.active().as_deref(), Some(id.as_str()));
        let verified = self.observation(&id, Phase::Verified);
        assert_eq!(verified["prefix_sha"], held["prefix_sha"]);
        let repeat = self.action(&id, "reconcile", StatusCode::OK).await;
        assert_eq!(repeat["publication_receipt_recorded"], false);
        assert_eq!(repeat["resume_authorized"], true);
        assert!(self.state.import_handles.lock().await.is_empty());

        commit_revision(&self.case.root, 52);
        let resumed = self.action(&id, "resume", StatusCode::OK).await;
        assert_eq!(resumed["operation_id"], id);
        assert_eq!(resumed["lifecycle"], "running");
        assert_eq!(resumed["resume_from_svn_rev"], 50);
        assert_eq!(resumed["total_revisions"], 51);
        join_last(&self.state, 1).await.unwrap();
        let done = self.status(&id, "completed", false).await;
        assert_eq!(done["total_revs"], 52);
        assert_eq!(done["current_rev"], 52);
        assert_eq!(self.active(), None);
        let complete = self.observation(&id, Phase::Completed);
        assert_eq!(complete["prefix_sha"], held["prefix_sha"]);
        assert_eq!(complete["svn_uuid"], held["svn_uuid"]);
        let verified_prefix = verified["prefix_map_ids"].as_array().unwrap();
        assert_eq!(verified_prefix.len(), 50);
        assert_eq!(complete["prefix_map_ids"].as_array(), Some(verified_prefix));
        json!({"held":held,"verified":verified,"completed":complete})
    }

    async fn quiet(self, id: String, before_restart: Value) {
        assert_eq!(
            self.observation(&id, Phase::Completed),
            before_restart,
            "restart changed the completed import before the first request"
        );
        let before = self.snapshot();
        let status = self.status(&id, "completed", false).await;
        assert_eq!(status["last_confirmed_git_sha"], before.1);
        let replay = response(
            self.client
                .post(&self.base)
                .header("x-request-id", REQUEST)
                .json(&json!({"import_mode":"full"})),
            StatusCode::OK,
        )
        .await;
        assert_eq!(replay["operation_id"], id);
        assert_eq!(replay["lifecycle"], "completed");
        let duplicate = self.action(&id, "resume", StatusCode::BAD_REQUEST).await;
        assert_eq!(
            duplicate["error"],
            "operation is not this repository's active import hold"
        );
        let prior = self.action(&id, "reconcile", StatusCode::OK).await;
        assert_eq!(prior["operation_id"], id);
        assert_eq!(prior["checkpoint_completed"], true);
        assert_eq!(prior["publication_receipt_recorded"], false);
        assert!(self.state.import_handles.lock().await.is_empty());
        assert_eq!(self.active(), None);
        assert_eq!(self.snapshot(), before);
    }
}

async fn response(request: RequestBuilder, expected: StatusCode) -> Value {
    let reply = request
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .unwrap();
    let code = reply.status();
    let body: Value = reply.json().await.unwrap();
    assert_eq!(code, expected, "{body}");
    body
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn published_prefix_resumes_after_restart() {
    let (addr, state, server, tmp, id, bare) = import_fixture().await;
    let case = Case {
        root: tmp.path().into(),
        bare,
        id,
    };
    for rev in 4..=51 {
        commit_revision(&case.root, rev);
    }
    Repository::open_bare(&case.bare)
        .unwrap()
        .config()
        .unwrap()
        .set_bool("core.logAllRefUpdates", true)
        .unwrap();
    // Only the fixture has this database. Abort the last insertion in the same
    // transaction that would flush all 50 mappings after Git accepted main.
    state
        .db
        .conn()
        .execute_batch(
            "CREATE TRIGGER r04_batch_receipt BEFORE INSERT ON commit_map
        WHEN NEW.svn_rev = 50 AND NEW.direction = 'svn_to_git'
        BEGIN SELECT RAISE(ABORT, 'r04 rejected first batch receipt'); END;",
        )
        .unwrap();
    let mut tmp = Some(tmp);
    let app = App::new(&case, addr, state);
    let (id, held) = app.run(&mut tmp, App::first_publication).await;
    app.shutdown(server, &mut tmp, "held import");

    let (addr, state, server) = reopen(&case.root).await;
    let app = App::new(&case, addr, state);
    let op = id.clone();
    let receipts = app.run(&mut tmp, |app| app.resume(op, held)).await;
    app.shutdown(server, &mut tmp, "completed import");

    let (addr, state, server) = reopen(&case.root).await;
    let app = App::new(&case, addr, state);
    let before_restart = receipts["completed"].clone();
    app.run(&mut tmp, |app| app.quiet(id.clone(), before_restart))
        .await;
    app.shutdown(server, &mut tmp, "cold verification");
    eprintln!(
        "RELIABILITY_EVIDENCE {}",
        json!({
            "case":"R04_VERIFIED_PARTIAL_RESUME", "status":"PASS", "repo_id":case.id,
            "operation_id":id, "observations":receipts
        })
    );
}
