//! Real owned preparation enrollment controls; no publication is authorized.
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use git2::{Oid, Repository, Signature, Time};
use uuid::Uuid;

#[path = "../../tests/fixtures/owned_process_group.rs"]
#[allow(clippy::duplicate_mod)]
mod fixture_process;

use super::*;

pub(super) struct SharedWriterTestGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

pub(super) struct WriterTestAdmission;
static SERIAL: WriterTestAdmission = WriterTestAdmission;

impl WriterTestAdmission {
    pub(super) async fn lock(&self) -> SharedWriterTestGuard {
        SharedWriterTestGuard {
            _guard: crate::writer_fence::tests::TEST_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        }
    }
}

fn run(command: &mut Command) -> String {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn svn(args: &[&str], directory: Option<&Path>) {
    let mut command = Command::new("svn");
    command
        .args(args)
        .args(["--non-interactive", "--no-auth-cache"]);
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    run(&mut command);
}

fn commit(repository: &Repository, bytes: &[u8], parents: &[Oid]) -> Oid {
    let mut tree = repository.treebuilder(None).unwrap();
    tree.insert("file.txt", repository.blob(bytes).unwrap(), 0o100644)
        .unwrap();
    let tree_id = tree.write().unwrap();
    let tree = repository.find_tree(tree_id).unwrap();
    let signature = Signature::new(
        "Disposable preparation fixture",
        "fixture@example.invalid",
        &Time::new(1_790_000_000, 0),
    )
    .unwrap();
    let parents = parents
        .iter()
        .map(|id| repository.find_commit(*id).unwrap())
        .collect::<Vec<_>>();
    let parents = parents.iter().collect::<Vec<_>>();
    repository
        .commit(None, &signature, &signature, "fixture", &tree, &parents)
        .unwrap()
}

// Case-owned resources are stored before observation. Local handles are only
// references to that storage; dropping an observer cannot detach the resource.
struct CaseServer {
    child: std::process::Child,
    group: i32,
    _root: std::sync::Arc<tempfile::TempDir>,
    stopping: Option<std::time::Instant>,
    killed: bool,
    reaped: bool,
    completed: bool,
    failure: Option<String>,
}

impl CaseServer {
    fn signal(&self, signal: i32) -> Result<(), String> {
        if self.group <= 0 { return Err("server has no owned process group".into()); }
        let result = unsafe { libc::kill(-self.group, signal) };
        if result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else { Err("owned server group signal failed".into()) }
    }

    fn request_stop(&mut self) -> Result<(), String> {
        if let Some(error) = &self.failure { return Err(error.clone()); }
        if self.stopping.is_none() {
            self.stopping = Some(std::time::Instant::now());
            if let Err(error) = self.signal(libc::SIGTERM) {
                self.failure = Some(error.clone());
                return Err(error);
            }
        }
        Ok(())
    }

    fn observe_stop(&mut self) -> Result<bool, String> {
        if let Some(error) = &self.failure { return Err(error.clone()); }
        if self.completed { return Ok(true); }
        let started = self.stopping.ok_or_else(|| "server stop was not requested".to_string())?;
        if !self.killed && started.elapsed() >= std::time::Duration::from_millis(500) {
            if let Err(error) = self.signal(libc::SIGKILL) {
                self.failure = Some(error.clone());
                return Err(error);
            }
            self.killed = true;
        }
        if !self.reaped {
            match self.child.try_wait() {
                Ok(Some(_status)) => self.reaped = true,
                Ok(None) => return Ok(false),
                Err(error) => {
                    let error = format!("server wait failed: {error}");
                    self.failure = Some(error.clone());
                    return Err(error);
                }
            }
        }
        // Reaping the leader (or ESRCH from signal) is not group quiescence.
        let quiet = crate::process::owned_group_quiescent(self.group)
            .map_err(|error| format!("server group observation failed: {error}"))?;
        self.completed = quiet;
        Ok(quiet)
    }
}

enum FixtureServer {
    Native(fixture_process::OwnedProcessGroup),
    Case(crate::process::CaseResourceHandle<CaseServer>),
}

impl FixtureServer {
    fn take_stdout(&mut self) -> Result<std::process::ChildStdout, String> {
        let stdout = match self {
            Self::Native(server) => server.stdout.take(),
            Self::Case(handle) => crate::process::with_case_resource(*handle,
                |server| server.child.stdout.take())?,
        };
        stdout.ok_or_else(|| "fixture readiness pipe is missing".to_string())
    }
}

struct CaseNativeLeases {
    _fence: crate::writer_fence::WriterFenceGuard,
    _lock: crate::data_dir::LockGuard,
    _root: std::sync::Arc<tempfile::TempDir>,
}

struct CaseShutdown {
    service: std::sync::Arc<crate::pair_refresh::service::RefreshService>,
    started: bool,
    handle: Option<tokio::task::JoinHandle<()>>,
    outcome: Option<Result<(), String>>,
}

impl CaseShutdown {
    fn start(&mut self, started: Option<tokio::sync::oneshot::Sender<()>>) -> Result<(), String> {
        if self.started { return Err("shutdown was already started".into()); }
        self.started = true;
        self.outcome = Some(Err("shutdown spawn did not finish".into()));
        let service = std::sync::Arc::clone(&self.service);
        let handle = tokio::spawn(async move {
            if let Some(started) = started { let _ = started.send(()); }
            service.shutdown().await;
        });
        self.handle = Some(handle);
        self.outcome = None;
        Ok(())
    }

    fn poll_join(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), String>> {
        use std::future::Future;
        if !self.started {
            return std::task::Poll::Ready(Err("shutdown remains armed, not started".into()));
        }
        if let Some(outcome) = &self.outcome { return std::task::Poll::Ready(outcome.clone()); }
        let Some(handle) = self.handle.as_mut() else {
            return std::task::Poll::Ready(Err("shutdown handle is missing".into()));
        };
        match std::pin::Pin::new(handle).poll(cx) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(joined) => {
                self.handle.take();
                let outcome = joined.map_err(|error| format!("shutdown join failed: {error}"));
                self.outcome = Some(outcome.clone());
                std::task::Poll::Ready(outcome)
            }
        }
    }
}

struct Fixture {
    db: Database,
    _server: Option<FixtureServer>,
    target: Repository,
    _parent_wc: PathBuf,
    root: String,
    baseline: Oid,
    parent: Oid,
    client: reqwest::Client,
    log: PathBuf,
    temp: std::sync::Arc<tempfile::TempDir>,
}

impl Fixture {
    fn new() -> Self {
        Self::open(None).expect("native enrollment fixture")
    }

    async fn complete_owned_server(&mut self) -> Result<(), String> {
        let handle = match self._server.as_ref() {
            Some(FixtureServer::Case(handle)) => *handle,
            _ => return Err("completion needs the registered case server".into()),
        };
        crate::process::with_case_resource(handle, |server| server.request_stop())??;
        let observed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if crate::process::with_case_resource(handle, |server| server.observe_stop())?? {
                    return Ok::<(), String>(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }).await.map_err(|_| "server cleanup observation timed out; resource retained".to_string())?;
        observed?;
        crate::process::finish_case_resource(handle)?;
        self._server.take();
        Ok(())
    }

