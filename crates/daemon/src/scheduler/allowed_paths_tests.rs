//! Fail-closed `allowed_paths` loader. Fixtures are local temp dirs and
//! `file://` URLs only: no credentials and no remote services.
//! These cases are authored source. They are NOT_RUN in this change.
//!
//! Admitted workers use a two-thread runtime. `block_in_place` panics on a
//! current-thread runtime before the cycle body runs, and a timeout-only
//! join treats that panic as success. Joining the worker proves the task
//! finished. It does not prove the sync succeeded.
//!
//! Dropping a Tokio 1.53.1 `JoinHandle` detaches the task. It does not abort
//! it. A join therefore polls `&mut JoinHandle`, which is cancel-safe, and
//! aborts only after a failure. `block_in_place` still runs to completion.
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reposync_core::config::IdentityConfig;
use reposync_core::models::Repository;
use tracing_subscriber::fmt::MakeWriter;

use super::*;

struct LogMake(Arc<Mutex<String>>);

struct LogWriter(Arc<Mutex<String>>);

impl<'a> MakeWriter<'a> for LogMake {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.0.clone())
    }
}

impl Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("log buffer")
            .push_str(&String::from_utf8_lossy(bytes));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn capture() -> (Arc<Mutex<String>>, tracing::subscriber::DefaultGuard) {
    let buf = Arc::new(Mutex::new(String::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(LogMake(buf.clone()))
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (buf, guard)
}

fn logged(buf: &str, message: &str, repo_name: &str) -> bool {
    buf.lines()
        .any(|line| line.contains(message) && line.contains(repo_name))
}

struct DueRepo {
    id: &'static str,
    name: &'static str,
    allowed_paths: Option<&'static str>,
    blocked_patterns: Option<&'static str>,
}

fn due_scheduler(repos: &[DueRepo]) -> (tempfile::TempDir, Scheduler) {
    let tmp = tempfile::tempdir().unwrap();
    let config_file = tmp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(
            "[daemon]\ndata_dir = \"{}\"\n[svn]\nurl = \"file:///nonexistent\"\nusername = \"fixture\"\n[github]\nrepo = \"local/fixture\"\n",
            tmp.path().display()
        ),
    )
    .unwrap();
    let config = AppConfig::load_from_file(&config_file).unwrap();
    let db = Database::new(&tmp.path().join("reposync.db")).unwrap();
    db.initialize().unwrap();
    let now = Utc::now().to_rfc3339();
    for repo in repos {
        db.insert_repository(&Repository {
            id: repo.id.into(),
            name: repo.name.into(),
            svn_url: "file:///nonexistent".into(),
            svn_branch: "trunk".into(),
            svn_username: "fixture".into(),
            git_provider: "gitea".into(),
            git_api_url: "file:///nonexistent".into(),
            git_repo: format!("local/{}", repo.id),
            git_branch: "main".into(),
            sync_mode: "team".into(),
            poll_interval_secs: 1,
            lfs_threshold_mb: 0,
            auto_merge: false,
            enabled: true,
            created_by: None,
            parent_id: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            last_svn_rev: 2,
            last_git_sha: "verified-old".into(),
            last_sync_at: Some("2000-01-01T00:00:00Z".into()),
            sync_status: "idle".into(),
            total_syncs: 0,
            total_errors: 0,
            allowed_paths: repo.allowed_paths.map(str::to_string),
            blocked_patterns: repo.blocked_patterns.map(str::to_string),
            consecutive_errors: 0,
            teams_webhook_url: None,
        })
        .unwrap();
        db.set_state(&format!("last_svn_rev_{}", repo.id), "2")
            .unwrap();
        let git_repo = tmp.path().join("repos").join(repo.id).join("git-repo");
        prepare_owned_git_repo(&git_repo);
    }
    let dummy_git = tmp.path().join("dummy-git");
    assert!(std::process::Command::new("git")
        .args(["init", dummy_git.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    let engine = SyncEngine::new(
        config.clone(),
        Database::in_memory().unwrap(),
        SvnClient::new("file:///nonexistent", "fixture", ""),
        GitClient::new(&dummy_git).unwrap(),
        Arc::new(IdentityMapper::new(&IdentityConfig::default()).unwrap()),
    );
    let (_, rx) = mpsc::channel(1);
    let (ws, _) = broadcast::channel(1);
    let scheduler = Scheduler::new(
        Arc::new(engine),
        Duration::from_secs(1),
        rx,
        ws,
        Arc::new(RwLock::new(ImportProgress::default())),
        db,
        config,
    );
    (tmp, scheduler)
}

fn install_allowed_observer() -> &'static Mutex<Vec<(String, Vec<String>)>> {
    PARSED_ALLOWED_PATHS.get_or_init(|| Mutex::new(Vec::new()))
}

fn install_blocked_observer() -> &'static Mutex<Vec<(String, Vec<String>)>> {
    PARSED_BLOCKED_PATTERNS.get_or_init(|| Mutex::new(Vec::new()))
}

fn parsed_allowed(repo_id: &str) -> Vec<Vec<String>> {
    install_allowed_observer()
        .lock()
        .expect("parsed allowed paths")
        .iter()
        .filter(|(id, _)| id == repo_id)
        .map(|(_, paths)| paths.clone())
        .collect()
}

fn parsed_blocked(repo_id: &str) -> Vec<Vec<String>> {
    install_blocked_observer()
        .lock()
        .expect("parsed blocked patterns")
        .iter()
        .filter(|(id, _)| id == repo_id)
        .map(|(_, patterns)| patterns.clone())
        .collect()
}

fn join_failure(error: tokio::task::JoinError) -> String {
    let panicked = error.is_panic();
    let detail = match error.try_into_panic() {
        Ok(payload) => {
            if let Some(message) = payload.downcast_ref::<&str>() {
                (*message).to_string()
            } else if let Some(message) = payload.downcast_ref::<String>() {
                message.clone()
            } else {
                "non-string panic payload".to_string()
            }
        }
        Err(error) => error.to_string(),
    };
    format!("spawned sync cycle failed: panicked={panicked}: {detail}")
}

const SPAWN_JOIN_LIMIT: Duration = Duration::from_secs(30);
const SPAWN_DRAIN_LIMIT: Duration = Duration::from_secs(1);

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn cleanup_note(drained: usize, still_running: usize) -> String {
    if still_running == 0 {
        format!("{drained} drained, aborted and drained")
    } else {
        format!(
            "{drained} drained, abort requested but {still_running} task(s) still running after bounded drain; block_in_place cannot be cancelled and dropping the JoinHandle detaches; outer process bound required"
        )
    }
}

/// `JoinHandle::abort` asks the runtime to cancel. A closure already inside
/// `block_in_place` does not observe that request. Tokio 1.53.1
/// `task::block_in_place` documents that this code cannot be cancelled.
async fn abort_and_drain<I>(handles: I, drain_limit: Duration) -> (usize, usize)
where
    I: IntoIterator<Item = tokio::task::JoinHandle<()>>,
{
    let mut drained = 0usize;
    let mut still_running = 0usize;
    for mut handle in handles {
        handle.abort();
        match tokio::time::timeout(drain_limit, &mut handle).await {
            Ok(_) => drained += 1,
            Err(_elapsed) => still_running += 1,
        }
    }
    (drained, still_running)
}

/// Poll each handle by mutable reference. `timeout` dropping that future
/// must not drop the `JoinHandle`: Tokio 1.53.1 detaches on `Drop` and does
/// not abort. `&mut JoinHandle` is cancel-safe, so a timeout leaves the
/// output owned by the handle. On failure the current handle, when it has
/// not already completed, and every remaining handle are aborted and then
/// awaited until `drain_limit`. A task still inside `block_in_place` is
/// counted as still running; dropping it afterwards detaches it, and the
/// outer process bound is what stops it.
async fn join_spawned_within(
    handles: Vec<tokio::task::JoinHandle<()>>,
    join_limit: Duration,
    drain_limit: Duration,
) -> Result<usize, String> {
    let count = handles.len();
    let mut pending = handles.into_iter();
    while let Some(mut handle) = pending.next() {
        match tokio::time::timeout(join_limit, &mut handle).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let failure = join_failure(error);
                // The output was taken. Polling this handle again is not safe.
                handle.abort();
                let (drained, still_running) = abort_and_drain(pending, drain_limit).await;
                return Err(format!(
                    "{failure}; cleanup: {}",
                    cleanup_note(drained, still_running)
                ));
            }
            Err(_elapsed) => {
                let (drained, still_running) =
                    abort_and_drain(std::iter::once(handle).chain(pending), drain_limit).await;
                return Err(format!(
                    "spawned sync cycle did not finish; cleanup: {}",
                    cleanup_note(drained, still_running)
                ));
            }
        }
    }
    Ok(count)
}