    fn open(
        mut early: Option<
            &mut dyn FnMut(
                &std::path::Path,
                &std::path::Path,
            ) -> Result<packaged_call_site::SpawnContext, String>,
        >,
    ) -> Result<Self, String> {
        let temp = std::sync::Arc::new(tempfile::tempdir().unwrap());
        let db = Database::new(temp.path().join("reposync.db")).unwrap();
        db.initialize().unwrap();
        let svn_repository = temp.path().join("svn-repository");
        run(Command::new("svnadmin").arg("create").arg(&svn_repository));
        let root = reqwest::Url::from_file_path(svn_repository.canonicalize().unwrap())
            .unwrap()
            .to_string();
        svn(
            &[
                "mkdir",
                &format!("{root}/trunk"),
                &format!("{root}/branches"),
                "-m",
                "layout",
            ],
            None,
        );
        let parent_wc = temp.path().join("parent-wc");
        svn(
            &[
                "checkout",
                &format!("{root}/trunk"),
                parent_wc.to_str().unwrap(),
            ],
            None,
        );
        std::fs::write(parent_wc.join("file.txt"), b"baseline\n").unwrap();
        svn(&["add", "file.txt"], Some(&parent_wc));
        svn(&["commit", "-m", "baseline"], Some(&parent_wc));
        svn(
            &[
                "copy",
                &format!("{root}/trunk@2"),
                &format!("{root}/branches/f"),
                "-m",
                "direct copy",
            ],
            None,
        );
        std::fs::write(parent_wc.join("file.txt"), b"advanced\n").unwrap();
        svn(&["commit", "-m", "parent advance"], Some(&parent_wc));

        let target = Repository::init_bare(temp.path().join("target.git")).unwrap();
        let baseline = commit(&target, b"baseline\n", &[]);
        let parent = commit(&target, b"advanced\n", &[baseline]);
        target
            .reference("refs/heads/feature", baseline, false, "fixture")
            .unwrap();
        target
            .reference("refs/heads/main", parent, false, "fixture")
            .unwrap();
        let cert = temp.path().join("cert.pem");
        let key = temp.path().join("key.pem");
        let config = temp.path().join("cert.cnf");
        std::fs::write(&config, "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=localhost\n[ext]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:false\n").unwrap();
        run(Command::new("openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-days", "1", "-nodes", "-keyout",
            ])
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .arg("-config")
            .arg(&config));
        let der = temp.path().join("cert.der");
        run(Command::new("openssl")
            .arg("x509")
            .arg("-in")
            .arg(&cert)
            .args(["-outform", "DER", "-out"])
            .arg(&der));
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .add_root_certificate(
                reqwest::Certificate::from_der(&std::fs::read(der).unwrap()).unwrap(),
            )
            .build()
            .unwrap();
        let log = temp.path().join("requests.jsonl");
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pair_refresh_http_server.py");
        let mut command = Command::new("python3");
        command
            .arg(script)
            .arg(temp.path())
            .arg("--cert")
            .arg(&cert)
            .arg("--key")
            .arg(&key)
            .arg("--token")
            .arg("disposable-preparation-token")
            .arg("--request-log")
            .arg(&log)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let spawn_context = if let Some(callback) = early.as_mut() {
            callback(temp.path(), target.path())?
        } else {
            packaged_call_site::native_spawn_context()
        };
        packaged_call_site::apply_spawn_context(&mut command, &spawn_context)?;
        let mut server = if let Some(session) = spawn_context.session() {
            use std::os::unix::process::CommandExt;
            let root = std::sync::Arc::clone(&temp);
            FixtureServer::Case(crate::process::register_case_resource(session, || {
                let child = command.process_group(0).spawn().map_err(|error| error.to_string())?;
                let group = i32::try_from(child.id()).unwrap_or(0);
                Ok(CaseServer { child, group, _root: root, stopping: None, killed: false,
                    reaped: false, completed: false, failure: None })
            })?)
        } else {
            FixtureServer::Native(fixture_process::OwnedProcessGroup::spawn(&mut command)
                .map_err(|error| error.to_string())?)
        };
        let mut line = String::new();
        BufReader::new(server.take_stdout()?)
            .read_line(&mut line)
            .unwrap();
        let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
        let url = ready["url"].as_str().unwrap();
        db.set_state("encryption_key", &"41".repeat(32)).unwrap();
        for (id, parent_id, sha, revision, git_branch, svn_branch) in [
            ("parent", None, parent, 4, "main", "trunk"),
            (
                "child",
                Some("parent"),
                baseline,
                3,
                "feature",
                "branches/f",
            ),
        ] {
            let scope = Uuid::new_v4().to_string();
            db.conn().execute("INSERT INTO repositories(id,name,svn_url,svn_branch,svn_username,git_provider,git_api_url,git_repo,git_branch,sync_mode,poll_interval_secs,lfs_threshold_mb,auto_merge,enabled,created_at,updated_at,last_svn_rev,last_git_sha,sync_status,total_syncs,total_errors,parent_id,scope_uuid) VALUES(?1,?1,?2,?3,'','gitea',?4,'target',?5,'team',5,0,0,1,'t','t',?6,?7,'idle',0,0,?8,?9)", rusqlite::params![id, root, svn_branch, format!("{}/api/v1", url.trim_end_matches("/target.git")), git_branch, revision, sha.to_string(), parent_id, scope]).unwrap();
            db.set_state(&format!("last_svn_rev_{id}"), &revision.to_string())
                .unwrap();
            db.set_state(&format!("last_git_sha_{scope}"), &sha.to_string())
                .unwrap();
            db.set_state(
                &format!("secret_git_token_{id}"),
                "disposable-preparation-token",
            )
            .unwrap();
        }
        Ok(Self {
            db,
            _server: Some(server),
            target,
            _parent_wc: parent_wc,
            root,
            baseline,
            parent,
            client,
            log,
            temp,
        })
    }

    fn engine(&self) -> crate::sync_engine::SyncEngine {
        let mut config: crate::config::AppConfig = toml::from_str(
            r#"
            [daemon]
            data_dir = "/unused"
            [svn]
            url = "file:///unused/svn"
            username = ""
            password_env = ""
            [github]
            repo = "fixture/repo"
            token_env = ""
        "#,
        )
        .unwrap();
        config.daemon.data_dir = self.temp.path().to_owned();
        let created = (chrono::Utc::now() - chrono::Duration::minutes(2)).to_rfc3339();
        self.db
            .insert_user(&crate::models::User {
                id: "admin".into(),
                username: "admin".into(),
                display_name: "Admin".into(),
                email: "admin@example.invalid".into(),
                password_hash: "fixture".into(),
                role: "admin".into(),
                enabled: true,
                created_at: created.clone(),
                updated_at: created,
            })
            .unwrap();
        self.db
            .insert_session(&crate::models::Session {
                token: "disposable-workspace-session".into(),
                user_id: "admin".into(),
                created_at: (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
                expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            })
            .unwrap();
        let git_dir = self.temp.path().join("engine-git");
        Repository::init(&git_dir).unwrap();
        crate::sync_engine::SyncEngine::new(
            config,
            Database::new(self.temp.path().join("reposync.db")).unwrap(),
            crate::svn::SvnClient::new("file:///unused/svn", "", ""),
            crate::git::client::GitClient::new(git_dir).unwrap(),
            std::sync::Arc::new(
                crate::identity::mapper::IdentityMapper::new(
                    &crate::config::IdentityConfig::default(),
                )
                .unwrap(),
            ),
        )
    }

    fn state(&self) -> Vec<(String, String, String)> {
        self.db
            .conn()
            .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

fn key_table(f: &Fixture) -> bool {
    f.db.conn()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='pair_refresh_journal_key_v3')",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn journal_state(f: &Fixture) -> Vec<(String, String, String)> {
    f.state()
        .into_iter()
        .filter(|(key, _, _)| key != "writer_fence_v1:lease")
        .collect()
}

async fn prepared<'engine>(
    f: &Fixture,
    engine: &'engine crate::sync_engine::SyncEngine,
) -> (
    super::super::actor_policy::LiveRefreshActorPolicy<'engine>,
    super::super::workspace::RefreshWorkspace,
    LivePreparedRefresh,
) {
    let actor = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let workspace = actor.create_workspace(engine).unwrap();
    let prepared = prepare_registered_in_workspace_for_test(
        &f.db,
        "child",
        workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    (actor, workspace, prepared)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_owned_preparation_enrolls_atomically_and_blocks_every_logical_writer() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, workspace, prepared) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    assert!(!key_table(&f));
    let op = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
    assert_eq!(op.version, 3);
    assert_eq!(
        op.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::Prepared
    );
    assert!(key_table(&f));
    let rows = journal_state(&f);
    assert_eq!(
        rows.iter()
            .filter(|(key, _, _)| key.starts_with("pair_refresh_execution_v3:"))
            .count(),
        5
    );
    assert!(!rows
        .iter()
        .any(|(_, value, _)| value.contains("disposable-workspace-session")));
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert_eq!(
        actor
            .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
            .unwrap(),
        op
    );
    assert_eq!(journal_state(&f), rows);
    for scope in [
        None,
        Some("child"),
        Some("parent"),
        Some("unregistered-alias"),
    ] {
        assert!(
            crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), scope)
                .is_err()
        );
        assert!(f.db.begin_ordinary_writer_window(scope).is_err());
    }
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_registration_mutation(
            &f.db.conn(),
            "unregistered-alias"
        )
        .is_err()
    );
    assert!(f
        .db
        .set_state("secret_git_token_unregistered-alias", "replacement")
        .is_err());
    assert_eq!(journal_state(&f), rows);
    drop(busy);
    drop(workspace);
    drop(prepared);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert!(op.pins.svn.working_copy.join(".svn").is_dir());
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(f.parent)
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/trunk", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        4
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn known_refusals_leave_key_job_request_and_holds_absent() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let original = journal_state(&f);
    for (request, approved) in [("", digest.as_str()), ("request", "invalid")] {
        assert!(actor
            .enroll_prepared(&engine, &prepared, request, approved, &busy)
            .is_err());
        assert_eq!(journal_state(&f), original);
        assert!(!key_table(&f));
    }
    let wrong_busy = crate::busy::try_acquire_pair("foreign-one", "foreign-two").unwrap();
    assert!(actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &wrong_busy)
        .is_err());
    assert_eq!(journal_state(&f), original);
    assert!(!key_table(&f));
    f.db.delete_session("disposable-workspace-session").unwrap();
    assert!(actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .is_err());
    assert_eq!(journal_state(&f), original);
    assert!(!key_table(&f));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrelated_ordinary_window_refuses_authenticated_enrollment() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let window =
        f.db.begin_ordinary_writer_window(Some("unregistered-alias"))
            .unwrap();
    let original = journal_state(&f);
    assert!(actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .is_err());
    assert_eq!(journal_state(&f), original);
    assert!(!key_table(&f));
    window.finish_preparation().unwrap();
    actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unfinished_unregistered_journal_and_unknown_family_version_refuse_without_key_repair() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    for key in [
        "import_operation_v1:active:deleted-id",
        "import_operation_v1:latest:deleted-id",
        "import_operation_v99:document:deleted-id",
        "managed_remove_v1:tombstone:deleted-id",
    ] {
        f.db.set_state(key, "missing-or-corrupt").unwrap();
        let original = journal_state(&f);
        assert!(
            actor
                .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
                .is_err(),
            "{key}"
        );
        assert_eq!(journal_state(&f), original);
        assert!(!key_table(&f));
        f.db.conn()
            .execute("DELETE FROM kv_state WHERE key=?1", [key])
            .unwrap();
    }
    actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn altered_missing_and_orphan_authority_cannot_downgrade_the_global_hold() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
    let saved = journal_state(&f);
    let authority_key = format!("pair_refresh_execution_v3:authority:{}", op.id);
    let document_key = format!("pair_refresh_execution_v3:document:{}", op.id);
    let authority = f.db.get_state(&authority_key).unwrap().unwrap();
    let document = f.db.get_state(&document_key).unwrap().unwrap();
    for (key, value) in [
        (&authority_key, None),
        (&authority_key, Some("{}")),
        (&document_key, Some("{}")),
    ] {
        match value {
            Some(value) => {
                f.db.conn()
                    .execute(
                        "UPDATE kv_state SET value=?2 WHERE key=?1",
                        rusqlite::params![key, value],
                    )
                    .unwrap();
            }
            None => {
                f.db.conn()
                    .execute("DELETE FROM kv_state WHERE key=?1", [key])
                    .unwrap();
            }
        }
        let broken = journal_state(&f);
        assert!(f.db.pair_refresh_operation(op.id).is_err());
        assert!(f.db.begin_ordinary_writer_window(Some("alias")).is_err());
        assert_eq!(journal_state(&f), broken);
        f.db.conn().execute("INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,'restore') ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![authority_key,authority]).unwrap();
        f.db.conn()
            .execute(
                "UPDATE kv_state SET value=?2 WHERE key=?1",
                rusqlite::params![document_key, document],
            )
            .unwrap();
    }
    let mut tampered: serde_json::Value = serde_json::from_str(&document).unwrap();
    tampered["admitted_writer_epoch"] = serde_json::json!(op.admitted_writer_epoch + 1);
    let mut typed: crate::db::pair_refresh_operations::PairRefreshOperation =
        serde_json::from_value(tampered).unwrap();
    // Canonical valid structure still cannot replace signed phase/owner fields.
    typed.updated_at = chrono::Utc::now().to_rfc3339();
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?2 WHERE key=?1",
            rusqlite::params![document_key, serde_json::to_string(&typed).unwrap()],
        )
        .unwrap();
    assert!(f.db.pair_refresh_operation(op.id).is_err());
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?2 WHERE key=?1",
            rusqlite::params![document_key, document],
        )
        .unwrap();
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert!(f.db.record_pair_refresh_svn_intent(op.id).is_err());
    assert!(f
        .db
        .record_pair_refresh_svn_intent_with_registered_sources(op.id, &busy)
        .is_err());
    assert!(f.db.cancel_unpublished_pair_refresh(op.id).is_err());
    assert!(f
        .db
        .hold_pair_refresh_reconciliation(op.id, "legacy bypass")
        .is_err());
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert_eq!(
        saved
            .iter()
            .filter(|(key, _, _)| key.starts_with("pair_refresh_execution_v3:"))
            .count(),
        5
    );
    f.db.conn()
        .execute(
            "DELETE FROM kv_state WHERE key LIKE 'pair_refresh_execution_v3:%'",
            [],
        )
        .unwrap();
    assert!(f.db.begin_ordinary_writer_window(Some("alias")).is_err());
    assert!(key_table(&f));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_journal_key_survives_session_and_fingerprint_rotation_but_never_repairs_itself(
) {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
    f.db.delete_session("disposable-workspace-session").unwrap();
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key='encryption_key'",
            ["42".repeat(32)],
        )
        .unwrap();
    let saved = journal_state(&f);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert!(actor.require_current(&engine).is_err());
    assert_eq!(journal_state(&f), saved);
    f.db.conn()
        .execute("DELETE FROM pair_refresh_journal_key_v3", [])
        .unwrap();
    let broken = journal_state(&f);
    assert!(f.db.pair_refresh_operation(op.id).is_err());
    assert!(f.db.begin_ordinary_writer_window(Some("alias")).is_err());
    let count: i64 =
        f.db.conn()
            .query_row(
                "SELECT count(*) FROM pair_refresh_journal_key_v3",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(count, 0);
    assert_eq!(journal_state(&f), broken);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_authority_insert_rolls_back_key_and_every_enrollment_row() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let original = journal_state(&f);
    f.db.conn().execute_batch("CREATE TEMP TRIGGER enrollment_authority_abort BEFORE INSERT ON kv_state WHEN new.key LIKE 'pair_refresh_execution_v3:authority:%' BEGIN SELECT RAISE(ABORT,'disposable enrollment interruption'); END;").unwrap();
    // The engine owns a distinct connection, so install the fault there too.
    engine.db().conn().execute_batch("CREATE TEMP TRIGGER enrollment_authority_abort BEFORE INSERT ON kv_state WHEN new.key LIKE 'pair_refresh_execution_v3:authority:%' BEGIN SELECT RAISE(ABORT,'disposable enrollment interruption'); END;").unwrap();
    let error = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("disposable enrollment interruption"),
        "{error}"
    );
    assert!(!key_table(&f));
    assert_eq!(journal_state(&f), original);
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
    let wc = prepared.pins().svn.working_copy.clone();
    drop(workspace);
    drop(prepared);
    assert!(wc.join(".svn").is_dir());
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_copied_authenticated_storage_cannot_inherit_the_original_hold_authority() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
    let other = tempfile::tempdir().unwrap();
    let copied = other.path().join("copied.db");
    f.db.conn()
        .execute("VACUUM INTO ?1", [copied.to_str().unwrap()])
        .unwrap();
    let foreign = Database::new(copied).unwrap();
    let rows: Vec<(String, String, String)> = foreign
        .conn()
        .prepare("SELECT key,value,updated_at FROM kv_state ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows, f.state());
    let original_key: Vec<u8> =
        f.db.conn()
            .query_row(
                "SELECT secret FROM pair_refresh_journal_key_v3",
                [],
                |row| row.get(0),
            )
            .unwrap();
    let copied_key: Vec<u8> = foreign
        .conn()
        .query_row(
            "SELECT secret FROM pair_refresh_journal_key_v3",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(copied_key, original_key);
    assert!(foreign.pair_refresh_operation(op.id).is_err());
    assert!(foreign.begin_ordinary_writer_window(Some("alias")).is_err());
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_pre_intent_cancellation_returns_a_signed_durable_idempotent_receipt() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
        .unwrap();
    let cancelled = actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .unwrap();
    assert_eq!(cancelled.id, op.id);
    assert_eq!(
        cancelled.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::CancelledBeforePublication
    );
    assert_eq!(cancelled.pins, op.pins);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), cancelled);
    assert_eq!(
        actor
            .enroll_prepared(&engine, &prepared, "request", &digest, &busy)
            .unwrap(),
        cancelled
    );
    let saved = journal_state(&f);
    assert!(actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .is_err());
    assert!(actor
        .enroll_prepared(&engine, &prepared, "different-request", &digest, &busy)
        .is_err());
    assert_eq!(journal_state(&f), saved);
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(
            &f.db.conn(),
            Some("alias")
        )
        .is_ok()
    );
    assert_eq!(
        saved
            .iter()
            .filter(|(key, _, _)| key.starts_with("pair_refresh_execution_v3:"))
            .count(),
        3
    );
    let window = f.db.begin_ordinary_writer_window(Some("alias")).unwrap();
    window.finish_preparation().unwrap();
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(f.parent)
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/trunk", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        4
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_svn_stage_commits_once_and_keeps_global_exclusion_for_git_completion() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let before = f.state();
    let observed = super::super::execution::publish_owned_svn_stage_with_client(
        &engine,
        &actor,
        &prepared,
        "svn-stage",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        observed.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::SvnObserved
    );
    let receipt = observed.svn_receipt.as_ref().unwrap();
    assert_eq!(receipt.revision, 5);
    assert_eq!(receipt.git_tree, prepared.pins().git.tree_sha);
    assert_eq!(receipt.mergeinfo, "/trunk:3-4");
    assert!(observed.git_receipt_sha.is_none());
    assert_eq!(f.db.pair_refresh_operation(observed.id).unwrap(), observed);
    assert!(f
        .db
        .begin_ordinary_writer_window(Some("unregistered-alias"))
        .is_err());
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(f.parent)
    );
    assert_eq!(
        f.state()
            .into_iter()
            .filter(|(key, _, _)| !key.starts_with("pair_refresh_execution_")
                && key != "writer_fence_v1:lease")
            .collect::<Vec<_>>(),
        before
            .into_iter()
            .filter(|(key, _, _)| key != "writer_fence_v1:lease")
            .collect::<Vec<_>>()
    );
    let repeated = super::super::execution::publish_owned_svn_stage_with_client(
        &engine,
        &actor,
        &prepared,
        "svn-stage",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(repeated, observed);
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unhandled_svn_drift_before_first_intent_keeps_a_cancellable_prepared_job() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    std::fs::write(
        f._parent_wc.join("file.txt"),
        b"external unhandled advance\n",
    )
    .unwrap();
    svn(
        &["commit", "-m", "unhandled external fixture advance"],
        Some(&f._parent_wc),
    );
    assert!(
        super::super::execution::publish_owned_svn_stage_with_client(
            &engine,
            &actor,
            &prepared,
            "drift-stage",
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    let op =
        f.db.pair_refresh_operation(prepared.pins().git.operation_id)
            .unwrap();
    assert_eq!(
        op.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::Prepared
    );
    assert!(op.svn_receipt.is_none());
    assert!(op.git_receipt_sha.is_none());
    assert!(f.db.begin_ordinary_writer_window(Some("alias")).is_err());
    let cancelled = actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .unwrap();
    assert_eq!(
        cancelled.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::CancelledBeforePublication
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(f.parent)
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

fn parent_row(f: &Fixture) -> Vec<rusqlite::types::Value> {
    f.db.conn()
        .query_row("SELECT * FROM repositories WHERE id='parent'", [], |row| {
            (0..row.as_ref().column_count())
                .map(|index| row.get(index))
                .collect()
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_owned_refresh_preserves_histories_and_atomically_advances_only_child() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let parent_before = parent_row(&f);
    let echoes_before: Vec<_> = f
        .state()
        .into_iter()
        .filter(|(key, _, _)| key.starts_with("repo_echo_generation_"))
        .collect();
    let completed = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "complete-first",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        completed.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::Completed
    );
    assert_eq!(completed.svn_receipt.as_ref().unwrap().revision, 5);
    assert_eq!(
        completed.git_receipt_sha.as_deref(),
        Some(prepared.pins().git.prepared_sha.as_str())
    );
    assert_eq!(parent_row(&f), parent_before);
    assert_eq!(
        f.state()
            .into_iter()
            .filter(|(key, _, _)| key.starts_with("repo_echo_generation_"))
            .collect::<Vec<_>>(),
        echoes_before
    );
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(f.parent)
    );
    let integrated = f
        .target
        .find_commit(Oid::from_str(&prepared.pins().git.prepared_sha).unwrap())
        .unwrap();
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(integrated.id())
    );
    assert_eq!(
        integrated.parent_ids().collect::<Vec<_>>(),
        vec![f.baseline, f.parent]
    );
    assert_eq!(
        integrated.tree_id().to_string(),
        prepared.pins().svn.result_git_tree
    );
    assert!(f
        .target
        .graph_descendant_of(integrated.id(), f.baseline)
        .unwrap());
    assert!(f
        .target
        .graph_descendant_of(integrated.id(), f.parent)
        .unwrap());
    let (sha, rev): (String, i64) =
        f.db.conn()
            .query_row(
                "SELECT last_git_sha,last_svn_rev FROM repositories WHERE id='child'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert_eq!(sha, integrated.id().to_string());
    assert_eq!(rev, 5);
    assert_eq!(
        f.db.get_state(&format!("last_git_sha_{}", completed.pins.child.scope_uuid))
            .unwrap()
            .as_deref(),
        Some(sha.as_str())
    );
    assert_eq!(
        f.db.get_state("last_svn_rev_child").unwrap().as_deref(),
        Some("5")
    );
    let correspondence: i64 = f.db.conn().query_row("SELECT count(*) FROM commit_map WHERE repo_id='child' AND svn_rev=5 AND git_sha=?1 AND direction='git_to_svn'", [&sha], |row| row.get(0)).unwrap();
    let sync_records: i64 = f.db.conn().query_row("SELECT count(*) FROM sync_records WHERE id=?1 AND repo_id='child' AND svn_rev=5 AND git_sha=?2", rusqlite::params![completed.id.to_string(),sha], |row| row.get(0)).unwrap();
    assert_eq!(correspondence, 1);
    assert_eq!(sync_records, 1);
    assert_eq!(
        f.db.pair_refresh_operation(completed.id).unwrap(),
        completed
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
    let state_before_repeat = journal_state(&f);
    let repeated = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "complete-first",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(repeated, completed);
    assert_eq!(journal_state(&f), state_before_repeat);
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(integrated.id())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_owned_refresh_uses_its_signed_completed_baseline_and_preserves_copy_origin() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, first) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let first_approval = actor.prepared_approval_digest(&engine, &first).unwrap();
    let completed_first = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &first,
        "first-before-next",
        &first_approval,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    let first_sha = Oid::from_str(completed_first.git_receipt_sha.as_ref().unwrap()).unwrap();
    // Model an independently handled parent advance in this preparation fixture.
    // Full ordinary SyncEngine initialization/cycle qualification is separate.
    std::fs::write(f._parent_wc.join("file.txt"), b"second parent advance\n").unwrap();
    svn(
        &["commit", "-m", "second fixture parent advance"],
        Some(&f._parent_wc),
    );
    let advanced = commit(&f.target, b"second parent advance\n", &[f.parent]);
    assert!(f.target.graph_descendant_of(advanced, f.parent).unwrap());
    f.target
        .find_reference("refs/heads/main")
        .unwrap()
        .set_target(advanced, "fixture parent fast-forward")
        .unwrap();
    f.db.insert_commit_map_with_repo(
        6,
        &advanced.to_string(),
        "svn_to_git",
        "fixture",
        "fixture",
        Some("parent"),
    )
    .unwrap();
    f.db.update_repo_watermark("parent", 6, &advanced.to_string())
        .unwrap();
    crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
        &f.db.conn(),
        "parent",
        &advanced.to_string(),
        &chrono::Utc::now().to_rfc3339(),
    )
    .unwrap();
    let parent_before = parent_row(&f);
    let second_workspace = actor.create_workspace(&engine).unwrap();
    let second = prepare_registered_in_workspace_for_test(
        &f.db,
        "child",
        second_workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(second.pins().git.pins.child_tip, first_sha.to_string());
    assert_eq!(
        second.pins().git.pins.parent_baseline_tip,
        f.parent.to_string()
    );
    assert_eq!(
        second
            .pins()
            .svn
            .pins
            .lineage
            .pins
            .child
            .parent_baseline_revision,
        4
    );
    assert_eq!(
        second.pins().svn.pins.lineage.child_copy.copy_from_revision,
        2
    );
    assert_eq!(
        second.pins().svn.pins.lineage.child_copy.created_revision,
        3
    );
    let approved = actor.prepared_approval_digest(&engine, &second).unwrap();
    let completed_second = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &second,
        "next-refresh",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        completed_second.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::Completed
    );
    let receipt = completed_second.svn_receipt.as_ref().unwrap();
    assert_eq!(receipt.revision, 7);
    assert_eq!(receipt.mergeinfo, "/trunk:3-6");
    assert_eq!(receipt.child_copy.copy_from_revision, 2);
    assert_eq!(receipt.child_copy.created_revision, 3);
    assert_eq!(parent_row(&f), parent_before);
    let tip = f
        .target
        .find_reference("refs/heads/feature")
        .unwrap()
        .target()
        .unwrap();
    let integrated = f.target.find_commit(tip).unwrap();
    assert_eq!(
        integrated.parent_ids().collect::<Vec<_>>(),
        vec![first_sha, advanced]
    );
    assert_eq!(integrated.tree_id().to_string(), receipt.git_tree);
    assert_eq!(
        f.target.find_reference("refs/heads/main").unwrap().target(),
        Some(advanced)
    );
    assert!(f.target.graph_descendant_of(tip, f.baseline).unwrap());
    assert!(f.target.graph_descendant_of(tip, f.parent).unwrap());
    assert_eq!(
        f.db.pair_refresh_operation(completed_first.id).unwrap(),
        completed_first
    );
    assert_eq!(
        f.db.pair_refresh_operation(completed_second.id).unwrap(),
        completed_second
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_owner_observes_lost_svn_and_git_receipts_without_republication() {
    use crate::db::pair_refresh_operations::PairRefreshPhase;

    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let parent_before = parent_row(&f);
    f.db.conn().execute_batch("CREATE TRIGGER interrupt_svn_receipt BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_execution_v3:document:%' AND json_extract(NEW.value,'$.phase')='svn_observed' BEGIN SELECT RAISE(ABORT,'disposable SVN receipt interruption'); END;").unwrap();
    let error = super::super::execution::publish_owned_svn_stage_with_client(
        &engine,
        &actor,
        &prepared,
        "lost-receipts",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("disposable SVN receipt interruption"),
        "{error}"
    );
    let intent =
        f.db.pair_refresh_operation(prepared.pins().git.operation_id)
            .unwrap();
    assert_eq!(intent.phase, PairRefreshPhase::SvnIntent);
    assert!(intent.svn_receipt.is_none());
    let svn_client = crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "");
    assert_eq!(svn_client.info().await.unwrap().latest_rev, 5);
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    assert!(
        super::super::execution::publish_owned_svn_stage_with_client(
            &engine,
            &actor,
            &prepared,
            "lost-receipts",
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    assert_eq!(svn_client.info().await.unwrap().latest_rev, 5);
    assert_eq!(f.db.pair_refresh_operation(intent.id).unwrap(), intent);
    f.db.conn()
        .execute_batch("DROP TRIGGER interrupt_svn_receipt;")
        .unwrap();
    let observed = super::super::execution::reconcile_owned_refresh_intent_with_client(
        &engine,
        &actor,
        &prepared,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(observed.phase, PairRefreshPhase::SvnObserved);
    assert_eq!(observed.svn_receipt.as_ref().unwrap().revision, 5);
    assert_eq!(svn_client.info().await.unwrap().latest_rev, 5);
    f.db.conn().execute_batch("CREATE TRIGGER interrupt_git_receipt BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_execution_v3:document:%' AND json_extract(NEW.value,'$.phase')='git_observed' BEGIN SELECT RAISE(ABORT,'disposable Git receipt interruption'); END;").unwrap();
    let error = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "lost-receipts",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("disposable Git receipt interruption"),
        "{error}"
    );
    let intent = f.db.pair_refresh_operation(intent.id).unwrap();
    assert_eq!(intent.phase, PairRefreshPhase::GitIntent);
    assert!(intent.git_receipt_sha.is_none());
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target()
            .unwrap()
            .to_string(),
        prepared.pins().git.prepared_sha
    );
    let writes_before_reconciliation = std::fs::read_to_string(&f.log)
        .unwrap()
        .lines()
        .filter(|line| line.contains("/git-receive-pack"))
        .count();
    assert!(writes_before_reconciliation > 0);
    assert!(super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "lost-receipts",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .is_err());
    assert_eq!(f.db.pair_refresh_operation(intent.id).unwrap(), intent);
    f.db.conn()
        .execute_batch("DROP TRIGGER interrupt_git_receipt;")
        .unwrap();
    let observed = super::super::execution::reconcile_owned_refresh_intent_with_client(
        &engine,
        &actor,
        &prepared,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(observed.phase, PairRefreshPhase::GitObserved);
    assert_eq!(
        observed.git_receipt_sha.as_deref(),
        Some(prepared.pins().git.prepared_sha.as_str())
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    let completed = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "lost-receipts",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(completed.phase, PairRefreshPhase::Completed);
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        writes_before_reconciliation
    );
    assert_eq!(svn_client.info().await.unwrap().latest_rev, 5);
    assert_eq!(parent_row(&f), parent_before);
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denied_svn_intent_and_absence_observation_never_authorize_a_retry() {
    use crate::db::pair_refresh_operations::PairRefreshPhase;
    use std::os::unix::fs::PermissionsExt;

    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let parent_before = parent_row(&f);
    let hook = f.temp.path().join("svn-repository/hooks/pre-commit");
    std::fs::write(
        &hook,
        b"#!/bin/sh\nprintf '%s\\n' 'disposable SVN permission refusal' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        super::super::execution::publish_owned_svn_stage_with_client(
            &engine,
            &actor,
            &prepared,
            "denied-svn",
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    let intent =
        f.db.pair_refresh_operation(prepared.pins().git.operation_id)
            .unwrap();
    assert_eq!(intent.phase, PairRefreshPhase::SvnIntent);
    assert!(intent.reconciliation_detail.is_some());
    assert!(intent.svn_receipt.is_none());
    let before = journal_state(&f);
    std::fs::remove_file(hook).unwrap();
    assert!(
        super::super::execution::reconcile_owned_refresh_intent_with_client(
            &engine,
            &actor,
            &prepared,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    assert_eq!(journal_state(&f), before);
    assert!(
        super::super::execution::publish_owned_svn_stage_with_client(
            &engine,
            &actor,
            &prepared,
            "denied-svn",
            &approved,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    assert!(actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .is_err());
    assert_eq!(journal_state(&f), before);
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        4
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(parent_row(&f), parent_before);
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
}

struct SvnDiag {
    _path: std::path::PathBuf,
}

impl SvnDiag {
    fn arm(temp: &std::path::Path) -> Self {
        let path = temp.join("svn-diag.log");
        std::env::set_var("REPOSYNC_SVN_DIAG_LOG", &path);
        Self { _path: path }
    }
}

impl Drop for SvnDiag {
    fn drop(&mut self) {
        std::env::remove_var("REPOSYNC_SVN_DIAG_LOG");
    }
}

fn exec_witness(label: &str, path: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map(|meta| format!("{:o}", meta.permissions().mode()))
        .unwrap_or_else(|error| format!("metadata {error}"));
    let test_x = std::process::Command::new("test")
        .arg("-x")
        .arg(path)
        .status()
        .map(|status| {
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".into())
        })
        .unwrap_or_else(|error| format!("spawn {error}"));
    format!(
        "{label} path={} mode={mode} test_x={test_x}",
        path.display()
    )
}

fn file_text(path: &std::path::Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => format!(
            "{} bytes={}",
            path.display(),
            String::from_utf8_lossy(&bytes)
        ),
        Err(error) => format!("{} absent ({error})", path.display()),
    }
}

fn case_diag(temp: &std::path::Path, hook: &std::path::Path, ready: &std::path::Path) -> String {
    format!(
        "{} ready={} diag={} backend={}",
        exec_witness("hook", hook),
        file_text(ready),
        file_text(&temp.join("svn-diag.log")),
        file_text(&temp.join("http-backend-stderr.txt"))
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protected_git_ref_retains_the_svn_result_without_retrying_a_push() {
    #[cfg(unix)]
    let _process_boundary = packaged_call_site::enter_process_boundary()
        .expect("serialization owner is free");
    #[cfg(unix)]
    match packaged_call_site::attest_catalogue() {
        Ok(packaged_call_site::CatalogueAttestation::PackagedImmutable { manifest }) => {
            if let Err(error) = packaged_f06_protected_ref(manifest, &_process_boundary).await {
                panic!("packaged call site failed closed: {error}");
            }
            return;
        }
        Ok(packaged_call_site::CatalogueAttestation::HistoricalNativeNotCatalogueProof) => {}
        Err(error) => panic!("missing H failed closed: {error}"),
    }
    use crate::db::pair_refresh_operations::PairRefreshPhase;
    use std::os::unix::fs::PermissionsExt;

    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let parent_before = parent_row(&f);
    let hook = f.target.path().join("hooks/pre-receive");
    std::fs::write(
        &hook,
        b"#!/bin/sh\nprintf '%s\\n' 'disposable protected ref refusal' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let before = f
        .target
        .find_reference("refs/heads/feature")
        .unwrap()
        .target();
    let result = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "protected-child",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await;
    let after = f
        .target
        .find_reference("refs/heads/feature")
        .ok()
        .and_then(|reference| reference.target());
    let summary = match &result {
        Ok(_) => "Ok".to_string(),
        Err(error) => crate::errors::redact_vcs_error_detail(&format!("{error:#}")),
    };
    assert!(
        result.is_err(),
        "result={summary} before={before:?} after={after:?} {}",
        case_diag(f.temp.path(), &hook, &f.temp.path().join("git-hook-ready"))
    );
    let intent =
        f.db.pair_refresh_operation(prepared.pins().git.operation_id)
            .unwrap();
    assert_eq!(intent.phase, PairRefreshPhase::GitIntent);
    assert_eq!(intent.svn_receipt.as_ref().unwrap().revision, 5);
    assert!(intent.git_receipt_sha.is_none());
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    let requests = std::fs::read_to_string(&f.log)
        .unwrap()
        .lines()
        .filter(|line| line.contains("/git-receive-pack"))
        .count();
    assert!(requests > 0);
    let before = journal_state(&f);
    std::fs::remove_file(hook).unwrap();
    assert!(
        super::super::execution::reconcile_owned_refresh_intent_with_client(
            &engine,
            &actor,
            &prepared,
            &busy,
            f.client.clone(),
        )
        .await
        .is_err()
    );
    assert!(super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "protected-child",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .is_err());
    assert_eq!(journal_state(&f), before);
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        requests
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(parent_row(&f), parent_before);
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
}

fn noop_projection(f: &Fixture) -> Vec<Vec<rusqlite::types::Value>> {
    let conn = f.db.conn();
    let mut result = Vec::new();
    for query in [
        "SELECT * FROM repositories ORDER BY id",
        "SELECT * FROM commit_map ORDER BY id",
        "SELECT * FROM sync_records ORDER BY id",
    ] {
        let mut statement = conn.prepare(query).unwrap();
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|index| row.get(index))
                    .collect::<Result<Vec<rusqlite::types::Value>, _>>()
            })
            .unwrap();
        result.extend(rows.collect::<Result<Vec<_>, _>>().unwrap());
    }
    result
}

fn initial_noop_fixture() -> Fixture {
    // New branches and a separate fresh SVN repository do not rewrite any
    // existing fixture ref/history. These are source-observation fixtures;
    // ordinary-engine initialization qualification remains a separate control.
    let mut f = Fixture::new();
    let repository = f.temp.path().join("initial-noop-svn");
    run(Command::new("svnadmin").arg("create").arg(&repository));
    let root = reqwest::Url::from_file_path(repository.canonicalize().unwrap())
        .unwrap()
        .to_string();
    svn(
        &[
            "mkdir",
            &format!("{root}/trunk"),
            &format!("{root}/branches"),
            "-m",
            "no-op layout",
        ],
        None,
    );
    let working = f.temp.path().join("initial-noop-parent-wc");
    svn(
        &[
            "checkout",
            &format!("{root}/trunk"),
            working.to_str().unwrap(),
        ],
        None,
    );
    std::fs::write(working.join("file.txt"), b"baseline\n").unwrap();
    svn(&["add", "file.txt"], Some(&working));
    svn(&["commit", "-m", "no-op baseline"], Some(&working));
    svn(
        &[
            "copy",
            &format!("{root}/trunk@2"),
            &format!("{root}/branches/f"),
            "-m",
            "no-op direct copy",
        ],
        None,
    );
    f.target
        .reference(
            "refs/heads/unchanged-parent",
            f.baseline,
            false,
            "fresh no-op fixture branch",
        )
        .unwrap();
    f.target
        .reference(
            "refs/heads/unchanged-child",
            f.baseline,
            false,
            "fresh no-op fixture branch",
        )
        .unwrap();
    for (id, branch, revision) in [
        ("parent", "unchanged-parent", 2),
        ("child", "unchanged-child", 3),
    ] {
        f.db.conn().execute("UPDATE repositories SET svn_url=?1,git_branch=?2,last_svn_rev=?3,last_git_sha=?4 WHERE id=?5",rusqlite::params![root,branch,revision,f.baseline.to_string(),id]).unwrap();
        f.db.set_state(&format!("last_svn_rev_{id}"), &revision.to_string())
            .unwrap();
        let scope: String =
            f.db.conn()
                .query_row(
                    "SELECT scope_uuid FROM repositories WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
        f.db.set_state(&format!("last_git_sha_{scope}"), &f.baseline.to_string())
            .unwrap();
    }
    f.root = root;
    f.parent = f.baseline;
    f._parent_wc = working;
    f
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_parent_after_completion_returns_a_distinct_signed_noop_without_publication() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, first) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let approved = actor.prepared_approval_digest(&engine, &first).unwrap();
    let completed = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &first,
        "before-noop",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    let before = noop_projection(&f);
    let original_kv = journal_state(&f);
    let before_writes = std::fs::read_to_string(&f.log)
        .unwrap()
        .lines()
        .filter(|line| line.contains("/git-receive-pack"))
        .count();
    let workspace = actor.create_workspace(&engine).unwrap();
    let path = workspace.pin().operation_directory.path.clone();
    let noop = super::super::noop::prepare_registered_noop_with_client(
        &f.db,
        "child",
        workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(noop.pins().baseline_operation_id, Some(completed.id));
    assert_eq!(noop.pins().parent_baseline_sha, f.parent.to_string());
    let approved = actor.noop_approval_digest(&engine, &noop).unwrap();
    let receipt = super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "unchanged-parent",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.version, 1);
    assert_eq!(receipt.id, noop.pins().operation_id);
    assert_ne!(receipt.id, completed.id);
    assert_eq!(receipt.pins.child.last_svn_rev, 5);
    assert_eq!(receipt.pins.parent.last_svn_rev, 4);
    assert_eq!(noop_projection(&f), before);
    assert_eq!(
        journal_state(&f)
            .into_iter()
            .filter(|(key, _, _)| !key.starts_with("pair_refresh_noop_"))
            .collect::<Vec<_>>(),
        original_kv
    );
    assert_eq!(
        f.db.pair_refresh_operation(completed.id).unwrap(),
        completed
    );
    assert!(f.db.pair_refresh_operation(receipt.id).is_err());
    assert_eq!(
        actor
            .noop_request_receipt(&engine, "unchanged-parent", &receipt.pins.child.scope_uuid)
            .unwrap(),
        Some(receipt.clone())
    );
    let original = journal_state(&f);
    let repeated = super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "unchanged-parent",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(repeated, receipt);
    assert_eq!(journal_state(&f), original);
    assert_eq!(noop_projection(&f), before);
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        before_writes
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
    drop(noop);
    drop(workspace);
    assert!(!path.exists());
    assert_eq!(
        actor
            .noop_request_receipt(&engine, "unchanged-parent", &receipt.pins.child.scope_uuid)
            .unwrap(),
        Some(receipt)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_copy_noop_is_atomic_authenticated_and_cannot_become_a_publication_job() {
    let _serial = SERIAL.lock().await;
    let f = initial_noop_fixture();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let actor = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let workspace = actor.create_workspace(&engine).unwrap();
    let noop = super::super::noop::prepare_registered_noop_with_client(
        &f.db,
        "child",
        workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_eq!(noop.pins().baseline_operation_id, None);
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let before = noop_projection(&f);
    let original = journal_state(&f);
    assert!(!key_table(&f));
    assert!(super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "first-noop",
        &"0".repeat(64),
        &busy,
        f.client.clone()
    )
    .await
    .is_err());
    assert!(!key_table(&f));
    assert_eq!(journal_state(&f), original);
    let approved = actor.noop_approval_digest(&engine, &noop).unwrap();
    f.db.conn().execute_batch("CREATE TRIGGER interrupt_noop BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_noop_v1:request:%' BEGIN SELECT RAISE(ABORT,'disposable no-op interruption'); END;").unwrap();
    assert!(super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "first-noop",
        &approved,
        &busy,
        f.client.clone()
    )
    .await
    .is_err());
    assert!(!key_table(&f));
    assert_eq!(journal_state(&f), original);
    f.db.conn()
        .execute_batch("DROP TRIGGER interrupt_noop;")
        .unwrap();
    let receipt = super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "first-noop",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert!(key_table(&f));
    assert_eq!(receipt.pins.child.last_git_sha, f.baseline.to_string());
    assert_eq!(receipt.pins.parent.last_git_sha, f.baseline.to_string());
    assert_eq!(receipt.pins.child.last_svn_rev, 3);
    assert_eq!(receipt.pins.parent.last_svn_rev, 2);
    assert_eq!(noop_projection(&f), before);
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        3
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
    let key = format!("pair_refresh_noop_v1:document:{}", receipt.id);
    let raw = f.db.get_state(&key).unwrap().unwrap();
    let mut altered: serde_json::Value = serde_json::from_str(&raw).unwrap();
    altered["receipt"]["pins"]["git_tree"] = serde_json::Value::String("a".repeat(40));
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key=?2",
            rusqlite::params![serde_json::to_string(&altered).unwrap(), key],
        )
        .unwrap();
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    assert!(actor
        .noop_request_receipt(&engine, "first-noop", &receipt.pins.child.scope_uuid)
        .is_err());
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key=?2",
            rusqlite::params![raw, key],
        )
        .unwrap();
    f.db.conn()
        .execute(
            "DELETE FROM kv_state WHERE key LIKE 'pair_refresh_noop_v1:request:%'",
            [],
        )
        .unwrap();
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    assert_eq!(noop_projection(&f), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn noop_refuses_git_and_svn_drift_without_establishing_receipt_authority() {
    let _serial = SERIAL.lock().await;
    for git_drift in [false, true] {
        let f = initial_noop_fixture();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let actor = engine
            .capture_pair_refresh_actor_policy("disposable-workspace-session")
            .unwrap();
        let workspace = actor.create_workspace(&engine).unwrap();
        let noop = super::super::noop::prepare_registered_noop_with_client(
            &f.db,
            "child",
            workspace,
            f.client.clone(),
        )
        .await
        .unwrap();
        let approved = actor.noop_approval_digest(&engine, &noop).unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        if git_drift {
            let advanced = commit(&f.target, b"external Git parent advance\n", &[f.baseline]);
            f.target
                .find_reference("refs/heads/unchanged-parent")
                .unwrap()
                .set_target(advanced, "external fixture fast-forward")
                .unwrap();
            assert!(f.target.graph_descendant_of(advanced, f.baseline).unwrap());
        } else {
            std::fs::write(
                f._parent_wc.join("file.txt"),
                b"external SVN parent advance\n",
            )
            .unwrap();
            svn(
                &["commit", "-m", "external fixture parent advance"],
                Some(&f._parent_wc),
            );
        }
        let before = noop_projection(&f);
        let original = journal_state(&f);
        assert!(super::super::noop::complete_owned_noop_with_client(
            &engine,
            &actor,
            &noop,
            "drifted-noop",
            &approved,
            &busy,
            f.client.clone()
        )
        .await
        .is_err());
        assert!(!key_table(&f));
        assert_eq!(journal_state(&f), original);
        assert_eq!(noop_projection(&f), before);
        assert!(!std::fs::read_to_string(&f.log)
            .unwrap()
            .contains("receive-pack"));
        assert_eq!(
            crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
                .info()
                .await
                .unwrap()
                .latest_rev,
            if git_drift { 3 } else { 4 }
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn noop_refuses_revocation_session_loss_competing_window_unknown_history_and_foreign_busy() {
    let _serial = SERIAL.lock().await;
    for case in ["credential", "session", "window", "unknown-history", "busy"] {
        let f = initial_noop_fixture();
        let engine = f.engine();
        let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
        let actor = engine
            .capture_pair_refresh_actor_policy("disposable-workspace-session")
            .unwrap();
        let workspace = actor.create_workspace(&engine).unwrap();
        let noop = super::super::noop::prepare_registered_noop_with_client(
            &f.db,
            "child",
            workspace,
            f.client.clone(),
        )
        .await
        .unwrap();
        let approved = actor.noop_approval_digest(&engine, &noop).unwrap();
        let mut window = None;
        match case {
            "credential" => {
                f.db.set_state("secret_git_token_child", "").unwrap();
            }
            "session" => {
                f.db.delete_session("disposable-workspace-session").unwrap();
            }
            "window" => {
                window = Some(
                    f.db.begin_ordinary_writer_window(Some("unregistered-alias"))
                        .unwrap(),
                );
            }
            "unknown-history" => {
                f.db.set_state("pair_refresh_noop_v99:document:unknown", "corrupt")
                    .unwrap();
            }
            "busy" => {}
            _ => unreachable!(),
        }
        let busy = if case == "busy" {
            crate::busy::try_acquire_pair("foreign-one", "foreign-two").unwrap()
        } else {
            crate::busy::try_acquire_pair("child", "parent").unwrap()
        };
        let before = noop_projection(&f);
        let original = journal_state(&f);
        assert!(
            super::super::noop::complete_owned_noop_with_client(
                &engine,
                &actor,
                &noop,
                "refused-noop",
                &approved,
                &busy,
                f.client.clone()
            )
            .await
            .is_err(),
            "{case}"
        );
        assert!(!key_table(&f), "{case}");
        assert_eq!(journal_state(&f), original, "{case}");
        assert_eq!(noop_projection(&f), before, "{case}");
        assert!(
            !std::fs::read_to_string(&f.log)
                .unwrap()
                .contains("receive-pack"),
            "{case}"
        );
        assert_eq!(
            crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
                .info()
                .await
                .unwrap()
                .latest_rev,
            3,
            "{case}"
        );
        if let Some(window) = window {
            window.finish_preparation().unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_noop_history_does_not_replace_the_copy_baseline_for_a_later_real_integration() {
    let _serial = SERIAL.lock().await;
    let f = initial_noop_fixture();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let actor = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let workspace = actor.create_workspace(&engine).unwrap();
    let noop = super::super::noop::prepare_registered_noop_with_client(
        &f.db,
        "child",
        workspace.clone(),
        f.client.clone(),
    )
    .await
    .unwrap();
    let approved = actor.noop_approval_digest(&engine, &noop).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let receipt = super::super::noop::complete_owned_noop_with_client(
        &engine,
        &actor,
        &noop,
        "noop-before-advance",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    let original_noop =
        f.db.get_state(&format!("pair_refresh_noop_v1:document:{}", receipt.id))
            .unwrap();
    drop(noop);
    drop(workspace);
    // Parent handling is modeled with production checkpoint helpers in this
    // source fixture. Full ordinary-engine qualification remains separate.
    std::fs::write(f._parent_wc.join("file.txt"), b"advance after no-op\n").unwrap();
    svn(
        &["commit", "-m", "fixture parent advance after no-op"],
        Some(&f._parent_wc),
    );
    let advanced = commit(&f.target, b"advance after no-op\n", &[f.baseline]);
    f.target
        .find_reference("refs/heads/unchanged-parent")
        .unwrap()
        .set_target(advanced, "fixture parent fast-forward")
        .unwrap();
    f.db.insert_commit_map_with_repo(
        4,
        &advanced.to_string(),
        "svn_to_git",
        "fixture",
        "fixture",
        Some("parent"),
    )
    .unwrap();
    f.db.update_repo_watermark("parent", 4, &advanced.to_string())
        .unwrap();
    crate::echo_receipt_scope::write_scoped_last_git_sha_kv(
        &f.db.conn(),
        "parent",
        &advanced.to_string(),
        &chrono::Utc::now().to_rfc3339(),
    )
    .unwrap();
    let parent_before = parent_row(&f);
    let workspace = actor.create_workspace(&engine).unwrap();
    let prepared =
        prepare_registered_in_workspace_for_test(&f.db, "child", workspace, f.client.clone())
            .await
            .unwrap();
    assert_eq!(
        prepared.pins().git.pins.parent_baseline_tip,
        f.baseline.to_string()
    );
    assert_eq!(
        prepared
            .pins()
            .svn
            .pins
            .lineage
            .child_copy
            .copy_from_revision,
        2
    );
    assert_eq!(
        prepared.pins().svn.pins.lineage.child_copy.created_revision,
        3
    );
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let completed = super::super::execution::complete_owned_refresh_with_client(
        &engine,
        &actor,
        &prepared,
        "advance-after-noop",
        &approved,
        &busy,
        f.client.clone(),
    )
    .await
    .unwrap();
    assert_ne!(completed.id, receipt.id);
    assert_eq!(
        completed.phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::Completed
    );
    assert_eq!(completed.svn_receipt.as_ref().unwrap().revision, 5);
    assert_eq!(parent_row(&f), parent_before);
    let tip = f
        .target
        .find_reference("refs/heads/unchanged-child")
        .unwrap()
        .target()
        .unwrap();
    assert_eq!(
        f.target
            .find_commit(tip)
            .unwrap()
            .parent_ids()
            .collect::<Vec<_>>(),
        vec![f.baseline, advanced]
    );
    assert!(f.target.graph_descendant_of(tip, f.baseline).unwrap());
    assert!(f.target.graph_descendant_of(tip, advanced).unwrap());
    assert_eq!(
        actor
            .noop_request_receipt(
                &engine,
                "noop-before-advance",
                &receipt.pins.child.scope_uuid
            )
            .unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        f.db.get_state(&format!("pair_refresh_noop_v1:document:{}", receipt.id))
            .unwrap(),
        original_noop
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_epoch_and_unknown_or_live_native_owner_cannot_adopt_signed_resources() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let operation = actor
        .enroll_prepared(&engine, &prepared, "native-owner-refusal", &approved, &busy)
        .unwrap();
    let old_epoch = fence.epoch();
    let prior_lease = f.db.get_state("writer_fence_v1:lease").unwrap().unwrap();
    drop(fence);
    let before_refused_claim = f.state();
    assert!(crate::writer_fence::claim(engine.db(), f.temp.path()).is_err());
    assert_eq!(f.state(), before_refused_claim);

    // Keep exercising the later recovery gate against a stale ordinary epoch,
    // even though ordinary startup now refuses to create that state. Acquire a
    // new epoch in a separate disposable database and copy only its lease rows
    // into this fixture. The signed journal and private key remain untouched.
    let held_journal: Vec<_> = before_refused_claim
        .iter()
        .filter(|(key, _, _)| key.starts_with("pair_refresh_execution_v3:"))
        .cloned()
        .collect();
    assert!(!held_journal.is_empty());
    let foreign_epoch_dir = tempfile::tempdir().unwrap();
    let foreign_epoch_db = Database::new(foreign_epoch_dir.path().join("reposync.db")).unwrap();
    foreign_epoch_db.initialize().unwrap();
    let mut stale_lease: serde_json::Value = serde_json::from_str(&prior_lease).unwrap();
    stale_lease["holder_pid"] = serde_json::json!(0);
    foreign_epoch_db
        .set_state(
            "writer_fence_v1:lease",
            &serde_json::to_string(&stale_lease).unwrap(),
        )
        .unwrap();
    let next = crate::writer_fence::claim(&foreign_epoch_db, foreign_epoch_dir.path()).unwrap();
    let foreign_rows: Vec<(String, String, String)> = foreign_epoch_db
        .conn()
        .prepare("SELECT key,value,updated_at FROM kv_state WHERE key IN ('writer_fence_v1:lease','writer_fence_v1:native') ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    f.db.immediate_transaction(|tx| {
        for (key, value, updated_at) in &foreign_rows {
            tx.execute(
                "INSERT INTO kv_state(key,value,updated_at) VALUES(?1,?2,?3)",
                rusqlite::params![key, value, updated_at],
            )?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(
        f.state()
            .into_iter()
            .filter(|(key, _, _)| key.starts_with("pair_refresh_execution_v3:"))
            .collect::<Vec<_>>(),
        held_journal
    );
    assert!(next.epoch() > old_epoch);
    let state = journal_state(&f);
    assert!(super::super::recovery::prepare_owned_refresh_recovery(
        &engine,
        &actor,
        operation.id,
        &busy
    )
    .is_err());
    assert_eq!(journal_state(&f), state);
    assert_eq!(
        f.db.pair_refresh_operation(operation.id).unwrap(),
        operation
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    let working_copy = prepared.pins().svn.working_copy.clone();
    drop(prepared);
    drop(workspace);
    assert!(working_copy.join(".svn").is_dir());
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "current_thread")]
async fn native_recovery_fixture_worker() {
    let Ok(root) = std::env::var("REPOSYNC_NATIVE_RECOVERY_FIXTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    assert_eq!(root.canonicalize().unwrap(), root);
    let stage = std::env::var("REPOSYNC_NATIVE_RECOVERY_FIXTURE_STAGE").unwrap();
    assert!(matches!(
        stage.as_str(),
        "prepared" | "svn_intent" | "git_intent"
    ));
    let db = Database::new(root.join("reposync.db")).unwrap();
    let mut config: crate::config::AppConfig = toml::from_str(
        r#"
        [daemon]
        data_dir = "/unused"
        [svn]
        url = "file:///unused/svn"
        username = ""
        password_env = ""
        [github]
        repo = "fixture/repo"
        token_env = ""
    "#,
    )
    .unwrap();
    config.daemon.data_dir = root.clone();
    let engine = crate::sync_engine::SyncEngine::new(
        config,
        db,
        crate::svn::SvnClient::new("file:///unused/svn", "", ""),
        crate::git::client::GitClient::new(root.join("engine-git")).unwrap(),
        std::sync::Arc::new(
            crate::identity::mapper::IdentityMapper::new(&crate::config::IdentityConfig::default())
                .unwrap(),
        ),
    );
    let _lock = crate::data_dir::acquire(&root).unwrap();
    let _fence = crate::writer_fence::claim_with_identity(
        engine.db(),
        &root,
        "native-recovery-fixture",
        std::process::id(),
    )
    .unwrap();
    let actor = engine
        .capture_pair_refresh_actor_policy("disposable-workspace-session")
        .unwrap();
    let workspace = actor.create_workspace(&engine).unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .add_root_certificate(
            reqwest::Certificate::from_der(&std::fs::read(root.join("cert.der")).unwrap()).unwrap(),
        )
        .build()
        .unwrap();
    let prepared =
        prepare_registered_in_workspace_for_test(engine.db(), "child", workspace, client.clone())
            .await
            .unwrap();
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    match stage.as_str() {
        "prepared" => {
            actor
                .enroll_prepared(&engine, &prepared, "native-worker", &approved, &busy)
                .unwrap();
        }
        "svn_intent" => {
            engine.db().conn().execute_batch("CREATE TRIGGER native_interrupt BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_execution_v3:document:%' AND json_extract(NEW.value,'$.phase')='svn_observed' BEGIN SELECT RAISE(ABORT,'native fixture lost SVN receipt'); END;").unwrap();
            let error = super::super::execution::publish_owned_svn_stage_with_client(
                &engine,
                &actor,
                &prepared,
                "native-worker",
                &approved,
                &busy,
                client.clone(),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains("native fixture lost SVN receipt"), "{error}");
            engine
                .db()
                .conn()
                .execute_batch("DROP TRIGGER native_interrupt;")
                .unwrap();
        }
        "git_intent" => {
            engine.db().conn().execute_batch("CREATE TRIGGER native_interrupt BEFORE INSERT ON kv_state WHEN NEW.key LIKE 'pair_refresh_execution_v3:document:%' AND json_extract(NEW.value,'$.phase')='git_observed' BEGIN SELECT RAISE(ABORT,'native fixture lost Git receipt'); END;").unwrap();
            let error = super::super::execution::complete_owned_refresh_with_client(
                &engine,
                &actor,
                &prepared,
                "native-worker",
                &approved,
                &busy,
                client.clone(),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains("native fixture lost Git receipt"), "{error}");
            engine
                .db()
                .conn()
                .execute_batch("DROP TRIGGER native_interrupt;")
                .unwrap();
        }
        _ => unreachable!(),
    }
    println!(
        "REPOSYNC_NATIVE_RECOVERY_READY {}",
        serde_json::json!({"id":prepared.pins().git.operation_id,"pid":std::process::id(),"stage":stage})
    );
    std::io::Write::flush(&mut std::io::stdout()).unwrap();
    // Deliberately skip Rust guard destruction. The parent verifies the durable
    // lease and signed resources survive, then proves this actual PID absent.
    std::process::exit(0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_worker_exit_recovers_prepared_and_both_uncertain_publication_intents() {
    use crate::db::pair_refresh_operations::PairRefreshPhase;
    let _serial = SERIAL.lock().await;
    for stage in ["prepared", "svn_intent", "git_intent"] {
        let f = Fixture::new();
        let engine = f.engine();
        let parent_before = parent_row(&f);
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact","pair_refresh::live_preparation::authenticated_enrollment_tests::native_recovery_fixture_worker","--nocapture","--test-threads=1"])
            .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_ROOT",f.temp.path().canonicalize().unwrap())
            .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_STAGE",stage);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let marker = stdout
            .lines()
            .find_map(|line| {
                line.split_once("REPOSYNC_NATIVE_RECOVERY_READY ")
                    .map(|(_, value)| value)
            })
            .expect("worker did not reach its durable phase");
        let ready: serde_json::Value = serde_json::from_str(marker).unwrap();
        let id = Uuid::parse_str(ready["id"].as_str().unwrap()).unwrap();
        let operation = f.db.pair_refresh_operation(id).unwrap();
        assert_eq!(
            operation.phase,
            match stage {
                "prepared" => PairRefreshPhase::Prepared,
                "svn_intent" => PairRefreshPhase::SvnIntent,
                "git_intent" => PairRefreshPhase::GitIntent,
                _ => unreachable!(),
            }
        );
        let authority_key = format!("pair_refresh_execution_v3:authority:{id}");
        let raw = f.db.get_state(&authority_key).unwrap().unwrap();
        let original: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let native = original["evidence"]["resources"]["workspace"].get("native_owner");
        #[cfg(target_os = "linux")]
        assert!(
            native.is_some(),
            "Linux worker must record native owner identity"
        );
        if let Some(native) = native {
            assert_eq!(native["pid"], ready["pid"]);
        }
        let lock = crate::data_dir::acquire(f.temp.path()).unwrap();
        let actor = engine
            .capture_pair_refresh_actor_policy("disposable-workspace-session")
            .unwrap();
        let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
        let before = journal_state(&f);
        let claimed = super::super::recovery::claim_owned_refresh_recovery_writer(
            &engine, &actor, id, &busy, &lock,
        );
        if native.is_none() {
            assert!(claimed.is_err());
            assert_eq!(journal_state(&f), before);
            assert_eq!(f.db.pair_refresh_operation(id).unwrap(), operation);
            assert!(
                crate::db::pair_refresh_operations::require_no_pair_refresh_hold(
                    &f.db.conn(),
                    None
                )
                .is_err()
            );
            assert_eq!(parent_row(&f), parent_before);
            continue;
        }
        let fence = claimed.unwrap();
        assert!(fence.epoch() > operation.admitted_writer_epoch);
        let recovery =
            super::super::recovery::prepare_owned_refresh_recovery(&engine, &actor, id, &busy)
                .unwrap();
        let approved = recovery.approval_digest(&engine, &actor).unwrap();
        let recovered = recovery
            .transfer(&engine, &actor, &approved, &busy)
            .unwrap();
        assert_eq!(recovered.operation(), &operation);
        let transferred: serde_json::Value =
            serde_json::from_str(&f.db.get_state(&authority_key).unwrap().unwrap()).unwrap();
        assert_eq!(transferred["evidence"], original["evidence"]);
        assert_eq!(transferred["recoveries"].as_array().unwrap().len(), 1);
        assert_eq!(
            transferred["recoveries"][0]["writer_epoch"].as_u64(),
            Some(fence.epoch())
        );
        if stage != "prepared" {
            let observed = super::super::execution::reconcile_owned_refresh_intent_with_client(
                &engine,
                &actor,
                recovered.prepared_for_test(),
                &busy,
                f.client.clone(),
            )
            .await
            .unwrap();
            assert_eq!(
                observed.phase,
                if stage == "svn_intent" {
                    PairRefreshPhase::SvnObserved
                } else {
                    PairRefreshPhase::GitObserved
                }
            );
        }
        let completed = super::super::execution::complete_owned_refresh_with_client(
            &engine,
            &actor,
            recovered.prepared_for_test(),
            &operation.request_id,
            &operation.approval_digest,
            &busy,
            f.client.clone(),
        )
        .await
        .unwrap();
        assert_eq!(completed.phase, PairRefreshPhase::Completed);
        assert_eq!(completed.id, operation.id);
        assert_eq!(
            completed.admitted_writer_epoch,
            operation.admitted_writer_epoch
        );
        assert_eq!(completed.svn_receipt.as_ref().unwrap().revision, 5);
        assert_eq!(parent_row(&f), parent_before);
        assert_eq!(
            crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
                .info()
                .await
                .unwrap()
                .latest_rev,
            5
        );
        assert_eq!(
            f.target
                .find_reference("refs/heads/feature")
                .unwrap()
                .target()
                .unwrap()
                .to_string(),
            operation.pins.git.prepared_sha
        );
        assert_eq!(
            std::fs::read_to_string(&f.log)
                .unwrap()
                .lines()
                .filter(|line| line.contains("/git-receive-pack"))
                .count(),
            1
        );
        assert!(
            crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
                .is_ok()
        );
        let final_authority: serde_json::Value =
            serde_json::from_str(&f.db.get_state(&authority_key).unwrap().unwrap()).unwrap();
        assert_eq!(final_authority["evidence"], original["evidence"]);
        assert_eq!(final_authority["recoveries"], transferred["recoveries"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_recovery_claim_refuses_live_or_unknown_owner_and_foreign_lock() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, workspace, prepared) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let operation = actor
        .enroll_prepared(&engine, &prepared, "native-claim-refusal", &approved, &busy)
        .unwrap();
    let before = journal_state(&f);
    let lease = f.db.get_state("writer_fence_v1:lease").unwrap();
    assert!(super::super::recovery::claim_owned_refresh_recovery_writer(
        &engine,
        &actor,
        operation.id,
        &busy,
        &lock,
    )
    .is_err());
    assert_eq!(journal_state(&f), before);
    assert_eq!(f.db.get_state("writer_fence_v1:lease").unwrap(), lease);
    let foreign = tempfile::tempdir().unwrap();
    let foreign_lock = crate::data_dir::acquire(foreign.path()).unwrap();
    assert!(super::super::recovery::claim_owned_refresh_recovery_writer(
        &engine,
        &actor,
        operation.id,
        &busy,
        &foreign_lock,
    )
    .is_err());
    assert_eq!(journal_state(&f), before);
    assert_eq!(f.db.get_state("writer_fence_v1:lease").unwrap(), lease);
    assert_eq!(
        f.db.pair_refresh_operation(operation.id).unwrap(),
        operation
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None,)
            .is_err()
    );
    assert!(workspace.pin().operation_directory.path.is_dir());
    assert!(prepared.pins().svn.working_copy.is_dir());
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
}

#[cfg(target_os = "linux")]
#[path = "native_claim_crash_tests.rs"]
mod native_claim_crash_tests;

#[path = "real_engine_refresh_tests.rs"]
mod real_engine_refresh_tests;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_request_lookup_is_read_only_scoped_and_rejects_corruption_and_logout() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let scope = prepared.pins().child.scope_uuid.clone();
    let before = journal_state(&f);
    assert!(actor
        .refresh_request_receipt(&engine, "lookup-request", &scope)
        .unwrap()
        .is_none());
    assert_eq!(journal_state(&f), before);
    assert!(!key_table(&f));
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let admitted = actor
        .enroll_prepared(&engine, &prepared, "lookup-request", &digest, &busy)
        .unwrap();
    let saved = journal_state(&f);
    assert_eq!(
        actor
            .refresh_request_receipt(&engine, "lookup-request", &scope)
            .unwrap(),
        Some(admitted.clone())
    );
    assert!(actor
        .refresh_request_receipt(&engine, "other-request", &scope)
        .unwrap()
        .is_none());
    assert!(actor
        .refresh_request_receipt(&engine, "lookup-request", &Uuid::new_v4().to_string())
        .unwrap()
        .is_none());
    assert_eq!(journal_state(&f), saved);
    let cancelled = actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .unwrap();
    let saved = journal_state(&f);
    assert_eq!(
        actor
            .refresh_request_receipt(&engine, "lookup-request", &scope)
            .unwrap(),
        Some(cancelled)
    );
    assert_eq!(journal_state(&f), saved);
    let authority = format!("pair_refresh_execution_v3:authority:{}", admitted.id);
    let original = f.db.get_state(&authority).unwrap().unwrap();
    f.db.conn()
        .execute("UPDATE kv_state SET value='{}' WHERE key=?1", [&authority])
        .unwrap();
    let corrupt = journal_state(&f);
    assert!(actor
        .refresh_request_receipt(&engine, "lookup-request", &scope)
        .is_err());
    assert_eq!(journal_state(&f), corrupt);
    f.db.conn()
        .execute(
            "UPDATE kv_state SET value=?1 WHERE key=?2",
            rusqlite::params![original, authority],
        )
        .unwrap();
    f.db.delete_session("disposable-workspace-session").unwrap();
    let logged_out = journal_state(&f);
    assert!(actor
        .refresh_request_receipt(&engine, "lookup-request", &scope)
        .is_err());
    assert_eq!(journal_state(&f), logged_out);
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_retains_preparation_and_returns_exact_publication_and_noop_receipts() {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let service = RefreshService::with_fixture_client(engine.clone(), f.client.clone());
    let token = "disposable-workspace-session";
    let approval = match service
        .prepare(token.into(), "child".into(), "service-request".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(approval) => approval,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(approval.kind, "integration");
    assert_ne!(approval.preparation_id, approval.operation_id);
    assert!(f.db.pair_refresh_operation(approval.operation_id).is_err());
    let before = journal_state(&f);
    assert!(service
        .act(
            token.into(),
            "child",
            "service-request",
            approval.preparation_id,
            "wrong".into(),
            RefreshAction::Execute
        )
        .await
        .is_err());
    assert_eq!(journal_state(&f), before);
    let receipt = service
        .act(
            token.into(),
            "child",
            "service-request",
            approval.preparation_id,
            approval.approval_digest.clone(),
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(receipt.operation_id, approval.operation_id);
    assert_eq!(receipt.phase, "completed");
    assert_eq!(receipt.svn_revision, Some(5));
    assert_eq!(
        f.db.get_repo_watermark("child").unwrap(),
        (5, receipt.git_sha.clone().unwrap())
    );
    assert_eq!(
        f.db.get_repo_watermark("parent").unwrap(),
        (4, f.parent.to_string())
    );
    let publications = || {
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count()
    };
    assert_eq!(publications(), 1);
    let retry = service
        .act(
            token.into(),
            "child",
            "service-request",
            approval.preparation_id,
            approval.approval_digest,
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(retry.operation_id, receipt.operation_id);
    assert_eq!(retry.git_sha, receipt.git_sha);
    assert_eq!(publications(), 1);
    match service
        .prepare(token.into(), "child".into(), "service-request".into())
        .await
        .unwrap()
    {
        RefreshResponse::Receipt(found) => assert_eq!(found.operation_id, receipt.operation_id),
        other => panic!("unexpected {other:?}"),
    }
    let noop = match service
        .prepare(token.into(), "child".into(), "service-noop".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(approval) => approval,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(noop.kind, "no_op");
    let before = f.db.get_repo_watermark("child").unwrap();
    let noop_receipt = service
        .act(
            token.into(),
            "child",
            "service-noop",
            noop.preparation_id,
            noop.approval_digest,
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(noop_receipt.phase, "no_op");
    assert_eq!(noop_receipt.operation_id, noop.operation_id);
    assert_ne!(noop_receipt.operation_id, receipt.operation_id);
    assert_eq!(f.db.get_repo_watermark("child").unwrap(), before);
    assert_eq!(publications(), 1);
    assert_eq!(
        crate::svn::SvnClient::new(&f.root, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_finishes_a_disconnected_request_without_republication() {
    #[cfg(unix)]
    let _process_boundary = packaged_call_site::enter_process_boundary()
        .expect("serialization owner is free");
    #[cfg(unix)]
    match packaged_call_site::attest_catalogue() {
        Ok(packaged_call_site::CatalogueAttestation::PackagedImmutable { manifest }) => {
            if let Err(error) = packaged_f09_disconnect(manifest, &_process_boundary).await {
                panic!("packaged call site failed closed: {error}");
            }
            return;
        }
        Ok(packaged_call_site::CatalogueAttestation::HistoricalNativeNotCatalogueProof) => {}
        Err(error) => panic!("missing H failed closed: {error}"),
    }
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    use std::os::unix::fs::PermissionsExt;
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let service = RefreshService::with_fixture_client(engine, f.client.clone());
    let token = "disposable-workspace-session";
    let approval = match service
        .prepare(token.into(), "child".into(), "disconnected-request".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(approval) => approval,
        other => panic!("unexpected {other:?}"),
    };
    let hook = f.temp.path().join("svn-repository/hooks/pre-commit");
    std::fs::write(
        &hook,
        r#"#!/usr/bin/env python3
from pathlib import Path
import time
root=Path(__file__).resolve().parents[2]
(root/'http-disconnect-ready').write_text('ready')
for _ in range(600):
    if (root/'http-disconnect-release').exists():
        raise SystemExit(0)
    time.sleep(0.05)
raise SystemExit(1)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _svn_diag = SvnDiag::arm(f.temp.path());
    let handle = approval.preparation_id;
    let digest = approval.approval_digest.clone();
    let diagnostic_digest = digest.clone();
    let request_service = service.clone();
    let mut request = tokio::spawn(async move {
        request_service
            .act(
                token.into(),
                "child",
                "disconnected-request",
                handle,
                digest,
                RefreshAction::Execute,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        while !f.temp.path().join("http-disconnect-ready").is_file() {
            assert!(
                !request.is_finished(),
                "request ended before entering actual SVN publication: {}",
                {
                    let detail = match (&mut request).await {
                        Ok(Err(error)) => format!("{error:#}"),
                        Ok(Ok(_)) => "success-before-barrier".to_owned(),
                        Err(error) => format!("request join failed: {error}"),
                    };
                    let mut detail = crate::errors::redact_vcs_error_detail(&detail);
                    for secret in [
                        token,
                        "disposable-preparation-token",
                        diagnostic_digest.as_str(),
                    ] {
                        detail = detail.replace(secret, "[REDACTED_FIXTURE_SECRET]");
                    }
                    detail.push(' ');
                    detail.push_str(&case_diag(
                        f.temp.path(),
                        &hook,
                        &f.temp.path().join("http-disconnect-ready"),
                    ));
                    detail
                }
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.db.pair_refresh_operation(approval.operation_id)
            .unwrap()
            .phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::SvnIntent
    );
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    std::fs::write(f.temp.path().join("http-disconnect-release"), b"release").unwrap();
    let receipt = tokio::time::timeout(std::time::Duration::from_secs(45), async {
        loop {
            if let Some(receipt) = service
                .receipt(token, "child", "disconnected-request")
                .unwrap()
            {
                if receipt.phase == "completed" {
                    break receipt;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(receipt.operation_id, approval.operation_id);
    assert_eq!(receipt.svn_revision, Some(5));
    let retry = service
        .act(
            token.into(),
            "child",
            "disconnected-request",
            approval.preparation_id,
            approval.approval_digest,
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(retry.operation_id, receipt.operation_id);
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        1
    );
    assert_eq!(
        crate::svn::SvnClient::new(&f.root, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert_eq!(
        f.db.get_repo_watermark("child").unwrap(),
        (5, receipt.git_sha.unwrap())
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_recovery_refuses_live_owner_without_claim_or_transfer() {
    use crate::pair_refresh::service::RefreshService;
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let lock = std::sync::Arc::new(crate::data_dir::acquire(f.temp.path()).unwrap());
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, workspace, prepared) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "live-recovery-refusal", &digest, &busy)
        .unwrap();
    let saved = journal_state(&f);
    let lease = f.db.get_state("writer_fence_v1:lease").unwrap();
    let native = f.db.get_state("writer_fence_v1:native").unwrap();
    drop(busy);
    let ordinary = RefreshService::with_fixture_client(engine.clone(), f.client.clone());
    assert!(ordinary
        .prepare_recovery(
            "disposable-workspace-session".into(),
            "child".into(),
            "live-recovery-refusal".into()
        )
        .await
        .is_err());
    let recovery = RefreshService::recovery_only(engine.clone(), lock).unwrap();
    assert!(recovery
        .prepare_recovery(
            "disposable-workspace-session".into(),
            "child".into(),
            "live-recovery-refusal".into()
        )
        .await
        .is_err());
    assert!(recovery
        .prepare(
            "disposable-workspace-session".into(),
            "child".into(),
            "new-request".into()
        )
        .await
        .is_err());
    assert_eq!(journal_state(&f), saved);
    assert_eq!(f.db.get_state("writer_fence_v1:lease").unwrap(), lease);
    assert_eq!(f.db.get_state("writer_fence_v1:native").unwrap(), native);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert!(workspace.pin().operation_directory.path.is_dir());
    assert!(prepared.pins().svn.working_copy.is_dir());
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_native_restart_handles_prepared_and_uncertain_svn_git() {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    let _serial = SERIAL.lock().await;
    for stage in ["prepared", "svn_intent", "git_intent"] {
        let f = Fixture::new();
        let engine = std::sync::Arc::new(f.engine());
        let parent_before = parent_row(&f);
        let output = run(Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "pair_refresh::live_preparation::authenticated_enrollment_tests::native_recovery_fixture_worker", "--nocapture", "--test-threads=1"])
            .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_ROOT", f.temp.path().canonicalize().unwrap())
            .env("REPOSYNC_NATIVE_RECOVERY_FIXTURE_STAGE", stage));
        let ready: serde_json::Value = serde_json::from_str(
            output
                .lines()
                .find_map(|line| {
                    line.split_once("REPOSYNC_NATIVE_RECOVERY_READY ")
                        .map(|(_, value)| value)
                })
                .unwrap(),
        )
        .unwrap();
        let id = Uuid::parse_str(ready["id"].as_str().unwrap()).unwrap();
        let op = f.db.pair_refresh_operation(id).unwrap();
        let authority_key = format!("pair_refresh_execution_v3:authority:{id}");
        let authority: serde_json::Value =
            serde_json::from_str(&f.db.get_state(&authority_key).unwrap().unwrap()).unwrap();
        let native = authority["evidence"]["resources"]["workspace"].get("native_owner");
        #[cfg(target_os = "linux")]
        assert!(
            native.is_some(),
            "Linux restart qualification requires actual native identity"
        );
        let lock = std::sync::Arc::new(crate::data_dir::acquire(f.temp.path()).unwrap());
        let service =
            RefreshService::with_fixture_recovery_client(engine.clone(), lock, f.client.clone())
                .unwrap();
        let before = journal_state(&f);
        let token = "disposable-workspace-session";
        let result = service
            .prepare_recovery(token.into(), "child".into(), "native-worker".into())
            .await;
        if native.is_none() {
            assert!(result.is_err());
            assert_eq!(journal_state(&f), before);
            assert_eq!(f.db.pair_refresh_operation(id).unwrap(), op);
            assert!(
                crate::db::pair_refresh_operations::require_no_pair_refresh_hold(
                    &f.db.conn(),
                    None
                )
                .is_err()
            );
            continue;
        }
        let approval = match result.unwrap() {
            RefreshResponse::Approval(value) => value,
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(approval.kind, "recovery");
        assert_eq!(approval.operation_id, id);
        let before = journal_after_native_lease_claim(
            &f,
            &before,
            op.admitted_writer_epoch,
            std::process::id(),
        );
        assert_eq!(journal_state(&f), before);
        assert_eq!(f.db.pair_refresh_operation(id).unwrap(), op);
        assert!(service
            .act(
                token.into(),
                "child",
                "native-worker",
                approval.preparation_id,
                "wrong".into(),
                RefreshAction::Execute
            )
            .await
            .is_err());
        assert_eq!(journal_state(&f), before);
        if stage != "prepared" {
            assert!(service
                .act(
                    token.into(),
                    "child",
                    "native-worker",
                    approval.preparation_id,
                    approval.approval_digest.clone(),
                    RefreshAction::Execute
                )
                .await
                .is_err());
            assert_eq!(f.db.pair_refresh_operation(id).unwrap().phase, op.phase);
            let observed = service
                .act(
                    token.into(),
                    "child",
                    "native-worker",
                    approval.preparation_id,
                    approval.approval_digest.clone(),
                    RefreshAction::Reconcile,
                )
                .await
                .unwrap();
            assert_eq!(
                observed.phase,
                if stage == "svn_intent" {
                    "svn_observed"
                } else {
                    "git_observed"
                }
            );
        }
        let completed = service
            .act(
                token.into(),
                "child",
                "native-worker",
                approval.preparation_id,
                approval.approval_digest.clone(),
                RefreshAction::Execute,
            )
            .await
            .unwrap();
        assert_eq!(completed.phase, "completed");
        assert_eq!(completed.operation_id, id);
        assert_eq!(completed.svn_revision, Some(5));
        assert_eq!(
            completed.git_sha.as_deref(),
            Some(op.pins.git.prepared_sha.as_str())
        );
        let repeated = service
            .act(
                token.into(),
                "child",
                "native-worker",
                approval.preparation_id,
                approval.approval_digest,
                RefreshAction::Execute,
            )
            .await
            .unwrap();
        assert_eq!(repeated.operation_id, id);
        assert_eq!(repeated.phase, "completed");
        assert_eq!(parent_row(&f), parent_before);
        assert_eq!(
            crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
                .info()
                .await
                .unwrap()
                .latest_rev,
            5
        );
        assert_eq!(
            f.target
                .find_reference("refs/heads/feature")
                .unwrap()
                .target()
                .unwrap()
                .to_string(),
            op.pins.git.prepared_sha
        );
        assert_eq!(
            std::fs::read_to_string(&f.log)
                .unwrap()
                .lines()
                .filter(|line| line.contains("/git-receive-pack"))
                .count(),
            1
        );
        assert!(
            crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
                .is_ok()
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while crate::busy::try_acquire_pair("child", "parent").is_none() {
            assert!(std::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_single_flights_requests_and_binds_terminal_receipts_to_approval() {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let service = RefreshService::with_fixture_client(engine, f.client.clone());
    let token = "disposable-workspace-session";
    let (first, second) = tokio::join!(
        service.prepare(token.into(), "child".into(), "single-flight".into()),
        service.prepare(token.into(), "child".into(), "single-flight".into())
    );
    let approval = match (first, second) {
        (Ok(RefreshResponse::Approval(approval)), Err(error))
        | (Err(error), Ok(RefreshResponse::Approval(approval))) => {
            assert!(error.to_string().contains("already has a live preparation"));
            approval
        }
        other => panic!("duplicate request did not have one owner: {other:?}"),
    };
    assert!(service
        .prepare(token.into(), "child".into(), "single-flight".into())
        .await
        .is_err());
    let completed = service
        .act(
            token.into(),
            "child",
            "single-flight",
            approval.preparation_id,
            approval.approval_digest.clone(),
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(completed.operation_id, approval.operation_id);
    assert_eq!(completed.phase, "completed");
    let saved = journal_state(&f);
    for handle in [approval.preparation_id, Uuid::new_v4()] {
        assert!(service
            .act(
                token.into(),
                "child",
                "single-flight",
                handle,
                "other-approval".into(),
                RefreshAction::Execute
            )
            .await
            .is_err());
    }
    assert_eq!(journal_state(&f), saved);
    let again = service
        .act(
            token.into(),
            "child",
            "single-flight",
            approval.preparation_id,
            approval.approval_digest,
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(again.operation_id, completed.operation_id);
    let noop = match service
        .prepare(token.into(), "child".into(), "single-noop".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(value) => value,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(noop.kind, "no_op");
    assert!(service
        .act(
            token.into(),
            "child",
            "single-noop",
            noop.preparation_id,
            noop.approval_digest.clone(),
            RefreshAction::Reconcile
        )
        .await
        .is_err());
    assert!(crate::busy::try_acquire_pair("child", "parent").is_some());
    assert!(service
        .receipt(token, "child", "single-noop")
        .unwrap()
        .is_none());
    assert_eq!(journal_state(&f), saved);
    let no_op = service
        .act(
            token.into(),
            "child",
            "single-noop",
            noop.preparation_id,
            noop.approval_digest,
            RefreshAction::Execute,
        )
        .await
        .unwrap();
    assert_eq!(no_op.phase, "no_op");
    assert!(service
        .act(
            token.into(),
            "child",
            "single-noop",
            Uuid::new_v4(),
            "other-noop-approval".into(),
            RefreshAction::Execute
        )
        .await
        .is_err());
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        1
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_releases_busy_after_revoked_credentials_refuse_before_admission() {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let service = RefreshService::with_fixture_client(engine, f.client.clone());
    let token = "disposable-workspace-session";
    let approval = match service
        .prepare(token.into(), "child".into(), "revoked-service".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(value) => value,
        other => panic!("unexpected {other:?}"),
    };
    f.db.set_state("secret_git_token_child", "").unwrap();
    let saved = journal_state(&f);
    assert!(service
        .act(
            token.into(),
            "child",
            "revoked-service",
            approval.preparation_id,
            approval.approval_digest,
            RefreshAction::Execute
        )
        .await
        .is_err());
    assert!(crate::busy::try_acquire_pair("child", "parent").is_some());
    assert_eq!(journal_state(&f), saved);
    assert!(service
        .receipt(token, "child", "revoked-service")
        .unwrap()
        .is_none());
    assert!(f.db.pair_refresh_operation(approval.operation_id).is_err());
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_ok()
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert!(!std::fs::read_to_string(&f.log)
        .unwrap()
        .contains("receive-pack"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovery_server_fence_lifetime_preserves_held_epoch_and_cleans_only_terminal_state() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let fence = crate::writer_fence::claim(engine.db(), f.temp.path())
        .unwrap()
        .into_recovery_lifetime();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "lifetime-request", &digest, &busy)
        .unwrap();
    let saved = journal_state(&f);
    let lease = f.db.get_state("writer_fence_v1:lease").unwrap();
    let native = f.db.get_state("writer_fence_v1:native").unwrap();
    assert!(fence.release_if_quiet(engine.db()).is_err());
    fence.require_current(engine.db()).unwrap();
    assert_eq!(journal_state(&f), saved);
    assert_eq!(f.db.get_state("writer_fence_v1:lease").unwrap(), lease);
    assert_eq!(f.db.get_state("writer_fence_v1:native").unwrap(), native);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    actor
        .cancel_prepared_enrollment(&engine, &prepared, &busy)
        .unwrap();
    let terminal = journal_state(&f)
        .into_iter()
        .filter(|(key, _, _)| key != "writer_fence_v1:native")
        .collect::<Vec<_>>();
    fence.release_if_quiet(engine.db()).unwrap();
    assert!(f.db.get_state("writer_fence_v1:lease").unwrap().is_none());
    assert!(f.db.get_state("writer_fence_v1:native").unwrap().is_none());
    assert_eq!(journal_state(&f), terminal);
    assert!(fence.require_current(engine.db()).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_service_shutdown_joins_an_authorized_publication_before_returning() {
    #[cfg(unix)]
    let _process_boundary = packaged_call_site::enter_process_boundary()
        .expect("serialization owner is free");
    #[cfg(unix)]
    match packaged_call_site::attest_catalogue() {
        Ok(packaged_call_site::CatalogueAttestation::PackagedImmutable { manifest }) => {
            if let Err(error) = packaged_f10_shutdown(manifest, &_process_boundary).await {
                panic!("packaged call site failed closed: {error}");
            }
            return;
        }
        Ok(packaged_call_site::CatalogueAttestation::HistoricalNativeNotCatalogueProof) => {}
        Err(error) => panic!("missing H failed closed: {error}"),
    }
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    use std::os::unix::fs::PermissionsExt;
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let service = RefreshService::with_fixture_client(engine, f.client.clone());
    let token = "disposable-workspace-session";
    let approval = match service
        .prepare(token.into(), "child".into(), "shutdown-request".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(value) => value,
        other => panic!("unexpected {other:?}"),
    };
    let hook = f.temp.path().join("svn-repository/hooks/pre-commit");
    std::fs::write(
        &hook,
        r#"#!/usr/bin/env python3
from pathlib import Path
import time
root=Path(__file__).resolve().parents[2]
(root/'shutdown-ready').write_text('ready')
for _ in range(600):
    if (root/'shutdown-release').exists():
        raise SystemExit(0)
    time.sleep(0.05)
raise SystemExit(1)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _svn_diag = SvnDiag::arm(f.temp.path());
    let request_service = service.clone();
    let handle = approval.preparation_id;
    let digest = approval.approval_digest;
    let diagnostic_digest = digest.clone();
    let mut request = tokio::spawn(async move {
        request_service
            .act(
                token.into(),
                "child",
                "shutdown-request",
                handle,
                digest,
                RefreshAction::Execute,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(45), async {
        while !f.temp.path().join("shutdown-ready").is_file() {
            assert!(
                !request.is_finished(),
                "request ended before entering actual shutdown SVN publication: {}",
                {
                    let detail = match (&mut request).await {
                        Ok(Err(error)) => format!("{error:#}"),
                        Ok(Ok(_)) => "success-before-barrier".to_owned(),
                        Err(error) => format!("request join failed: {error}"),
                    };
                    let mut detail = crate::errors::redact_vcs_error_detail(&detail);
                    for secret in [
                        token,
                        "disposable-preparation-token",
                        diagnostic_digest.as_str(),
                    ] {
                        detail = detail.replace(secret, "[REDACTED_FIXTURE_SECRET]");
                    }
                    detail.push(' ');
                    detail.push_str(&case_diag(
                        f.temp.path(),
                        &hook,
                        &f.temp.path().join("shutdown-ready"),
                    ));
                    detail
                }
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.db.pair_refresh_operation(approval.operation_id)
            .unwrap()
            .phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::SvnIntent
    );
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let shutdown_service = service.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let shutdown = tokio::spawn(async move {
        started.send(()).unwrap();
        shutdown_service.shutdown().await;
    });
    ready.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(!shutdown.is_finished());
    assert!(service
        .prepare(token.into(), "child".into(), "during-shutdown".into())
        .await
        .is_err());
    std::fs::write(f.temp.path().join("shutdown-release"), b"release").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(45), shutdown)
        .await
        .unwrap()
        .unwrap();
    let receipt = service
        .receipt(token, "child", "shutdown-request")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.operation_id, approval.operation_id);
    assert_eq!(receipt.phase, "completed");
    assert_eq!(receipt.svn_revision, Some(5));
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        1
    );
    assert!(crate::busy::try_acquire_pair("child", "parent").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_startup_and_unproved_claim_preserve_a_valid_authenticated_hold() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let engine = f.engine();
    let lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let (actor, _workspace, prepared) = prepared(&f, &engine).await;
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let digest = actor.prepared_approval_digest(&engine, &prepared).unwrap();
    let op = actor
        .enroll_prepared(&engine, &prepared, "held-startup", &digest, &busy)
        .unwrap();
    drop(fence);
    drop(lock);
    let before = f.state();
    let path = f.temp.path().join("reposync.db");
    let wal = path.with_extension("db-wal");
    let bytes = std::fs::read(&path).unwrap();
    let wal_bytes = std::fs::read(&wal).unwrap();
    assert!(Database::open_with_exclusive_owner(f.temp.path()).is_err());
    assert_eq!(f.state(), before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_bytes);
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    assert!(crate::writer_fence::claim(engine.db(), f.temp.path()).is_err());
    assert_eq!(f.state(), before);
    assert_eq!(f.db.pair_refresh_operation(op.id).unwrap(), op);
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
}

// Only successful native claims use this expected-state builder. Ordinary
// journal snapshots keep native metadata visible, including renewal invariance.
fn journal_after_native_lease_claim(
    f: &Fixture,
    before: &[(String, String, String)],
    previous_epoch: u64,
    expected_pid: u32,
) -> Vec<(String, String, String)> {
    let lease: serde_json::Value =
        serde_json::from_str(&f.db.get_state("writer_fence_v1:lease").unwrap().unwrap()).unwrap();
    let native_row = journal_state(f)
        .into_iter()
        .find(|(key, _, _)| key == "writer_fence_v1:native")
        .unwrap();
    let native: serde_json::Value = serde_json::from_str(&native_row.1).unwrap();
    for field in [
        "version",
        "epoch",
        "holder_pid",
        "holder_host",
        "acquired_at",
    ] {
        assert_eq!(native["lease"][field], lease[field], "{field}");
    }
    assert!(lease["epoch"].as_u64().unwrap() > previous_epoch);
    assert_eq!(lease["holder_pid"].as_u64(), Some(u64::from(expected_pid)));
    let owner: crate::pair_refresh::native_owner::NativeProcessOwner =
        serde_json::from_value(native["owner"].clone()).unwrap();
    owner.validate().unwrap();
    assert_eq!(owner.pid(), expected_pid);
    assert!(owner
        .same_domain(&crate::pair_refresh::native_owner::NativeProcessOwner::capture().unwrap()));
    let mut expected = before
        .iter()
        .filter(|(key, _, _)| key != "writer_fence_v1:native")
        .cloned()
        .collect::<Vec<_>>();
    expected.push(native_row);
    expected.sort();
    expected
}

#[cfg(unix)]
mod packaged_call_site {
    // Unapplied H6 packaged call-site runtime. Missing H fails closed when a
    // packaged marker or an explicit mode is present. Explicit native-generated-v1
    // with no packaged pin selects historical native. An absent mode is not native.
    // An unknown explicit mode fails closed even when H is well shaped. Constructors do not force native-generated-v1. Inherited packaged
    // markers are rejected before any environment clearing. REPOSYNC_EXEC_CASE_DRAW is refused and is
    // not a nonce. Core reads 16 bytes from /dev/urandom. A harness instance pin is
    // checked against that value and is not the allocator. The nonce may equal
    // case_root_id. Case root is uid 65532 and mode 0700. Repository identity is
    // uid 65532 only. A 64-character hex shape is not authentication of S.

    use std::ffi::{CString, OsStr, OsString};
    use std::io::Error;
    use std::os::fd::RawFd;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use sha2::Digest;

    pub const F11_CATALOGUE_GIT: &str = "/opt/reliability-exec-fixtures/setup-shim/git";
    const F11_AUTH_SHA256: &str = "78b7f16c98bcf36b6fe852850172544a69a8e471e7d67615040d48cff0d4d9cf";

    pub const PRODUCER_CASES: &[(&str, &str, &str)] = &[
        ("F01", "git-reject-v1", "crates/core/tests/pair_refresh_import_exclusion.rs"),
        ("F02", "git-reject-v1", "crates/core/tests/pair_refresh_legacy_batches.rs"),
        ("F03", "git-final-barrier-v1", "crates/core/tests/pair_refresh_legacy_batches.rs"),
        ("F04", "git-reject-v1", "crates/core/tests/pair_refresh_publication.rs"),
        ("F05", "git-ref-move-v1", "crates/core/tests/pair_refresh_publication.rs"),
        ("F06", "git-reject-v1", "crates/core/src/pair_refresh/authenticated_enrollment_tests.rs:1618"),
        ("F07", "git-ref-move-v1", "crates/core/src/pair_refresh/publication_http_tests.rs"),
        ("F08", "git-ref-move-v1", "crates/core/src/pair_refresh/publication_http_tests.rs"),
        ("F09", "svn-disconnect-barrier-v1", "crates/core/src/pair_refresh/authenticated_enrollment_tests.rs"),
        ("F10", "svn-shutdown-barrier-v1", "crates/core/src/pair_refresh/authenticated_enrollment_tests.rs"),
        ("F11", "setup-push-shim-v1", "crates/web/tests/setup_wizard_busy.rs"),
    ];

    pub fn regression_source_obligations(case_id: &str) -> Result<(), String> {
        if PRODUCER_CASES.len() != 11 {
            return Err("regression source lost an F01-F11 producer".to_string());
        }
        let mut seen = [false; 11];
        for (index, row) in PRODUCER_CASES.iter().enumerate() {
            let expected = format!("F{:02}", index + 1);
            if row.0 != expected {
                return Err(format!("regression source case order drifted at {expected}"));
            }
            seen[index] = true;
        }
        if seen.iter().any(|present| !present) {
            return Err("regression source does not cover every case".to_string());
        }
        if !PRODUCER_CASES.iter().any(|row| {
            row.0 == "F06" && row.2.contains("authenticated_enrollment_tests.rs:1618")
        }) {
            return Err("F06 protected-ref call site is missing".to_string());
        }
        if !PRODUCER_CASES.iter().any(|row| {
            row.0 == "F07"
                && row.2 == "crates/core/src/pair_refresh/publication_http_tests.rs"
        }) || !PRODUCER_CASES.iter().any(|row| {
            row.0 == "F08"
                && row.2 == "crates/core/src/pair_refresh/publication_http_tests.rs"
        }) {
            return Err("F07/F08 http advertisement producers are missing".to_string());
        }
        if !PRODUCER_CASES.iter().any(|row| row.0 == case_id) {
            return Err(format!("case {case_id} is not a connected producer"));
        }
        Ok(())
    }

    pub struct HarnessValidatedH {
        digest: String,
    }

    impl HarnessValidatedH {
        pub fn from_harness_hex(hex_digest: &str) -> Result<Self, String> {
            if hex_digest.len() != 64
                || !hex_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(
                    "harness-validated H must be 64 lowercase hex characters before preparation"
                        .to_string(),
                );
            }
            Ok(Self {
                digest: hex_digest.to_string(),
            })
        }

        pub fn as_str(&self) -> &str {
            &self.digest
        }
    }

    pub enum CatalogueAttestation {
        HistoricalNativeNotCatalogueProof,
        PackagedImmutable { manifest: HarnessValidatedH },
    }

    pub fn attest_catalogue() -> Result<CatalogueAttestation, String> {
        let mode = std::env::var_os("REPOSYNC_FIXTURE_EXEC_MODE");
        let mode_text = match &mode {
            None => None,
            Some(raw) => Some(
                raw.to_str()
                    .ok_or_else(|| "REPOSYNC_FIXTURE_EXEC_MODE is not UTF-8".to_string())?
                    .to_string(),
            ),
        };
        let packaged_pins = [
            "REPOSYNC_EXEC_EXPECTED_ENTRY",
            "REPOSYNC_EXEC_EXPECTED_CASE",
            "REPOSYNC_EXEC_EXPECTED_INSTANCE",
            "REPOSYNC_EXEC_EXPECTED_ROOT_ID",
            "REPOSYNC_EXEC_EXPECTED_DATA_SHA256",
            "REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256",
        ];
        let any_pin = packaged_pins
            .iter()
            .any(|key| std::env::var_os(key).is_some());
        match mode_text.as_deref() {
            Some("native-generated-v1") => {
                if any_pin {
                    return Err(
                        "native-generated-v1 refuses a packaged manifest pin".to_string(),
                    );
                }
                Ok(CatalogueAttestation::HistoricalNativeNotCatalogueProof)
            }
            Some("packaged-immutable-v1") => {
                match std::env::var_os("REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256") {
                    None => Err("absent H".to_string()),
                    Some(raw) => {
                        let text = raw.to_str().unwrap_or_else(|| {
                            panic!("packaged manifest pin failed closed: value is not UTF-8");
                        });
                        match HarnessValidatedH::from_harness_hex(text) {
                            Ok(manifest) => {
                                Ok(CatalogueAttestation::PackagedImmutable { manifest })
                            }
                            Err(error) => {
                                panic!("packaged manifest pin failed closed: {error}")
                            }
                        }
                    }
                }
            }
            Some(_) => Err("explicit mode is required before packaged preparation".to_string()),
            None => Err("explicit mode is required; absent mode is not native".to_string()),
        }
    }

    static SERIAL_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static RETAINED_SESSIONS: Mutex<Vec<RetainedSession>> = Mutex::new(Vec::new());

    struct RetainedSession {
        owner: u64,
        context: crate::process::CaseContextId,
        env: EnvGuard,
        fds: Vec<RawFd>,
        publication_joined: bool,
        blocking_joined: bool,
        server_duty: u8,
    }

    struct SerializationPermit {
        owner: u64,
        context: crate::process::CaseContextId,
    }

    fn acquire_serialization() -> Result<SerializationPermit, String> {
        let context = crate::process::admit_case_context()?;
        let owner = context.serial_number();
        match SERIAL_OWNER.compare_exchange(0, owner,
            std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire)
        {
            Ok(_) => Ok(SerializationPermit { owner, context }),
            Err(held) => {
                let _ = crate::process::release_case_context(context, false);
                Err(format!("serialization owner {held} still excludes this caller"))
            }
        }
    }

    impl Drop for SerializationPermit {
        fn drop(&mut self) {
            if RETAINED_SESSIONS.lock().unwrap_or_else(|poison| poison.into_inner())
                .iter().any(|session| session.context == self.context)
            { return; }
            let _ = SERIAL_OWNER.compare_exchange(self.owner, 0,
                std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire);
            if crate::process::release_case_context(self.context, false).is_err()
                && crate::process::case_context_held(self.context)
            {
                let _ = SERIAL_OWNER.compare_exchange(0, self.owner,
                    std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire);
            }
        }
    }

    /// Observes already-owned work only. In particular, an armed F09 service
    /// is not started by cleanup or Drop, and no release marker is synthesized.
    pub async fn observe_retained_session(owner: u64) -> Result<(), String> {
        let (context, publication_joined) = {
            let sessions = RETAINED_SESSIONS.lock().unwrap_or_else(|poison| poison.into_inner());
            let session = sessions.iter().find(|session| session.owner == owner)
                .ok_or_else(|| "session has no retained owner".to_string())?;
            (session.context, session.publication_joined)
        };
        for handle in crate::process::case_resource_handles::<super::CaseShutdown>(context) {
            if !crate::process::with_case_resource(handle, |shutdown| shutdown.started)? {
                return Err("service remains armed; observation cannot start shutdown".into());
            }
            tokio::time::timeout(Duration::from_secs(45), std::future::poll_fn(|cx| {
                match crate::process::with_case_resource(handle, |shutdown| shutdown.poll_join(cx)) {
                    Ok(poll) => poll,
                    Err(error) => std::task::Poll::Ready(Err(error)),
                }
            })).await.map_err(|_| "retained shutdown is still incomplete".to_string())??;
            crate::process::finish_case_resource(handle)?;
        }
        crate::process::close_case_registration(context)?;
        crate::pair_refresh::execution::observe_retained(
            crate::pair_refresh::execution::BlockingAssoc::Case(context),
        ).await.map_err(|error| error.to_string())?;
        crate::pair_refresh::execution::blocking_proof(context)?.require_session(context)?;
        for handle in crate::process::case_resource_handles::<super::CaseServer>(context) {
            crate::process::with_case_resource(handle, |server| server.request_stop())??;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if crate::process::with_case_resource(handle, |server| server.observe_stop())?? {
                        return Ok::<(), String>(());
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await.map_err(|_| "retained server observation timed out".to_string())??;
            crate::process::finish_case_resource(handle)?;
        }
        // Physical cleanup cannot manufacture the missing publication proof or
        // retire a failed case. Preserve native lock/fence/root on that path.
        if !publication_joined {
            return Err("resources observed; original publication qualification remains absent".into());
        }
        for handle in crate::process::case_resource_handles::<super::CaseNativeLeases>(context) {
            crate::process::finish_case_resource(handle)?;
        }
        let mut sessions = RETAINED_SESSIONS.lock().unwrap_or_else(|poison| poison.into_inner());
        let session = sessions.iter_mut().find(|session| session.context == context)
            .ok_or_else(|| "retained context changed during observation".to_string())?;
        session.blocking_joined = true;
        session.server_duty = 2;
        Ok(())
    }

    pub fn release_retained_session(owner: u64) -> Result<(), String> {
        let mut session = {
            let mut sessions = RETAINED_SESSIONS.lock().unwrap_or_else(|poison| poison.into_inner());
            let index = sessions.iter().position(|session| session.owner == owner)
                .ok_or_else(|| "session has no retained resources".to_string())?;
            let session = &sessions[index];
            duties_allow_release(session.publication_joined, session.blocking_joined, session.server_duty)?;
            if !crate::process::case_context_ready(session.context)
                || crate::process::retained_ownership_incomplete()
            { return Err("actual case work or failure remains retained".into()); }
            sessions.remove(index)
        };
        session.env.restore();
        let mut seen = Vec::new();
        for fd in session.fds {
            if fd >= 0 && !seen.contains(&fd) {
                unsafe { libc::close(fd) }; seen.push(fd);
            }
        }
        let _ = SERIAL_OWNER.compare_exchange(owner, 0,
            std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire);
        if let Err(error) = crate::process::release_case_context(session.context, true) {
            if crate::process::case_context_held(session.context) {
                let _ = SERIAL_OWNER.compare_exchange(0, owner,
                    std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire);
            }
            return Err(error);
        }
        Ok(())
    }

    pub struct ProcessBoundary { permit: SerializationPermit }
    pub fn enter_process_boundary() -> Result<ProcessBoundary, String> {
        Ok(ProcessBoundary { permit: acquire_serialization()? })
    }
    impl ProcessBoundary {
        pub fn assert_held(&self) {
            assert_eq!(SERIAL_OWNER.load(std::sync::atomic::Ordering::Acquire), self.permit.owner,
                "process boundary does not own serialization");
            assert!(crate::process::case_context_held(self.permit.context),
                "process-wide case admission changed");
        }
        pub fn owner(&self) -> u64 { self.permit.owner }
        pub(super) fn context(&self) -> crate::process::CaseContextId { self.permit.context }
    }

    pub struct EnvGuard {
        saved: Vec<(String, Option<OsString>)>,
    }

    impl EnvGuard {
        pub fn new() -> Self {
            Self { saved: Vec::new() }
        }

        pub fn push_os(&mut self, key: &str, value: &OsStr) {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            self.saved.push((key.to_string(), previous));
        }

        pub fn push_utf8(&mut self, key: &str, value: &str) {
            self.push_os(key, OsStr::new(value));
        }

        fn restore(&mut self) {
            while let Some((key, previous)) = self.saved.pop() {
                match previous {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            self.restore();
        }
    }

    struct CleanupInner {
        error: Option<String>,
        explicit_ok: bool,
    }

    #[derive(Clone)]
    pub struct CleanupState {
        inner: Arc<Mutex<CleanupInner>>,
    }

    impl CleanupState {
        pub fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(CleanupInner {
                    error: None,
                    explicit_ok: false,
                })),
            }
        }

        pub fn record(&self, error: impl Into<String>) {
            let mut guard = self.inner.lock().unwrap_or_else(|poison| poison.into_inner());
            if guard.error.is_none() {
                guard.error = Some(error.into());
            }
        }

        pub fn current(&self) -> Option<String> {
            self.inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .error
                .clone()
        }

        fn mark_explicit_ok(&self) -> Result<(), String> {
            let mut guard = self.inner.lock().unwrap_or_else(|poison| poison.into_inner());
            if let Some(error) = &guard.error {
                return Err(format!("cleanup failure prevents qualification: {error}"));
            }
            guard.explicit_ok = true;
            Ok(())
        }

        fn explicit_ok(&self) -> bool {
            self.inner
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .explicit_ok
        }
    }

    pub struct JoinProof {
        _private: (),
    }

    pub struct JoinReport {
        pub proof: JoinProof,
        pub cancelled_tasks: usize,
        pub exit_codes: Vec<i32>,
    }

    pub fn qualify(proof: JoinProof, state: &CleanupState) -> Result<(), String> {
        let _proof = proof;
        state.mark_explicit_ok()?;
        if !state.explicit_ok() {
            return Err("drop cleanup cannot prove completion".to_string());
        }
        Ok(())
    }

    pub struct UnjoinedGuard {
        joined: bool,
        failure: CleanupState,
    }

    impl UnjoinedGuard {
        pub fn arm(failure: &CleanupState) -> Self {
            Self {
                joined: false,
                failure: failure.clone(),
            }
        }

        pub fn mark_joined(mut self) -> Result<JoinProof, String> {
            self.joined = true;
            if let Some(error) = self.failure.current() {
                return Err(error);
            }
            Ok(JoinProof { _private: () })
        }
    }

    impl Drop for UnjoinedGuard {
        fn drop(&mut self) {
            if !self.joined {
                self.failure
                    .record("owned work dropped without an explicit successful join");
            }
        }
    }

    pub fn join_scoped<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> Result<T, String> {
        handle.join().map_err(|_| {
            "scoped worker panicked; cleanup failure prevents qualification".to_string()
        })
    }

    struct ChildSlot {
        child: Option<Child>,
        reaped: bool,
        qualified: bool,
        failure: CleanupState,
    }

    impl ChildSlot {
        fn vacant(failure: CleanupState) -> Self {
            Self {
                child: None,
                reaped: false,
                qualified: false,
                failure,
            }
        }

        fn explicit_wait(&mut self) -> Result<i32, String> {
            let child = self
                .child
                .as_mut()
                .ok_or_else(|| "missing child".to_string())?;
            let code = wait_child(child, false)?;
            self.reaped = true;
            self.qualified = true;
            self.child.take();
            Ok(code)
        }
    }

    impl Drop for ChildSlot {
        fn drop(&mut self) {
            if self.reaped {
                return;
            }
            if let Some(mut child) = self.child.take() {
                self.reaped = true;
                self.qualified = false;
                match wait_child(&mut child, true) {
                    Ok(_) => self
                        .failure
                        .record("child reaped by drop; drop cleanup is not qualification"),
                    Err(error) => self.failure.record(error),
                }
            }
        }
    }

    fn wait_child(child: &mut Child, kill_first: bool) -> Result<i32, String> {
        if kill_first {
            let _ = child.kill();
        }
        match child.wait() {
            Ok(status) => exit_code(&status),
            Err(error) => Err(format!("wait failed: {error}")),
        }
    }

    fn exit_code(status: &ExitStatus) -> Result<i32, String> {
        status
            .code()
            .ok_or_else(|| "child ended without a status; qualification refused".to_string())
    }

    struct ThreadSlot {
        handle: Option<std::thread::JoinHandle<()>>,
        reaped: bool,
        qualified: bool,
        failure: CleanupState,
    }

    impl ThreadSlot {
        fn vacant(failure: CleanupState) -> Self {
            Self {
                handle: None,
                reaped: false,
                qualified: false,
                failure,
            }
        }

        fn explicit_join(&mut self) -> Result<(), String> {
            let handle = self
                .handle
                .take()
                .ok_or_else(|| "missing thread".to_string())?;
            match handle.join() {
                Ok(()) => {
                    self.reaped = true;
                    self.qualified = true;
                    Ok(())
                }
                Err(_) => {
                    self.reaped = true;
                    self.qualified = false;
                    let error =
                        "owned thread panicked; cleanup failure prevents qualification".to_string();
                    self.failure.record(error.clone());
                    Err(error)
                }
            }
        }
    }

    impl Drop for ThreadSlot {
        fn drop(&mut self) {
            if self.reaped {
                return;
            }
            if let Some(handle) = self.handle.take() {
                self.reaped = true;
                self.qualified = false;
                if handle.join().is_err() {
                    self.failure
                        .record("drop join observed a thread panic; completion is not proven");
                } else {
                    self.failure
                        .record("thread joined by drop; drop cleanup is not qualification");
                }
            }
        }
    }

    struct TaskSlot {
        handle: Option<tokio::task::JoinHandle<()>>,
        reaped: bool,
        qualified: bool,
        failure: CleanupState,
    }

    impl TaskSlot {
        fn vacant(failure: CleanupState) -> Self {
            Self {
                handle: None,
                reaped: false,
                qualified: false,
                failure,
            }
        }
    }

    impl Drop for TaskSlot {
        fn drop(&mut self) {
            if self.reaped {
                return;
            }
            if let Some(handle) = self.handle.take() {
                crate::process::retain_unjoined_task(handle);
                self.qualified = false;
                self.failure
                    .record("unjoined task retained; drop abort is not qualification");
            }
        }
    }

    pub struct OwnedWork {
        children: Vec<ChildSlot>,
        threads: Vec<ThreadSlot>,
        tasks: Vec<TaskSlot>,
        failure: CleanupState,
        explicit: bool,
        settled: bool,
    }

    impl OwnedWork {
        pub fn new() -> Self {
            Self {
                children: Vec::new(),
                threads: Vec::new(),
                tasks: Vec::new(),
                failure: CleanupState::new(),
                explicit: false,
                settled: false,
            }
        }

        pub fn state(&self) -> CleanupState {
            self.failure.clone()
        }

        pub fn spawn_command(&mut self, mut command: Command) -> Result<(), String> {
            self.children.push(ChildSlot::vacant(self.failure.clone()));
            let child = match command.spawn() {
                Ok(child) => child,
                Err(error) => return Err(format!("spawn: {error}")),
            };
            self.children
                .last_mut()
                .ok_or_else(|| "child slot was not registered".to_string())?
                .child = Some(child);
            Ok(())
        }

        pub fn spawn_thread<F>(&mut self, work: F) -> Result<(), String>
        where
            F: FnOnce() -> Result<(), String> + Send + 'static,
        {
            self.threads.push(ThreadSlot::vacant(self.failure.clone()));
            let failure = self.failure.clone();
            let handle = match std::thread::Builder::new().spawn(move || {
                if let Err(error) = work() {
                    failure.record(error);
                }
            }) {
                Ok(handle) => handle,
                Err(error) => return Err(format!("thread spawn: {error}")),
            };
            self.threads
                .last_mut()
                .ok_or_else(|| "thread slot was not registered".to_string())?
                .handle = Some(handle);
            Ok(())
        }

        pub fn spawn_task<F>(&mut self, future: F)
        where
            F: std::future::Future<Output = ()> + Send + 'static,
        {
            self.tasks.push(TaskSlot::vacant(self.failure.clone()));
            let handle = tokio::spawn(future);
            if let Some(slot) = self.tasks.last_mut() {
                slot.handle = Some(handle);
            }
        }

        pub fn task_finished(&self, index: usize) -> bool {
            self.tasks
                .get(index)
                .and_then(|slot| slot.handle.as_ref())
                .map(tokio::task::JoinHandle::is_finished)
                .unwrap_or(true)
        }

        pub fn abort_task(&mut self, index: usize) {
            if let Some(handle) = self.tasks.get(index).and_then(|slot| slot.handle.as_ref()) {
                handle.abort();
            }
        }

        pub fn join_sync(mut self) -> Result<JoinReport, String> {
            if !self.tasks.is_empty() {
                return Err("async tasks require join_async".to_string());
            }
            let report = match self.finish_sync() {
                Ok(report) => report,
                Err(error) => {
                    self.settled = true;
                    return Err(error);
                }
            };
            self.explicit = true;
            self.settled = true;
            Ok(report)
        }

        fn finish_sync(&mut self) -> Result<JoinReport, String> {
            let mut errors = Vec::new();
            let mut exit_codes = Vec::new();
            for slot in &mut self.children {
                match slot.explicit_wait() {
                    Ok(code) => exit_codes.push(code),
                    Err(error) => errors.push(error),
                }
            }
            for slot in &mut self.threads {
                if let Err(error) = slot.explicit_join() {
                    errors.push(error);
                }
            }
            if let Some(error) = self.failure.current() {
                errors.push(error);
            }
            if !errors.is_empty() {
                return Err(errors.join("; "));
            }
            if self
                .children
                .iter()
                .any(|slot| !slot.qualified)
                || self.threads.iter().any(|slot| !slot.qualified)
            {
                return Err("a joined slot was not qualified".to_string());
            }
            Ok(JoinReport {
                proof: JoinProof { _private: () },
                cancelled_tasks: 0,
                exit_codes,
            })
        }

        pub async fn join_async(mut self) -> Result<JoinReport, String> {
            use std::future::Future;
            let mut report = match self.finish_sync() {
                Ok(report) => report,
                Err(error) => {
                    self.settled = true;
                    return Err(error);
                }
            };
            let mut errors = Vec::new();
            for slot in &mut self.tasks {
                if slot.handle.is_none() {
                    errors.push("missing task".to_string());
                    continue;
                }
                let polled = std::future::poll_fn(|cx| {
                    let handle = slot
                        .handle
                        .as_mut()
                        .expect("task handle remains registered until poll completes");
                    std::pin::Pin::new(handle).poll(cx)
                })
                .await;
                match polled {
                    Ok(()) => {
                        slot.reaped = true;
                        slot.qualified = true;
                        slot.handle.take();
                    }
                    Err(error) if error.is_cancelled() => {
                        slot.reaped = true;
                        slot.qualified = false;
                        slot.handle.take();
                        report.cancelled_tasks += 1;
                    }
                    Err(error) => {
                        slot.reaped = true;
                        slot.qualified = false;
                        slot.handle.take();
                        errors.push(format!(
                            "task panicked or join failed; qualification refused: {error}"
                        ));
                    }
                }
            }
            if let Some(error) = self.failure.current() {
                errors.push(error);
            }
            if !errors.is_empty() {
                self.settled = true;
                return Err(errors.join("; "));
            }
            self.explicit = true;
            self.settled = true;
            Ok(report)
        }
    }

    impl Drop for OwnedWork {
        fn drop(&mut self) {
            if self.explicit || self.settled {
                return;
            }
            self.failure
                .record("owned work dropped before explicit join proved completion");
        }
    }


    pub struct ReceiverStatus {
        pub publication_rejected: bool,
        pub observed_exit: Option<i32>,
    }

    impl ReceiverStatus {
        pub fn from_publication_result(publication_rejected: bool) -> Self {
            Self {
                publication_rejected,
                observed_exit: None,
            }
        }
    }

    pub struct AuditChildExit {
        pub value: i64,
    }

    pub struct RefMoveObservation {
        pub audit_child_exit: AuditChildExit,
        pub receiver_status: ReceiverStatus,
        pub another_update_ref_permitted: bool,
    }

    fn canonical_number(raw: &str) -> Result<String, String> {
        if raw.is_empty() || raw.starts_with('+') || raw.starts_with('-') {
            return Err("number is not a canonical non-negative integer".to_string());
        }
        if raw.len() > 1 && raw.starts_with('0') {
            return Err("number spelling is not canonical".to_string());
        }
        if !raw.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("number is not decimal".to_string());
        }
        let value: i64 = raw
            .parse()
            .map_err(|_| "number is not canonical".to_string())?;
        let canon = value.to_string();
        if canon != raw {
            return Err("number spelling is not canonical".to_string());
        }
        Ok(canon)
    }

    fn parse_flat_object(bytes: &[u8]) -> Result<std::collections::BTreeMap<String, String>, String> {
        if bytes.last() != Some(&b'\n') || bytes.iter().filter(|byte| **byte == b'\n').count() != 1 {
            return Err("witness is not one LF-terminated line".to_string());
        }
        let line = std::str::from_utf8(&bytes[..bytes.len() - 1])
            .map_err(|_| "witness is not UTF-8".to_string())?;
        if line.contains(' ') || !line.starts_with('{') || !line.ends_with('}') {
            return Err("witness is not canonical".to_string());
        }
        let mut map = std::collections::BTreeMap::new();
        let mut rest = &line[1..line.len() - 1];
        if rest.is_empty() {
            return Err("witness object is empty".to_string());
        }
        while !rest.is_empty() {
            if !rest.starts_with('"') {
                return Err("witness key is not a string".to_string());
            }
            let key_end = rest[1..]
                .find('"')
                .ok_or_else(|| "witness key is unterminated".to_string())?;
            let key = &rest[1..1 + key_end];
            rest = &rest[1 + key_end + 1..];
            if !rest.starts_with(':') {
                return Err("witness separator is missing".to_string());
            }
            rest = &rest[1..];
            let (encoded, next) = if let Some(value_rest) = rest.strip_prefix('"') {
                let value_end = value_rest
                    .find('"')
                    .ok_or_else(|| "witness string is unterminated".to_string())?;
                let value = &value_rest[..value_end];
                if value.contains('\\') || value.contains('"') {
                    return Err("witness string encoding is refused".to_string());
                }
                (format!("\"{value}\""), &value_rest[value_end + 1..])
            } else {
                let number = rest
                    .split(',')
                    .next()
                    .ok_or_else(|| "witness number is missing".to_string())?;
                let canon = canonical_number(number)?;
                (canon, &rest[number.len()..])
            };
            if map.insert(key.to_string(), encoded).is_some() {
                return Err("witness repeats a key".to_string());
            }
            if next.is_empty() {
                break;
            }
            if !next.starts_with(',') {
                return Err("witness field separator is refused".to_string());
            }
            rest = &next[1..];
        }
        let mut rebuilt = String::from("{");
        for (index, (key, value)) in map.iter().enumerate() {
            if index > 0 {
                rebuilt.push(',');
            }
            rebuilt.push('"');
            rebuilt.push_str(key);
            rebuilt.push_str("\":");
            rebuilt.push_str(value);
        }
        rebuilt.push('}');
        rebuilt.push('\n');
        if rebuilt.as_bytes() != bytes {
            return Err("witness bytes are not canonical".to_string());
        }
        Ok(map)
    }

    fn last_os_error(context: &str) -> String {
        format!("{context}: {}", Error::last_os_error())
    }

    fn c_name(name: &str) -> Result<CString, String> {
        CString::new(name).map_err(|_| format!("component contains NUL: {name}"))
    }

    fn is_component(name: &str) -> bool {
        !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
    }

    fn close_fd(fd: RawFd) {
        if fd >= 0 {
            unsafe { libc::close(fd) };
        }
    }

    fn file_type(mode: libc::mode_t) -> libc::mode_t {
        mode & libc::S_IFMT
    }

    fn perm_bits(mode: libc::mode_t) -> u32 {
        (mode & 0o777) as u32
    }

    fn fstat_fd(fd: RawFd) -> Result<libc::stat, String> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } < 0 {
            return Err(last_os_error("fstat"));
        }
        Ok(st)
    }

    fn open_nofollow(path: &Path, want_dir: bool) -> Result<RawFd, String> {
        let text = path
            .to_str()
            .ok_or_else(|| "path is not UTF-8".to_string())?;
        if !text.starts_with('/') || text.contains("//") || text.ends_with('/') {
            return Err(format!("refusing non-canonical absolute path {text}"));
        }
        let root = c_name("/")?;
        let mut fd = unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(last_os_error("open /"));
        }
        let parts: Vec<&str> = text.split('/').filter(|part| !part.is_empty()).collect();
        for (index, part) in parts.iter().enumerate() {
            if !is_component(part) {
                close_fd(fd);
                return Err(format!("refusing path component in {text}"));
            }
            let last = index + 1 == parts.len();
            let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            if !last || want_dir {
                flags |= libc::O_DIRECTORY;
            }
            let c = match c_name(part) {
                Ok(c) => c,
                Err(error) => {
                    close_fd(fd);
                    return Err(error);
                }
            };
            let next = unsafe { libc::openat(fd, c.as_ptr(), flags, 0) };
            close_fd(fd);
            if next < 0 {
                return Err(last_os_error(&format!("nofollow open {text}")));
            }
            fd = next;
        }
        Ok(fd)
    }

    fn require_case_root_directory(fd: RawFd) -> Result<(), String> {
        let st = fstat_fd(fd)?;
        if file_type(st.st_mode) != libc::S_IFDIR || st.st_uid != 65532 || perm_bits(st.st_mode) != 0o700
        {
            return Err(
                "case root must be uid 65532 and mode 0700; creation does not chown"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn require_repository_directory(fd: RawFd) -> Result<(), String> {
        let st = fstat_fd(fd)?;
        if file_type(st.st_mode) != libc::S_IFDIR || st.st_uid != 65532 {
            return Err(
                "repository directory must be uid 65532; mode bits are not a repository rule"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn fstatat_nofollow(dir_fd: RawFd, name: &str) -> Result<Option<libc::stat>, String> {
        if !is_component(name) {
            return Err(format!("refusing component {name}"));
        }
        let c = c_name(name)?;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstatat(dir_fd, c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
        if rc == 0 {
            return Ok(Some(st));
        }
        let error = Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        Err(format!("fstatat {name}: {error}"))
    }

    pub fn publish_nofollow(dir_fd: RawFd, name: &str, payload: &[u8], mode: u32) -> Result<(), String> {
        if !is_component(name) || name.starts_with('.') {
            return Err(format!("refusing publication name {name}"));
        }
        if mode != 0o644 && mode != 0o600 {
            return Err("publication mode must be 0644 or 0600".to_string());
        }
        if payload.len() > 4096 {
            return Err("publication payload exceeds 4096 bytes".to_string());
        }
        let tmp_name = format!(".rs-publish-{name}");
        let tmp = c_name(&tmp_name)?;
        let dest = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                dir_fd,
                tmp.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::mode_t,
            )
        };
        if fd < 0 {
            return Err(last_os_error(
                "exclusive nofollow temporary create refused; destination was not replaced",
            ));
        }
        let written = (|| {
            if unsafe { libc::fchmod(fd, mode as libc::mode_t) } < 0 {
                return Err(last_os_error("fchmod publication"));
            }
            let mut offset = 0usize;
            while offset < payload.len() {
                let wrote = unsafe {
                    libc::write(
                        fd,
                        payload[offset..].as_ptr() as *const libc::c_void,
                        payload.len() - offset,
                    )
                };
                if wrote < 0 {
                    let error = Error::last_os_error();
                    if error.raw_os_error() == Some(libc::EINTR) {
                        continue;
                    }
                    return Err(format!("write: {error}"));
                }
                if wrote == 0 {
                    return Err("short write".to_string());
                }
                offset += wrote as usize;
            }
            if unsafe { libc::fsync(fd) } < 0 {
                return Err(last_os_error("fsync publication"));
            }
            Ok(())
        })();
        close_fd(fd);
        if let Err(error) = written {
            unsafe { libc::unlinkat(dir_fd, tmp.as_ptr(), 0) };
            return Err(error);
        }
        let linked = unsafe { libc::linkat(dir_fd, tmp.as_ptr(), dir_fd, dest.as_ptr(), 0) };
        unsafe { libc::unlinkat(dir_fd, tmp.as_ptr(), 0) };
        if linked < 0 {
            return Err("linkat refused; an existing node was not replaced".to_string());
        }
        unsafe { libc::fsync(dir_fd) };
        let verify = unsafe {
            libc::openat(
                dir_fd,
                dest.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )
        };
        if verify < 0 {
            return Err(last_os_error("reopen published node"));
        }
        let verified = (|| {
            let st = fstat_fd(verify)?;
            if file_type(st.st_mode) != libc::S_IFREG
                || st.st_nlink as u64 != 1
                || st.st_uid != 65532
                || perm_bits(st.st_mode) != mode
                || st.st_size as u64 != payload.len() as u64
            {
                return Err("published node failed regular/nlink/owner/mode check".to_string());
            }
            let mut body = vec![0u8; payload.len()];
            read_exact(verify, &mut body)?;
            if body != payload {
                return Err("published node bytes changed".to_string());
            }
            Ok(())
        })();
        close_fd(verify);
        verified
    }

    fn read_nofollow(dir_fd: RawFd, name: &str) -> Result<Vec<u8>, String> {
        if !is_component(name) {
            return Err(format!("refusing component {name}"));
        }
        let c = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                dir_fd,
                c.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )
        };
        if fd < 0 {
            return Err(last_os_error(&format!("nofollow read {name}")));
        }
        let read = (|| {
            let st = fstat_fd(fd)?;
            if file_type(st.st_mode) != libc::S_IFREG
                || st.st_nlink as u64 != 1
                || st.st_uid != 65532
                || (perm_bits(st.st_mode) != 0o600 && perm_bits(st.st_mode) != 0o644)
                || st.st_size < 0
                || st.st_size as u64 > 4096
            {
                return Err(format!("refusing node {name}"));
            }
            let mut body = vec![0u8; st.st_size as usize];
            read_exact(fd, &mut body)?;
            Ok(body)
        })();
        close_fd(fd);
        read
    }

    fn read_exact(fd: RawFd, body: &mut [u8]) -> Result<(), String> {
        let mut offset = 0usize;
        while offset < body.len() {
            let got = unsafe {
                libc::read(
                    fd,
                    body[offset..].as_mut_ptr() as *mut libc::c_void,
                    body.len() - offset,
                )
            };
            if got < 0 {
                let error = Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(format!("read: {error}"));
            }
            if got == 0 {
                return Err("short read".to_string());
            }
            offset += got as usize;
        }
        Ok(())
    }

    pub fn publish_at(path: &Path, name: &str, payload: &[u8], mode: u32) -> Result<(), String> {
        let fd = open_nofollow(path, true)?;
        let published = publish_nofollow(fd, name, payload, mode);
        close_fd(fd);
        published
    }

    pub async fn wait_marker_async(dir: &Path, name: &str, bound: Duration) -> Result<(), String> {
        let deadline = Instant::now() + bound;
        loop {
            if marker_is_regular(dir, name)? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("bounded readiness expired waiting for {name}"));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub fn wait_marker(dir: &Path, name: &str, bound: Duration) -> Result<(), String> {
        let deadline = Instant::now() + bound;
        loop {
            if marker_is_regular(dir, name)? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!("bounded readiness expired waiting for {name}"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn marker_is_regular(dir: &Path, name: &str) -> Result<bool, String> {
        let fd = open_nofollow(dir, true)?;
        let found = match fstatat_nofollow(fd, name)? {
            Some(st) => file_type(st.st_mode) == libc::S_IFREG,
            None => false,
        };
        close_fd(fd);
        Ok(found)
    }

    fn canonical_json(fields: &[(&str, &str)]) -> Result<Vec<u8>, String> {
        let mut encoded: Vec<(&str, String)> = Vec::with_capacity(fields.len() + 1);
        for (key, value) in fields {
            if key.contains('"') || key.contains('\\') || value.contains('"') || value.contains('\\') {
                return Err("canonical field contains a character this encoder refuses".to_string());
            }
            encoded.push((*key, format!("\"{value}\"")));
        }
        encoded.push(("schema", "1".to_string()));
        encoded.sort_by(|left, right| left.0.cmp(right.0));
        let mut text = String::from("{");
        for (index, (key, value)) in encoded.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push('"');
            text.push_str(key);
            text.push_str("\":");
            text.push_str(value);
        }
        text.push('}');
        text.push('\n');
        Ok(text.into_bytes())
    }

    fn validate_initial_oid(value: &str) -> Result<(), String> {
        if value.len() == 40 && value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(())
        } else {
            Err("initial_oid must be 40 lowercase hex characters".to_string())
        }
    }

    struct AdmittedHook {
        entry_id: &'static str,
        slot: &'static str,
        target: &'static str,
    }

    const ADMITTED_HOOKS: &[AdmittedHook] = &[
        AdmittedHook {
            entry_id: "git-reject-v1",
            slot: "pre-receive",
            target: "/opt/reliability-exec-fixtures/git-reject/pre-receive",
        },
        AdmittedHook {
            entry_id: "git-final-barrier-v1",
            slot: "pre-receive",
            target: "/opt/reliability-exec-fixtures/git-final-barrier/pre-receive",
        },
        AdmittedHook {
            entry_id: "git-ref-move-v1",
            slot: "pre-receive",
            target: "/opt/reliability-exec-fixtures/git-ref-move/pre-receive",
        },
        AdmittedHook {
            entry_id: "svn-disconnect-barrier-v1",
            slot: "pre-commit",
            target: "/opt/reliability-exec-fixtures/svn-disconnect/pre-commit",
        },
        AdmittedHook {
            entry_id: "svn-shutdown-barrier-v1",
            slot: "pre-commit",
            target: "/opt/reliability-exec-fixtures/svn-shutdown/pre-commit",
        },
    ];

    pub struct PreparedRepo {
        case_fd: RawFd,
        repo_fd: RawFd,
        config_fd: RawFd,
        marker_fd: RawFd,
        env: EnvGuard,
        selected_path: String,
        selected_sha256: String,
        selected_len: u64,
        selected_mode: u32,
        case_root_id: String,
        explicitly_completed: bool,
        owner: u64,
        context: crate::process::CaseContextId,
        shutdown: Option<crate::process::CaseResourceHandle<super::CaseShutdown>>,
        publication_joined: bool,
        blocking_joined: bool,
        server_duty: u8,
        proved_markers: std::sync::Mutex<Vec<String>>,
        selected_installed_relative_path: String,
        selected_installed_mode: u32,
        pub entry_id: String,
        pub case_id: String,
        pub instance_id: String,
        pub manifest_sha256: String,
        pub data_sha256: String,
        pub initial_oid: Option<String>,
        pub marker_path: PathBuf,
        hook_target: Option<String>,
        hook_dev: Option<u64>,
        hook_ino: Option<u64>,
        hook_link_dev: Option<u64>,
        hook_link_ino: Option<u64>,
    }

    impl Drop for PreparedRepo {
        fn drop(&mut self) {
            if self.explicitly_completed {
                return;
            }
            crate::process::retain_case_context(self.context);
            let mut retained = RETAINED_SESSIONS
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let mut fds = Vec::new();
            for fd in [
                &mut self.case_fd,
                &mut self.repo_fd,
                &mut self.config_fd,
                &mut self.marker_fd,
            ] {
                if *fd >= 0 {
                    if !fds.contains(fd) { fds.push(*fd); }
                    *fd = -1;
                }
            }
            retained.push(RetainedSession {
                owner: self.owner,
                context: self.context,
                env: std::mem::replace(&mut self.env, EnvGuard::new()),
                fds,
                publication_joined: self.publication_joined,
                blocking_joined: self.blocking_joined,
                server_duty: self.server_duty,
            });
        }
    }

    fn duties_allow_release(
        publication_joined: bool,
        blocking_joined: bool,
        server_duty: u8,
    ) -> Result<(), String> {
        if !publication_joined || !blocking_joined || server_duty == 0 {
            return Err(
                "this session has not joined its publication, blocking work, and server"
                    .to_string(),
            );
        }
        if server_duty != 1 && server_duty != 2 {
            return Err("server completion for this session is uncertain".to_string());
        }
        Ok(())
    }

    impl PreparedRepo {
        pub fn seal(&mut self) -> Result<(), String> {
            duties_allow_release(
                self.publication_joined,
                self.blocking_joined,
                self.server_duty,
            )?;
            if !crate::process::case_context_ready(self.context)
                || crate::process::retained_ownership_incomplete()
            {
                return Err("actual case resources or cleanup remain incomplete".to_string());
            }
            self.release_owned_resources();
            self.explicitly_completed = true;
            Ok(())
        }

        fn release_owned_resources(&mut self) {
            self.env.restore();
            let mut seen = [-1i32; 4];
            let mut count = 0usize;
            for fd in [
                &mut self.case_fd,
                &mut self.repo_fd,
                &mut self.config_fd,
                &mut self.marker_fd,
            ] {
                if *fd >= 0 && !seen[..count].contains(fd) {
                    unsafe { libc::close(*fd) };
                    seen[count] = *fd;
                    count += 1;
                }
                *fd = -1;
            }
        }

        pub fn record_publication_joined(&mut self) {
            self.publication_joined = true;
        }

        pub fn record_blocking_joined(
            &mut self,
            proof: crate::pair_refresh::execution::BlockingProof,
        ) -> Result<(), String> {
            proof.require_session(self.context)?;
            self.blocking_joined = true;
            Ok(())
        }

        pub(super) fn arm_shutdown(
            &mut self,
            service: std::sync::Arc<crate::pair_refresh::service::RefreshService>,
        ) -> Result<(), String> {
            if self.shutdown.is_some() { return Err("shutdown already armed".into()); }
            self.shutdown = Some(crate::process::register_case_resource(self.context, || {
                Ok(super::CaseShutdown { service, started: false, handle: None, outcome: None })
            })?);
            Ok(())
        }

        pub(super) fn start_shutdown(
            &mut self,
            started: Option<tokio::sync::oneshot::Sender<()>>,
        ) -> Result<(), String> {
            let handle = self.shutdown.ok_or_else(|| "shutdown not armed".to_string())?;
            crate::process::with_case_resource(handle, |shutdown| shutdown.start(started))?
        }

        pub(super) fn shutdown_finished(&self) -> Result<bool, String> {
            let handle = self.shutdown.ok_or_else(|| "shutdown not armed".to_string())?;
            crate::process::with_case_resource(handle, |shutdown| {
                shutdown.outcome.is_some() || shutdown.handle.as_ref().is_some_and(|h| h.is_finished())
            })
        }

        pub(super) async fn join_shutdown(&mut self) -> Result<(), String> {
            let handle = self.shutdown.ok_or_else(|| "shutdown not armed".to_string())?;
            tokio::time::timeout(Duration::from_secs(45), std::future::poll_fn(|cx| {
                match crate::process::with_case_resource(handle, |shutdown| shutdown.poll_join(cx)) {
                    Ok(poll) => poll,
                    Err(error) => std::task::Poll::Ready(Err(error)),
                }
            })).await.map_err(|_| "shutdown observation timed out; handle retained".to_string())??;
            crate::process::finish_case_resource(handle)?;
            self.shutdown = None;
            Ok(())
        }

        pub fn record_server_completed(&mut self) {
            self.server_duty = 2;
        }

        pub fn record_server_not_owned(&mut self) {
            if self.server_duty == 0 {
                self.server_duty = 1;
            }
        }

        pub fn install_admitted_hook(&mut self) -> Result<(), String> {
            let admitted = ADMITTED_HOOKS
                .iter()
                .find(|row| row.entry_id == self.entry_id)
                .ok_or_else(|| "this entry has no admitted hook slot".to_string())?;
            let hooks = open_hook_parent(self.repo_fd)?;
            let installed = (|| {
                if fstatat_nofollow(hooks, admitted.slot)?.is_some() {
                    return Err("hook slot already exists; stale slot was not overwritten".to_string());
                }
                if !archive_relative_source(&self.selected_path) {
                    return Err(
                        "source_path is archive-relative, not an installed absolute path".to_string(),
                    );
                }
                if self.selected_installed_relative_path == admitted.slot
                    || !self.selected_installed_relative_path
                        .ends_with(&format!("/{}", admitted.slot))
                {
                    return Err(
                        "installed_relative_path is the catalogue path, not the hook slot".to_string(),
                    );
                }
                let target_fd = open_nofollow(Path::new(admitted.target), false)?;
                let target_st = fstat_fd(target_fd)?;
                close_fd(target_fd);
                if file_type(target_st.st_mode) != libc::S_IFREG
                    || target_st.st_nlink as u64 != 1
                    || target_st.st_uid != 0
                    || perm_bits(target_st.st_mode) != self.selected_installed_mode
                {
                    return Err(
                        "catalogue target is not a root-owned mode 0755 regular nlink-1 file"
                            .to_string(),
                    );
                }
                let (sha, length, mode, dev, ino) = catalogue_identity(admitted.target)?;
                if sha != self.selected_sha256
                    || length != self.selected_len
                    || mode != self.selected_installed_mode
                    || dev != target_st.st_dev as u64
                    || ino != target_st.st_ino as u64
                {
                    return Err(
                        "hook target bytes do not match the selected catalogue row".to_string(),
                    );
                }
                let target_c = c_name(admitted.target)?;
                let slot_c = c_name(admitted.slot)?;
                if unsafe { libc::symlinkat(target_c.as_ptr(), hooks, slot_c.as_ptr()) } < 0 {
                    return Err(last_os_error("symlinkat"));
                }
                let linked = readlink_at(hooks, admitted.slot)?;
                if linked != admitted.target {
                    return Err("hook symlink target does not match the admitted entry".to_string());
                }
                let identity_fd = open_nofollow(Path::new(admitted.target), false)?;
                let identity = fstat_fd(identity_fd)?;
                close_fd(identity_fd);
                if file_type(identity.st_mode) != libc::S_IFREG
                    || identity.st_nlink as u64 != 1
                    || identity.st_uid != 0
                    || perm_bits(identity.st_mode) != self.selected_installed_mode
                {
                    return Err(
                        "catalogue target identity is not a root-owned mode 0755 regular nlink-1 file"
                            .to_string(),
                    );
                }
                if identity.st_dev != target_st.st_dev || identity.st_ino != target_st.st_ino {
                    return Err(
                        "catalogue target changed between validation and link; link text is not object identity"
                            .to_string(),
                    );
                }
                let (sha_after, len_after, mode_after, dev_after, ino_after) =
                    catalogue_identity(admitted.target)?;
                if sha_after != sha
                    || len_after != length
                    || mode_after != mode
                    || dev_after != dev
                    || ino_after != ino
                {
                    return Err(
                        "catalogue target bytes changed between the anchored checks".to_string(),
                    );
                }
                let link_node = fstatat_nofollow(hooks, admitted.slot)?
                    .ok_or_else(|| "hook symlink node is missing after link".to_string())?;
                if file_type(link_node.st_mode) != libc::S_IFLNK {
                    return Err(
                        "hook slot text was installed but the node is not a symlink".to_string(),
                    );
                }
                Ok((
                    identity.st_dev as u64,
                    identity.st_ino as u64,
                    link_node.st_dev as u64,
                    link_node.st_ino as u64,
                ))
            })();
            close_fd(hooks);
            let (dev, ino, link_dev, link_ino) = installed?;
            self.hook_target = Some(admitted.target.to_string());
            self.hook_dev = Some(dev);
            self.hook_ino = Some(ino);
            self.hook_link_dev = Some(link_dev);
            self.hook_link_ino = Some(link_ino);
            Ok(())
        }

        pub fn recheck_hook(&self) -> Result<(), String> {
            let target = self
                .hook_target
                .as_deref()
                .ok_or_else(|| "hook was not installed".to_string())?;
            let expected_dev = self
                .hook_dev
                .ok_or_else(|| "hook object identity was not recorded".to_string())?;
            let expected_ino = self
                .hook_ino
                .ok_or_else(|| "hook object identity was not recorded".to_string())?;
            let admitted = ADMITTED_HOOKS
                .iter()
                .find(|row| row.entry_id == self.entry_id)
                .ok_or_else(|| "this entry has no admitted hook slot".to_string())?;
            let hooks = open_hook_parent_existing(self.repo_fd)?;
            let checked = (|| {
                let linked = readlink_at(hooks, admitted.slot)?;
                if linked != target || linked != admitted.target {
                    return Err("hook slot text does not name the admitted target".to_string());
                }
                let fd = open_nofollow(Path::new(admitted.target), false)?;
                let st = fstat_fd(fd)?;
                close_fd(fd);
                if file_type(st.st_mode) != libc::S_IFREG
                    || st.st_nlink as u64 != 1
                    || st.st_uid != 0
                    || perm_bits(st.st_mode) != self.selected_installed_mode
                {
                    return Err("hook target is not the admitted regular object".to_string());
                }
                let (sha, length, mode, _dev, _ino) = catalogue_identity(admitted.target)?;
                if sha != self.selected_sha256
                    || length != self.selected_len
                    || mode != self.selected_installed_mode
                {
                    return Err(
                        "hook target bytes do not match the selected catalogue row".to_string(),
                    );
                }
                if st.st_dev as u64 != expected_dev || st.st_ino as u64 != expected_ino {
                    return Err(
                        "hook link text matches but the object identity does not".to_string(),
                    );
                }
                let link_node = fstatat_nofollow(hooks, admitted.slot)?
                    .ok_or_else(|| "hook symlink node disappeared".to_string())?;
                if file_type(link_node.st_mode) != libc::S_IFLNK
                    || self.hook_link_dev != Some(link_node.st_dev as u64)
                    || self.hook_link_ino != Some(link_node.st_ino as u64)
                {
                    return Err("hook symlink node identity does not match the install".to_string());
                }
                Ok(())
            })();
            close_fd(hooks);
            checked
        }

        pub fn publish_marker(&self, name: &str, payload: &[u8], mode: u32) -> Result<(), String> {
            publish_nofollow(self.marker_fd, name, payload, mode)
        }

        pub fn release_token(&self) -> Result<Vec<u8>, String> {
            canonical_json(&[
                ("case_id", self.case_id.as_str()),
                ("entry_id", self.entry_id.as_str()),
                ("instance_id", self.instance_id.as_str()),
                ("manifest_sha256", self.manifest_sha256.as_str()),
            ])
        }

        pub fn entered_witness(&self) -> Result<Vec<u8>, String> {
            canonical_json(&[
                ("case_id", self.case_id.as_str()),
                ("data_sha256", self.data_sha256.as_str()),
                ("entry_id", self.entry_id.as_str()),
                ("instance_id", self.instance_id.as_str()),
                ("manifest_sha256", self.manifest_sha256.as_str()),
            ])
        }

        pub fn require_entered_witness(&self, marker: &str) -> Result<(), String> {
            let expected = self.entered_witness()?;
            self.require_marker(marker, &expected)?;
            let case_bytes = read_nofollow(self.config_fd, "execution-case-v1.json")?;
            let hashed = hex::encode(sha2::Sha256::digest(&case_bytes));
            if hashed != self.data_sha256 {
                return Err("case file bytes do not match the bound data_sha256".to_string());
            }
            let text = String::from_utf8(expected)
                .map_err(|_| "entered witness is not UTF-8".to_string())?;
            if !text.contains(&format!("\"instance_id\":\"{}\"", self.instance_id))
                || !text.contains(&format!("\"manifest_sha256\":\"{}\"", self.manifest_sha256))
                || !text.contains(&format!("\"data_sha256\":\"{}\"", self.data_sha256))
            {
                return Err(
                    "entered witness does not bind the nonce, H, and data bytes".to_string(),
                );
            }
            Ok(())
        }

        pub fn require_marker(&self, name: &str, expected: &[u8]) -> Result<(), String> {
            let body = read_nofollow(self.marker_fd, name)?;
            if body != expected {
                return Err(format!("marker {name} does not match the retained case"));
            }
            self.proved_markers
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(name.to_string());
            Ok(())
        }

        pub fn read_marker(&self, name: &str) -> Result<Vec<u8>, String> {
            read_nofollow(self.marker_fd, name)
        }

        pub fn push_f11_identity(&mut self) {
            self.env
                .push_utf8("REPOSYNC_EXEC_F11_IDENTITY_PROFILE", "reposync-test-v1");
            self.env
                .push_utf8("REPOSYNC_EXEC_F11_AUTH_KIND", "http-extraheader-v1");
            self.env
                .push_utf8("REPOSYNC_EXEC_F11_AUTH_SHA256", F11_AUTH_SHA256);
            self.env.push_utf8("GIT_AUTHOR_NAME", "RepoSync Test");
            self.env
                .push_utf8("GIT_AUTHOR_EMAIL", "reposync@localhost");
            self.env.push_utf8("GIT_COMMITTER_NAME", "RepoSync Test");
            self.env
                .push_utf8("GIT_COMMITTER_EMAIL", "reposync@localhost");
            let mut path = OsString::from("/opt/reliability-exec-fixtures/setup-shim");
            if let Some(previous) = std::env::var_os("PATH") {
                path.push(":");
                path.push(previous);
            }
            self.env.push_os("PATH", &path);
        }
    }

    fn open_hook_parent(repo_fd: RawFd) -> Result<RawFd, String> {
        match fstatat_nofollow(repo_fd, "hooks")? {
            None => Err("hooks directory is absent".to_string()),
            Some(st) => {
                if file_type(st.st_mode) != libc::S_IFDIR {
                    return Err("hooks node is not a directory".to_string());
                }
                let c = c_name("hooks")?;
                let fd = unsafe {
                    libc::openat(
                        repo_fd,
                        c.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                        0,
                    )
                };
                if fd < 0 {
                    return Err(last_os_error("open hooks"));
                }
                Ok(fd)
            }
        }
    }

    fn open_hook_parent_existing(repo_fd: RawFd) -> Result<RawFd, String> {
        open_hook_parent(repo_fd)
    }

    fn readlink_at(dir_fd: RawFd, name: &str) -> Result<String, String> {
        let c = c_name(name)?;
        let mut buffer = vec![0u8; 4096];
        let got = unsafe {
            libc::readlinkat(
                dir_fd,
                c.as_ptr(),
                buffer.as_mut_ptr() as *mut libc::c_char,
                buffer.len(),
            )
        };
        if got < 0 {
            return Err(last_os_error("readlinkat"));
        }
        if got as usize == buffer.len() {
            return Err("readlinkat result was truncated".to_string());
        }
        buffer.truncate(got as usize);
        String::from_utf8(buffer).map_err(|_| "symlink target is not UTF-8".to_string())
    }

    fn actual_case_root_id(case_root: &Path) -> Result<String, String> {
        let text = case_root
            .to_str()
            .ok_or_else(|| "case root is not UTF-8".to_string())?;
        if !text.starts_with('/') || text.contains("//") || text.ends_with('/') || text.contains("/./") || text.contains("/../") {
            return Err(format!("case root {text} is not a canonical absolute path"));
        }
        const PARENT: &str = "/fixture/tmp/";
        if !text.starts_with(PARENT) {
            return Err(format!(
                "case root {text} is not an immediate child of /fixture/tmp"
            ));
        }
        let id = &text[PARENT.len()..];
        if id.is_empty()
            || id.contains('/')
            || id == "."
            || id == ".."
            || id.len() > 96
            || !id.is_ascii()
            || !is_component(id)
        {
            return Err(
                "case_root_id must be one ASCII basename under /fixture/tmp, at most 96 bytes"
                    .to_string(),
            );
        }
        if let Some(pinned) = std::env::var_os("REPOSYNC_EXEC_EXPECTED_ROOT_ID") {
            let pinned = pinned
                .to_str()
                .ok_or_else(|| "REPOSYNC_EXEC_EXPECTED_ROOT_ID is not UTF-8".to_string())?;
            if pinned != id {
                return Err(
                    "REPOSYNC_EXEC_EXPECTED_ROOT_ID does not match the actual case directory basename"
                        .to_string(),
                );
            }
        }
        Ok(id.to_string())
    }

    fn pinned_instance_nonce(case_root_id: &str) -> Result<String, String> {
        if std::env::var_os("REPOSYNC_EXEC_CASE_DRAW").is_some() {
            return Err(
                "REPOSYNC_EXEC_CASE_DRAW is not adopted as a nonce or case root".to_string(),
            );
        }
        let _allows_equality_with_case_root = case_root_id;
        let mut bytes = [0u8; 16];
        let mut source = std::fs::File::open("/dev/urandom")
            .map_err(|error| format!("core nonce source is unavailable: {error}"))?;
        std::io::Read::read_exact(&mut source, &mut bytes)
            .map_err(|error| format!("core nonce read failed: {error}"))?;
        let nonce: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        if let Some(raw) = std::env::var_os("REPOSYNC_EXEC_EXPECTED_INSTANCE") {
            let text = raw
                .to_str()
                .ok_or_else(|| "instance nonce pin is not UTF-8".to_string())?;
            if text != nonce {
                return Err(
                    "REPOSYNC_EXEC_EXPECTED_INSTANCE does not match the core-owned nonce"
                        .to_string(),
                );
            }
        }
        Ok(nonce)
    }

    fn repository_stays_in_case(case_root: &Path, repository: &Path) -> Result<(), String> {
        let case_text = case_root
            .to_str()
            .ok_or_else(|| "case root is not UTF-8".to_string())?;
        let repo_text = repository
            .to_str()
            .ok_or_else(|| "repository path is not UTF-8".to_string())?;
        if repo_text == case_text {
            return Ok(());
        }
        let prefix = format!("{case_text}/");
        if !repo_text.starts_with(&prefix) {
            return Err("repository is outside the case root".to_string());
        }
        let rest = &repo_text[prefix.len()..];
        if !is_component(rest) {
            return Err("repository must be the case root or one basename inside it".to_string());
        }
        Ok(())
    }

    fn catalogue_identity(path: &str) -> Result<(String, u64, u32, u64, u64), String> {
        let fd = open_nofollow(Path::new(path), false)?;
        let read = (|| {
            let st = fstat_fd(fd)?;
            if file_type(st.st_mode) != libc::S_IFREG || st.st_size < 0 {
                return Err("catalogue target is not a regular file".to_string());
            }
            let mut body = vec![0u8; st.st_size as usize];
            read_exact(fd, &mut body)?;
            Ok((
                hex::encode(sha2::Sha256::digest(&body)),
                body.len() as u64,
                perm_bits(st.st_mode),
                st.st_dev as u64,
                st.st_ino as u64,
            ))
        })();
        close_fd(fd);
        read
    }

    pub struct SelectedEntry {
        pub entry_id: String,
        pub source_path: String,
        pub sha256: String,
        pub bytes: u64,
        pub source_mode: u32,
        pub installed_relative_path: String,
        pub installed_mode: u32,
    }

    fn archive_relative_source(path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        !path.is_empty()
            && !path.starts_with('/')
            && parts.len() >= 2
            && parts
                .iter()
                .all(|part| !part.is_empty() && *part != "." && *part != "..")
    }

    pub fn require_selected_entry(entry: Option<&SelectedEntry>) -> Result<(), String> {
        let Some(row) = entry else {
            return Err(
                "validated entry is required; validated catalogue metadata is missing; an explicit mode is required"
                    .to_string(),
            );
        };
        let mut problems = Vec::new();
        if row.entry_id.is_empty() {
            problems.push("validated entry is required");
        }
        let digest = row.sha256.len() == 64
            && row
                .sha256
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if !digest || row.bytes == 0 {
            problems.push("validated catalogue metadata is missing");
        }
        if !archive_relative_source(&row.source_path) {
            problems.push("source_path is archive-relative");
        }
        if !archive_relative_source(&row.installed_relative_path) {
            problems.push("installed_relative_path is not the hook slot");
        }
        if row.source_mode == 0 || row.installed_mode == 0 {
            problems.push("an explicit mode is required");
        } else {
            if row.source_mode != 0o100755 {
                problems.push("source_mode is git 100755");
            }
            if row.installed_mode != 0o755 {
                problems.push("installed_mode is 0755");
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems.join("; "))
        }
    }

    pub struct CatalogueAttributes<'a> {
        pub source_path: &'a str,
        pub installed_relative_path: &'a str,
        pub sha256: &'a str,
        pub bytes: u64,
        pub source_mode: u32,
        pub installed_mode: u32,
    }

    pub fn select_attested_entry(
        entry_id: &str,
        attributes: Option<CatalogueAttributes<'_>>,
    ) -> Result<SelectedEntry, String> {
        let Some(row) = attributes else {
            return Err("validated catalogue metadata is missing".to_string());
        };
        let selected = SelectedEntry {
            entry_id: entry_id.to_string(),
            source_path: row.source_path.to_string(),
            sha256: row.sha256.to_string(),
            bytes: row.bytes,
            source_mode: row.source_mode,
            installed_relative_path: row.installed_relative_path.to_string(),
            installed_mode: row.installed_mode,
        };
        require_selected_entry(Some(&selected))?;
        Ok(selected)
    }

    pub fn attested_entry_from_validated_metadata(entry_id: &str) -> Result<SelectedEntry, String> {
        let _ = entry_id;
        Err(format!(
            "derived runtime projection is unbound for {entry_id}; harness owner 01a1434e-b4d8-7583-969f-75ad62ef10ce catalogue reposync-immutable-exec-v2.1 reconciled v2.3.2 has no acknowledgment; a process-wide entry id is not provenance"
        ))
    }

    pub fn prepare_existing_repository(
        boundary: &ProcessBoundary,
        manifest: &HarnessValidatedH,
        selected: &SelectedEntry,
        entry_id: &str,
        case_id: &str,
        case_root: &Path,
        repository: Option<&Path>,
        markers_on_case: bool,
        initial_oid: Option<&str>,
    ) -> Result<PreparedRepo, String> {
        boundary.assert_held();
        if !crate::process::case_registration_open(boundary.context()) {
            return Err("case registration closed before preparation".into());
        }
        if crate::process::retained_ownership_incomplete() {
            return Err("retained process ownership is unresolved".to_string());
        }
        require_selected_entry(Some(selected))?;
        if selected.entry_id != entry_id {
            return Err("selected entry does not match the requested catalogue row".to_string());
        }
        if !archive_relative_source(&selected.source_path) {
            return Err(
                "source_path is archive-relative, not an installed absolute path".to_string(),
            );
        }
        if let Some(admitted) = ADMITTED_HOOKS.iter().find(|row| row.entry_id == entry_id) {
            if selected.installed_relative_path == admitted.slot
                || !selected.installed_relative_path.ends_with(&format!("/{}", admitted.slot))
            {
                return Err(
                    "installed_relative_path is the catalogue path, not the hook slot".to_string(),
                );
            }
            let (anchored_sha, anchored_len, anchored_mode, _dev, _ino) =
                catalogue_identity(admitted.target)?;
            if anchored_sha != selected.sha256
                || anchored_len != selected.bytes
                || anchored_mode != selected.installed_mode
            {
                return Err(
                    "anchored installed identity does not match the selected row".to_string(),
                );
            }
        } else if entry_id == "setup-push-shim-v1" {
            return Err(
                "derived runtime projection is unbound for setup-push-shim-v1; harness owner 01a1434e-b4d8-7583-969f-75ad62ef10ce has not acknowledged an H-row binding"
                    .to_string(),
            );
        } else {
            return Err("this entry has no admitted installed target".to_string());
        }
        regression_source_obligations(case_id)?;
        let case_root_id = actual_case_root_id(case_root)?;
        let instance = pinned_instance_nonce(&case_root_id)?;
        if let Some(repository) = repository {
            repository_stays_in_case(case_root, repository)?;
        }
        let retained_oid = if entry_id == "git-ref-move-v1" {
            let oid = initial_oid.ok_or_else(|| "git-ref-move-v1 requires initial_oid".to_string())?;
            validate_initial_oid(oid)?;
            Some(oid.to_string())
        } else if initial_oid.is_some() {
            return Err("this entry schema has no initial_oid".to_string());
        } else {
            None
        };
        let mut fields = vec![
            ("case_id", case_id),
            ("case_root_id", case_root_id.as_str()),
            ("entry_id", entry_id),
            ("instance_id", instance.as_str()),
        ];
        if let Some(oid) = retained_oid.as_deref() {
            fields.push(("initial_oid", oid));
        }
        let case_bytes = canonical_json(&fields)?;
        let data_sha256 = hex::encode(sha2::Sha256::digest(&case_bytes));
        let case_fd = open_nofollow(case_root, true)?;
        if let Err(error) = require_case_root_directory(case_fd) {
            close_fd(case_fd);
            return Err(error);
        }
        let repo_fd = if let Some(repository) = repository {
            match open_nofollow(repository, true) {
                Ok(fd) => fd,
                Err(error) => {
                    close_fd(case_fd);
                    return Err(error);
                }
            }
        } else {
            case_fd
        };
        if repo_fd != case_fd {
            if let Err(error) = require_repository_directory(repo_fd) {
                close_fd(repo_fd);
                close_fd(case_fd);
                return Err(error);
            }
        }
        let config_fd = repo_fd;
        if let Err(error) = publish_nofollow(config_fd, "execution-case-v1.json", &case_bytes, 0o600) {
            if repo_fd != case_fd {
                close_fd(repo_fd);
            }
            close_fd(case_fd);
            return Err(error);
        }
        let marker_fd = if markers_on_case { case_fd } else { repo_fd };
        let marker_path = if markers_on_case {
            case_root.to_path_buf()
        } else {
            repository
                .unwrap_or(case_root)
                .to_path_buf()
        };
        let mut prepared = PreparedRepo {
            case_fd,
            repo_fd,
            config_fd,
            marker_fd,
            env: EnvGuard::new(),
            selected_path: selected.source_path.clone(),
            selected_sha256: selected.sha256.clone(),
            selected_len: selected.bytes,
            selected_mode: selected.source_mode,
            case_root_id: case_root_id.clone(),
            explicitly_completed: false,
            owner: boundary.owner(),
            context: boundary.context(),
            shutdown: None,
            publication_joined: false,
            blocking_joined: false,
            server_duty: 0,
            proved_markers: std::sync::Mutex::new(Vec::new()),
            selected_installed_relative_path: selected.installed_relative_path.clone(),
            selected_installed_mode: selected.installed_mode,
            entry_id: entry_id.to_string(),
            case_id: case_id.to_string(),
            instance_id: instance.clone(),
            manifest_sha256: manifest.as_str().to_string(),
            data_sha256,
            initial_oid: retained_oid,
            marker_path,
            hook_target: None,
            hook_dev: None,
            hook_ino: None,
            hook_link_dev: None,
            hook_link_ino: None,
        };
        prepared.env.push_utf8("REPOSYNC_FIXTURE_EXEC_MODE", "packaged-immutable-v1");
        prepared.env.push_utf8("REPOSYNC_EXEC_EXPECTED_ENTRY", entry_id);
        prepared.env.push_utf8("REPOSYNC_EXEC_EXPECTED_CASE", case_id);
        prepared
            .env
            .push_utf8("REPOSYNC_EXEC_EXPECTED_INSTANCE", &instance);
        prepared
            .env
            .push_utf8("REPOSYNC_EXEC_EXPECTED_ROOT_ID", &case_root_id);
        prepared
            .env
            .push_utf8("REPOSYNC_EXEC_EXPECTED_DATA_SHA256", &prepared.data_sha256);
        prepared
            .env
            .push_utf8("REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256", manifest.as_str());
        Ok(prepared)
    }

    pub fn observe_ref_move_after_join(
        prepared: &PreparedRepo,
        receiver_status: ReceiverStatus,
    ) -> Result<RefMoveObservation, String> {
        let audit = match prepared.read_marker("action-audit-witness") {
            Ok(bytes) => bytes,
            Err(error) => {
                return Err(format!(
                    "action-audit-witness absent after join; publication_rejected={} observed_exit={:?} is not qualification and another update-ref is forbidden: {error}",
                    receiver_status.publication_rejected, receiver_status.observed_exit
                ));
            }
        };
        let parsed = parse_flat_object(&audit).map_err(|error| {
            format!("action-audit-witness refused; another update-ref is forbidden: {error}")
        })?;
        let required = [
            "action",
            "after_oid",
            "before_oid",
            "case_id",
            "child_exit",
            "data_sha256",
            "entry_id",
            "initial_oid",
            "instance_id",
            "manifest_sha256",
            "schema",
        ];
        if parsed.len() != required.len() || required.iter().any(|key| !parsed.contains_key(*key)) {
            return Err("action-audit-witness keys are not the sealed set".to_string());
        }
        let string_field = |key: &str| -> Result<String, String> {
            let encoded = parsed.get(key).ok_or_else(|| format!("missing {key}"))?;
            encoded
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .map(str::to_string)
                .ok_or_else(|| format!("{key} is not a string"))
        };
        if string_field("action")? != "update-ref" {
            return Err("audit action is not update-ref".to_string());
        }
        let intended = prepared
            .initial_oid
            .clone()
            .ok_or_else(|| "retained case has no initial_oid".to_string())?;
        if string_field("initial_oid")? != intended {
            return Err("audit initial_oid does not equal the retained value".to_string());
        }
        if string_field("case_id")? != prepared.case_id
            || string_field("entry_id")? != prepared.entry_id
            || string_field("instance_id")? != prepared.instance_id
            || string_field("manifest_sha256")? != prepared.manifest_sha256
            || string_field("data_sha256")? != prepared.data_sha256
        {
            return Err("audit identity does not match the retained case".to_string());
        }
        if parsed.get("schema").map(String::as_str) != Some("1") {
            return Err("audit schema is not canonical integer 1".to_string());
        }
        let child_exit = parsed
            .get("child_exit")
            .ok_or_else(|| "child_exit missing".to_string())?
            .parse::<i64>()
            .map_err(|_| "child_exit is not an integer".to_string())?;
        if !(0..=255).contains(&child_exit) {
            return Err("child_exit is outside the sealed range".to_string());
        }
        Ok(RefMoveObservation {
            audit_child_exit: AuditChildExit { value: child_exit },
            receiver_status,
            another_update_ref_permitted: false,
        })
    }


    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ExplicitMode {
        NativeGenerated,
        PackagedImmutable,
    }

    pub fn allocate_case_nonce() -> Result<String, String> {
        let mut bytes = [0u8; 16];
        let mut source = std::fs::File::open("/dev/urandom")
            .map_err(|error| format!("core nonce source is unavailable: {error}"))?;
        std::io::Read::read_exact(&mut source, &mut bytes)
            .map_err(|error| format!("core nonce read failed: {error}"))?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    pub fn admit_child_environment(
        command: &std::process::Command,
        mode: ExplicitMode,
    ) -> Result<(), String> {
        let value = |key: &str| -> Option<std::ffi::OsString> {
            match command.get_envs().find(|(name, _)| *name == key) {
                Some((_, item)) => item.map(std::ffi::OsString::from),
                None => std::env::var_os(key),
            }
        };
        let malformed = |raw: &std::ffi::OsStr| -> bool {
            let Some(text) = raw.to_str() else {
                return true;
            };
            text.len() != 64 || !text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        };
        let packaged_pins = [
            "REPOSYNC_EXEC_EXPECTED_ENTRY",
            "REPOSYNC_EXEC_EXPECTED_CASE",
            "REPOSYNC_EXEC_EXPECTED_INSTANCE",
            "REPOSYNC_EXEC_EXPECTED_ROOT_ID",
            "REPOSYNC_EXEC_EXPECTED_DATA_SHA256",
        ];
        match mode {
            ExplicitMode::NativeGenerated => {
                if packaged_pins.iter().any(|key| value(key).is_some())
                    || value("REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256").is_some()
                {
                    return Err(
                        "native admission rejected a packaged marker before env cleanup".to_string(),
                    );
                }
                if let Some(raw) = value("REPOSYNC_FIXTURE_EXEC_MODE") {
                    if raw.to_str() != Some("native-generated-v1") {
                        return Err(
                            "native admission rejected a packaged marker before env cleanup"
                                .to_string(),
                        );
                    }
                }
                Ok(())
            }
            ExplicitMode::PackagedImmutable => match value("REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256") {
                None => Err("absent H".to_string()),
                Some(raw) if malformed(&raw) => Err("malformed H".to_string()),
                Some(_) => Ok(()),
            },
        }
    }


    pub struct SpawnContext {
        session: Option<crate::process::CaseContextId>,
        packaged: bool,
        entry_id: String,
        case_id: String,
        instance_id: String,
        case_root_id: String,
        data_sha256: String,
        manifest_sha256: String,
    }

    impl SpawnContext {
        pub(super) fn session(&self) -> Option<crate::process::CaseContextId> { self.session }
    }

    pub fn native_spawn_context() -> SpawnContext {
        SpawnContext {
            session: None,
            packaged: false,
            entry_id: String::new(),
            case_id: String::new(),
            instance_id: String::new(),
            case_root_id: String::new(),
            data_sha256: String::new(),
            manifest_sha256: String::new(),
        }
    }

    pub fn packaged_spawn_context(prepared: &PreparedRepo) -> SpawnContext {
        SpawnContext {
            session: Some(prepared.context),
            packaged: true,
            entry_id: prepared.entry_id.clone(),
            case_id: prepared.case_id.clone(),
            instance_id: prepared.instance_id.clone(),
            case_root_id: prepared.case_root_id.clone(),
            data_sha256: prepared.data_sha256.clone(),
            manifest_sha256: prepared.manifest_sha256.clone(),
        }
    }

    pub fn apply_spawn_context(
        command: &mut std::process::Command,
        context: &SpawnContext,
    ) -> Result<(), String> {
        if !context.packaged {
            return admit_child_environment(command, ExplicitMode::NativeGenerated);
        }
        command.env("REPOSYNC_FIXTURE_EXEC_MODE", "packaged-immutable-v1");
        command.env("REPOSYNC_EXEC_EXPECTED_ENTRY", &context.entry_id);
        command.env("REPOSYNC_EXEC_EXPECTED_CASE", &context.case_id);
        command.env("REPOSYNC_EXEC_EXPECTED_INSTANCE", &context.instance_id);
        command.env("REPOSYNC_EXEC_EXPECTED_ROOT_ID", &context.case_root_id);
        command.env("REPOSYNC_EXEC_EXPECTED_DATA_SHA256", &context.data_sha256);
        command.env(
            "REPOSYNC_EXEC_EXPECTED_MANIFEST_SHA256",
            &context.manifest_sha256,
        );
        admit_child_environment(command, ExplicitMode::PackagedImmutable)
    }

    pub struct CaseSession {
        mode: ExplicitMode,
        nonce: Option<String>,
        spawned: bool,
        mutated: bool,
        cleanup_unconfirmed: bool,
        guard_held: bool,
    }

    impl CaseSession {
        pub fn new(mode: ExplicitMode) -> Self {
            Self {
                mode,
                nonce: None,
                spawned: false,
                mutated: false,
                cleanup_unconfirmed: false,
                guard_held: false,
            }
        }

        pub fn begin(&mut self, entry: Option<&SelectedEntry>) -> Result<(), String> {
            if self.cleanup_unconfirmed || crate::process::retained_ownership_incomplete() {
                self.guard_held = true;
                return Err("retained ownership is unresolved".to_string());
            }
            if self.mode == ExplicitMode::PackagedImmutable {
                require_selected_entry(entry)?;
            }
            self.nonce = Some(allocate_case_nonce()?);
            Ok(())
        }

        pub fn spawned(&self) -> bool { self.spawned }
        pub fn mutated(&self) -> bool { self.mutated }
        pub fn nonce(&self) -> Option<&str> { self.nonce.as_deref() }
        pub fn guard_held(&self) -> bool { self.guard_held }
        pub fn mark_cleanup_unconfirmed(&mut self) {
            self.cleanup_unconfirmed = true;
            self.guard_held = true;
        }
    }

    pub struct NofollowObservation {
        pub bytes: Vec<u8>,
        pub sha256: String,
        pub link_dev: u64,
        pub link_ino: u64,
        pub target_dev: u64,
        pub target_ino: u64,
    }

    pub fn sha256_hex(bytes: &[u8]) -> String {
        hex::encode(sha2::Sha256::digest(bytes))
    }

    pub fn observe_nofollow(link: &std::path::Path) -> Result<NofollowObservation, String> {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;
        let parent = link.parent().ok_or_else(|| "link has no parent".to_string())?;
        let name = link.file_name().ok_or_else(|| "link has no name".to_string())?;
        let parent_c = std::ffi::CString::new(parent.as_os_str().as_bytes())
            .map_err(|_| "parent path is not a C string".to_string())?;
        let name_c = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| "link name is not a C string".to_string())?;
        let parent_fd = unsafe {
            libc::open(
                parent_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if parent_fd < 0 {
            return Err("anchored parent open failed".to_string());
        }
        let mut link_st: libc::stat = unsafe { std::mem::zeroed() };
        let linked = unsafe {
            libc::fstatat(parent_fd, name_c.as_ptr(), &mut link_st, libc::AT_SYMLINK_NOFOLLOW)
        };
        unsafe { libc::close(parent_fd) };
        if linked != 0 {
            return Err("symlink node stat failed".to_string());
        }
        let bytes = std::fs::read(link).map_err(|error| error.to_string())?;
        let target = std::fs::metadata(link).map_err(|error| error.to_string())?;
        Ok(NofollowObservation {
            sha256: sha256_hex(&bytes),
            bytes,
            link_dev: link_st.st_dev as u64,
            link_ino: link_st.st_ino as u64,
            target_dev: target.dev(),
            target_ino: target.ino(),
        })
    }

    pub fn recheck_nofollow(link: &std::path::Path, before: &NofollowObservation) -> Result<(), String> {
        let after = observe_nofollow(link)?;
        if after.bytes != before.bytes
            || after.sha256 != before.sha256
            || after.link_dev != before.link_dev
            || after.link_ino != before.link_ino
            || after.target_dev != before.target_dev
            || after.target_ino != before.target_ino
        {
            return Err("descriptor or symlink substitution".to_string());
        }
        Ok(())
    }

    #[derive(Clone, Copy)]
    pub enum CaseId { GitFinalBarrier, SvnDisconnect, SvnShutdown }

    pub fn ready_bytes(case_id: CaseId) -> &'static [u8] {
        match case_id {
            CaseId::GitFinalBarrier => b"",
            CaseId::SvnDisconnect | CaseId::SvnShutdown => b"ready",
        }
    }

    pub fn require_exact(actual: &[u8], expected: &[u8]) -> Result<(), String> {
        if actual == expected { Ok(()) } else { Err("marker bytes differ".to_string()) }
    }

    pub fn push_control_witness_bytes(instance: &str, manifest: &str, data: &str) -> Vec<u8> {
        let text = format!(
            "{{\"argv\":[\"push\",\"--force\",\"origin\",\"main\"],\"case_id\":\"F11\",\"data_sha256\":\"{data}\",\"entry_id\":\"setup-push-shim-v1\",\"instance_id\":\"{instance}\",\"manifest_sha256\":\"{manifest}\",\"schema\":1,\"suppressed\":true}}\n"
        );
        text.into_bytes()
    }

#[cfg(test)]
mod exact_witness_bytes {
    use super::{canonical_json, push_control_witness_bytes, ready_bytes, require_exact, CaseId};

    #[test]
    fn string_witness_uses_the_existing_integer_schema_and_newline() {
        let bytes = canonical_json(&[
            ("case_id", "F09"),
            ("data_sha256", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            ("entry_id", "svn-disconnect-barrier-v1"),
            ("instance_id", "0123456789abcdef0123456789abcdef"),
            ("manifest_sha256", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ])
        .unwrap();
        assert!(bytes.ends_with(b"\n"));
        assert!(bytes.windows(10).any(|window| window == b"\"schema\":1"));
        assert!(!bytes.windows(12).any(|window| window == b"\"schema\":\"1\""));
        assert_eq!(
            bytes,
            br#"{"case_id":"F09","data_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","entry_id":"svn-disconnect-barrier-v1","instance_id":"0123456789abcdef0123456789abcdef","manifest_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","schema":1}
"#
        );
    }

    #[test]
    fn push_control_witness_matches_the_catalogue_object() {
        let bytes = push_control_witness_bytes(
            "0123456789abcdef0123456789abcdef",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );
        assert_eq!(
            bytes,
            br#"{"argv":["push","--force","origin","main"],"case_id":"F11","data_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","entry_id":"setup-push-shim-v1","instance_id":"0123456789abcdef0123456789abcdef","manifest_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","schema":1,"suppressed":true}
"#
        );
    }

    #[test]
    fn ready_bodies_stay_exact() {
        assert_eq!(ready_bytes(CaseId::GitFinalBarrier).as_slice(), b"");
        assert_eq!(ready_bytes(CaseId::SvnDisconnect).as_slice(), b"ready");
        assert_eq!(ready_bytes(CaseId::SvnShutdown).as_slice(), b"ready");
        assert!(require_exact(b"ready\n", b"ready").is_err());
        assert!(require_exact(b"", b"").is_ok());
    }
}

#[cfg(test)]
mod case_session_owner {
    use super::{
        attest_catalogue, attested_entry_from_validated_metadata, CaseSession, CatalogueAttestation,
        CatalogueAttributes, EnvGuard, ExplicitMode, enter_process_boundary, observe_nofollow,
        recheck_nofollow, release_retained_session, select_attested_entry, sha256_hex,
        duties_allow_release, RetainedSession, RETAINED_SESSIONS,
    };
    use std::time::Duration;

    #[test]
    fn missing_s_refuses_packaged_preparation_only() {
        let mut packaged = CaseSession::new(ExplicitMode::PackagedImmutable);
        let error = packaged.begin(None).unwrap_err();
        assert!(error.contains("validated entry"));
        assert!(!packaged.spawned());
        assert!(!packaged.mutated());

        let mut native = CaseSession::new(ExplicitMode::NativeGenerated);
        native.begin(None).unwrap();
        assert!(!native.spawned());
        assert!(!native.mutated());
        let nonce = native.nonce().expect("native begin binds a core nonce without S");
        assert_eq!(nonce.len(), 32);
        assert!(nonce.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
        let missing = select_attested_entry("git-reject-v1", None).unwrap_err();
        assert!(missing.contains("validated catalogue metadata is missing"));
        let implicit = select_attested_entry(
            "git-reject-v1",
            Some(CatalogueAttributes {
                source_path: "/opt/reliability-exec-fixtures/git-reject/pre-receive",
                installed_relative_path: "pre-receive",
                sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                bytes: 1,
                source_mode: 0,
                installed_mode: 0o755,
            }),
        )
        .unwrap_err();
        assert!(implicit.contains("explicit mode"));
        assert!(implicit.contains("archive-relative"));
        assert!(implicit.contains("installed_relative_path is not the hook slot"));
        let unbound = attested_entry_from_validated_metadata("git-reject-v1").unwrap_err();
        assert!(unbound.contains("01a1434e-b4d8-7583-969f-75ad62ef10ce"));
        assert!(unbound.contains("reposync-immutable-exec-v2.1"));
        assert!(unbound.contains("process-wide entry id is not provenance"));
        let other = attested_entry_from_validated_metadata("git-final-barrier-v1").unwrap_err();
        assert!(other.contains("git-final-barrier-v1"));
        let mut mode = EnvGuard::new();
        mode.push_utf8("REPOSYNC_FIXTURE_EXEC_MODE", "native-generated-v1");
        let native_mode = attest_catalogue().unwrap();
        assert!(matches!(
            native_mode,
            CatalogueAttestation::HistoricalNativeNotCatalogueProof
        ));
        drop(mode);
        let absent = attest_catalogue().unwrap_err();
        assert!(absent.contains("absent mode is not native"));
        let packaged_mode = {
            let mut packaged = EnvGuard::new();
            packaged.push_utf8("REPOSYNC_FIXTURE_EXEC_MODE", "packaged-immutable-v1");
            let missing_h = attest_catalogue().unwrap_err();
            assert!(missing_h.contains("absent H"));
            missing_h
        };
        assert!(packaged_mode.contains("absent H"));
    }

    #[test]
    fn seal_requires_this_sessions_joined_resources() {
        assert!(duties_allow_release(false, true, 2).is_err());
        assert!(duties_allow_release(true, true, 0).is_err());
        let boundary = enter_process_boundary().unwrap();
        let owner = boundary.owner();
        let context = boundary.context();
        // Real retained descriptors, even with all legacy flags set, prevent
        // restoration. The typed registry, not a handwritten result vector,
        // owns them while the local observer is gone.
        let resource = crate::process::register_case_resource(context, || {
            std::os::unix::net::UnixStream::pair().map_err(|error| error.to_string())
        }).unwrap();
        crate::process::retain_case_context(context);
        RETAINED_SESSIONS.lock().unwrap_or_else(|poison| poison.into_inner())
            .push(RetainedSession { owner, context, env: EnvGuard::new(), fds: Vec::new(),
                publication_joined: true, blocking_joined: true, server_duty: 2 });
        drop(boundary);
        assert!(enter_process_boundary().is_err());
        assert!(release_retained_session(owner).is_err());
        crate::process::finish_case_resource(resource).unwrap();
        crate::process::close_case_registration(context).unwrap();
        release_retained_session(owner).unwrap();
        let released = enter_process_boundary().unwrap();
        drop(released);
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_recheck_rejects_substituted_bytes_and_link_nodes() {
        // The digest under test is the descriptor read. A ValidatedEntry literal
        // compared with a handwritten digest would not prove the row came from S.
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("hook");
        std::fs::write(&target, b"original-bytes").unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let before = observe_nofollow(&link).unwrap();
        assert_eq!(before.bytes.as_slice(), b"original-bytes");
        assert_eq!(before.sha256, sha256_hex(&before.bytes));
        assert!(recheck_nofollow(&link, &before).is_ok());

        std::fs::write(&target, b"replaced-bytes").unwrap();
        assert!(recheck_nofollow(&link, &before).is_err());
        std::fs::write(&target, b"original-bytes").unwrap();
        assert!(recheck_nofollow(&link, &before).is_ok());

        std::fs::remove_file(&link).unwrap();
        let replacement = temp.path().join("other");
        std::fs::write(&replacement, b"original-bytes").unwrap();
        std::os::unix::fs::symlink(&replacement, &link).unwrap();
        let after = observe_nofollow(&link).unwrap();
        assert_ne!(before.link_ino, after.link_ino);
        assert!(recheck_nofollow(&link, &before).is_err());
        let supplied = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let illegal = select_attested_entry(
            "git-reject-v1",
            Some(CatalogueAttributes {
                source_path: target.to_str().unwrap(),
                installed_relative_path: "pre-receive",
                sha256: supplied,
                bytes: before.bytes.len() as u64,
                source_mode: 0o644,
                installed_mode: 0o755,
            }),
        )
        .unwrap_err();
        assert!(illegal.contains("archive-relative"));
        assert!(illegal.contains("source_mode is git 100755"));
        let selected = select_attested_entry(
            "git-reject-v1",
            Some(CatalogueAttributes {
                source_path: "crates/core/tests/fixtures/exec-v1/git-reject/pre-receive",
                installed_relative_path: "git-reject/pre-receive",
                sha256: supplied,
                bytes: 1,
                source_mode: 0o100755,
                installed_mode: 0o755,
            }),
        )
        .unwrap();
        assert_eq!(selected.sha256, supplied);
        assert_ne!(selected.sha256, before.sha256);
        assert_eq!(selected.source_mode, 0o100755);
        assert_ne!(selected.installed_relative_path, "pre-receive");
    }

    #[test]
    fn failed_cleanup_retains_the_guard_and_fails_the_next_begin() {
        const CHILD: &str = "REPOSYNC_LIFECYCLE_LATCH_CHILD";
        const NAME: &str = "pair_refresh::live_preparation::authenticated_enrollment_tests::packaged_call_site::failed_cleanup_retains_the_guard_and_fails_the_next_begin";
        if std::env::var(CHILD).ok().as_deref() != Some("1") {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture"])
                .env(CHILD, "1").env_remove("REPOSYNC_LAUNCH_BINDING_V1").output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(output.status.success(), "child failed: {}", String::from_utf8_lossy(&output.stderr));
            assert!(stdout.contains(NAME) && stdout.contains("1 passed; 0 failed"),
                "the exact latch regression did not execute: {stdout}");
            return;
        }
        crate::process::note_unresolved_context();
        let mut session = CaseSession::new(ExplicitMode::NativeGenerated);
        assert!(crate::process::retained_ownership_incomplete());
        assert!(session.begin(None).is_err());
        assert!(session.guard_held());
        assert!(session.nonce().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_process_observer_blocks_the_next_session() {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("30");
        let observer = tokio::spawn(crate::process::run(
            command,
            Duration::from_secs(30),
            None,
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if crate::process::retained_child_and_readers_held() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child, wait, readers, and task handle were retained before return");
        observer.abort();
        let _ = observer.await;
        assert!(crate::process::retained_child_and_readers_held());
        assert!(crate::process::retained_ownership_incomplete());
        let mut second = CaseSession::new(ExplicitMode::NativeGenerated);
        assert!(second.begin(None).is_err());
        crate::process::observe_retained_until_quiescent(Duration::from_secs(5))
            .await
            .expect("verified completion releases the retained child");
        assert!(!crate::process::retained_ownership_incomplete());
        second.begin(None).unwrap();
        assert!(second.nonce().is_some());
        assert!(!second.spawned());
    }
}

}

#[cfg(unix)]
async fn packaged_f06_protected_ref(
    manifest: packaged_call_site::HarnessValidatedH,
    boundary: &packaged_call_site::ProcessBoundary,
) -> Result<(), String> {
    use crate::db::pair_refresh_operations::PairRefreshPhase;
    boundary.assert_held();
    let selected = packaged_call_site::attested_entry_from_validated_metadata("git-reject-v1")?;
    let session = boundary.context();
    let blocking = super::super::execution::BlockingAssoc::Case(session);
    let _serial = SERIAL.lock().await;
    let mut prepared_slot: Option<packaged_call_site::PreparedRepo> = None;
    let mut f = Fixture::open(Some(&mut |case_root, repository| {
        let mut prepared = packaged_call_site::prepare_existing_repository(
            boundary,
            &manifest,
            &selected,
            "git-reject-v1",
            "F06",
            case_root,
            Some(repository),
            false,
            None,
        )?;
        prepared.install_admitted_hook()?;
        prepared.recheck_hook()?;
        let context = packaged_call_site::packaged_spawn_context(&prepared);
        prepared_slot = Some(prepared);
        Ok(context)
    }))?;
    let mut prepared_repo =
        prepared_slot.ok_or_else(|| "prepared session missing before launch".to_string())?;
    let engine = f.engine();
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let root = std::sync::Arc::clone(&f.temp);
    let native_leases = crate::process::register_case_resource(session, || {
        Ok(CaseNativeLeases { _lock, _fence, _root: root })
    })?;
    let (actor, _workspace, prepared_job) = prepared(&f, &engine).await;
    let approved = actor.prepared_approval_digest(&engine, &prepared_job).unwrap();
    let busy = crate::busy::try_acquire_pair("child", "parent").unwrap();
    let parent_before = parent_row(&f);
    let hook = f.target.path().join("hooks/pre-receive");
    let before = f
        .target
        .find_reference("refs/heads/feature")
        .unwrap()
        .target();
    let failure = packaged_call_site::CleanupState::new();
    let guard = packaged_call_site::UnjoinedGuard::arm(&failure);
    let result = super::super::execution::complete_owned_refresh_with_client_assoc(
        &engine,
        &actor,
        &prepared_job,
        "protected-child",
        &approved,
        &busy,
        f.client.clone(),
        blocking,
    )
    .await;
    let proof = guard.mark_joined()?;
    packaged_call_site::qualify(proof, &failure)?;
    prepared_repo.record_publication_joined();
    let after = f
        .target
        .find_reference("refs/heads/feature")
        .ok()
        .and_then(|reference| reference.target());
    let summary = match &result {
        Ok(_) => "Ok".to_string(),
        Err(error) => crate::errors::redact_vcs_error_detail(&format!("{error:#}")),
    };
    assert!(
        result.is_err(),
        "result={summary} before={before:?} after={after:?} {}",
        case_diag(f.temp.path(), &hook, &f.temp.path().join("git-hook-ready"))
    );
    let intent = f
        .db
        .pair_refresh_operation(prepared_job.pins().git.operation_id)
        .unwrap();
    assert_eq!(intent.phase, PairRefreshPhase::GitIntent);
    assert_eq!(intent.svn_receipt.as_ref().unwrap().revision, 5);
    assert!(intent.git_receipt_sha.is_none());
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    let requests = std::fs::read_to_string(&f.log)
        .unwrap()
        .lines()
        .filter(|line| line.contains("/git-receive-pack"))
        .count();
    assert!(requests > 0);
    let before = journal_state(&f);
    std::fs::remove_file(hook).unwrap();
    assert!(
        super::super::execution::reconcile_owned_refresh_intent_with_client_assoc(
            &engine,
            &actor,
            &prepared_job,
            &busy,
            f.client.clone(),
            blocking,
        )
        .await
        .is_err()
    );
    assert!(super::super::execution::complete_owned_refresh_with_client_assoc(
        &engine,
        &actor,
        &prepared_job,
        "protected-child",
        &approved,
        &busy,
        f.client.clone(),
        blocking,
    )
    .await
    .is_err());
    assert_eq!(journal_state(&f), before);
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        requests
    );
    assert_eq!(
        crate::svn::SvnClient::new(format!("{}/branches/f", f.root), "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert_eq!(
        f.target
            .find_reference("refs/heads/feature")
            .unwrap()
            .target(),
        Some(f.baseline)
    );
    assert_eq!(parent_row(&f), parent_before);
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None)
            .is_err()
    );
    prepared_repo.require_entered_witness("entered-witness")?;
    crate::process::close_case_registration(session)?;
    super::super::execution::observe_retained(blocking).await.map_err(|error| error.to_string())?;
    let blocking_proof = super::super::execution::blocking_proof(session)?;
    prepared_repo.record_blocking_joined(blocking_proof)?;
    f.complete_owned_server().await?;
    prepared_repo.record_server_completed();
    crate::process::finish_case_resource(native_leases)?;
    prepared_repo.seal()?;
    Ok(())
}

#[cfg(unix)]
async fn packaged_f09_disconnect(
    manifest: packaged_call_site::HarnessValidatedH,
    boundary: &packaged_call_site::ProcessBoundary,
) -> Result<(), String> {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    boundary.assert_held();
    let selected = packaged_call_site::attested_entry_from_validated_metadata("svn-disconnect-barrier-v1")?;
    let session = boundary.context();
    let blocking = super::super::execution::BlockingAssoc::Case(session);
    let _serial = SERIAL.lock().await;
    let mut prepared_slot: Option<packaged_call_site::PreparedRepo> = None;
    let mut f = Fixture::open(Some(&mut |case_root, _repository| {
        let repository = case_root.join("svn-repository");
        let mut prepared = packaged_call_site::prepare_existing_repository(
            boundary,
            &manifest,
            &selected,
            "svn-disconnect-barrier-v1",
            "F09",
            case_root,
            Some(&repository),
            true,
            None,
        )?;
        prepared.install_admitted_hook()?;
        prepared.recheck_hook()?;
        let context = packaged_call_site::packaged_spawn_context(&prepared);
        prepared_slot = Some(prepared);
        Ok(context)
    }))?;
    let mut prepared_repo =
        prepared_slot.ok_or_else(|| "prepared session missing before launch".to_string())?;
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let root = std::sync::Arc::clone(&f.temp);
    let native_leases = crate::process::register_case_resource(session, || {
        Ok(CaseNativeLeases { _lock, _fence, _root: root })
    })?;
    let service = RefreshService::with_fixture_client(engine, f.client.clone());
    prepared_repo.arm_shutdown(service.clone())?;
    let token = "disposable-workspace-session";
    let approval = match service
        .prepare(token.into(), "child".into(), "disconnected-request".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(approval) => approval,
        other => panic!("unexpected {other:?}"),
    };
    let _svn_diag = SvnDiag::arm(f.temp.path());
    let handle = approval.preparation_id;
    let digest = approval.approval_digest.clone();
    let request_service = service.clone();
    let mut owned = packaged_call_site::OwnedWork::new();
    owned.spawn_task(async move {
        let _ = request_service
            .act_case(
                token.into(),
                "child",
                "disconnected-request",
                handle,
                digest,
                RefreshAction::Execute,
                session,
            )
            .await;
    });
    packaged_call_site::wait_marker_async(
        f.temp.path(),
        "http-disconnect-ready",
        std::time::Duration::from_secs(45),
    )
    .await?;
    prepared_repo.require_marker(
        "http-disconnect-ready",
        packaged_call_site::ready_bytes(packaged_call_site::CaseId::SvnDisconnect),
    )?;
    packaged_call_site::wait_marker_async(
        f.temp.path(),
        "http-disconnect-entered-witness",
        std::time::Duration::from_secs(45),
    )
    .await?;
    prepared_repo.require_entered_witness("http-disconnect-entered-witness")?;
    if owned.task_finished(0) {
        return Err("request ended before the disconnect barrier".to_string());
    }
    assert_eq!(
        f.db.pair_refresh_operation(approval.operation_id)
            .unwrap()
            .phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::SvnIntent
    );
    owned.abort_task(0);
    let report = owned.join_async().await?;
    if report.cancelled_tasks != 1 {
        return Err("disconnect join was not the expected cancellation".to_string());
    }
    // Request cancellation is its own join. It is not passed to qualify.
    let token_bytes = prepared_repo.release_token()?;
    packaged_call_site::publish_at(
        f.temp.path(),
        "http-disconnect-release",
        &token_bytes,
        0o644,
    )?;
    let receipt = tokio::time::timeout(std::time::Duration::from_secs(45), async {
        loop {
            if let Some(receipt) = service
                .receipt(token, "child", "disconnected-request")
                .unwrap()
            {
                if receipt.phase == "completed" {
                    break receipt;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(receipt.operation_id, approval.operation_id);
    assert_eq!(receipt.svn_revision, Some(5));
    let retry = service
        .act_case(
            token.into(),
            "child",
            "disconnected-request",
            approval.preparation_id,
            approval.approval_digest,
            RefreshAction::Execute,
            session,
        )
        .await
        .unwrap();
    assert_eq!(retry.operation_id, receipt.operation_id);
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        1
    );
    assert_eq!(
        crate::svn::SvnClient::new(&f.root, "", "")
            .info()
            .await
            .unwrap()
            .latest_rev,
        5
    );
    assert_eq!(
        f.db.get_repo_watermark("child").unwrap(),
        (5, receipt.git_sha.unwrap())
    );
    assert!(
        crate::db::pair_refresh_operations::require_no_pair_refresh_hold(&f.db.conn(), None).is_ok()
    );
    // Arm happened before the request. Start only after durable receipt and
    // the no-republication retry assertion, then join actual service shutdown.
    prepared_repo.start_shutdown(None)?;
    prepared_repo.join_shutdown().await?;
    prepared_repo.record_publication_joined();
    crate::process::close_case_registration(session)?;
    super::super::execution::observe_retained(blocking).await.map_err(|error| error.to_string())?;
    let blocking_proof = super::super::execution::blocking_proof(session)?;
    prepared_repo.record_blocking_joined(blocking_proof)?;
    f.complete_owned_server().await?;
    prepared_repo.record_server_completed();
    crate::process::finish_case_resource(native_leases)?;
    prepared_repo.seal()?;
    Ok(())
}

#[cfg(unix)]
async fn packaged_f10_shutdown(
    manifest: packaged_call_site::HarnessValidatedH,
    boundary: &packaged_call_site::ProcessBoundary,
) -> Result<(), String> {
    use crate::pair_refresh::service::{RefreshAction, RefreshResponse, RefreshService};
    boundary.assert_held();
    let selected = packaged_call_site::attested_entry_from_validated_metadata("svn-shutdown-barrier-v1")?;
    let session = boundary.context();
    let blocking = super::super::execution::BlockingAssoc::Case(session);
    let _serial = SERIAL.lock().await;
    let mut prepared_slot: Option<packaged_call_site::PreparedRepo> = None;
    let mut f = Fixture::open(Some(&mut |case_root, _repository| {
        let repository = case_root.join("svn-repository");
        let mut prepared = packaged_call_site::prepare_existing_repository(
            boundary,
            &manifest,
            &selected,
            "svn-shutdown-barrier-v1",
            "F10",
            case_root,
            Some(&repository),
            true,
            None,
        )?;
        prepared.install_admitted_hook()?;
        prepared.recheck_hook()?;
        let context = packaged_call_site::packaged_spawn_context(&prepared);
        prepared_slot = Some(prepared);
        Ok(context)
    }))?;
    let mut prepared_repo =
        prepared_slot.ok_or_else(|| "prepared session missing before launch".to_string())?;
    let engine = std::sync::Arc::new(f.engine());
    let _lock = crate::data_dir::acquire(f.temp.path()).unwrap();
    let _fence = crate::writer_fence::claim(engine.db(), f.temp.path()).unwrap();
    let root = std::sync::Arc::clone(&f.temp);
    let native_leases = crate::process::register_case_resource(session, || {
        Ok(CaseNativeLeases { _lock, _fence, _root: root })
    })?;
    let service = RefreshService::with_fixture_client(engine, f.client.clone());
    prepared_repo.arm_shutdown(service.clone())?;
    let token = "disposable-workspace-session";
    let approval = match service
        .prepare(token.into(), "child".into(), "shutdown-request".into())
        .await
        .unwrap()
    {
        RefreshResponse::Approval(value) => value,
        other => panic!("unexpected {other:?}"),
    };
    let _svn_diag = SvnDiag::arm(f.temp.path());
    let request_service = service.clone();
    let handle = approval.preparation_id;
    let digest = approval.approval_digest;
    let mut request = packaged_call_site::OwnedWork::new();
    request.spawn_task(async move {
        let _ = request_service
            .act_case(
                token.into(),
                "child",
                "shutdown-request",
                handle,
                digest,
                RefreshAction::Execute,
                session,
            )
            .await;
    });
    packaged_call_site::wait_marker_async(
        f.temp.path(),
        "shutdown-ready",
        std::time::Duration::from_secs(45),
    )
    .await?;
    prepared_repo.require_marker(
        "shutdown-ready",
        packaged_call_site::ready_bytes(packaged_call_site::CaseId::SvnShutdown),
    )?;
    packaged_call_site::wait_marker_async(
        f.temp.path(),
        "shutdown-entered-witness",
        std::time::Duration::from_secs(45),
    )
    .await?;
    prepared_repo.require_entered_witness("shutdown-entered-witness")?;
    if request.task_finished(0) {
        return Err("request ended before the shutdown barrier".to_string());
    }
    assert_eq!(
        f.db.pair_refresh_operation(approval.operation_id)
            .unwrap()
            .phase,
        crate::db::pair_refresh_operations::PairRefreshPhase::SvnIntent
    );
    request.abort_task(0);
    let request_report = request.join_async().await?;
    if request_report.cancelled_tasks != 1 {
        return Err("shutdown request join was not the expected cancellation".to_string());
    }
    // Request cancellation is its own join. Service completion is qualified later.
    let (started, ready) = tokio::sync::oneshot::channel();
    prepared_repo.start_shutdown(Some(started))?;
    ready.await.map_err(|_| "shutdown task ended before it started".to_string())?;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    if prepared_repo.shutdown_finished()? {
        return Err("shutdown joined before release; that is not the sealed order".to_string());
    }
    assert!(service
        .prepare(token.into(), "child".into(), "during-shutdown".into())
        .await
        .is_err());
    let token_bytes = prepared_repo.release_token()?;
    packaged_call_site::publish_at(f.temp.path(), "shutdown-release", &token_bytes, 0o644)?;
    prepared_repo.join_shutdown().await?;
    prepared_repo.record_publication_joined();
    let receipt = service
        .receipt(token, "child", "shutdown-request")
        .unwrap()
        .unwrap();
    assert_eq!(receipt.operation_id, approval.operation_id);
    assert_eq!(receipt.phase, "completed");
    assert_eq!(receipt.svn_revision, Some(5));
    assert_eq!(
        std::fs::read_to_string(&f.log)
            .unwrap()
            .lines()
            .filter(|line| line.contains("/git-receive-pack"))
            .count(),
        1
    );
    assert!(crate::busy::try_acquire_pair("child", "parent").is_some());
    crate::process::close_case_registration(session)?;
    super::super::execution::observe_retained(blocking).await.map_err(|error| error.to_string())?;
    let blocking_proof = super::super::execution::blocking_proof(session)?;
    prepared_repo.record_blocking_joined(blocking_proof)?;
    f.complete_owned_server().await?;
    prepared_repo.record_server_completed();
    crate::process::finish_case_resource(native_leases)?;
    prepared_repo.seal()?;
    Ok(())
}