async fn join_spawned(handles: Vec<tokio::task::JoinHandle<()>>) -> Result<usize, String> {
    join_spawned_within(handles, SPAWN_JOIN_LIMIT, SPAWN_DRAIN_LIMIT).await
}

async fn spawned(scheduler: &Scheduler) -> Result<usize, String> {
    let handles = scheduler.sync_handles.lock().await.drain(..).collect();
    join_spawned(handles).await
}

fn prepare_owned_git_repo(path: &std::path::Path) {
    std::fs::create_dir_all(path).expect("git repo dir");
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    };
    // The configured branch is main. An unborn HEAD makes head_sha fail
    // before history inspection, and that GitError is persisted as error.
    run(&["init", "--initial-branch", "main"]);
    std::fs::write(path.join("README"), b"fixture\n").expect("readme");
    run(&["add", "README"]);
    run(&[
        "-c",
        "user.name=fixture",
        "-c",
        "user.email=fixture@example.com",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-m",
        "fixture",
    ]);
    // A clean born HEAD continues to git ls-remote. This untracked file is
    // refused as local_dirty before that command.
    std::fs::write(path.join("unpublished.txt"), b"not committed\n").expect("dirty file");
}

fn assert_admitted_worker_did_not_sync(scheduler: &Scheduler, repo_id: &str) {
    let repo = stored(scheduler, repo_id);
    assert_eq!(
        repo.sync_status, "reconciliation_required",
        "{repo_id} was admitted, so the worker must record a blocked cycle"
    );
    assert_eq!(repo.consecutive_errors, 1, "{repo_id}");
    assert_eq!(repo.total_errors, 1, "{repo_id}");
    assert_eq!(repo.total_syncs, 0, "{repo_id}");
    assert_eq!(repo.last_svn_rev, 2, "{repo_id}");
    assert_eq!(repo.last_git_sha, "verified-old", "{repo_id}");
    let audits = scheduler.db.list_audit_log(20, 0).expect("audit log");
    assert_eq!(
        audits
            .iter()
            .filter(|entry| entry.action == "sync_cycle" && !entry.success)
            .count(),
        1,
        "{repo_id}"
    );
    assert_eq!(
        audits
            .iter()
            .filter(|entry| entry.action == "sync_cycle" && entry.success)
            .count(),
        0,
        "{repo_id} admission must not be recorded as a successful sync"
    );
    assert!(
        audits.iter().any(|entry| {
            entry.action == "sync_cycle"
                && !entry.success
                && entry.details.as_deref().is_some_and(|details| {
                    details.contains("team history reconciliation required (local_dirty)")
                        && details.contains("bridge index or worktree has unpublished changes")
                })
        }),
        "{repo_id} must record the local_dirty history refusal"
    );
    let key = match reposync_core::history_inspect::history_block_key_for_repo(&scheduler.db, repo_id)
    {
        Ok(key) => key,
        Err(_) => reposync_core::history_inspect::history_block_key(Some(repo_id)),
    };
    let raw = scheduler
        .db
        .get_state(&key)
        .expect("history block read")
        .unwrap_or_else(|| panic!("{repo_id} missing history block {key}"));
    let block: serde_json::Value = serde_json::from_str(&raw).expect("history block json");
    assert_eq!(block["state"], "reconciliation_required", "{repo_id}");
    assert_eq!(block["reason"], "local_dirty", "{repo_id}");
    assert_eq!(
        block["detail"], "bridge index or worktree has unpublished changes",
        "{repo_id}"
    );
    assert!(
        block["r_fresh_remote"].is_null(),
        "{repo_id} refusal must happen before a remote tip"
    );
    let bridge = block["l_bridge"]
        .as_str()
        .unwrap_or_else(|| panic!("{repo_id} missing local HEAD"));
    assert_eq!(bridge.len(), 40, "{repo_id} {bridge}");
    assert!(
        bridge.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{repo_id} {bridge}"
    );
}

fn stored(scheduler: &Scheduler, repo_id: &str) -> Repository {
    scheduler
        .db
        .get_repository(repo_id)
        .unwrap()
        .expect("repository")
}

async fn assert_allowed_refused(id: &'static str, raw: &'static str) {
    install_allowed_observer();
    install_blocked_observer();
    let (_tmp, scheduler) = due_scheduler(&[DueRepo {
        id,
        name: id,
        allowed_paths: Some(raw),
        blocked_patterns: Some("[\"*.bin\"]"),
    }]);
    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let log = buf.lock().expect("log buffer").clone();
    assert!(
        parsed_allowed(id).is_empty(),
        "{raw} must not parse as an empty allow list"
    );
    assert!(
        parsed_blocked(id).is_empty(),
        "{raw} must refuse before blocked_patterns is applied"
    );
    assert!(
        logged(
            &log,
            "allowed_paths must be a JSON array of strings",
            id
        ),
        "{raw}"
    );
    assert!(
        logged(&log, "setting=allowed_paths", id)
            || logged(&log, "setting=\"allowed_paths\"", id),
        "{raw}"
    );
    assert!(!logged(&log, "starting per-repo sync cycle", id), "{raw}");
    assert!(!logged(&log, "path validation rules configured", id), "{raw}");
    assert_eq!(
        spawned(&scheduler)
            .await
            .expect("refused repository must not spawn a worker"),
        0,
        "{raw}"
    );
    let repo = stored(&scheduler, id);
    assert_eq!(repo.sync_status, "idle");
    assert_eq!(repo.consecutive_errors, 0);
    assert_eq!(repo.allowed_paths.as_deref(), Some(raw));
    assert_eq!(repo.blocked_patterns.as_deref(), Some("[\"*.bin\"]"));
    assert_eq!(repo.last_svn_rev, 2);
    assert_eq!(repo.last_git_sha, "verified-old");
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_allowed_paths_refuse_that_repository() {
    assert_allowed_refused("malformed-allow-repo", "{").await;
}

#[tokio::test(flavor = "current_thread")]
async fn scalar_allowed_paths_refuse_that_repository() {
    assert_allowed_refused("scalar-string-allow-repo", "\"src\"").await;
    assert_allowed_refused("scalar-number-allow-repo", "1").await;
    assert_allowed_refused("scalar-bool-allow-repo", "true").await;
}

#[tokio::test(flavor = "current_thread")]
async fn object_allowed_paths_refuse_that_repository() {
    assert_allowed_refused("object-allow-repo", "{}").await;
}

#[tokio::test(flavor = "current_thread")]
async fn json_null_allowed_paths_refuse_that_repository() {
    assert_allowed_refused("json-null-allow-repo", "null").await;
}

#[tokio::test(flavor = "current_thread")]
async fn non_string_allowed_paths_arrays_refuse_that_repository() {
    assert_allowed_refused("numeric-allow-repo", "[1]").await;
    assert_allowed_refused("mixed-allow-repo", "[\"src/\",1]").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_allowed_paths_array_starts_without_a_restriction() {
    install_allowed_observer();
    let (_tmp, scheduler) = due_scheduler(&[DueRepo {
        id: "empty-allow-repo",
        name: "empty-allow-repo",
        allowed_paths: Some("[]"),
        blocked_patterns: None,
    }]);
    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let log = buf.lock().expect("log buffer").clone();
    assert_eq!(parsed_allowed("empty-allow-repo"), vec![Vec::<String>::new()]);
    assert!(!logged(
        &log,
        "path validation rules configured",
        "empty-allow-repo"
    ));
    assert!(logged(
        &log,
        "starting per-repo sync cycle",
        "empty-allow-repo"
    ));
    assert!(!logged(
        &log,
        "refusing repository sync",
        "empty-allow-repo"
    ));
    assert_eq!(
        spawned(&scheduler)
            .await
            .expect("admitted worker must finish without panic"),
        1
    );
    assert_admitted_worker_did_not_sync(&scheduler, "empty-allow-repo");
    let repo = stored(&scheduler, "empty-allow-repo");
    assert_eq!(repo.allowed_paths.as_deref(), Some("[]"));
    assert_eq!(repo.blocked_patterns, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absent_allowed_paths_start_without_a_restriction() {
    install_allowed_observer();
    let (_tmp, scheduler) = due_scheduler(&[DueRepo {
        id: "absent-allow-repo",
        name: "absent-allow-repo",
        allowed_paths: None,
        blocked_patterns: None,
    }]);
    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let log = buf.lock().expect("log buffer").clone();
    assert!(parsed_allowed("absent-allow-repo").is_empty());
    assert!(!logged(
        &log,
        "path validation rules configured",
        "absent-allow-repo"
    ));
    assert!(logged(
        &log,
        "starting per-repo sync cycle",
        "absent-allow-repo"
    ));
    assert!(!logged(
        &log,
        "refusing repository sync",
        "absent-allow-repo"
    ));
    assert_eq!(
        spawned(&scheduler)
            .await
            .expect("admitted worker must finish without panic"),
        1
    );
    assert_admitted_worker_did_not_sync(&scheduler, "absent-allow-repo");
    let repo = stored(&scheduler, "absent-allow-repo");
    assert_eq!(repo.allowed_paths, None);
    assert_eq!(repo.blocked_patterns, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_allowed_path_array_configures_rules_and_starts_the_cycle() {
    install_allowed_observer();
    let (_tmp, scheduler) = due_scheduler(&[DueRepo {
        id: "valid-allow-repo",
        name: "valid-allow-repo",
        allowed_paths: Some("[\"src/\"]"),
        blocked_patterns: None,
    }]);
    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let log = buf.lock().expect("log buffer").clone();
    assert_eq!(
        parsed_allowed("valid-allow-repo"),
        vec![vec!["src/".to_string()]]
    );
    assert!(logged(
        &log,
        "path validation rules configured",
        "valid-allow-repo"
    ));
    assert!(logged(
        &log,
        "starting per-repo sync cycle",
        "valid-allow-repo"
    ));
    assert!(!logged(
        &log,
        "refusing repository sync",
        "valid-allow-repo"
    ));
    assert_eq!(
        spawned(&scheduler)
            .await
            .expect("admitted worker must finish without panic"),
        1
    );
    assert_admitted_worker_did_not_sync(&scheduler, "valid-allow-repo");
    let repo = stored(&scheduler, "valid-allow-repo");
    assert_eq!(repo.allowed_paths.as_deref(), Some("[\"src/\"]"));
    assert_eq!(repo.blocked_patterns, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_allowed_paths_leave_the_other_repository_running() {
    install_allowed_observer();
    install_blocked_observer();
    let (_tmp, scheduler) = due_scheduler(&[
        DueRepo {
            id: "a-bad-allow",
            name: "a-bad-allow",
            allowed_paths: Some("{"),
            blocked_patterns: Some("[\"*.bin\"]"),
        },
        DueRepo {
            id: "b-good-allow",
            name: "b-good-allow",
            allowed_paths: None,
            blocked_patterns: Some("[\"secret-*\"]"),
        },
    ]);
    let (buf, _guard) = capture();
    scheduler.maybe_run_repo_cycles().await;
    let log = buf.lock().expect("log buffer").clone();
    assert!(parsed_allowed("a-bad-allow").is_empty());
    assert!(parsed_blocked("a-bad-allow").is_empty());
    assert!(parsed_allowed("b-good-allow").is_empty());
    assert_eq!(
        parsed_blocked("b-good-allow"),
        vec![vec!["secret-*".to_string()]]
    );
    assert!(logged(
        &log,
        "allowed_paths must be a JSON array of strings",
        "a-bad-allow"
    ));
    assert!(!logged(&log, "starting per-repo sync cycle", "a-bad-allow"));
    assert!(logged(&log, "starting per-repo sync cycle", "b-good-allow"));
    assert!(logged(
        &log,
        "path validation rules configured",
        "b-good-allow"
    ));
    assert!(!logged(
        &log,
        "path validation rules configured",
        "a-bad-allow"
    ));
    assert_eq!(
        spawned(&scheduler)
            .await
            .expect("admitted worker must finish without panic"),
        1
    );
    let bad = stored(&scheduler, "a-bad-allow");
    assert_eq!(bad.sync_status, "idle");
    assert_eq!(bad.consecutive_errors, 0);
    assert_eq!(bad.total_errors, 0);
    assert_eq!(bad.total_syncs, 0);
    assert_eq!(bad.allowed_paths.as_deref(), Some("{"));
    assert_eq!(bad.blocked_patterns.as_deref(), Some("[\"*.bin\"]"));
    assert_eq!(bad.last_svn_rev, 2);
    assert_eq!(bad.last_git_sha, "verified-old");
    assert_admitted_worker_did_not_sync(&scheduler, "b-good-allow");
    let good = stored(&scheduler, "b-good-allow");
    assert_eq!(good.allowed_paths, None);
    assert_eq!(good.blocked_patterns.as_deref(), Some("[\"secret-*\"]"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_panic_cannot_satisfy_the_spawn_helper() {
    let finished = tokio::spawn(async {});
    assert_eq!(
        join_spawned(vec![finished])
            .await
            .expect("a finished worker counts"),
        1
    );
    let panicked = tokio::spawn(async {
        panic!("synthetic worker panic");
    });
    let error = join_spawned(vec![panicked])
        .await
        .expect_err("a panicked worker must not count as a finished cycle");
    assert!(error.contains("panicked=true"), "{error}");
    assert!(error.contains("synthetic worker panic"), "{error}");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn worker_timeout_aborts_and_drains_the_current_task_and_sibling() {
    let current_dropped = Arc::new(AtomicBool::new(false));
    let sibling_dropped = Arc::new(AtomicBool::new(false));
    let current_flag = DropFlag(current_dropped.clone());
    let sibling_flag = DropFlag(sibling_dropped.clone());
    let (current_tx, current_rx) = tokio::sync::oneshot::channel();
    let (sibling_tx, sibling_rx) = tokio::sync::oneshot::channel();
    let current = tokio::spawn(async move {
        let _flag = current_flag;
        let _ = current_tx.send(());
        std::future::pending::<()>().await;
    });
    let sibling = tokio::spawn(async move {
        let _flag = sibling_flag;
        let _ = sibling_tx.send(());
        std::future::pending::<()>().await;
    });
    current_rx.await.expect("current task entered");
    sibling_rx.await.expect("sibling task entered");
    let error = join_spawned_within(
        vec![current, sibling],
        Duration::from_secs(5),
        Duration::from_secs(1),
    )
    .await
    .expect_err("timeout must not count as a finished cycle");
    assert!(error.contains("did not finish"), "{error}");
    assert!(error.contains("2 drained"), "{error}");
    assert!(error.contains("aborted and drained"), "{error}");
    assert!(!error.contains("still running"), "{error}");
    assert!(
        current_dropped.load(Ordering::SeqCst),
        "the timed-out task must be aborted and dropped"
    );
    assert!(
        sibling_dropped.load(Ordering::SeqCst),
        "a later sibling must be aborted and dropped, not detached"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn worker_panic_aborts_and_drains_remaining_siblings() {
    let sibling_dropped = Arc::new(AtomicBool::new(false));
    let sibling_flag = DropFlag(sibling_dropped.clone());
    let (sibling_tx, sibling_rx) = tokio::sync::oneshot::channel();
    let sibling = tokio::spawn(async move {
        let _flag = sibling_flag;
        let _ = sibling_tx.send(());
        std::future::pending::<()>().await;
    });
    sibling_rx.await.expect("sibling task entered");
    let panicked = tokio::spawn(async {
        panic!("synthetic worker panic");
    });
    tokio::task::yield_now().await;
    let error = join_spawned_within(
        vec![panicked, sibling],
        Duration::from_secs(5),
        Duration::from_secs(1),
    )
    .await
    .expect_err("a panicked worker must not count as a finished cycle");
    assert!(error.contains("panicked=true"), "{error}");
    assert!(error.contains("synthetic worker panic"), "{error}");
    assert!(error.contains("1 drained"), "{error}");
    assert!(error.contains("aborted and drained"), "{error}");
    assert!(
        sibling_dropped.load(Ordering::SeqCst),
        "a sibling must be aborted after a panic, not detached"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn block_in_place_keeps_running_after_bounded_abort() {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let handle = tokio::spawn(async move {
        tokio::task::block_in_place(|| {
            let _ = entered_tx.send(());
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
        });
    });
    entered_rx.await.expect("closure entered block_in_place");
    let error = join_spawned_within(
        vec![handle],
        Duration::from_millis(50),
        Duration::from_millis(50),
    )
    .await
    .expect_err("block_in_place must outlive abort");
    assert!(error.contains("did not finish"), "{error}");
    assert!(error.contains("0 drained"), "{error}");
    assert!(error.contains("still running"), "{error}");
    assert!(error.contains("block_in_place cannot be cancelled"), "{error}");
    assert!(error.contains("detaches"), "{error}");
    assert!(error.contains("outer process bound required"), "{error}");
    let _ = release_tx.send(());
}
